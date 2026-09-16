# SPDX-License-Identifier: Apache-2.0
"""An MCP transport over the editing session: the same tools as the Node server, with Python scripts.

The `mcp` package (the official Python SDK, 1.x) is an optional extra. Stdout is
the protocol, so every other line goes to stderr and script output is captured.
"""

from __future__ import annotations

import io
import json
import sys
import time
from pathlib import Path

from .assistant import SYSTEM_PROMPT
from .examples import PYTHON_EXAMPLES
from .session import EditSession, SessionBusy, SessionError

OUTPUT_LIMIT = 12_000
APPLIED_WAIT_SECONDS = 20.0
VIEWER_RECENT_SECONDS = 60.0

SCRIPT_API = SYSTEM_PROMPT.split("\n\n", 1)[1] if "\n\n" in SYSTEM_PROMPT else SYSTEM_PROMPT

_RUN_RESULT = {
    "type": "object",
    "properties": {
        "ok": {"type": "boolean"}, "changed": {"type": "boolean"}, "revision": {"type": ["string", "null"]},
        "version": {"type": ["string", "null"]}, "stdout": {"type": "string"}, "error": {"type": ["string", "null"]},
        "traceback": {"type": ["string", "null"]}, "operations": {"type": "object"}, "affectedProducts": {"type": ["array", "null"]},
        "removedProducts": {"type": ["array", "null"]}, "metadataProducts": {"type": ["array", "null"]}, "fullRebuild": {"type": ["boolean", "null"]},
        "diagnostics": {"type": "array"}, "history": {"type": "object"}, "saved": {"type": "boolean"}, "label": {"type": ["string", "null"]},
        "note": {"type": ["string", "null"]},
    },
    "required": ["ok", "changed", "revision", "version", "stdout", "error", "traceback", "operations", "affectedProducts",
                 "removedProducts", "metadataProducts", "fullRebuild", "diagnostics", "history", "saved", "label", "note"],
}

