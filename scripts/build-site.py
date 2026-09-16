#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the TessIFC website with Material for MkDocs, including the viewer.

    python -m pip install -r requirements-site.txt
    python scripts/build-wasm.py --target web
    python scripts/build-site.py [--out dist/site] [--base /]
    python -m http.server 8000 --bind 127.0.0.1 --directory dist/site

The build downloads nothing. MkDocs renders Markdown, syntax highlighting and
the search index at build time; its theme assets are served with the website.
Guides live at <base>/docs/<name>/ and the viewer at <base>/viewer/. Previous
docs/<name>.html addresses continue to work through redirects.

Documentation is staged so the existing repository Markdown and its GitHub
links stay useful. An existing generated site is replaced only after the new
site has built successfully. Unrelated output directories are never replaced.
"""

from __future__ import annotations

import argparse
import html
from importlib.metadata import distribution
import json
import logging
import posixpath
import re
import shutil
import stat
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

REPO = Path(__file__).resolve().parent.parent
REPO_URL = "https://github.com/nbharathik/tessifc"

# Source file, navigation label, and page description. Repository documents
# remain the source of truth; only the homepage and overview are site-specific.
PAGES = [
    ("getting-started.md", "Getting started", "Build the kernel, open the viewer and convert a file."),
    ("sdk.md", "SDK and API", "The packages, the API surface and what each call promises."),
    ("preview.md", "Developer preview", "What the preview covers, its limits and how to validate results."),
    ("architecture.md", "Architecture", "How a file becomes triangles, crate by crate, with diagrams."),
    ("igp-format.md", "IGP format", "The mesh container the kernel writes and how to read it."),
    ("coverage.md", "IFC coverage", "Which classes and representations are supported today."),
    ("editing.md", "Editing", "Attribute edits, revisions, scripts and the assistant in the viewer."),
    ("agents.md", "Agents and pipelines", "MCP servers, the Node and Python loops, deltas and verification."),
]

SITE_MARKER = ".tessifc-site"
SITE_SIGNATURE = "TessIFC generated website v1\n"
SOURCE_DIRS = {".git", ".github", "crates", "bindings", "viewer", "adapters", "examples", "docs", "scripts"}

FAVICON = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
<rect width="64" height="64" rx="14" fill="#101a2d"/>
<polygon points="32,6 56,19 32,32 8,19" fill="#a9c7ff"/>
<polygon points="8,19 32,32 32,58 8,45" fill="#3b82f6"/>
<polygon points="56,19 56,45 32,58 32,32" fill="#2157bd"/>
</svg>
"""


def page_stem(source: str) -> str:
    return Path(source).stem


def legacy_slug(value: str, separator: str = "-") -> str:
    """Preserve published heading fragments such as #three-js and #v0-1."""
    plain = re.sub(r"<[^>]+>", "", value)
    return re.sub(r"[^a-z0-9]+", separator, plain.lower()).strip(separator) or "section"


def normalize_base(base: str) -> str:
    """Accept a root or project path, never an external redirect destination."""
    if urlsplit(base).scheme or base.startswith("//") or any(c in base for c in "?#\\"):
        raise ValueError("base must be a URL path such as / or /tessifc/")
    if any(part in {".", ".."} for part in base.split("/")):
        raise ValueError("base must not contain relative path segments")
    return "/" + base.strip("/") + "/" if base.strip("/") else "/"


