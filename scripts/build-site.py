#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the TessIFC website: the landing page, the documentation and the viewer.

    python scripts/build-site.py [--out dist/site] [--base /]

The result is a static directory that any web server, including GitHub Pages,
can serve. Build the browser WASM package first, or the viewer will not start:

    python scripts/build-wasm.py --target web
    python scripts/build-site.py
    python -m http.server 8000 --bind 127.0.0.1 --directory dist/site

Every page is a directory with an index.html, so the site is reached at
<base>/, the viewer at <base>/viewer/ and each guide at <base>/docs/<name>/.
The old <name>.html addresses redirect to the new ones.

The build downloads nothing and reads nothing outside this repository. The
Markdown renderer covers the subset the documentation uses: headings,
paragraphs, lists, tables, fenced code, block quotes, rules, links, images and
inline emphasis or code. Mermaid diagrams are drawn in the browser by a pinned
copy of Mermaid fetched from a CDN when a page contains one; without it the
diagram source stays readable as a code block.
"""

from __future__ import annotations

import argparse
import html
import re
import shutil
import sys
import tempfile
import stat
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The documentation set, in reading order: source file, sidebar label and the
# one-line description shown on the landing page and the documentation index.
PAGES = [
    ("getting-started.md", "Getting started", "Build the kernel, open the viewer and convert a file."),
    ("sdk.md", "SDK and API", "The packages, the API surface and what each call promises."),
    ("preview.md", "Developer preview", "What the preview covers, its limits and how to validate results."),
    ("architecture.md", "Architecture", "How a file becomes triangles, crate by crate, with diagrams."),
    ("igp-format.md", "IGP format", "The mesh container the kernel writes and how to read it."),
    ("coverage.md", "IFC coverage", "Which classes and representations are supported today."),
    ("editing.md", "Editing", "Changing attributes without rewriting the rest of the file."),
]

EXTRA = [
    (Path("CONTRIBUTING.md"), "contributing", "Contributing", "How to build, test and add evaluators."),
    (Path("SECURITY.md"), "security", "Security", "How to report a vulnerability."),
]

REPO_URL = "https://github.com/nbharathik/tessifc"

# Pinned so that a page renders the same way next year. Only loaded by pages
# that contain a diagram.
MERMAID_URL = "https://cdn.jsdelivr.net/npm/mermaid@11.17.2/dist/mermaid.esm.min.mjs"

# The URL prefix the site is served from. Set once per build so that links
# rendered from Markdown are absolute and work from any page depth.
BASE = "/"


def page_stem(source: str) -> str:
    return Path(source).stem


def docs_url(stem: str) -> str:
    return f"{BASE}docs/{stem}/"


# --------------------------------------------------------------- markdown


def inline(text: str) -> str:
    """Render the inline subset. Code spans are protected before anything else."""
    spans: list[str] = []

    def keep(match: re.Match[str]) -> str:
        spans.append(f"<code>{html.escape(match.group(1))}</code>")
        return f"\x00{len(spans) - 1}\x00"

    text = re.sub(r"`([^`]+)`", keep, text)
    text = html.escape(text)
    text = re.sub(r"!\[([^\]]*)\]\(([^)\s]+)\)", image, text)
    text = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", link, text)
    text = re.sub(r"&lt;(https?://[^&\s]+)&gt;", r'<a href="\1">\1</a>', text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<![\w*])\*([^*\n]+)\*(?![\w*])", r"<em>\1</em>", text)
    return re.sub(r"\x00(\d+)\x00", lambda m: spans[int(m.group(1))], text)


def asset_url(target: str) -> str | None:
    """The site address of a file under docs/assets, or None for anything else."""
    for prefix in ("docs/assets/", "assets/", "../docs/assets/"):
        if target.startswith(prefix):
            return f"{BASE}assets/{target[len(prefix):]}"
    return None


def image(match: re.Match[str]) -> str:
    alt, target = match.group(1), match.group(2)
    src = target if target.startswith(("http://", "https://")) else asset_url(target) or target
    return f'<img src="{src}" alt="{alt}" loading="lazy" />'


def link(match: re.Match[str]) -> str:
    """Rewrite in-repository links: docs to their page, everything else to GitHub."""
    label, target = match.group(1), match.group(2)
    if target.startswith(("http://", "https://", "#", "mailto:")):
        external = ' target="_blank" rel="noopener"' if target.startswith("http") else ""
        return f'<a href="{target}"{external}>{label}</a>'
    clean = target.split("#")[0]
    anchor = target[len(clean):]
    if clean.startswith("docs/"):
        clean = clean[len("docs/"):]
    if clean.endswith(".md"):
        name = Path(clean).name
        known = {source: docs_url(page_stem(source)) for source, _, _ in PAGES}
        known.update({source.name: docs_url(stem) for source, stem, _, _ in EXTRA})
        if name in known:
            return f'<a href="{known[name]}{anchor}">{label}</a>'
    asset = asset_url(clean)
    if asset:
        return f'<a href="{asset}">{label}</a>'
    while clean.startswith("../"):
        clean = clean[3:]
    return f'<a href="{REPO_URL}/blob/main/{clean}{anchor}" target="_blank" rel="noopener">{label}</a>'


def slug(text: str) -> str:
    plain = re.sub(r"<[^>]+>", "", text)
    return re.sub(r"[^a-z0-9]+", "-", plain.lower()).strip("-") or "section"


def render(source: str) -> tuple[str, list[tuple[int, str, str]]]:
    """Return the page body and its heading outline as (level, id, text)."""
    lines = source.splitlines()
    out: list[str] = []
    outline: list[tuple[int, str, str]] = []
    index = 0
    while index < len(lines):
        line = lines[index]

        if line.startswith("<!--") and "-->" in line:
            index += 1
            continue

        if line.startswith("```"):
            language = line[3:].strip()
            index += 1
            block: list[str] = []
            while index < len(lines) and not lines[index].startswith("```"):
                block.append(lines[index])
                index += 1
            index += 1
            code = html.escape("\n".join(block))
            if language == "mermaid":
                out.append(f'<figure class="diagram"><pre class="mermaid">{code}</pre></figure>')
            else:
                classes = f' class="lang-{html.escape(language)}"' if language else ""
                out.append(f'<div class="code"><pre><code{classes}>{code}</code></pre></div>')
            continue

        heading = re.match(r"^(#{1,6})\s+(.*)$", line)
        if heading:
            level = len(heading.group(1))
            text = inline(heading.group(2).strip())
            anchor = slug(text)
            if 2 <= level <= 3:
                outline.append((level, anchor, re.sub(r"<[^>]+>", "", text)))
            out.append(f'<h{level} id="{anchor}">{text}</h{level}>')
            index += 1
            continue

        if re.match(r"^(-{3,}|\*{3,})\s*$", line):
            out.append("<hr />")
            index += 1
            continue

        if line.lstrip().startswith("|"):
            table, index = read_table(lines, index)
            out.append(table)
            continue

        if re.match(r"^\s*([-*+]|\d+\.)\s+", line):
            items, index = read_list(lines, index)
            out.append(items)
            continue

        if line.startswith(">"):
            quote: list[str] = []
            while index < len(lines) and lines[index].startswith(">"):
                quote.append(lines[index].lstrip(">").strip())
                index += 1
            out.append(f"<blockquote>{inline(' '.join(quote))}</blockquote>")
            continue

        if not line.strip():
            index += 1
            continue

        paragraph: list[str] = []
        while index < len(lines) and lines[index].strip() and not starts_block(lines[index]):
            paragraph.append(lines[index].strip())
            index += 1
        out.append(f"<p>{inline(' '.join(paragraph))}</p>")

    return "\n".join(out), outline


def starts_block(line: str) -> bool:
    return (
        line.startswith(("```", "#", ">"))
        or line.lstrip().startswith("|")
        or bool(re.match(r"^\s*([-*+]|\d+\.)\s+", line))
        or bool(re.match(r"^(-{3,}|\*{3,})\s*$", line))
    )


def read_table(lines: list[str], index: int) -> tuple[str, int]:
    rows: list[list[str]] = []
    while index < len(lines) and lines[index].lstrip().startswith("|"):
        cells = [cell.strip() for cell in lines[index].strip().strip("|").split("|")]
        rows.append(cells)
        index += 1
    if not rows:
        return "", index
    body = rows[1:]
    aligned = bool(body) and all(re.fullmatch(r":?-{2,}:?", cell) for cell in body[0] if cell)
    if aligned:
        body = body[1:]
    head = "".join(f"<th>{inline(cell)}</th>" for cell in rows[0])
    out = [f'<div class="table"><table><thead><tr>{head}</tr></thead><tbody>']
    for row in body:
        out.append("<tr>" + "".join(f"<td>{inline(cell)}</td>" for cell in row) + "</tr>")
    out.append("</tbody></table></div>")
    return "".join(out), index


def read_list(lines: list[str], index: int) -> tuple[str, int]:
    ordered = bool(re.match(r"^\s*\d+\.\s+", lines[index]))
    items: list[str] = []
    current: list[str] = []
    while index < len(lines):
        line = lines[index]
        start = re.match(r"^\s*([-*+]|\d+\.)\s+(.*)$", line)
        if start:
            if current:
                items.append(" ".join(current))
            current = [start.group(2).strip()]
            index += 1
            continue
        # An indented continuation belongs to the item above it.
        if line.startswith(("  ", "\t")) and line.strip() and current:
            current.append(line.strip())
            index += 1
            continue
        break
    if current:
        items.append(" ".join(current))
    tag = "ol" if ordered else "ul"
    body = "".join(f"<li>{inline(item)}</li>" for item in items)
    return f"<{tag}>{body}</{tag}>", index


# ------------------------------------------------------------------ site


# Runs before the stylesheet so the first paint already has the right theme.
HEAD_SCRIPT = """(() => {
  let mode = "system";
  try {
    const saved = localStorage.getItem("tessifc.theme");
    if (saved === "light" || saved === "dark" || saved === "system") mode = saved;
  } catch {
  }
  const dark = mode === "dark" || (mode === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.documentElement.dataset.themeMode = mode;
})();"""


def shell(*, title: str, description: str, body: str, nav: str, kind: str, diagrams: bool = False) -> str:
    mermaid = f'\n<script>window.TESSIFC_MERMAID = "{MERMAID_URL}";</script>' if diagrams else ""
    return f"""<!doctype html>
<!-- SPDX-License-Identifier: Apache-2.0 -->
<html lang="en" data-theme="light">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta name="color-scheme" content="light dark" />
<meta name="description" content="{html.escape(description)}" />
<title>{html.escape(title)}</title>
<link rel="icon" href="{BASE}favicon.svg" />
<script>{HEAD_SCRIPT}</script>
<link rel="stylesheet" href="{BASE}site.css" />{mermaid}
</head>
<body class="{kind}">
<a class="skip" href="#content">Skip to the content</a>
<header class="top">
  <a class="brand" href="{BASE}"><img src="{BASE}favicon.svg" alt="" width="22" height="22" /><span>TessIFC</span></a>
  <nav class="topnav" aria-label="Site">
    <a href="{BASE}docs/">Docs</a>
    <a href="{BASE}viewer/">Viewer</a>
    <a href="{REPO_URL}" target="_blank" rel="noopener">GitHub</a>
  </nav>
  <button class="themer" type="button" id="themer" aria-label="Switch the theme">
    <svg class="sun" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></svg>
    <svg class="moon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 14.5A8.5 8.5 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5z" /></svg>
    <svg class="auto" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><circle cx="12" cy="12" r="8.5" /><path d="M12 3.5v17A8.5 8.5 0 0 0 12 3.5z" fill="currentColor" stroke="none" /></svg>
  </button>
</header>
{nav}
<main id="content">
{body}
</main>
<footer class="foot">
  <p>Apache-2.0. IFC is a standard of buildingSMART International. TessIFC is an independent implementation and is not endorsed by buildingSMART.</p>
  <p><a href="{BASE}docs/">Documentation</a> <a href="{BASE}viewer/">Viewer</a> <a href="{REPO_URL}" target="_blank" rel="noopener">GitHub</a> <a href="{REPO_URL}/releases" target="_blank" rel="noopener">Releases</a></p>
</footer>
<script src="{BASE}site.js"></script>
</body>
</html>
"""


def sidebar(entries: list[tuple[str, str]], current: str, outline: list[tuple[int, str, str]]) -> str:
    links = [f'<a href="{BASE}docs/"{" class=\"on\"" if current == "" else ""}>Overview</a>']
    for stem, label in entries:
        active = ' class="on"' if stem == current else ""
        links.append(f'<a href="{docs_url(stem)}"{active}>{html.escape(label)}</a>')
        if stem == current and outline:
            links.append('<span class="sub">')
            for level, anchor, text in outline:
                links.append(f'<a class="l{level}" href="#{anchor}">{html.escape(text)}</a>')
            links.append("</span>")
    return (
        '<aside class="side"><nav aria-label="Documentation">'
        f'<p class="sidetitle">Documentation</p>{"".join(links)}</nav></aside>'
    )


def doc_list(entries: list[tuple[str, str, str]]) -> str:
    items = "".join(
        f'<li><a href="{docs_url(stem)}"><strong>{html.escape(label)}</strong>'
        f"<span>{html.escape(blurb)}</span></a></li>"
        for stem, label, blurb in entries
    )
    return f'<ul class="doclist">{items}</ul>'


def docs_index() -> str:
    guides = [(page_stem(source), label, blurb) for source, label, blurb in PAGES]
    project = [(stem, label, blurb) for _, stem, label, blurb in EXTRA]
    return f"""<article class="prose">
<h1>Documentation</h1>
<p>Everything from the first build to the container format. Start with the getting started
guide, then read the preview contract before you rely on a mesh downstream.</p>
{doc_list(guides)}
<h2 id="project">Project</h2>
{doc_list(project)}
</article>
"""


def landing() -> str:
    guides = [(page_stem(source), label, blurb) for source, label, blurb in PAGES]
    return f"""
<section class="hero">
  <div class="pitch">
    <p class="preview">Developer preview 0.1</p>
    <h1>IFC files in.<br />Render-ready meshes out.</h1>
    <p class="lede">TessIFC is an open-source IFC geometry kernel written in Rust. It reads
    IFC2X3, IFC4 and IFC4X3 and produces triangle meshes for the browser, Node and the
    command line.</p>
    <div class="cta">
      <a class="button primary" href="{BASE}viewer/">Open the viewer</a>
      <a class="button" href="{BASE}docs/">Read the docs</a>
      <a class="button quiet" href="{REPO_URL}" target="_blank" rel="noopener">GitHub</a>
    </div>
    <p class="note">The viewer runs entirely in your browser. Files never leave your machine.</p>
  </div>
  <dl class="facts">
    <div><dt>Reads</dt><dd>IFC2X3, IFC4, IFC4X3</dd></div>
    <div><dt>Writes</dt><dd>IGP, a compact triangle mesh container</dd></div>
    <div><dt>Runs in</dt><dd>Browser, Node.js, command line</dd></div>
    <div><dt>Built with</dt><dd>Rust, compiled to WebAssembly and native code</dd></div>
    <div><dt>Licence</dt><dd>Apache-2.0</dd></div>
  </dl>
</section>

<figure class="shot">
  <img src="{BASE}assets/viewer.png" alt="The TessIFC viewer with a building model open, showing the structure tree, the properties panel and a section cut" />
</figure>

<section class="points">
  <div>
    <h2>Streams while it works</h2>
    <p>Geometry arrives in batches, so the first storey is on screen while the rest is still being evaluated.</p>
  </div>
  <div>
    <h2>Keeps every element id</h2>
    <p>Each mesh stays linked to the IFC element it came from, and each problem is reported against that element.</p>
  </div>
  <div>
    <h2>Reports what it could not build</h2>
    <p>Unsupported, repaired and degraded geometry is listed per product, so the limits are visible before you use a mesh.</p>
  </div>
  <div>
    <h2>Edits without rewriting</h2>
    <p>Change an attribute and export the file with every other byte left exactly as it was.</p>
  </div>
</section>

<section class="start">
  <h2>Quick start</h2>
  <div class="starts">
    <div>
      <h3>Browser</h3>
      <div class="code"><pre><code>import init, {{ Kernel }} from "./bindings/wasm/pkg/tessifc_wasm.js";

await init();
const kernel = new Kernel();
const id = kernel.openModel(bytes);
kernel.evaluateGeometry(id, "{{}}");
const pack = kernel.takePack(id);
kernel.closeModel(id);
kernel.free();</code></pre></div>
    </div>
    <div>
      <h3>Command line</h3>
      <div class="code"><pre><code>cargo build --locked --release -p tessifc-cli

target/release/tessifc info model.ifc
target/release/tessifc convert model.ifc -o model.igp</code></pre></div>
      <p class="aside">The <a href="{docs_url("getting-started")}">getting started guide</a> covers the viewer build, Node and Rust.</p>
    </div>
  </div>
</section>

<section class="guides">
  <h2>Documentation</h2>
  {doc_list(guides)}
</section>
"""


def redirect(target: str) -> str:
    return f"""<!doctype html>
<!-- SPDX-License-Identifier: Apache-2.0 -->
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta http-equiv="refresh" content="0; url={target}" />
<link rel="canonical" href="{target}" />
<title>Redirecting</title>
</head>
<body><p>This page has moved to <a href="{target}">{target}</a>.</p></body>
</html>
"""


def copy_tree(source: Path, target: Path, skip: set[str] | None = None) -> None:
    if not source.exists():
        return
    ignore = shutil.ignore_patterns(*sorted(skip)) if skip else None
    shutil.copytree(source, target, dirs_exist_ok=True, ignore=ignore)


SITE_MARKER = ".tessifc-site"
SITE_SIGNATURE = "TessIFC generated website v1\n"
SOURCE_DIRS = {".git", ".github", "crates", "bindings", "viewer", "adapters", "examples", "docs", "scripts"}


def output_path(out: Path) -> Path:
    """Only replace directories owned by this builder, outside the source tree."""
    out = out.absolute()
    for part in (out, *out.parents):
        reparse = part.exists() and getattr(part.lstat(), "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
        if part.is_symlink() or reparse:
            raise ValueError(f"output path contains a link: {part}")
    out = out.resolve()
    if out == REPO or out in REPO.parents or out == Path(out.anchor):
        raise ValueError("output must not contain the repository or a filesystem root")
    if out.is_relative_to(REPO) and out.relative_to(REPO).parts[0] in SOURCE_DIRS:
        raise ValueError("output must not be inside a source directory")
    if out.exists():
        if not out.is_dir():
            raise ValueError("output is not a directory")
        marker = out / SITE_MARKER
        if any(out.iterdir()) and (
            not marker.is_file() or marker.is_symlink()
            or marker.read_text(encoding="utf-8") != SITE_SIGNATURE
        ):
            raise ValueError("output is not an empty directory or a TessIFC generated website")
    return out


def build(out: Path, base: str) -> int:
    try:
        out = output_path(out)
        out.parent.mkdir(parents=True, exist_ok=True)
        # Finish the new site before replacing an existing generated directory.
        with tempfile.TemporaryDirectory(prefix=".tessifc-site-", dir=out.parent) as temporary:
            stage = Path(temporary) / "site"
            result = build_contents(stage, base)
            if result:
                return result
            (stage / SITE_MARKER).write_text(SITE_SIGNATURE, encoding="utf-8")
            output_path(out)
            backup = Path(temporary) / "previous"
            if out.exists():
                out.rename(backup)
            try:
                stage.rename(out)
            except OSError:
                if backup.exists():
                    backup.rename(out)
                raise
        print(f"\n  site written to {out}")
        return 0
    except (OSError, ValueError) as error:
        print(f"site build failed: {error}", file=sys.stderr)
        return 1


def build_contents(out: Path, base: str) -> int:
    global BASE
    BASE = base if base.endswith("/") else f"{base}/"

    pkg = REPO / "bindings" / "wasm" / "pkg"
    for name in ("tessifc_wasm.js", "tessifc_wasm_bg.wasm"):
        if not (pkg / name).is_file():
            print("browser WASM package is missing: run python scripts/build-wasm.py --target web", file=sys.stderr)
            return 1
    (out / "docs").mkdir(parents=True)

    entries = [(page_stem(source), label) for source, label, _ in PAGES]
    entries += [(stem, label) for _, stem, label, _ in EXTRA]

    sources = [(REPO / "docs" / source, page_stem(source), label, blurb) for source, label, blurb in PAGES]
    sources += [(REPO / path, stem, label, blurb) for path, stem, label, blurb in EXTRA]

    for path, stem, label, blurb in sources:
        if not path.exists():
            print(f"  missing {path.relative_to(REPO)}", file=sys.stderr)
            return 1
        body, outline = render(path.read_text(encoding="utf-8"))
        page = shell(
            title=f"{label} - TessIFC",
            description=blurb,
            nav=sidebar(entries, stem, outline),
            body=f'<article class="prose">{body}</article>',
            kind="doc",
            diagrams='class="mermaid"' in body,
        )
        (out / "docs" / stem).mkdir()
        (out / "docs" / stem / "index.html").write_text(page, encoding="utf-8")
        # The address the site used before pages became directories.
        (out / "docs" / f"{stem}.html").write_text(redirect(docs_url(stem)), encoding="utf-8")
        print(f"  docs/{stem}/")

    (out / "docs" / "index.html").write_text(
        shell(
            title="Documentation - TessIFC",
            description="The TessIFC guides: getting started, the SDK, the architecture, the IGP format and IFC coverage.",
            nav=sidebar(entries, "", []),
            body=docs_index(),
            kind="doc",
        ),
        encoding="utf-8",
    )
    print("  docs/")

    (out / "index.html").write_text(
        shell(
            title="TessIFC",
            description="An open-source IFC geometry kernel in Rust. Open, inspect, section and measure IFC models in the browser.",
            nav="",
            body=landing(),
            kind="home",
        ),
        encoding="utf-8",
    )
    print("  index.html")

    (out / "site.css").write_text(STYLE, encoding="utf-8")
    (out / "site.js").write_text(SCRIPT, encoding="utf-8")
    (out / "favicon.svg").write_text(FAVICON, encoding="utf-8")
    copy_tree(REPO / "docs" / "assets", out / "assets")

    # The viewer ships its page and its modules; its tests and tooling do not.
    copy_tree(REPO / "viewer" / "src", out / "viewer" / "src")
    shutil.copy2(REPO / "viewer" / "index.html", out / "viewer" / "index.html")
    copy_tree(pkg, out / "bindings" / "wasm" / "pkg")

    # GitHub Pages serves _-prefixed paths only when Jekyll is switched off.
    (out / ".nojekyll").write_text("", encoding="utf-8")
    return 0


STYLE = """/* SPDX-License-Identifier: Apache-2.0 */
:root {
  color-scheme: light;
  --bg: #ffffff;
  --surface: #f5f7fa;
  --raised: #ffffff;
  --line: #e2e6ec;
  --line-strong: #c9d0da;
  --fg: #10151c;
  --fg-2: #3f4854;
  --fg-3: #6b7482;
  --accent: #1f5eea;
  --accent-ink: #164ac0;
  --on-accent: #ffffff;
  --shadow: 0 1px 2px rgba(16, 21, 28, 0.06), 0 12px 40px rgba(16, 21, 28, 0.08);
  --font: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  --mono: ui-monospace, "Cascadia Code", "SF Mono", Menlo, Consolas, "Liberation Mono", monospace;
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --bg: #0f1216;
  --surface: #161a20;
  --raised: #1b2028;
  --line: #262c36;
  --line-strong: #3a424f;
  --fg: #f1f4f8;
  --fg-2: #c2c9d4;
  --fg-3: #8b94a3;
  --accent: #7fa8ff;
  --accent-ink: #a3c0ff;
  --on-accent: #0b1220;
  --shadow: 0 1px 2px rgba(0, 0, 0, 0.4), 0 16px 48px rgba(0, 0, 0, 0.45);
}

* { box-sizing: border-box; }
html { scroll-padding-top: 72px; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--fg);
  font-family: var(--font);
  font-size: 15.5px;
  line-height: 1.6;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}
