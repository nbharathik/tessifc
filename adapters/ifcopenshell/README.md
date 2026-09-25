<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-session

A live editing session for the TessIFC viewer. It follows one IFC file, runs
Python scripts against it with an installed IfcOpenShell, saves every accepted
change atomically, and lets the viewer refresh only the affected products. An
optional assistant answers questions about the model and proposes edit scripts.

The kernel stays a geometry engine: this package is an optional integration
that talks to the viewer over a loopback HTTP protocol. IfcOpenShell and the
assistant SDK are installed separately by you. For reading and tessellating
from Python without IfcOpenShell, the `tessifc` wheel in `bindings/python`
is the kernel itself.

```sh
pip install -e "adapters/ifcopenshell[authoring,assistant,mcp]"
tessifc-session model.ifc                # or: python scripts/serve-edit-session.py model.ifc
tessifc-session house.ifc --new          # a new model: project, site, building and storeys
```

Open the printed address as it is: the part after `#token=` is the session's
key, which the page keeps and removes from the address bar. The viewer's
**Session** panel then runs Python instead of its built-in browser
JavaScript, and, when a provider is configured, the Ask and Edit assistant
runs on this process. `--new` creates the file when it is missing
(`--schema IFC2X3|IFC4|IFC4X3`, `--storeys "Ground floor:0,Upper floor:3"`).
The server listens on port 8000, or on a free port when 8000 is taken;
`--port` names one and fails if it is taken (`0` picks a free one).

## For an agent over MCP

```sh
claude mcp add tessifc-py -- python scripts/serve-edit-session.py house.ifc --mcp --new
```

With `--mcp` (the `mcp` extra) the process speaks the Model Context Protocol
on stdin and stdout while the viewer server keeps running, so Claude Code,
Claude Desktop or any MCP client edits the model with Python scripts and a
person watches at the printed address (`describe_model` returns it as
`viewer.url` when the client hides stderr). The tools are the ones `@tessifc/mcp`
offers (`describe_model`, `find_products`, `product_info`, `inspect_model`,
`edit_model`, `undo`, `redo`, `export_model`, `new_model`, `open_model`,
`get_selection`, `list_examples`; `verify_revision` needs the TessIFC kernel
and is Node only). An `edit_model` result carries the affected products the
viewer reported once it applied the version, or `null` with a note when no
viewer is attached. Every message the process prints goes to stderr;
stdout is the protocol.

## From Python

```python
from tessifc_session import EditSession

session = EditSession("model.ifc")
result = session.run_script("""
wall = model.by_type("IfcWall")[0]
wall.Representation.Representations[0].Items[0].Depth += 0.5
print("raised", wall.Name)
""")
print(result["ok"], result["changed"], result["stdout"], result["operations"])
session.undo()
```

Scripts see `model`, `ifcopenshell`, `api`, `element`, `guid`, `selection` and
`selected`. A script runs inside an IfcOpenShell transaction; an exception rolls
it back and nothing is written. A script that changes nothing publishes nothing.
Each accepted run replaces the file atomically, and a running viewer session
picks the new content up through the same snapshot path as **Update IFC**.
In IFC2X3 the API's owner hooks are set while a script runs, so entities the
script creates carry a valid owner history.

`tessifc_session.model.create_model(schema, name=, units=, site=, building=,
storeys=)` returns a new `ifcopenshell.file` with the same skeleton the
JavaScript `createModel` writes, and `write_model(model, path)` saves it
atomically. `examples/agent-building/build_house.py` builds the demo house
with `ifcopenshell.api` on top of it.

## Assistant

Set `ANTHROPIC_API_KEY` and start with `--assistant anthropic` (the default
when a key is present). `--model` and `--effort` tune the request. Ask mode
inspects the model through a script tool whose model edits are discarded; Edit mode proposes one
script that you review and run from the panel, or runs it immediately when
the panel's automatic policy is on. `--assistant fake` is a deterministic
stand-in used by the tests.

## Your own agent loop

`Assistant(session, provider)` accepts any provider object with a
`complete(system=, tools=, messages=)` method that returns a Messages-API
shaped response (`content` blocks, `stop_reason`, `usage`); `FakeProvider`
is the smallest example. To drive the tools from a loop you already have,
call `assistant.execute_tool(name, arguments, selection, policy)` with the
definitions in `INSPECT_TOOL`, `PROPOSE_TOOL` and `UNDO_TOOL`; it returns the
tool result text and an error flag. [Agents and pipelines](https://github.com/nbharathik/tessifc/blob/main/docs/agents.md)
describes the loop and the review policy.

## Boundary

The server binds to the loopback interface, checks the Host and Origin
headers, and requires a per-session token on every session request, reads
included; the token reaches the page only through the printed address. Scripts
run inside the session process with your user's permissions and without a
sandbox, `inspect_model` included (only its model edits are discarded), so
review generated code before running it. The provider key is removed from the
process environment before any script runs, but the provider client still
holds it in memory, where a script can reach it.
