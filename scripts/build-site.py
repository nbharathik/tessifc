#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the TessIFC website: the landing page, the documentation and the viewer.

    python scripts/build-site.py [--out dist/site] [--base /]

The result is a static directory that any web server, including GitHub Pages,
can serve. Build the browser WASM package first, or the viewer will not start:

    python scripts/build-wasm.py --target web
    python scripts/build-site.py
    python -m http.server 8000 --bind 127.0.0.1 --directory dist/site

Nothing is downloaded and nothing outside this repository is read. The
Markdown renderer covers the subset the documentation uses: headings,
paragraphs, lists, tables, fenced code, block quotes, rules, links, images and
inline emphasis or code.
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

# The documentation set, in reading order. The title is the sidebar label.
PAGES = [
    ("getting-started.md", "Getting started"),
    ("sdk.md", "SDK and API"),
    ("preview.md", "Developer preview"),
    ("architecture.md", "Architecture"),
    ("igp-format.md", "IGP format"),
    ("coverage.md", "IFC coverage"),
    ("editing.md", "Editing"),
]

EXTRA = [
    (Path("CONTRIBUTING.md"), "contributing.html", "Contributing"),
    (Path("SECURITY.md"), "security.html", "Security"),
]

REPO_URL = "https://github.com/nbharathik/tessifc"


# --------------------------------------------------------------- markdown


def inline(text: str) -> str:
    """Render the inline subset. Code spans are protected before anything else."""
    spans: list[str] = []

    def keep(match: re.Match[str]) -> str:
        spans.append(f"<code>{html.escape(match.group(1))}</code>")
        return f"\x00{len(spans) - 1}\x00"

    text = re.sub(r"`([^`]+)`", keep, text)
    text = html.escape(text)
    text = re.sub(r"!\[([^\]]*)\]\(([^)\s]+)\)", r'<img src="\2" alt="\1" loading="lazy" />', text)
    text = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", link, text)
    text = re.sub(r"&lt;(https?://[^&\s]+)&gt;", r'<a href="\1">\1</a>', text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"(?<![\w*])\*([^*\n]+)\*(?![\w*])", r"<em>\1</em>", text)
    return re.sub(r"\x00(\d+)\x00", lambda m: spans[int(m.group(1))], text)


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
        known = {source: f"{Path(source).stem}.html" for source, _ in PAGES}
        known.update({source.name: target for source, target, _ in EXTRA})
        if name in known:
            return f'<a href="{known[name]}{anchor}">{label}</a>'
    if clean.startswith(("docs/assets/", "assets/")):
        return f'<a href="{clean.split("assets/")[-1]}">{label}</a>'
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
            classes = f' class="lang-{html.escape(language)}"' if language else ""
            code = html.escape("\n".join(block))
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


def shell(*, title: str, description: str, base: str, body: str, nav: str, wide: bool = False) -> str:
    return f"""<!doctype html>
<!-- SPDX-License-Identifier: Apache-2.0 -->
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<meta name="color-scheme" content="dark light" />
<meta name="description" content="{html.escape(description)}" />
<title>{html.escape(title)}</title>
<link rel="icon" href="{base}favicon.svg" />
<link rel="stylesheet" href="{base}site.css" />
</head>
<body class="{'wide' if wide else 'doc'}">
<a class="skip" href="#content">Skip to the content</a>
<header class="top">
  <a class="brand" href="{base}index.html"><img src="{base}favicon.svg" alt="" width="24" height="24" /><span>TessIFC</span></a>
  <nav class="topnav">
    <a href="{base}docs/getting-started.html">Docs</a>
    <a href="{base}viewer/">Viewer</a>
    <a href="{REPO_URL}" target="_blank" rel="noopener">GitHub</a>
  </nav>
  <button class="themer" type="button" id="themer" aria-label="Switch the theme">
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M12 3v18" /><path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" stroke="none" /></svg>
  </button>
</header>
{nav}
<main id="content">
{body}
</main>
<footer class="foot">
  <p>Apache-2.0. IFC is a standard of buildingSMART International. TessIFC is an independent implementation and is not endorsed by buildingSMART.</p>
</footer>
<script src="{base}site.js"></script>
</body>
</html>
"""