TOOLS = [
    {"name": "describe_model", "description": "The open model: file, schema, revision, length unit, product counts by class, storeys, undo depth and the viewer's state.",
     "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    {"name": "find_products", "description": "Products of a class (and its subtypes), optionally filtered by name or storey, with ids, names and GlobalIds.",
     "inputSchema": {"type": "object", "properties": {
         "class": {"type": "string", "description": "IFC class, e.g. IfcWall", "default": "IfcProduct"},
         "name": {"type": "string", "description": "Case-insensitive substring of the Name"},
         "storey": {"type": "string", "description": "Name of the containing storey"},
         "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}}, "additionalProperties": False}},
    {"name": "product_info", "description": "One entity by express id or GlobalId: attributes, container, property sets, representation items and placement.",
     "inputSchema": {"type": "object", "properties": {"id": {"type": "integer"}, "guid": {"type": "string"}}, "additionalProperties": False}},
    {"name": "inspect_model", "description": "Run read-only Python against the model and return what it prints; every modification is discarded. Same names as edit_model scripts.",
     "inputSchema": {"type": "object", "properties": {"code": {"type": "string", "description": "Python that prints what you need to know"}}, "required": ["code"], "additionalProperties": False}},
    {"name": "edit_model", "description": "Run a Python script that changes the model inside a transaction; the file is saved and the viewer follows. The result carries the revision and, when a viewer is attached, the kernel's affected and removed products.",
     "inputSchema": {"type": "object", "properties": {"script": {"type": "string", "description": "Complete script using model, ifcopenshell, api, element, guid, selection, selected"},
                                                      "summary": {"type": "string", "description": "One sentence about the change"}}, "required": ["script"], "additionalProperties": False},
     "outputSchema": _RUN_RESULT},
    {"name": "undo", "description": "Undo the last change as a new version.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}, "outputSchema": _RUN_RESULT},
    {"name": "redo", "description": "Redo the change the last undo removed, as a new version.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}, "outputSchema": _RUN_RESULT},
    {"name": "export_model", "description": "Write the committed IFC to a path (default: the followed file).",
     "inputSchema": {"type": "object", "properties": {"path": {"type": "string", "description": "Destination .ifc path"}}, "additionalProperties": False}},
    {"name": "new_model", "description": "Start a new IFC model with a project, site, building and storeys, saved to a path that the session then follows.",
     "inputSchema": {"type": "object", "properties": {
         "schema": {"type": "string", "enum": ["IFC2X3", "IFC4", "IFC4X3"], "default": "IFC4"}, "name": {"type": "string", "default": "New project"},
         "units": {"type": "string", "enum": ["m", "mm"], "default": "m"}, "site": {"type": "string"}, "building": {"type": "string"},
         "storeys": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "elevation": {"type": "number"}}, "required": ["name"]}},
         "path": {"type": "string", "description": "Where the model is saved and followed"},
         "force": {"type": "boolean", "default": False, "description": "Overwrite an existing file"}}, "additionalProperties": False}},
    {"name": "open_model", "description": "Follow another IFC file; it becomes the file that commits are saved to.",
     "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": False}},
    {"name": "get_selection", "description": "What the user selected in the viewer that follows this session, if any.",
     "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    {"name": "list_examples", "description": "Ready-made Python scripts: a door, a wall raise, and more.",
     "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
]

RESOURCES = [
    {"uri": "tessifc://script-api", "name": "Script API", "description": "The names available to inspect_model and edit_model scripts.", "mimeType": "text/plain"},
    {"uri": "tessifc://model/summary", "name": "Model summary", "description": "The open model in a few lines.", "mimeType": "text/plain"},
]

BUILD_PROMPT = """Build a small {style} in the open IFC model: {storeys} storeys, footprint about {footprint}.
Call describe_model first. Then call edit_model once per step, in this order: exterior walls of the ground floor, the base slab, interior walls, doors, windows, the upper storey's slab and walls with windows, the roof slab, a few columns and a beam, property sets (Pset_WallCommon with IsExternal and LoadBearing), and colours.
Use ifcopenshell.api (root.create_entity, geometry.add_wall_representation, geometry.add_slab_representation, geometry.assign_representation, geometry.edit_object_placement, spatial.assign_container, void.add_opening, void.add_filling, pset.add_pset, pset.edit_pset, style.add_style, style.add_surface_style, style.assign_representation_styles); read tessifc://script-api when unsure. After every step check the result and fix any error before continuing.
Finish with the words BUILDING COMPLETE and the product counts from describe_model."""


def clip(text: str, limit: int = OUTPUT_LIMIT) -> str:
    text = str(text or "")
    return text if len(text) <= limit else f"{text[:limit]}\n... {len(text) - limit} more characters"


class ToolError(Exception):
    """A tool result the client should see as an error; the message is the JSON payload."""


class ModelTools:
    """The tool implementations over an EditSession, independent of the transport."""

    def __init__(self, session: EditSession, *, viewer_url: str | None = None):
        self.session = session
        self.viewer_url = viewer_url

    # ---------------------------------------------------------- helpers

    def _query(self, code: str) -> dict:
        result = self.session.run_script(code, None, commit=False, label="query")
        if not result["ok"]:
            raise ToolError(json.dumps({"error": result.get("error"), "traceback": result.get("traceback", "")}))
        return json.loads(result["stdout"].strip() or "null")

    def _viewer_recent(self) -> bool:
        seen = getattr(self.session, "viewer_seen", None)
        return seen is not None and time.time() - seen < VIEWER_RECENT_SECONDS

    def _run_record(self, result: dict, *, label: str | None) -> dict:
        applied = None
        note = None
        if result.get("changed") and self._viewer_recent():
            applied = self.session.wait_for_applied(result["version"], APPLIED_WAIT_SECONDS)
            if applied is None:
                note = "The viewer did not report this version in time; affected products are unknown."
        elif result.get("changed"):
            note = "No viewer is attached; connect one for the kernel's affected products."
        status = self.session.describe()
        return {
            "ok": bool(result["ok"]), "changed": bool(result.get("changed")), "revision": str(result.get("revision")),
            "version": result.get("version"), "stdout": result.get("stdout", ""), "error": result.get("error"),
            "traceback": result.get("traceback"), "operations": result.get("operations", {}),
            "affectedProducts": applied.get("affectedProducts") if applied else None,
            "removedProducts": applied.get("removedProducts") if applied else None,
            "metadataProducts": None, "fullRebuild": applied.get("fullRebuild") if applied else None,
            "diagnostics": [], "history": {"undo": status["undo"], "redo": status["redo"]}, "saved": True,
            "label": label, "note": note,
        }

    def context(self) -> str:
        summary = self.session.summary()
        status = self.session.describe()
        products = ", ".join(f"{name} {count}" for name, count in list(summary.get("products", {}).items())[:40]) or "none"
        storeys = "; ".join(f"#{item['id']} {item.get('name') or 'unnamed'}" for item in summary.get("storeys", [])[:20])
        lines = [f"File: {status['name']} ({summary.get('schema') or 'unknown schema'}), revision {status['revision']}, lengths in {summary.get('lengthUnit') or 'the model unit'}.",
                 f"Products by class: {products}."]
        if storeys:
            lines.append(f"Storeys: {storeys}.")
        selection = self.session.selection
        if selection:
            lines.append(f"Selected in the viewer: {selection.get('className') or 'entity'} {selection.get('ids') or selection.get('guids')}.")
        else:
            lines.append("Nothing is selected in the viewer.")
        return "\n".join(lines)

    # ------------------------------------------------------------ tools

    def describe_model(self, **_) -> dict:
        summary = self.session.summary()
        status = self.session.describe()
        return {
            "open": True, "name": status["name"], "path": str(self.session.path), "schema": summary.get("schema"),
            "revision": str(status["revision"]), "version": status["version"], "generation": status["generation"],
            "lengthUnit": summary.get("lengthUnit"), "entities": summary.get("entities"), "products": summary.get("products", {}),
            "storeys": [{"expressId": item["id"], "name": item.get("name"), "elevation": item.get("elevation")} for item in summary.get("storeys", [])],
            "history": {"undo": status["undo"], "redo": status["redo"]}, "saved": True,
            "viewer": {"url": self.viewer_url, "selection": self.session.selection, "applied": self.session.applied} if self.viewer_url else None,
            "text": self.context(),
        }

    def find_products(self, **arguments) -> dict:
        code = f"""
import json
needle = {arguments.get("name")!r}
storey = {arguments.get("storey")!r}
limit = {int(arguments.get("limit", 50))}
products = []
total = 0
for product in model.by_type({str(arguments.get("class", "IfcProduct"))!r}):
    container = element.get_container(product)
    if needle and needle.lower() not in (product.Name or "").lower():
        continue
    if storey and (container is None or container.Name != storey):
        continue
    total += 1
    if len(products) < limit:
        products.append({{"id": product.id(), "class": product.is_a(), "name": product.Name, "guid": product.GlobalId, "storey": container.Name if container else None}})
print(json.dumps({{"products": products, "total": total}}))
"""
        return self._query(code)

    def product_info(self, **arguments) -> dict:
        code = f"""
import json
entity = model.by_guid({arguments.get("guid")!r}) if {arguments.get("guid")!r} else (model.by_id({int(arguments.get("id") or 0)}) if {arguments.get("id")!r} is not None else None)
if entity is None:
    raise ValueError("Give an express id or a GlobalId of an existing entity.")
def plain(value):
    if isinstance(value, ifcopenshell.entity_instance):
        return {{"id": value.id(), "class": value.is_a()}}
    if isinstance(value, (list, tuple)):
        return [plain(item) for item in value]
    return value
attributes = {{name: plain(value) for name, value in entity.get_info(recursive=False).items() if name not in ("id", "type")}}
psets = element.get_psets(entity) if hasattr(entity, "GlobalId") else {{}}
container = element.get_container(entity) if hasattr(entity, "ObjectPlacement") else None
items = []
representation = getattr(entity, "Representation", None)
if representation:
    for rep in representation.Representations:
        for item in rep.Items:
            items.append({{"id": item.id(), "class": item.is_a(), "representation": rep.RepresentationIdentifier}})
placement = None
if getattr(entity, "ObjectPlacement", None) and entity.ObjectPlacement.is_a("IfcLocalPlacement"):
    placement = {{"location": list(entity.ObjectPlacement.RelativePlacement.Location.Coordinates)}}
print(json.dumps({{"id": entity.id(), "class": entity.is_a(), "guid": getattr(entity, "GlobalId", None), "name": getattr(entity, "Name", None),
    "attributes": attributes, "container": container.Name if container else None,
    "propertySets": [{{"name": name, "properties": [{{"name": key, "value": value}} for key, value in values.items() if key != "id"]}} for name, values in psets.items()],
    "representations": items, "placement": placement}}, default=str))
"""
        return self._query(code)

    def inspect_model(self, *, code: str = "", **_) -> dict:
        result = self.session.run_script(str(code), self.session.selection or None, commit=False, label="inspect")
        if not result["ok"]:
            raise ToolError(json.dumps({"ok": False, "error": result.get("error"), "traceback": result.get("traceback", ""), "stdout": result.get("stdout", "")}))
        return {"ok": True, "stdout": result.get("stdout", ""), "error": None, "traceback": None}

    def edit_model(self, *, script: str = "", summary: str | None = None, **_) -> dict:
        result = self.session.run_script(str(script), self.session.selection or None, label=clip(summary, 200) if summary else "edit")
        record = self._run_record(result, label=result.get("label"))
        if not result["ok"]:
            raise ToolError(json.dumps(record, default=str))
        return record

    def undo(self, **_) -> dict:
        return self._run_record(self.session.undo(), label="undo")

    def redo(self, **_) -> dict:
        return self._run_record(self.session.redo(), label="redo")

    def export_model(self, *, path: str | None = None, **_) -> dict:
        target = Path(path).resolve() if path else self.session.path
        if target.suffix.lower() != ".ifc":
            raise ToolError(json.dumps({"error": "The path must end with .ifc"}))
        version, payload = self.session.snapshot()
        if target != self.session.path:
            target.write_bytes(payload)
        return {"path": str(target), "bytes": len(payload), "version": version}

    def new_model(self, *, path: str | None = None, force: bool = False, **options) -> dict:
        from .model import create_model, write_model

        target = Path(path).resolve() if path else self.session.path.with_name(f"{options.get('name', 'New project')}.ifc")
        if target.exists() and not force and target != self.session.path:
            raise ToolError(json.dumps({"error": f"{target} exists; pass force to overwrite it"}))
        model = create_model(options.get("schema", "IFC4"), name=options.get("name", "New project"), units=options.get("units", "m"),
                             site=options.get("site", "Site"), building=options.get("building", "Building"), storeys=options.get("storeys"))
        write_model(model, target)
        self.session.switch_to(target)
        status = self.session.describe()
        summary = self.session.summary()
        return {"revision": str(status["revision"]), "version": status["version"], "generation": status["generation"],
                "storeys": [{"expressId": item["id"], "name": item.get("name"), "elevation": item.get("elevation")} for item in summary.get("storeys", [])],
                "products": 0, "path": str(target)}

    def open_model(self, *, path: str, **_) -> dict:
        self.session.switch_to(path)
        status = self.session.describe()
        summary = self.session.summary()
        return {"revision": str(status["revision"]), "version": status["version"], "generation": status["generation"],
                "products": sum(summary.get("products", {}).values()), "entities": summary.get("entities"), "schema": summary.get("schema"),
                "path": str(self.session.path)}

    def get_selection(self, **_) -> dict:
        return self.session.selection or {"ids": [], "guids": [], "className": None, "name": None, "reportedAt": None}

    def list_examples(self, **_) -> dict:
        return {"examples": [{"title": example["title"], "source": example["source"]} for example in PYTHON_EXAMPLES]}

    def call(self, name: str, arguments: dict) -> dict:
        handler = getattr(self, name, None)
        if handler is None or name.startswith("_") or name in ("call", "context"):
            raise ToolError(json.dumps({"error": f"Unknown tool {name}."}))
        try:
            return handler(**(arguments or {}))
        except SessionBusy as error:
            raise ToolError(json.dumps({"error": str(error), "busy": True})) from error
        except SessionError as error:
            raise ToolError(json.dumps({"error": str(error)})) from error
        except (OSError, ValueError, TypeError) as error:
            raise ToolError(json.dumps({"error": str(error)})) from error


def create_mcp_server(session: EditSession, *, viewer_url: str | None = None, version: str = "0.0.0"):
    """The low-level MCP server over the session; `run_stdio` connects it to stdin and stdout."""
    from mcp import types
    from mcp.server import Server

    tools = ModelTools(session, viewer_url=viewer_url)
    instructions = " ".join(part for part in [
        "tessifc-session edits one IFC file with Python scripts run through IfcOpenShell inside a transaction and saved atomically.",
        "Start with describe_model, inspect with inspect_model, change with edit_model, one step per call; scripts see model, ifcopenshell, api, element, guid, selection and selected.",
        "Read the resource tessifc://script-api for the script names.",
        f"A viewer follows every change at {viewer_url}; get_selection returns what the user clicked there." if viewer_url else "",
    ] if part)
    server = Server("tessifc", version=version, instructions=instructions)

    @server.list_tools()
    async def list_tools():
        return [types.Tool(**tool) for tool in TOOLS]

    @server.call_tool()
    async def call_tool(name: str, arguments: dict):
        import anyio

        try:
            payload = await anyio.to_thread.run_sync(tools.call, name, arguments or {})
        except ToolError as error:
            raise RuntimeError(str(error)) from error
        text = clip(json.dumps(payload, indent=2, default=str))
        if any(tool["name"] == name and "outputSchema" in tool for tool in TOOLS):
            return [types.TextContent(type="text", text=text)], payload
        return [types.TextContent(type="text", text=text)]

    @server.list_resources()
    async def list_resources():
        return [types.Resource(uri=item["uri"], name=item["name"], description=item["description"], mimeType=item["mimeType"]) for item in RESOURCES]

    @server.read_resource()
    async def read_resource(uri):
        if str(uri) == "tessifc://script-api":
            return SCRIPT_API
        if str(uri) == "tessifc://model/summary":
            return tools.context()
        raise ValueError(f"Unknown resource {uri}")

    @server.list_prompts()
    async def list_prompts():
        return [types.Prompt(name="build-a-building", description="Step-by-step brief for building a small building in the open model.",
                             arguments=[types.PromptArgument(name=name, required=False) for name in ("storeys", "footprint", "style")])]

    @server.get_prompt()
    async def get_prompt(name: str, arguments: dict | None):
        if name != "build-a-building":
            raise ValueError(f"Unknown prompt {name}")
        arguments = arguments or {}
        text = BUILD_PROMPT.format(storeys=arguments.get("storeys") or "2", footprint=arguments.get("footprint") or "10 x 8 m", style=arguments.get("style") or "house")
        return types.GetPromptResult(messages=[types.PromptMessage(role="user", content=types.TextContent(type="text", text=text))])

    return server


async def run_stdio(session: EditSession, *, viewer_url: str | None = None, version: str = "0.0.0"):
    """Serve MCP on the real stdout; everything else printed goes to stderr from here on."""
    import anyio
    from mcp.server.stdio import stdio_server

    # The process's original stdout carries the protocol even after prints were redirected.
    real_stdout = sys.__stdout__
    sys.stdout = sys.stderr
    stdout = anyio.wrap_file(io.TextIOWrapper(real_stdout.buffer, encoding="utf-8", write_through=True))
    stdin = anyio.wrap_file(io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8"))
    server = create_mcp_server(session, viewer_url=viewer_url, version=version)
    async with stdio_server(stdin=stdin, stdout=stdout) as (read_stream, write_stream):
        await server.run(read_stream, write_stream, server.create_initialization_options())
