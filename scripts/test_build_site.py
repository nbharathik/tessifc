# SPDX-License-Identifier: Apache-2.0
"""Regression tests for the website builder."""

import importlib.util
import json
from html.parser import HTMLParser
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import unquote, urljoin, urlsplit

spec = importlib.util.spec_from_file_location("build_site", Path(__file__).with_name("build-site.py"))
site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(site)


class Page(HTMLParser):
    """Collect rendered links and theme controls without depending on spacing."""

    def __init__(self, source):
        super().__init__()
        self.tags = []
        self.ids = set()
        self.feed(source)

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        self.tags.append((tag, attributes))
        if attributes.get("id"):
            self.ids.add(attributes["id"])

    def urls(self):
        for tag, attrs in self.tags:
            for name in ("href", "src"):
                if attrs.get(name):
                    yield tag, name, attrs[name]


class MarkdownLinks(unittest.TestCase):
    def test_repository_links_are_rewritten_for_staged_markdown(self):
        source = (
            "[Contribute](../CONTRIBUTING.md)\n"
            "[Preview](preview.md)\n"
            "[Header](igp-format.md#header)\n"
            "[Viewer](../viewer/README.md)\n"
            "![Shot](assets/viewer.png)\n"
        )
        rewritten = site.rewrite_markdown(
            source, site.REPO / "docs" / "getting-started.md", "docs/getting-started.md"
        )
        self.assertIn("(contributing.md)", rewritten)
        self.assertIn("(preview.md)", rewritten)
        self.assertIn("(igp-format.md#header)", rewritten)
        self.assertIn(f"({site.REPO_URL}/blob/main/viewer/README.md)", rewritten)
        self.assertIn("(../assets/viewer.png)", rewritten)

    def test_external_links_and_literal_code_are_preserved(self):
        source = (
            "[External](https://example.org/guide.md#section)\n"
            "[Email](mailto:hello@example.org)\n"
            "```markdown\n[Example](../CONTRIBUTING.md)\n```\n"
        )
        self.assertEqual(
            site.rewrite_markdown(
                source, site.REPO / "docs" / "sdk.md", "docs/sdk.md"
            ),
            source,
        )


