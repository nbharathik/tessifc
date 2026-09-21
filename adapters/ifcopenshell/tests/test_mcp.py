# SPDX-License-Identifier: Apache-2.0
"""The MCP transport end to end: the CLI is spawned with --mcp, an SDK client speaks to it over stdio,
the tools match the shared contract, a model is created, edited, undone and exported, and nothing but
the protocol reaches stdout."""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

try:
    import mcp  # noqa: F401
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
except ImportError:  # pragma: no cover - optional extra
    mcp = None

try:
    import ifcopenshell  # noqa: F401
except ImportError:  # pragma: no cover - optional runtime
    ifcopenshell = None

ROOT = Path(__file__).resolve().parents[3]
CONTRACT = json.loads((ROOT / "bindings" / "mcp" / "contract.json").read_text(encoding="utf-8"))


def text(result) -> dict:
    return json.loads(result.content[0].text)


@unittest.skipIf(mcp is None or ifcopenshell is None, "needs the mcp package and IfcOpenShell")
class McpTransportTests(unittest.TestCase):
    def test_the_tools_edit_a_new_model_over_stdio(self):
        import anyio

        anyio.run(self._exercise)

    async def _exercise(self):
        directory = Path(tempfile.mkdtemp(prefix="tessifc-mcp-"))
        path = directory / "house.ifc"
        env = dict(os.environ, PYTHONPATH=str(ROOT / "adapters" / "ifcopenshell"), TESSIFC_ROOT=str(ROOT))
        params = StdioServerParameters(
            command=sys.executable,
            args=["-m", "tessifc_session.cli", str(path), "--mcp", "--new", "--port", "0", "--storeys", "Ground floor:0,Upper floor:3", "--assistant", "none"],
            env=env, cwd=str(ROOT),
        )
        async with stdio_client(params) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                listed = await session.list_tools()
                names = sorted(tool.name for tool in listed.tools)
                expected = sorted(name for name, spec in CONTRACT["tools"].items() if spec["python"])
                self.assertEqual(names, expected)
                for tool in listed.tools:
                    spec = CONTRACT["tools"][tool.name]
                    self.assertEqual(sorted(tool.inputSchema.get("properties", {}).keys()), sorted(spec["input"]["properties"]), tool.name)
                    self.assertEqual(sorted(tool.inputSchema.get("required", [])), sorted(spec["input"]["required"]), tool.name)

                described = text(await session.call_tool("describe_model", {}))
                self.assertEqual(described["revision"], "0")
                self.assertEqual([item["name"] for item in described["storeys"]], ["Ground floor", "Upper floor"])
                for key in CONTRACT["tools"]["describe_model"]["result"]:
                    self.assertIn(key, described)

                inspected = text(await session.call_tool("inspect_model", {"code": "print(len(model.by_type('IfcBuildingStorey')))"}))
                self.assertEqual(inspected["stdout"].strip(), "2")

                script = """
wall = api.run("root.create_entity", model, ifc_class="IfcWall", name="South wall")
storey = model.by_type("IfcBuildingStorey")[0]
api.run("spatial.assign_container", model, products=[wall], relating_structure=storey)
api.run("geometry.edit_object_placement", model, product=wall)
context = [c for c in model.by_type("IfcGeometricRepresentationSubContext") if c.ContextIdentifier == "Body"][0]
shape = api.run("geometry.add_wall_representation", model, context=context, length=6.0, height=3.0, thickness=0.3)
api.run("geometry.assign_representation", model, product=wall, representation=shape)
print(wall.id())
"""
                edited = await session.call_tool("edit_model", {"script": script, "summary": "Adds the south wall."})
                record = text(edited)
                self.assertFalse(edited.isError, record)
                self.assertTrue(record["ok"] and record["changed"])
                self.assertEqual(record["revision"], "1")
                self.assertEqual(record["history"]["undo"], 1)
                self.assertIsNone(record["affectedProducts"])
                self.assertIn("No viewer", record["note"])
                for key in CONTRACT["tools"]["edit_model"]["result"]:
                    self.assertIn(key, record)
                self.assertIn(b"IFCWALL", path.read_bytes())

                failing = await session.call_tool("edit_model", {"script": "missing()"})
                self.assertTrue(failing.isError)
                self.assertIn("NameError", text(failing)["error"])

                found = text(await session.call_tool("find_products", {"class": "IfcWall"}))
                self.assertEqual(found.get("total"), 1, found)
                self.assertEqual(found["products"][0]["storey"], "Ground floor")
                info = text(await session.call_tool("product_info", {"id": found["products"][0]["id"]}))
                self.assertEqual(info["class"], "IfcWall")
                self.assertEqual(info["container"], "Ground floor")

                undone = text(await session.call_tool("undo", {}))
                self.assertEqual(undone["revision"], "2")
                self.assertNotIn(b"IFCWALL", path.read_bytes())
                redone = text(await session.call_tool("redo", {}))
                self.assertEqual(redone["revision"], "3")

                copy = directory / "copy.ifc"
                exported = text(await session.call_tool("export_model", {"path": str(copy)}))
                self.assertTrue(copy.exists() and exported["bytes"] == copy.stat().st_size)

                selection = text(await session.call_tool("get_selection", {}))
                self.assertEqual(selection["ids"], [])
                examples = text(await session.call_tool("list_examples", {}))
                self.assertTrue(any(example["title"] == "Add a door to a wall" for example in examples["examples"]))

                second = directory / "second.ifc"
                fresh = text(await session.call_tool("new_model", {"name": "Second", "path": str(second)}))
                self.assertEqual(fresh["revision"], "0")
                self.assertEqual(fresh["generation"], 2)
                self.assertTrue(second.exists())
                reopened = text(await session.call_tool("open_model", {"path": str(path)}))
                self.assertEqual(reopened["generation"], 3)
                self.assertEqual(reopened["products"], 5)  # site, building, two storeys and the wall

                api = await session.read_resource("tessifc://script-api")
                self.assertIn("model.by_guid", api.contents[0].text)
                prompts = await session.list_prompts()
                self.assertEqual([prompt.name for prompt in prompts.prompts], ["build-a-building"])


if __name__ == "__main__":
    unittest.main()