def sidebar(pages: list[tuple[str, str]], current: str, base: str, outline: list[tuple[int, str, str]]) -> str:
    links = []
    for target, label in pages:
        active = ' class="on"' if target == current else ""
        links.append(f'<a href="{base}docs/{target}"{active}>{html.escape(label)}</a>')
        if target == current and outline:
            links.append('<span class="sub">')
            for level, anchor, text in outline:
                links.append(f'<a class="l{level}" href="#{anchor}">{html.escape(text)}</a>')
            links.append("</span>")
    return (
        '<aside class="side"><nav aria-label="Documentation">'
        f'<p class="sidetitle">Documentation</p>{"".join(links)}</nav></aside>'
    )


def landing(base: str) -> str:
    return f"""
<section class="hero">
  <p class="eyebrow">v0.1 developer preview · Apache-2.0</p>
  <h1>IFC files in.<br />Render-ready meshes out.</h1>
  <p class="lede">TessIFC reads IFC2X3, IFC4 and IFC4X3 and turns them into triangle meshes a GPU
  can draw. One Rust codebase runs in the browser, in Node and on the command line, with a
  viewer that opens a model without uploading it anywhere.</p>
  <div class="cta">
    <a class="button primary" href="{base}viewer/">Open the viewer</a>
    <a class="button" href="{base}docs/getting-started.html">Read the docs</a>
    <a class="button ghost" href="{REPO_URL}" target="_blank" rel="noopener">View on GitHub</a>
  </div>
  <p class="note">Everything runs in your browser. Your file never leaves your machine.</p>
</section>

<section class="shot">
  <img src="{base}viewer.png" alt="The TessIFC viewer with a building model open" />
</section>

<section class="grid">
  <article>
    <h2>Reads real files</h2>
    <p>IFC2X3, IFC4 and IFC4X3 through one schema-agnostic parser. Walls, slabs, roofs,
    Product geometry is evaluated when its representation is supported. See the coverage guide for limits.</p>
  </article>
  <article>
    <h2>Reports what it could build</h2>
    <p>Inspect product outcomes and diagnostics for unsupported, repaired or degraded geometry.
    The preview makes its limits visible before you accept a mesh.</p>
  </article>
  <article>
    <h2>Streams while it works</h2>
    <p>Geometry arrives in batches, so the first storey is on screen while the rest is still
    being evaluated. Repeated objects are drawn once and placed many times.</p>
  </article>
  <article>
    <h2>Keeps its element ids</h2>
    <p>Every mesh stays linked to the IFC element it came from, and every problem is reported
    against the element that caused it.</p>
  </article>
  <article>
    <h2>Runs everywhere</h2>
    <p>The same kernel compiles to WebAssembly for the browser and Node, and to a native binary
    for the command line. No server, no service, no account.</p>
  </article>
  <article>
    <h2>Edits without rewriting</h2>
    <p>Change an attribute and export the file with every other byte left exactly as it was, so
    a round trip through TessIFC is not a rewrite of someone else's model.</p>
  </article>
</section>

<section class="start">
  <h2>Quick start</h2>
  <div class="starts">
    <div>
      <h3>In the browser</h3>
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
      <h3>On the command line</h3>
      <div class="code"><pre><code>cargo build --locked --release -p tessifc-cli

target/release/tessifc info model.ifc
target/release/tessifc convert model.ifc -o model.igp</code></pre></div>
      <h3>From a checkout</h3>
      <div class="code"><pre><code>python scripts/build-wasm.py --target web
python -m http.server 8000</code></pre></div>
    </div>
  </div>
</section>

<section class="links">
  <h2>Documentation</h2>
  <div class="cards">
    <a href="{base}docs/getting-started.html"><strong>Getting started</strong><span>The browser, Node, the command line and Rust.</span></a>
    <a href="{base}docs/sdk.html"><strong>SDK and API</strong><span>The packages, the API and what it promises.</span></a>
    <a href="{base}docs/architecture.html"><strong>Architecture</strong><span>How the pipeline fits together, with diagrams.</span></a>
    <a href="{base}docs/igp-format.html"><strong>IGP format</strong><span>The mesh container the kernel writes.</span></a>
    <a href="{base}docs/coverage.html"><strong>IFC coverage</strong><span>Which classes are supported today.</span></a>
    <a href="{base}docs/editing.html"><strong>Editing</strong><span>Changing attributes without rewriting a file.</span></a>
  </div>
</section>
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
    pkg = REPO / "bindings" / "wasm" / "pkg"
    for name in ("tessifc_wasm.js", "tessifc_wasm_bg.wasm"):
        if not (pkg / name).is_file():
            print("browser WASM package is missing: run python scripts/build-wasm.py --target web", file=sys.stderr)
            return 1
    (out / "docs").mkdir(parents=True)

    pages = [(f"{Path(name).stem}.html", label) for name, label in PAGES]
    pages += [(target, label) for _, target, label in EXTRA]

    sources = [(REPO / "docs" / name, f"{Path(name).stem}.html", label) for name, label in PAGES]
    sources += [(REPO / path, target, label) for path, target, label in EXTRA]

    for path, target, label in sources:
        if not path.exists():
            print(f"  missing {path.relative_to(REPO)}", file=sys.stderr)
            return 1
        body, outline = render(path.read_text(encoding="utf-8"))
        page = shell(
            title=f"{label} - TessIFC",
            description=f"{label} for TessIFC, an Apache-2.0 IFC geometry kernel.",
            base=f"{base}",
            nav=sidebar(pages, target, base, outline),
            body=f'<article class="prose">{body}</article>',
        )
        (out / "docs" / target).write_text(page, encoding="utf-8")
        print(f"  docs/{target}")

    (out / "index.html").write_text(
        shell(
            title="TessIFC, an Apache-2.0 IFC geometry kernel",
            description="Open, inspect, section and measure IFC models in the browser. An Apache-2.0 IFC geometry kernel in Rust.",
            base=base,
            nav="",
            body=landing(base),
            wide=True,
        ),
        encoding="utf-8",
    )
    print("  index.html")

    (out / "site.css").write_text(STYLE, encoding="utf-8")
    (out / "site.js").write_text(SCRIPT, encoding="utf-8")
    (out / "favicon.svg").write_text(FAVICON, encoding="utf-8")

    shot = REPO / "docs" / "assets" / "viewer.png"
    if shot.exists():
        shutil.copy2(shot, out / "viewer.png")

    # The viewer ships its page and its modules; its tests and tooling do not.
    copy_tree(REPO / "viewer" / "src", out / "viewer" / "src")
    shutil.copy2(REPO / "viewer" / "index.html", out / "viewer" / "index.html")
    copy_tree(pkg, out / "bindings" / "wasm" / "pkg")

    # GitHub Pages serves _-prefixed paths only when Jekyll is switched off.
    (out / ".nojekyll").write_text("", encoding="utf-8")
    return 0


STYLE = """/* SPDX-License-Identifier: Apache-2.0 */
:root {
  color-scheme: dark;
  --app: #0b0c0e;
  --surface: #101216;
  --raised: #171a20;
  --line: #23272f;
  --line-hi: #363b46;
  --fg: #f4f6f9;
  --fg-2: #cbd1dc;
  --fg-3: #a5adbc;
  --accent: #3b82f6;
  --accent-hi: #7cb2ff;
  --on-accent: #fff;
  --solid: #eef0f3;
  --on-solid: #0b0c0e;
  --font: Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  --mono: "JetBrains Mono", ui-monospace, Consolas, monospace;
}
:root[data-theme="light"] {
  color-scheme: light;
  --app: #ffffff;
  --surface: #f6f7f9;
  --raised: #ffffff;
  --line: #e3e7ec;
  --line-hi: #cbd2db;
  --fg: #0a0d12;
  --fg-2: #38414f;
  --fg-3: #576073;
  --accent: #1d4ed8;
  --accent-hi: #1741b6;
  --solid: #14171c;
  --on-solid: #ffffff;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--app);
  color: var(--fg);
  font-family: var(--font);
  font-size: 15px;
  line-height: 1.65;
  -webkit-font-smoothing: antialiased;
}
a { color: var(--accent-hi); }
:root[data-theme="light"] a { color: var(--accent); }
img { max-width: 100%; height: auto; }
code, pre { font-family: var(--mono); }