def rewrite_markdown(source: str, source_path: Path, staged_path: str) -> str:
    """Retarget repository links without changing code or source documents.

    MkDocs resolves links between staged Markdown pages itself, including their
    anchors and deployment prefix. Other repository files continue to link to
    GitHub. Fenced examples and inline code are left byte-for-byte unchanged.
    """
    source_path = source_path if source_path.is_absolute() else REPO / source_path
    known = {(REPO / "docs" / name).resolve(): f"docs/{page_stem(name)}.md" for name, _, _ in PAGES}
    assets = (REPO / "docs" / "assets").resolve()

    def target_url(target: str) -> str:
        parts = urlsplit(target)
        if parts.scheme or parts.netloc or target.startswith(("#", "/")):
            return target
        path = (source_path.parent / parts.path).resolve()
        suffix = (f"?{parts.query}" if parts.query else "") + (f"#{parts.fragment}" if parts.fragment else "")
        if path in known:
            destination = known[path]
        elif path.is_relative_to(assets):
            destination = "assets/" + path.relative_to(assets).as_posix()
        elif path.is_relative_to(REPO):
            return f"{REPO_URL}/blob/main/{path.relative_to(REPO).as_posix()}{suffix}"
        else:
            return target
        return posixpath.relpath(destination, posixpath.dirname(staged_path) or ".") + suffix

    # Preserve labels, image alt text and optional link titles.
    link = re.compile(r"(!?\[[^\]\n]*\]\()([^\s)]+)([^)\n]*\))")
    inline_code = re.compile(r"(`+)(.*?)(?<!`)\1(?!`)")
    out: list[str] = []
    fence = ""
    for line in source.splitlines(keepends=True):
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if marker:
            current = marker.group(1)
            if not fence:
                fence = current
            elif current[0] == fence[0] and len(current) >= len(fence):
                fence = ""
            out.append(line)
            continue
        if fence:
            out.append(line)
            continue
        pieces: list[str] = []
        cursor = 0
        for code in inline_code.finditer(line):
            pieces.append(link.sub(lambda m: m[1] + target_url(m[2]) + m[3], line[cursor:code.start()]))
            pieces.append(code.group(0))
            cursor = code.end()
        pieces.append(link.sub(lambda m: m[1] + target_url(m[2]) + m[3], line[cursor:]))
        out.append("".join(pieces))
    return "".join(out)


