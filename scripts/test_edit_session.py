# SPDX-License-Identifier: Apache-2.0
"""The documented launcher still serves the viewer and the followed IFC file."""

import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import Request, urlopen

REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("edit_session", Path(__file__).with_name("serve-edit-session.py"))
session_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(session_module)


class LauncherTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "example.ifc"
        self.path.write_bytes(b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n")
        self.session = session_module.FileSession(self.path)
        self.server = session_module.create_server(self.session, 0, root=REPO)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.origin = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.directory.cleanup()

    def get(self, path, headers=None):
        with urlopen(Request(self.origin + path, headers=headers or {}), timeout=5) as response:
            return response.read()

    def test_serves_the_viewer_and_the_followed_file(self):
        token = {"X-Tessifc-Token": self.server.session_token}
        self.assertTrue(self.server.viewer_url.endswith("/viewer/?session=file#token=" + self.server.session_token))
        metadata = json.loads(self.get("/__tessifc/session", token))
        self.assertEqual(metadata["name"], "example.ifc")
        self.assertIn("capabilities", metadata)
        self.assertNotIn("token", metadata)
        self.assertEqual(self.get("/__tessifc/model.ifc?version=" + metadata["version"], token), self.path.read_bytes())
        self.assertIn(b"TessIFC", self.get("/viewer/"))
        for headers in ({}, {"Origin": "https://example.com", **token}):
            with self.subTest(headers=headers), self.assertRaises(HTTPError) as error:
                self.get("/__tessifc/session", headers)
            self.assertEqual(error.exception.code, 403)


if __name__ == "__main__":
    unittest.main()