class GeneratedSite(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not (site.REPO / "bindings" / "wasm" / "pkg" / "tessifc_wasm_bg.wasm").is_file():
            raise unittest.SkipTest("build the browser WASM package to run the generated-site checks")
        temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(temporary.cleanup)
        cls.out = Path(temporary.name) / "site"
        if site.build_contents(cls.out, "/tessifc") != 0:
            raise AssertionError("the real website build failed")
        cls.stems = [site.page_stem(source) for source, _, _ in site.PAGES]
        cls.stems += [stem for _, stem, _, _ in site.EXTRA]
        cls.paths = [Path("index.html"), Path("docs/index.html")]
        cls.paths += [Path("docs") / stem / "index.html" for stem in cls.stems]
        cls.pages = {
            path: (cls.out / path).read_text(encoding="utf-8") for path in cls.paths
        }

    def test_pages_are_directories_with_redirects_from_old_names(self):
        for stem in self.stems:
            with self.subTest(stem=stem):
                page = Page(self.pages[Path("docs") / stem / "index.html"])
                canonical = [
                    attrs["href"] for tag, attrs in page.tags
                    if tag == "link" and attrs.get("rel") == "canonical"
                ]
                self.assertEqual(len(canonical), 1)
                self.assertEqual(urlsplit(canonical[0]).path, f"/tessifc/docs/{stem}/")
                redirect = (self.out / "docs" / f"{stem}.html").read_text(encoding="utf-8")
                self.assertIn(f"url=/tessifc/docs/{stem}/", redirect)

    def test_published_heading_anchors_are_preserved(self):
        historical = {
            "getting-started": "three-js",
            "preview": "v0-1-developer-preview",
            "security": "v0-1-threat-model",
        }
        for stem, anchor in historical.items():
            with self.subTest(page=stem, anchor=anchor):
                page = Page(self.pages[Path("docs") / stem / "index.html"])
                heading_ids = {
                    attrs.get("id") for tag, attrs in page.tags
                    if tag in ("h1", "h2", "h3", "h4", "h5", "h6")
                }
                self.assertIn(anchor, heading_ids)

    def test_root_deployment_uses_root_addresses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root_site = Path(temporary) / "site"
            self.assertEqual(site.build_contents(root_site, "/"), 0)
            landing = Page((root_site / "index.html").read_text(encoding="utf-8"))
            paths = {
                urlsplit(urljoin("https://site.test/", target)).path
                for tag, attribute, target in landing.urls()
                if tag == "a" and attribute == "href"
            }
            self.assertIn("/docs/", paths)
            self.assertIn("/viewer/", paths)
            canonical = [
                attrs["href"] for tag, attrs in landing.tags
                if tag == "link" and attrs.get("rel") == "canonical"
            ]
            self.assertEqual(len(canonical), 1)
            self.assertEqual(urlsplit(canonical[0]).path, "/")
            redirect = (root_site / "docs" / "getting-started.html").read_text(encoding="utf-8")
            self.assertIn("url=/docs/getting-started/", redirect)

    def test_site_links_and_assets_resolve_beneath_the_project_base(self):
        origin = "https://site.test"
        documents = {self.out / path: Page(source) for path, source in self.pages.items()}
        for path, source in self.pages.items():
            page_url = origin + "/tessifc/" + path.as_posix().removesuffix("index.html")
            for tag, attribute, target in documents[self.out / path].urls():
                resolved = urlsplit(urljoin(page_url, target))
                if resolved.netloc != "site.test" or resolved.scheme not in ("http", "https"):
                    continue
                with self.subTest(page=str(path), tag=tag, attribute=attribute, target=target):
                    self.assertTrue(resolved.path.startswith("/tessifc/"), "link escaped the project base")
                    relative = unquote(resolved.path.removeprefix("/tessifc/"))
                    destination = self.out / relative
                    if destination.is_dir():
                        destination /= "index.html"
                    self.assertTrue(destination.is_file(), f"missing local target: {relative}")
                    self.assertFalse(resolved.path.endswith(".md"), "a source Markdown link leaked into the site")
                    if resolved.fragment and destination.suffix == ".html":
                        if destination not in documents:
                            documents[destination] = Page(destination.read_text(encoding="utf-8"))
                        linked_page = documents[destination]
                        self.assertIn(unquote(resolved.fragment), linked_page.ids, "broken heading link")

    def test_viewer_package_and_public_assets_are_included(self):
        for relative in (
            "viewer/index.html", "viewer/src/main.js", "bindings/wasm/pkg/tessifc_wasm.js",
            "bindings/wasm/pkg/tessifc_wasm_bg.wasm", "assets/viewer.png", ".nojekyll",
        ):
            with self.subTest(path=relative):
                self.assertTrue((self.out / relative).is_file())
        for relative in ("viewer/index.html", "bindings/wasm/pkg/tessifc_wasm_bg.wasm"):
            self.assertEqual((self.out / relative).read_bytes(), (site.REPO / relative).read_bytes())

    def test_builtin_search_indexes_the_documentation(self):
        search = json.loads((self.out / "search" / "search_index.json").read_text(encoding="utf-8"))
        locations = {item["location"].split("#")[0] for item in search["docs"]}
        for stem in self.stems:
            self.assertIn(f"docs/{stem}/", locations)
        self.assertTrue(any("geometry" in item["text"].lower() for item in search["docs"]))
        for path, source in self.pages.items():
            with self.subTest(page=path):
                self.assertIn('data-md-component="search"', source)

    def test_english_search_keeps_its_worker_without_unused_language_assets(self):
        search = json.loads((self.out / "search" / "search_index.json").read_text(encoding="utf-8"))
        self.assertEqual(search["config"]["lang"], ["en"])
        javascript = self.out / "assets" / "javascripts"
        self.assertFalse((javascript / "lunr").exists())
        workers = list((javascript / "workers").glob("search.*.min.js"))
        self.assertEqual(len(workers), 1)
        self.assertIn("lunr", workers[0].read_text(encoding="utf-8"))

    def test_distributed_theme_and_icons_include_their_license_notices(self):
        notices = self.out / "assets" / "licenses"
        theme = (notices / "mkdocs-material.txt").read_text(encoding="utf-8")
        icons = (notices / "material-design-icons.txt").read_text(encoding="utf-8")
        self.assertIn("Martin Donath", theme)
        self.assertIn("Permission is hereby granted, free of charge", theme)
        self.assertIn("Pictogrammers", icons)
        self.assertIn("Icons: Apache 2.0", icons)

    def test_both_color_schemes_and_copy_controls_are_configured(self):
        for path, source in self.pages.items():
            with self.subTest(page=path):
                page = Page(source)
                schemes = {
                    attrs["data-md-color-scheme"] for _, attrs in page.tags
                    if attrs.get("data-md-color-scheme")
                }
                self.assertTrue({"default", "slate"}.issubset(schemes))
                self.assertIn("content.code.copy", source)
                self.assertIn('data-md-component="palette"', source)
                self.assertNotIn("IFC is a standard of buildingSMART International", source)
                self.assertNotIn("TessIFC is an independent implementation and is not endorsed", source)

    def test_saved_theme_is_applied_before_material_initializes_the_palette(self):
        for path, source in self.pages.items():
            with self.subTest(page=path):
                storage_helper = source.index("__md_set=")
                theme_bridge = source.index('localStorage.getItem("tessifc.theme")')
                palette_initialization = source.index('var palette=__md_get("__palette")')
                self.assertLess(storage_helper, theme_bridge)
                self.assertLess(theme_bridge, source.index("</head>"))
                self.assertLess(source.index("<body"), palette_initialization)
                self.assertLess(theme_bridge, palette_initialization)

    def test_code_languages_are_highlighted_at_build_time(self):
        expected = {
            "docs/getting-started/index.html": ("js", "sh", "rust", "toml"),
            "docs/sdk/index.html": ("ts",),
            "docs/igp-format/index.html": ("json", "js", "python"),
        }
        for path, languages in expected.items():
            source = self.pages[Path(path)]
            with self.subTest(page=path):
                for language in languages:
                    self.assertIn(f'language-{language} highlight', source)
                self.assertRegex(source, r'<span class="(?:k|kd|kn|s1|s2|nb|nf)">')
        self.assertRegex(self.pages[Path("index.html")], r'<span class="(?:k|kd|kn|s1|s2|nb|nf)">')

    def test_mermaid_fences_remain_diagrams(self):
        for stem in ("architecture", "sdk"):
            source = self.pages[Path("docs") / stem / "index.html"]
            with self.subTest(page=stem):
                self.assertIn('<pre class="mermaid"><code>', source)
                self.assertNotIn('language-mermaid highlight', source)


class OutputSafety(unittest.TestCase):
    def test_sources_and_ancestors_are_rejected(self):
        for path in (site.REPO, site.REPO.parent, site.REPO / "viewer" / "generated"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                site.output_path(path)

    def test_unrelated_files_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "output"
            out.mkdir()
            document = out / "notes.txt"
            document.write_text("keep this", encoding="utf-8")
            self.assertEqual(site.build(out, "/"), 1)
            self.assertEqual(document.read_text(encoding="utf-8"), "keep this")

    def test_failed_build_preserves_previous_site(self):
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "output"
            out.mkdir()
            (out / site.SITE_MARKER).write_text(site.SITE_SIGNATURE, encoding="utf-8")
            (out / "index.html").write_text("previous", encoding="utf-8")
            with patch.object(site, "build_contents", return_value=1):
                self.assertEqual(site.build(out, "/"), 1)
            self.assertEqual((out / "index.html").read_text(encoding="utf-8"), "previous")

    def test_success_replaces_only_owned_output(self):
        def render(out, base):
            out.mkdir()
            (out / "index.html").write_text(base, encoding="utf-8")
            return 0

        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "output"
            with patch.object(site, "build_contents", side_effect=render):
                self.assertEqual(site.build(out, "/first/"), 0)
                (out / "obsolete.html").write_text("old", encoding="utf-8")
                self.assertEqual(site.build(out, "/second/"), 0)
            self.assertEqual((out / "index.html").read_text(encoding="utf-8"), "/second/")
            self.assertFalse((out / "obsolete.html").exists())

    def test_linked_output_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            real = Path(temporary) / "real"
            real.mkdir()
            link = Path(temporary) / "link"
            try:
                link.symlink_to(real, target_is_directory=True)
            except OSError:
                self.skipTest("creating directory symlinks is not permitted")
            with self.assertRaises(ValueError):
                site.output_path(link / "site")


if __name__ == "__main__":
    unittest.main()