def redirect(target: str) -> str:
    escaped = html.escape(target, quote=True)
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta name="robots" content="noindex" />
<meta http-equiv="refresh" content="0; url={escaped}" />
<link rel="canonical" href="{escaped}" />
<title>Redirecting</title>
<script>location.replace({json.dumps(target)} + location.search + location.hash);</script>
</head>
<body><p>This page has moved to <a href="{escaped}">{escaped}</a>.</p></body>
</html>
"""


def copy_tree(source: Path, target: Path, skip: set[str] | None = None) -> None:
    if not source.exists():
        return
    ignore = shutil.ignore_patterns(*sorted(skip)) if skip else None
    shutil.copytree(source, target, dirs_exist_ok=True, ignore=ignore)


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
        base = normalize_base(base)
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


def stage_documents(docs: Path) -> None:
    (docs / "docs").mkdir(parents=True)
    site = REPO / "docs" / "site"
    shutil.copy2(site / "index.md", docs / "index.md")
    shutil.copy2(site / "docs-index.md", docs / "docs" / "index.md")
    sources = [(REPO / "docs" / name, page_stem(name), label, blurb) for name, label, blurb in PAGES]
    for source, stem, label, description in sources:
        destination = f"docs/{stem}.md"
        content = rewrite_markdown(source.read_text(encoding="utf-8"), source, destination)
        metadata = f"---\ntitle: {json.dumps(label)}\ndescription: {json.dumps(description)}\n---\n\n"
        (docs / destination).write_text(metadata + content, encoding="utf-8")
    copy_tree(REPO / "docs" / "assets", docs / "assets")
    copy_tree(site / "stylesheets", docs / "stylesheets")
    copy_tree(site / "javascripts", docs / "javascripts")
    (docs / "favicon.svg").write_text(FAVICON, encoding="utf-8")


def package_theme_assets(out: Path, languages: list[str]) -> None:
    # English search is bundled in the worker. Material also ships optional
    # language stemmers, which are unused by this site's English-only index.
    if languages == ["en"]:
        unused = (out / "assets" / "javascripts" / "lunr").resolve()
        if not unused.is_relative_to(out.resolve()):
            raise ValueError("theme asset path escaped the generated website")
        if unused.exists():
            shutil.rmtree(unused)

    # Keep third-party notices with the distributed assets, outside page copy.
    package = distribution("mkdocs-material")
    license_text = package.read_text("licenses/LICENSE") or package.read_text("LICENSE")
    if not license_text:
        raise ValueError("the Material for MkDocs package is missing its license")
    licenses = out / "assets" / "licenses"
    licenses.mkdir(parents=True, exist_ok=True)
    (licenses / "mkdocs-material.txt").write_text(license_text, encoding="utf-8")
    shutil.copy2(package.locate_file("material/templates/.icons/material/LICENSE"), licenses / "material-design-icons.txt")


def build_contents(out: Path, base: str) -> int:
    base = normalize_base(base)
    pkg = REPO / "bindings" / "wasm" / "pkg"
    for name in ("tessifc_wasm.js", "tessifc_wasm_bg.wasm"):
        if not (pkg / name).is_file():
            print("browser WASM package is missing: run python scripts/build-wasm.py --target web", file=sys.stderr)
            return 1
    try:
        from mkdocs.commands.build import build as mkdocs_build
        from mkdocs.config import load_config
        from mkdocs.exceptions import MkDocsException
    except ImportError:
        print("website dependencies are missing: run python -m pip install -r requirements-site.txt", file=sys.stderr)
        return 1

    out.parent.mkdir(parents=True, exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(prefix=".tessifc-docs-", dir=out.parent) as temporary:
            docs = Path(temporary) / "docs"
            stage_documents(docs)
            config = load_config(
                str(REPO / "mkdocs.yml"), docs_dir=str(docs), site_dir=str(out),
                site_url=f"https://nbharathik.github.io{base}", strict=True,
            )
            config.extra["base_path"] = base
            config.extra["viewer_url"] = f"{base}viewer/"
            config.mdx_configs["toc"]["slugify"] = legacy_slug
            for entry in config.nav:
                if "Viewer" in entry:
                    entry["Viewer"] = f"{base}viewer/"
            config.plugins.on_startup(command="build", dirty=False)
            try:
                mkdocs_build(config)
            finally:
                config.plugins.on_shutdown()
            package_theme_assets(out, config.plugins["material/search"].config.lang)
    except MkDocsException as error:
        print(f"documentation build failed: {error}", file=sys.stderr)
        return 1

    # Keep all previously published guide addresses, including URL fragments.
    stems = [page_stem(name) for name, _, _ in PAGES]
    for stem in stems:
        (out / "docs" / f"{stem}.html").write_text(redirect(f"{base}docs/{stem}/"), encoding="utf-8")

    # The viewer ships its page and modules; its tests and tooling do not.
    copy_tree(REPO / "viewer" / "src", out / "viewer" / "src")
    shutil.copy2(REPO / "viewer" / "index.html", out / "viewer" / "index.html")
    copy_tree(pkg, out / "bindings" / "wasm" / "pkg")
    # The viewer imports the shared editing modules from the edit package and
    # its renderer from the viewer package.
    copy_tree(REPO / "bindings" / "edit" / "src", out / "bindings" / "edit" / "src")
    copy_tree(REPO / "bindings" / "viewer" / "src", out / "bindings" / "viewer" / "src")
    (out / ".nojekyll").write_text("", encoding="utf-8")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="dist/site", help="generated output directory (default: dist/site)")
    parser.add_argument("--base", default="/", help="URL prefix the site is served from")
    args = parser.parse_args()
    out = Path(args.out)
    if not out.is_absolute():
        out = REPO / out
    logging.basicConfig(level=logging.INFO, format="%(levelname)-7s - %(message)s")
    print(f"building the site into {out}")
    return build(out, args.base)


if __name__ == "__main__":
    raise SystemExit(main())
