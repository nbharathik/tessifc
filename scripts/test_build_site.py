# SPDX-License-Identifier: Apache-2.0
"""Regression tests for the website builder."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("build_site", Path(__file__).with_name("build-site.py"))
site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(site)


class Rendering(unittest.TestCase):
    def test_document_links_become_directory_addresses(self):
        with patch.object(site, "BASE", "/tessifc/"):
            self.assertIn('href="/tessifc/docs/contributing/"', site.inline("[Contribute](../CONTRIBUTING.md)"))
            self.assertIn('href="/tessifc/docs/preview/"', site.inline("[Preview](preview.md)"))
            self.assertIn('href="/tessifc/docs/igp-format/#header"', site.inline("[Header](igp-format.md#header)"))
            self.assertIn('/blob/main/viewer/README.md"', site.inline("[Viewer](../viewer/README.md)"))
            self.assertIn('src="/tessifc/assets/viewer.png"', site.inline("![Shot](assets/viewer.png)"))

    def test_mermaid_fences_become_diagrams_and_other_fences_stay_code(self):
        body, _ = site.render("```mermaid\nflowchart LR\n  a --> b\n```\n\n```sh\nls\n```\n")
        self.assertIn('<pre class="mermaid">flowchart LR\n  a --&gt; b</pre>', body)
        self.assertIn('<code class="lang-sh">ls</code>', body)

    def test_pages_only_load_the_diagram_library_when_needed(self):
        with_diagram = site.shell(title="t", description="d", body='<pre class="mermaid">x</pre>', nav="", kind="doc", diagrams=True)
        without = site.shell(title="t", description="d", body="<p>x</p>", nav="", kind="doc")
        self.assertIn(site.MERMAID_URL, with_diagram)
        self.assertNotIn(site.MERMAID_URL, without)

    def test_theme_is_resolved_before_the_stylesheet(self):
        page = site.shell(title="t", description="d", body="", nav="", kind="home")
        self.assertLess(page.index("tessifc.theme"), page.index("site.css"))


class GeneratedSite(unittest.TestCase):
    def test_pages_are_directories_with_redirects_from_old_names(self):
        if not (site.REPO / "bindings" / "wasm" / "pkg" / "tessifc_wasm_bg.wasm").is_file():
            self.skipTest("the browser WASM package is not built")
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary) / "site"
            self.assertEqual(site.build_contents(out, "/tessifc"), 0)
            self.assertTrue((out / "index.html").is_file())
            self.assertTrue((out / "docs" / "index.html").is_file())
            self.assertTrue((out / "viewer" / "index.html").is_file())
            self.assertTrue((out / "assets" / "viewer.png").is_file())
            for source, _, _ in site.PAGES:
                stem = site.page_stem(source)
                page = (out / "docs" / stem / "index.html").read_text(encoding="utf-8")
                self.assertIn(f'href="/tessifc/docs/{stem}/"', page)
                self.assertNotIn(".html", page.split("<main")[1])
                redirect = (out / "docs" / f"{stem}.html").read_text(encoding="utf-8")
                self.assertIn(f"url=/tessifc/docs/{stem}/", redirect)
            landing = (out / "index.html").read_text(encoding="utf-8")
            self.assertIn('href="/tessifc/viewer/"', landing)
            self.assertIn('href="/tessifc/docs/"', landing)
            self.assertNotIn("index.html", landing)
            architecture = (out / "docs" / "architecture" / "index.html").read_text(encoding="utf-8")
            self.assertIn('<pre class="mermaid">', architecture)
            self.assertIn(site.MERMAID_URL, architecture)


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
