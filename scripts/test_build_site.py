# SPDX-License-Identifier: Apache-2.0
"""Regression tests for safe replacement of generated websites."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("build_site", Path(__file__).with_name("build-site.py"))
site = importlib.util.module_from_spec(spec)
spec.loader.exec_module(site)


class OutputSafety(unittest.TestCase):
    def test_document_links_match_generated_names(self):
        self.assertIn('href="contributing.html"', site.inline("[Contribute](../CONTRIBUTING.md)"))
        self.assertIn('href="preview.html"', site.inline("[Preview](preview.md)"))
        self.assertIn('/blob/main/viewer/README.md"', site.inline("[Viewer](../viewer/README.md)"))

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