.skip { position: absolute; left: -999px; }
.skip:focus { left: 12px; top: 12px; z-index: 20; background: var(--accent); color: var(--on-accent); padding: 8px 14px; border-radius: 6px; }

.top {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  gap: 22px;
  height: 56px;
  padding: 0 22px;
  background: color-mix(in srgb, var(--app) 88%, transparent);
  backdrop-filter: blur(10px);
  border-bottom: 1px solid var(--line);
}
.brand { display: flex; align-items: center; gap: 9px; color: var(--fg); font-weight: 650; text-decoration: none; letter-spacing: -0.01em; }
.topnav { display: flex; gap: 20px; margin-left: auto; }
.topnav a { color: var(--fg-2); text-decoration: none; font-size: 14px; }
.topnav a:hover { color: var(--fg); }
.themer { display: grid; place-items: center; width: 30px; height: 30px; padding: 0; border: 1px solid var(--line-hi); border-radius: 8px; background: none; color: var(--fg-3); cursor: pointer; }
.themer:hover { color: var(--fg); }
.themer svg { width: 15px; height: 15px; }

.foot { border-top: 1px solid var(--line); padding: 28px 22px 48px; color: var(--fg-3); font-size: 13px; }
.foot p { max-width: 62rem; margin: 0 auto; }

