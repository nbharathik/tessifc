# SPDX-License-Identifier: Apache-2.0
"""Session semantics, the assistant tool loop and the loopback routes."""

from __future__ import annotations

import json
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
REPO = PACKAGE_ROOT.parents[1]
sys.path.insert(0, str(PACKAGE_ROOT))

from tessifc_session import Assistant, EditSession, FakeProvider, SessionError, create_server  # noqa: E402
from tessifc_session.assistant import AnthropicProvider, create_provider  # noqa: E402

try:
    import ifcopenshell  # noqa: F401
except ImportError:
    ifcopenshell = None

# A first-party IFC4 fragment: one storey, a wall with an extruded rectangle and a second wall.
FIXTURE = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');
FILE_NAME('fixture.ifc','2026-09-11T00:00:00',(''),(''),'','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCCARTESIANPOINT((0.,0.,0.));
#2=IFCAXIS2PLACEMENT3D(#1,$,$);
#3=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#2,$);
#4=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCUNITASSIGNMENT((#4));
#6=IFCPROJECT('2HbdjmBtT3hRIGf7KS_$sO',$,'Fixture',$,$,$,$,(#3),#5);
#7=IFCBUILDINGSTOREY('0kWNrqhh53OQeUf_S8TjHU',$,'Level 0',$,$,$,$,$,.ELEMENT.,0.);
#8=IFCRELAGGREGATES('1Jc6mDGb99eBLhjpQqh5Kc',$,$,$,#6,(#7));
#9=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCAXIS2PLACEMENT3D(#9,$,$);
#11=IFCLOCALPLACEMENT($,#10);
#12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,5.,0.3);
#13=IFCDIRECTION((0.,0.,1.));
#14=IFCEXTRUDEDAREASOLID(#12,$,#13,3.);
#15=IFCSHAPEREPRESENTATION(#3,'Body','SweptSolid',(#14));
#16=IFCPRODUCTDEFINITIONSHAPE($,$,(#15));
#17=IFCWALL('3vB2YO$MX4xv5uCqZZG05x',$,'Fixture wall',$,$,#11,#16,'W1',$);
#18=IFCCARTESIANPOINT((8.,0.,0.));
#19=IFCAXIS2PLACEMENT3D(#18,$,$);
#20=IFCLOCALPLACEMENT($,#19);
#21=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,5.,0.3);
#22=IFCEXTRUDEDAREASOLID(#21,$,#13,3.);
#23=IFCSHAPEREPRESENTATION(#3,'Body','SweptSolid',(#22));
#24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));
#25=IFCWALL('1kTvXnbbzCWw8lcMdlWtUX',$,'Second wall',$,$,#20,#24,'W2',$);
#26=IFCRELCONTAINEDINSPATIALSTRUCTURE('2c$sNgTa1Cxw4kxdDwJVgG',$,$,$,(#17,#25),#7);
ENDSEC;
END-ISO-10303-21;
"""
WALL_GUID = "3vB2YO$MX4xv5uCqZZG05x"


def replace_atomically(path: Path, payload: bytes):
    staging = path.with_suffix(".staging")
    staging.write_bytes(payload)
    staging.replace(path)


class SessionFixture(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "fixture.ifc"
        self.path.write_bytes(FIXTURE.encode())
        self.session = EditSession(self.path)

    def tearDown(self):
        self.directory.cleanup()


class SnapshotTests(SessionFixture):
    def test_initial_snapshot_and_external_write(self):
        first = self.session.version
        self.assertEqual(self.session.revision, 0)
        replace_atomically(self.path, FIXTURE.replace("Fixture wall", "Renamed outside").encode())
        version, payload = self.session.snapshot()
        self.assertNotEqual(version, first)
        self.assertIn(b"Renamed outside", payload)
        self.assertEqual(self.session.revision, 1)
        self.assertEqual(len(self.session.undo_stack), 1)

    def test_wait_for_change_returns_on_external_write(self):
        first = self.session.version
        threading.Timer(0.2, lambda: replace_atomically(self.path, FIXTURE.replace("W2", "W3").encode())).start()
        started = time.monotonic()
        version = self.session.wait_for_change(first, 5.0)
        self.assertNotEqual(version, first)
        self.assertLess(time.monotonic() - started, 4.0)

    def test_wait_for_change_times_out_quietly(self):
        started = time.monotonic()
        self.assertEqual(self.session.wait_for_change(self.session.version, 0.3), self.session.version)
        self.assertGreaterEqual(time.monotonic() - started, 0.25)

    def test_partially_written_file_is_not_adopted(self):
        # A file with no bytes yet is what an in-progress write looks like.
        self.path.write_bytes(b"")
        with self.assertRaises(OSError):
            self.session.snapshot()

    def test_describe_reports_capabilities(self):
        status = self.session.describe()
        self.assertEqual(status["name"], "fixture.ifc")
        self.assertEqual(status["capabilities"]["authoring"], ifcopenshell is not None)
        self.assertIsNone(status["capabilities"]["assistant"])


@unittest.skipIf(ifcopenshell is None, "IfcOpenShell is not installed")
class ScriptTests(SessionFixture):
    def wall_name(self):
        result = self.session.run_script("print(model.by_guid(WALL).Name)".replace("WALL", repr(WALL_GUID)), commit=False)
        return result["stdout"].strip()

    def test_script_commits_a_new_version_and_journals_operations(self):
        before = self.session.version
        result = self.session.run_script("selected.Name = 'Scripted'\nprint('done')", {"guids": [WALL_GUID]})
        self.assertTrue(result["ok"] and result["changed"])
        self.assertEqual(result["stdout"].strip(), "done")
        self.assertEqual(result["operations"], {"created": 0, "modified": 1, "deleted": 0})
        self.assertNotEqual(result["version"], before)
        self.assertIn(b"'Scripted'", self.path.read_bytes())
        self.assertEqual(self.session.revision, 1)

    def test_unchanged_script_keeps_the_version(self):
        before = self.session.version
        result = self.session.run_script("print(len(model.by_type('IfcWall')))")
        self.assertTrue(result["ok"])
        self.assertFalse(result["changed"])
        self.assertEqual(self.session.version, before)
        self.assertEqual(result["stdout"].strip(), "2")

    def test_failed_script_is_rolled_back(self):
        result = self.session.run_script("selected.Name = 'Broken'\nraise RuntimeError('stop')", {"guids": [WALL_GUID]})
        self.assertFalse(result["ok"])
        self.assertEqual(result["error"], "RuntimeError: stop")
        self.assertIn("line 2", result["traceback"])
        self.assertEqual(self.wall_name(), "Fixture wall")
        self.assertNotIn(b"Broken", self.path.read_bytes())

    def test_syntax_error_is_reported_not_raised(self):
        result = self.session.run_script("def (:")
        self.assertFalse(result["ok"])
        self.assertTrue(result["error"].startswith("SyntaxError"))

    def test_inspect_discards_changes(self):
        result = self.session.run_script("selected.Name = 'Peek'", {"guids": [WALL_GUID]}, commit=False)
        self.assertTrue(result["ok"])
        self.assertFalse(result["changed"])
        self.assertEqual(self.wall_name(), "Fixture wall")

    def test_undo_and_redo_are_new_versions(self):
        self.session.run_script("selected.Name = 'One'", {"guids": [WALL_GUID]})
        self.session.run_script("selected.Name = 'Two'", {"guids": [WALL_GUID]})
        self.assertEqual(self.session.revision, 2)
        undone = self.session.undo()
        self.assertTrue(undone["ok"] and undone["changed"])
        self.assertEqual(self.session.revision, 3)
        self.assertEqual(self.wall_name(), "One")
        self.session.undo()
        self.assertEqual(self.wall_name(), "Fixture wall")
        with self.assertRaises(SessionError):
            self.session.undo()
        self.session.redo()
        self.assertEqual(self.wall_name(), "One")
        self.session.run_script("selected.Name = 'Three'", {"guids": [WALL_GUID]})
        with self.assertRaises(SessionError):
            self.session.redo()

    def test_external_write_reloads_the_model_before_the_next_script(self):
        self.session.run_script("selected.Name = 'Scripted'", {"guids": [WALL_GUID]})
        external = self.path.read_bytes().replace(b"'Second wall'", b"'External wall'")
        replace_atomically(self.path, external)
        result = self.session.run_script("print(model.by_guid('1kTvXnbbzCWw8lcMdlWtUX').Name)", commit=False)
        self.assertEqual(result["stdout"].strip(), "External wall")
        self.assertEqual(self.wall_name(), "Scripted")

    def test_summary_and_selection_context(self):
        summary = self.session.summary()
        self.assertEqual(summary["schema"], "IFC4")
        self.assertEqual(summary["lengthUnit"], "METRE")
        self.assertEqual(summary["products"]["IfcWall"], 2)
        described = self.session.describe_selection({"guids": [WALL_GUID]})
        self.assertEqual(described[0]["Name"], "Fixture wall")
        self.assertIn("representations", described[0])
        self.assertTrue(described[0]["container"].startswith("IfcBuildingStorey"))


@unittest.skipIf(ifcopenshell is None, "IfcOpenShell is not installed")
class AssistantTests(SessionFixture):
    def setUp(self):
        super().setUp()
        self.assistant = Assistant(self.session, FakeProvider())

    def test_ask_mode_inspects_and_answers_without_changes(self):
        outcome = self.assistant.respond({"mode": "ask", "prompt": "How many products?"})
        self.assertEqual(outcome["mode"], "ask")
        self.assertIn("products 3", outcome["answer"])
        self.assertIsNone(outcome["proposal"])
        self.assertEqual(self.session.revision, 0)

    def test_edit_mode_records_a_proposal_under_review(self):
        outcome = self.assistant.respond({"mode": "edit", "prompt": "Rename it", "selection": {"guids": [WALL_GUID]}})
        self.assertIn("target.Name", outcome["proposal"]["script"])
        self.assertIsNone(outcome["run"])
        self.assertEqual(self.session.revision, 0)

    def test_edit_mode_runs_under_the_automatic_policy(self):
        outcome = self.assistant.respond({"mode": "edit", "prompt": "Rename it", "policy": "auto",
                                          "selection": {"guids": [WALL_GUID]}})
        self.assertTrue(outcome["run"]["ok"] and outcome["run"]["changed"])
        self.assertEqual(self.session.revision, 1)
        self.assertIn(b"Assistant renamed wall", self.path.read_bytes())

    def test_history_is_bounded_and_alternating(self):
        history = [{"role": "assistant", "content": "orphan"}, {"role": "user", "content": "a"},
                   {"role": "user", "content": "b"}, {"role": "assistant", "content": "c"}, {"role": "assistant", "content": ""}]
        trimmed = Assistant._history(history)
        self.assertEqual(trimmed, [{"role": "user", "content": "a\n\nb"}, {"role": "assistant", "content": "c"}])

    def test_empty_prompt_is_refused(self):
        from tessifc_session import AssistantError
        with self.assertRaises(AssistantError):
            self.assistant.respond({"mode": "ask", "prompt": "  "})


class ProviderSelectionTests(unittest.TestCase):
    def test_names_map_to_providers(self):
        self.assertIsNone(create_provider("none"))
        self.assertIsInstance(create_provider("fake"), FakeProvider)
        provider = create_provider("anthropic", "claude-opus-5", "low")
        self.assertIsInstance(provider, AnthropicProvider)
        self.assertEqual((provider.model, provider.effort), ("claude-opus-5", "low"))
        from tessifc_session import AssistantError
        with self.assertRaises(AssistantError):
            create_provider("mystery")


class ServerTests(SessionFixture):
    def setUp(self):
        super().setUp()
        self.assistant = Assistant(self.session, FakeProvider()) if ifcopenshell else None
        self.server = create_server(self.session, 0, root=REPO, assistant=self.assistant)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.origin = f"http://127.0.0.1:{self.server.server_port}"
        self.token = self.status()["token"]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        super().tearDown()

    def get(self, path, headers=None):
        with urlopen(Request(self.origin + path, headers=headers or {}), timeout=30) as response:
            return response.read()

    def status(self):
        return json.loads(self.get("/__tessifc/session"))

    def post(self, path, body, headers=None, token=True):
        request_headers = {"Content-Type": "application/json", "Origin": self.origin}
        if token:
            request_headers["X-Tessifc-Token"] = self.token
        request_headers.update(headers or {})
        request = Request(self.origin + path, data=json.dumps(body).encode(), headers=request_headers, method="POST")
        with urlopen(request, timeout=30) as response:
            return json.loads(response.read())

    def test_version_is_bound_to_the_returned_snapshot(self):
        metadata = self.status()
        self.assertEqual(metadata["name"], "fixture.ifc")
        self.assertEqual(self.get("/__tessifc/model.ifc?version=" + metadata["version"]), FIXTURE.encode())
        replace_atomically(self.path, FIXTURE.replace("W2", "W9").encode())
        with self.assertRaises(HTTPError) as error:
            self.get("/__tessifc/model.ifc?version=" + metadata["version"])
        self.assertEqual(error.exception.code, 409)
        current = self.status()
        self.assertNotEqual(current["version"], metadata["version"])

    def test_long_poll_returns_when_the_file_changes(self):
        version = self.status()["version"]
        threading.Timer(0.2, lambda: replace_atomically(self.path, FIXTURE.replace("W2", "W8").encode())).start()
        started = time.monotonic()
        changed = json.loads(self.get(f"/__tessifc/session?after={version}&timeout=5"))
        self.assertNotEqual(changed["version"], version)
        self.assertLess(time.monotonic() - started, 4.0)

    def test_static_routes_do_not_expose_workspace_files(self):
        self.assertIn(b"TessIFC", self.get("/viewer/"))
        for path in ("/Cargo.toml", "/viewer/%2e%2e/Cargo.toml", "/viewer/node_modules/playwright/index.js"):
            with self.subTest(path=path), self.assertRaises(HTTPError) as error:
                self.get(path)
            self.assertEqual(error.exception.code, 404)

    def test_rejects_foreign_origin_and_host(self):
        for headers in ({"Origin": "https://example.com"}, {"Host": "example.com"}):
            with self.subTest(headers=headers), self.assertRaises(HTTPError) as error:
                self.get("/__tessifc/session", headers)
            self.assertEqual(error.exception.code, 403)

    def test_posts_need_the_token_and_an_origin(self):
        with self.assertRaises(HTTPError) as error:
            self.post("/__tessifc/run", {"script": "print(1)"}, token=False)
        self.assertEqual(error.exception.code, 403)
        request = Request(self.origin + "/__tessifc/run", data=b"{}", method="POST",
                          headers={"Content-Type": "application/json", "X-Tessifc-Token": self.token})
        with self.assertRaises(HTTPError) as error:
            urlopen(request, timeout=30)
        self.assertEqual(error.exception.code, 403)

    def test_malformed_bodies_are_rejected(self):
        request = Request(self.origin + "/__tessifc/run", data=b"[1,2]", method="POST",
                          headers={"Content-Type": "application/json", "Origin": self.origin, "X-Tessifc-Token": self.token})
        with self.assertRaises(HTTPError) as error:
            urlopen(request, timeout=30)
        self.assertEqual(error.exception.code, 400)

    @unittest.skipIf(ifcopenshell is None, "IfcOpenShell is not installed")
    def test_run_undo_and_assistant_routes(self):
        before = self.status()["version"]
        result = self.post("/__tessifc/run", {"script": "selected.Name = 'Served'\nprint('ok')", "selection": {"guids": [WALL_GUID]}})
        self.assertTrue(result["ok"] and result["changed"])
        self.assertNotEqual(result["version"], before)
        self.assertEqual(result["status"]["undo"], 1)
        undone = self.post("/__tessifc/undo", {})
        self.assertTrue(undone["changed"])
        self.assertEqual(undone["status"]["redo"], 1)
        with self.assertRaises(HTTPError) as error:
            self.post("/__tessifc/undo", {})
        self.assertEqual(error.exception.code, 400)
        answer = self.post("/__tessifc/assistant", {"mode": "ask", "prompt": "count"})
        self.assertIn("products 3", answer["answer"])
        proposal = self.post("/__tessifc/assistant", {"mode": "edit", "prompt": "raise the wall",
                                                      "selection": {"guids": [WALL_GUID]}})
        self.assertIn("Depth", proposal["proposal"]["script"])
        self.assertIsNone(proposal["run"])

    @unittest.skipIf(ifcopenshell is None, "IfcOpenShell is not installed")
    def test_concurrent_scripts_report_busy(self):
        release = threading.Event()
        holder = {"result": None}

        def slow():
            holder["result"] = self.post("/__tessifc/run", {"script": "import time\nwhile not time.time() > STOP: time.sleep(0.05)".replace("STOP", str(time.time() + 1.5))})

        thread = threading.Thread(target=slow)
        thread.start()
        time.sleep(0.4)
        with self.assertRaises(HTTPError) as error:
            self.post("/__tessifc/run", {"script": "print(1)"})
        self.assertEqual(error.exception.code, 409)
        self.assertTrue(self.status()["busy"])
        release.set()
        thread.join()
        self.assertTrue(holder["result"]["ok"])


if __name__ == "__main__":
    unittest.main()