a { color: var(--accent-ink); text-decoration-thickness: 1px; text-underline-offset: 2px; }
a:hover { color: var(--accent); }
img { max-width: 100%; height: auto; }
code, pre { font-family: var(--mono); }
:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
@media (prefers-reduced-motion: reduce) { *, *::before, *::after { transition: none !important; animation: none !important; } }

.skip { position: absolute; left: -999px; }
.skip:focus { left: 12px; top: 12px; z-index: 20; background: var(--accent); color: var(--on-accent); padding: 8px 14px; border-radius: 6px; }

/* --------------------------------------------------------------- header */
.top {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  gap: 28px;
  height: 58px;
  padding: 0 max(20px, calc((100% - 1080px) / 2));
  background: color-mix(in srgb, var(--bg) 84%, transparent);
  backdrop-filter: blur(12px);
  -webkit-backdrop-filter: blur(12px);
  border-bottom: 1px solid var(--line);
}
.brand { display: flex; align-items: center; gap: 9px; color: var(--fg); font-weight: 600; text-decoration: none; letter-spacing: -0.01em; font-size: 15px; }
.brand:hover { color: var(--fg); }
.topnav { display: flex; gap: 22px; margin-left: auto; }
.topnav a { color: var(--fg-2); text-decoration: none; font-size: 14px; }
.topnav a:hover { color: var(--fg); }
.themer { display: grid; place-items: center; width: 32px; height: 32px; padding: 0; border: 1px solid var(--line); border-radius: 8px; background: var(--raised); color: var(--fg-2); cursor: pointer; }
.themer:hover { color: var(--fg); border-color: var(--line-strong); }
.themer svg { display: none; width: 16px; height: 16px; }
.themer[data-mode="light"] .sun, .themer[data-mode="dark"] .moon, .themer[data-mode="system"] .auto, .themer:not([data-mode]) .auto { display: block; }