/* ------------------------------------------------------------- landing */
body.wide main { max-width: 1060px; margin: 0 auto; padding: 0 22px; }
.hero { padding: 76px 0 40px; text-align: center; }
.eyebrow { margin: 0 0 16px; color: var(--accent-hi); font-size: 13px; font-weight: 600; letter-spacing: 0.08em; text-transform: uppercase; }
:root[data-theme="light"] .eyebrow { color: var(--accent); }
.hero h1 { margin: 0; font-size: clamp(34px, 6vw, 60px); line-height: 1.06; letter-spacing: -0.03em; font-weight: 680; }
.lede { max-width: 38rem; margin: 22px auto 0; color: var(--fg-2); font-size: 17px; }
.cta { display: flex; flex-wrap: wrap; gap: 11px; justify-content: center; margin-top: 30px; }
.button {
  display: inline-flex;
  align-items: center;
  height: 42px;
  padding: 0 22px;
  border: 1px solid var(--line-hi);
  border-radius: 9px;
  background: var(--raised);
  color: var(--fg);
  font-weight: 550;
  text-decoration: none;
}
.button:hover { border-color: var(--fg-3); }
.button.primary { background: var(--solid); border-color: transparent; color: var(--on-solid); }
.button.ghost { background: none; }
.note { margin-top: 18px; color: var(--fg-3); font-size: 13px; }

.shot { margin: 26px 0 64px; }
.shot img { display: block; width: 100%; border: 1px solid var(--line); border-radius: 14px; }

.grid { display: grid; gap: 20px; grid-template-columns: repeat(auto-fit, minmax(268px, 1fr)); padding-bottom: 64px; }
.grid article { padding: 22px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
.grid h2 { margin: 0 0 8px; font-size: 16px; letter-spacing: -0.01em; }
.grid p { margin: 0; color: var(--fg-2); font-size: 14px; }

.start { padding-bottom: 60px; }
.start h2, .links h2 { font-size: 24px; letter-spacing: -0.02em; margin: 0 0 20px; }
.starts { display: grid; gap: 22px; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); }
.starts h3 { font-size: 14px; color: var(--fg-3); font-weight: 600; margin: 0 0 10px; }
.starts h3 + .code + h3 { margin-top: 22px; }

.links { padding-bottom: 72px; }
.cards { display: grid; gap: 12px; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); }
.cards a {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 16px 18px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  text-decoration: none;
  color: var(--fg);
}
.cards a:hover { border-color: var(--accent-line, var(--fg-3)); }
.cards span { color: var(--fg-3); font-size: 13px; }

