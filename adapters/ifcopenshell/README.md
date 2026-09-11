<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-session

A live editing session for the TessIFC viewer. It follows one IFC file, runs
Python scripts against it with an installed IfcOpenShell, saves every accepted
change atomically, and lets the viewer refresh only the affected products. An
optional assistant answers questions about the model and proposes edit scripts.

The kernel stays a geometry engine: this package is an optional integration
that talks to the viewer over a loopback HTTP protocol. IfcOpenShell and the
assistant SDK are installed separately by you.

```sh
pip install -e adapters/ifcopenshell[authoring,assistant]
tessifc-session model.ifc                # or: python scripts/serve-edit-session.py model.ifc
```

Open the printed address. The viewer's **Session** panel then runs Python
instead of its built-in browser JavaScript, and, when a provider is
configured, the Ask and Edit assistant runs on this process.

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

## Assistant

Set `ANTHROPIC_API_KEY` and start with `--assistant anthropic` (the default
when a key is present). `--model` and `--effort` tune the request. Ask mode
inspects the model through a read-only script tool; Edit mode proposes one
script that you review and run from the panel, or runs it immediately when
the panel's automatic policy is on. `--assistant fake` is a deterministic
stand-in used by the tests.

## Your own agent loop

`Assistant(session, provider)` accepts any provider object with a
`complete(system=, tools=, messages=)` method that returns a Messages-API
shaped response (`content` blocks, `stop_reason`, `usage`); `FakeProvider`
is the smallest example. To drive the tools from a loop you already have,
call `assistant.execute_tool(name, arguments, selection, policy)` with the
definitions in `INSPECT_TOOL` and `PROPOSE_TOOL`; it returns the tool result
text and an error flag. [Agents and pipelines](../../docs/agents.md) describes
the loop and the review policy.

## Boundary

The server binds to the loopback interface, checks the Host and Origin
headers, and requires a per-session token on every command. Scripts run inside
the session process with your user's permissions and without a sandbox, so
review generated code before running it. The provider key is removed from the
process environment before any script runs.