/* --------------------------------------------------------------- footer */
.foot { border-top: 1px solid var(--line); padding: 26px max(20px, calc((100% - 1080px) / 2)) 44px; color: var(--fg-3); font-size: 13px; }
.foot p { margin: 0 0 8px; }
.foot a { color: var(--fg-2); text-decoration: none; margin-right: 18px; }
.foot a:hover { color: var(--fg); }

/* -------------------------------------------------------------- landing */
body.home main { max-width: 1120px; margin: 0 auto; padding: 0 20px; }
.hero { display: grid; grid-template-columns: minmax(0, 7fr) minmax(0, 4fr); gap: 56px; align-items: end; padding: 80px 0 44px; }
.preview { margin: 0 0 18px; color: var(--fg-3); font-size: 14px; }
.hero h1 { margin: 0; font-size: clamp(40px, 6.4vw, 72px); line-height: 1.02; letter-spacing: -0.035em; font-weight: 600; text-wrap: balance; }
.lede { max-width: 34em; margin: 24px 0 0; color: var(--fg-2); font-size: 17.5px; line-height: 1.55; }
.cta { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 30px; }
.button {
  display: inline-flex;
  align-items: center;
  height: 42px;
  padding: 0 20px;
  border: 1px solid var(--line-strong);
  border-radius: 8px;
  background: var(--raised);
  color: var(--fg);
  font-weight: 500;
  font-size: 14.5px;
  text-decoration: none;
  transition: border-color 120ms, background-color 120ms;
}
.button:hover { color: var(--fg); border-color: var(--fg-3); }
.button.primary { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
.button.primary:hover { background: var(--accent-ink); border-color: var(--accent-ink); color: var(--on-accent); }
.button.quiet { background: none; border-color: transparent; color: var(--fg-2); }
.button.quiet:hover { border-color: var(--line-strong); }
.note { margin: 18px 0 0; color: var(--fg-3); font-size: 13.5px; }

.facts { margin: 0; padding: 4px 0 0; border-top: 1px solid var(--line-strong); font-size: 14px; }
.facts div { display: grid; grid-template-columns: 96px 1fr; gap: 16px; padding: 11px 0; border-bottom: 1px solid var(--line); }
.facts dt { margin: 0; color: var(--fg-3); }
.facts dd { margin: 0; color: var(--fg); }

.shot { margin: 20px 0 0; padding: 0; }
.shot img { display: block; width: 100%; border: 1px solid var(--line); border-radius: 12px; box-shadow: var(--shadow); }

.points { display: grid; gap: 32px 40px; grid-template-columns: repeat(4, minmax(0, 1fr)); margin: 72px 0 0; padding-top: 32px; border-top: 1px solid var(--line); }
.points h2 { margin: 0 0 8px; font-size: 16px; font-weight: 600; letter-spacing: -0.01em; }
.points p { margin: 0; color: var(--fg-2); font-size: 14.5px; }

.start { margin-top: 80px; }
.start h2, .guides h2 { margin: 0 0 20px; font-size: 24px; letter-spacing: -0.02em; font-weight: 600; }
.starts { display: grid; gap: 22px; grid-template-columns: repeat(2, minmax(0, 1fr)); }
.starts > div { min-width: 0; }
.starts h3 { margin: 0 0 10px; font-size: 14px; font-weight: 500; color: var(--fg-3); }
.aside { margin: 14px 0 0; color: var(--fg-3); font-size: 14px; }

.guides { margin: 72px 0 96px; }
.doclist { list-style: none; margin: 0; padding: 0; display: grid; gap: 0 40px; grid-template-columns: repeat(2, minmax(0, 1fr)); }
.doclist li { border-top: 1px solid var(--line); }
.doclist a { display: flex; flex-direction: column; gap: 3px; padding: 14px 0 15px; color: var(--fg); text-decoration: none; }
.doclist a:hover strong { color: var(--accent); }
.doclist strong { font-weight: 600; font-size: 15px; }
.doclist span { color: var(--fg-3); font-size: 14px; }

@media (max-width: 960px) {
  .hero { grid-template-columns: minmax(0, 1fr); gap: 36px; padding-top: 56px; }
  .points { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
@media (max-width: 640px) {
  .hero { padding-top: 40px; }
  .points, .starts, .doclist { grid-template-columns: minmax(0, 1fr); }
  .topnav { gap: 16px; }
}

/* ------------------------------------------------------------ doc pages */
body.doc main { width: 100%; max-width: 1120px; margin: 0 auto; padding: 0 20px 72px; }
.side { padding: 24px 20px 24px; }
.sidetitle { margin: 0 0 10px; padding: 0 10px; color: var(--fg-3); font-size: 13px; font-weight: 500; }
.side nav a { display: block; padding: 5px 10px; border-radius: 6px; color: var(--fg-2); text-decoration: none; font-size: 14px; }
.side nav a:hover { background: var(--surface); color: var(--fg); }
.side nav a.on { background: var(--surface); color: var(--fg); font-weight: 600; }
.side .sub { display: block; margin: 2px 0 8px 10px; border-left: 1px solid var(--line); padding-left: 6px; }
.side .sub a { padding: 3px 8px; color: var(--fg-3); font-size: 13px; }
.side .sub a.l3 { padding-left: 18px; }

@media (min-width: 900px) {
  body.doc {
    display: grid;
    grid-template-columns: minmax(20px, 1fr) 232px minmax(0, 848px) minmax(20px, 1fr);
    grid-template-areas: "top top top top" ". side main ." "foot foot foot foot";
  }
  .top { grid-area: top; }
  .side { grid-area: side; position: sticky; top: 58px; align-self: start; max-height: calc(100vh - 58px); overflow: auto; padding: 28px 12px 28px 0; }
  body.doc main { grid-area: main; margin: 0; max-width: none; padding: 0 20px 72px 44px; }
  .foot { grid-area: foot; }
}
@media (max-width: 899px) { .side { border-bottom: 1px solid var(--line); } .side .sub { display: none; } }

.prose { padding-top: 28px; }
.prose h1 { margin: 0 0 16px; font-size: 34px; line-height: 1.15; letter-spacing: -0.025em; font-weight: 600; }
.prose h2 { margin: 44px 0 12px; padding-top: 18px; border-top: 1px solid var(--line); font-size: 22px; letter-spacing: -0.015em; font-weight: 600; }
.prose h3 { margin: 28px 0 8px; font-size: 16.5px; font-weight: 600; }
.prose p { margin: 0 0 14px; color: var(--fg-2); max-width: 70ch; }
.prose li { margin-bottom: 5px; color: var(--fg-2); }
.prose ul, .prose ol { margin: 0 0 14px; padding-left: 22px; }
.prose blockquote { margin: 0 0 16px; padding: 10px 16px; border-left: 3px solid var(--accent); background: var(--surface); border-radius: 0 8px 8px 0; color: var(--fg-2); }
.prose hr { border: none; border-top: 1px solid var(--line); margin: 30px 0; }
.prose code { background: var(--surface); border: 1px solid var(--line); border-radius: 5px; padding: 1px 5px; font-size: 0.86em; }
.prose strong { color: var(--fg); }
.prose .doclist { margin-top: 8px; }

.code { margin: 0 0 16px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); overflow: hidden; }
.code pre { margin: 0; padding: 14px 16px; overflow-x: auto; font-size: 13px; line-height: 1.6; }
.code code { background: none; border: none; padding: 0; font-size: inherit; }

.table { margin: 0 0 18px; overflow-x: auto; border: 1px solid var(--line); border-radius: 10px; }
.table table { width: 100%; border-collapse: collapse; font-size: 13.5px; }
.table th, .table td { padding: 8px 12px; text-align: left; border-bottom: 1px solid var(--line); vertical-align: top; }
.table th { background: var(--surface); font-weight: 600; white-space: nowrap; }
.table tr:last-child td { border-bottom: none; }

.diagram { margin: 0 0 18px; padding: 18px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); overflow-x: auto; }
.diagram pre.mermaid { margin: 0; font-size: 12.5px; line-height: 1.55; white-space: pre; }
.diagram .drawn { display: flex; justify-content: center; }
.diagram .drawn svg { max-width: 100%; height: auto; }
"""

SCRIPT = """// SPDX-License-Identifier: Apache-2.0
(() => {
  const root = document.documentElement;
  const modes = ["system", "light", "dark"];
  const media = matchMedia("(prefers-color-scheme: dark)");
  let mode = modes.includes(root.dataset.themeMode) ? root.dataset.themeMode : "system";

  const apply = () => {
    const dark = mode === "dark" || (mode === "system" && media.matches);
    root.dataset.theme = dark ? "dark" : "light";
    root.dataset.themeMode = mode;
    const button = document.getElementById("themer");
    if (button) {
      button.dataset.mode = mode;
      button.title = `Theme: ${mode}. Click for the next one.`;
    }
    drawDiagrams();
  };

  media.addEventListener("change", () => {
    if (mode === "system") apply();
  });
  document.getElementById("themer")?.addEventListener("click", () => {
    mode = modes[(modes.indexOf(mode) + 1) % modes.length];
    try {
      localStorage.setItem("tessifc.theme", mode);
    } catch {
    }
    apply();
  });

  // Diagrams are drawn in the browser so the documentation source stays plain
  // Markdown. If the library cannot be loaded the source stays visible.
  const blocks = [...document.querySelectorAll("pre.mermaid")];
  let library = null;
  let drawing = false;
  let again = false;
  let renders = 0;

  async function drawDiagrams() {
    if (!blocks.length || !window.TESSIFC_MERMAID) return;
    if (drawing) {
      again = true;
      return;
    }
    drawing = true;
    try {
      if (!library) library = await import(window.TESSIFC_MERMAID).then((m) => m.default);
      const dark = root.dataset.theme === "dark";
      const style = getComputedStyle(root);
      const token = (name) => style.getPropertyValue(name).trim();
      const bg = token("--bg");
      const surface = token("--surface");
      const raised = token("--raised");
      const line = token("--line-strong");
      const fg = token("--fg");
      const fg2 = token("--fg-2");
      const fg3 = token("--fg-3");
      const accent = token("--accent");
      library.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        theme: "base",
        themeVariables: {
          darkMode: dark,
          fontFamily: getComputedStyle(document.body).fontFamily,
          fontSize: "14px",
          background: surface,
          primaryColor: raised,
          primaryTextColor: fg,
          primaryBorderColor: line,
          secondaryColor: surface,
          secondaryTextColor: fg,
          secondaryBorderColor: line,
          tertiaryColor: bg,
          tertiaryTextColor: fg,
          tertiaryBorderColor: line,
          lineColor: fg3,
          textColor: fg2,
          edgeLabelBackground: surface,
          clusterBkg: bg,
          clusterBorder: line,
          titleColor: fg,
          actorBkg: raised,
          actorBorder: line,
          actorTextColor: fg,
          actorLineColor: fg3,
          signalColor: fg2,
          signalTextColor: fg2,
          labelBoxBkgColor: surface,
          labelBoxBorderColor: line,
          labelTextColor: fg,
          loopTextColor: fg,
          noteBkgColor: surface,
          noteBorderColor: line,
          noteTextColor: fg,
          activationBkgColor: surface,
          activationBorderColor: accent,
          sequenceNumberColor: bg,
        },
        flowchart: { htmlLabels: true, curve: "basis" },
      });
      for (const pre of blocks) {
        const source = pre.dataset.source ?? (pre.dataset.source = pre.textContent);
        try {
          const { svg } = await library.render(`diagram-${(renders += 1)}`, source);
          let host = pre.nextElementSibling;
          if (!host || !host.classList.contains("drawn")) {
            host = document.createElement("div");
            host.className = "drawn";
            pre.after(host);
          }
          host.innerHTML = svg;
          pre.hidden = true;
        } catch {
          pre.hidden = false;
        }
      }
    } catch {
      // No library: the source stays readable.
    } finally {
      drawing = false;
      if (again) {
        again = false;
        drawDiagrams();
      }
    }
  }

  apply();
})();
"""

FAVICON = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
<rect width="64" height="64" rx="14" fill="#0f1115"/>
<polygon points="32,6 56,19 32,32 8,19" fill="#93b9fb"/>
<polygon points="8,19 32,32 32,58 8,45" fill="#3b82f6"/>
<polygon points="56,19 56,45 32,58 32,32" fill="#1e5bd0"/>
</svg>
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="dist/site", help="generated output directory (default: dist/site)")
    parser.add_argument("--base", default="/", help="URL prefix the site is served from")
    args = parser.parse_args()
    base = args.base if args.base.endswith("/") else f"{args.base}/"
    out = Path(args.out)
    if not out.is_absolute():
        out = REPO / out
    print(f"building the site into {out}")
    return build(out, base)


if __name__ == "__main__":
    raise SystemExit(main())