/* ------------------------------------------------------------ doc page */
body.doc { display: grid; grid-template-columns: 1fr; }
body.doc main { max-width: 1180px; margin: 0 auto; padding: 0 22px 60px; width: 100%; }
.side {
  position: sticky;
  top: 56px;
  align-self: start;
  max-height: calc(100vh - 56px);
  overflow: auto;
  padding: 26px 18px;
}
.sidetitle { margin: 0 0 12px; font-size: 11px; letter-spacing: 0.09em; text-transform: uppercase; color: var(--fg-3); }
.side nav a { display: block; padding: 5px 10px; border-radius: 6px; color: var(--fg-2); text-decoration: none; font-size: 14px; }
.side nav a:hover { background: var(--surface); color: var(--fg); }
.side nav a.on { background: var(--surface); color: var(--fg); font-weight: 600; }
.side .sub { display: block; margin: 2px 0 8px; border-left: 1px solid var(--line); padding-left: 8px; }
.side .sub a { font-size: 13px; color: var(--fg-3); padding: 3px 8px; }
.side .sub a.l3 { padding-left: 18px; }

@media (min-width: 900px) {
  body.doc { grid-template-columns: 260px 1fr; grid-template-areas: "top top" "side main" "foot foot"; }
  .top { grid-area: top; }
  .side { grid-area: side; }
  body.doc main { grid-area: main; margin: 0; max-width: 46rem; }
  .foot { grid-area: foot; }
}
@media (max-width: 899px) { .side { position: static; max-height: none; border-bottom: 1px solid var(--line); } .side .sub { display: none; } }

.prose { padding-top: 26px; }
.prose h1 { font-size: 32px; letter-spacing: -0.025em; margin: 0 0 18px; }
.prose h2 { font-size: 21px; letter-spacing: -0.015em; margin: 42px 0 12px; padding-top: 14px; border-top: 1px solid var(--line); }
.prose h3 { font-size: 16px; margin: 28px 0 8px; }
.prose p { margin: 0 0 14px; color: var(--fg-2); }
.prose li { color: var(--fg-2); margin-bottom: 5px; }
.prose ul, .prose ol { padding-left: 22px; margin: 0 0 14px; }
.prose blockquote { margin: 0 0 16px; padding: 10px 16px; border-left: 3px solid var(--accent); background: var(--surface); border-radius: 0 8px 8px 0; color: var(--fg-2); }
.prose hr { border: none; border-top: 1px solid var(--line); margin: 30px 0; }
.prose code { background: var(--surface); border: 1px solid var(--line); border-radius: 5px; padding: 1px 5px; font-size: 0.88em; }

.code { margin: 0 0 16px; border: 1px solid var(--line); border-radius: 10px; background: var(--surface); overflow: hidden; }
.code pre { margin: 0; padding: 14px 16px; overflow-x: auto; font-size: 13px; line-height: 1.6; }
.code code { background: none; border: none; padding: 0; font-size: inherit; }

.table { margin: 0 0 18px; overflow-x: auto; border: 1px solid var(--line); border-radius: 10px; }
.table table { width: 100%; border-collapse: collapse; font-size: 13.5px; }
.table th, .table td { padding: 8px 12px; text-align: left; border-bottom: 1px solid var(--line); vertical-align: top; }
.table th { background: var(--surface); font-weight: 600; white-space: nowrap; }
.table tr:last-child td { border-bottom: none; }
"""

SCRIPT = """// SPDX-License-Identifier: Apache-2.0
(() => {
  const modes = ["system", "light", "dark"];
  const media = matchMedia("(prefers-color-scheme: light)");
  let mode = "system";
  try {
    const saved = localStorage.getItem("tessifc.theme");
    if (modes.includes(saved)) mode = saved;
  } catch {
  }
  const apply = () => {
    const light = mode === "light" || (mode === "system" && media.matches);
    document.documentElement.dataset.theme = light ? "light" : "dark";
    const button = document.getElementById("themer");
    if (button) button.title = `Theme: ${mode}. Click for the next one.`;
  };
  apply();
  media.addEventListener("change", () => { if (mode === "system") apply(); });
  document.getElementById("themer")?.addEventListener("click", () => {
    mode = modes[(modes.indexOf(mode) + 1) % modes.length];
    try {
      localStorage.setItem("tessifc.theme", mode);
    } catch {
    }
    apply();
  });
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
