<!-- SPDX-License-Identifier: Apache-2.0 -->
# Incremental IFC editing

A script in the viewer, a Python process or the assistant edits the IFC;
TessIFC detects changed entities and their dependencies, then replaces affected
viewer geometry. The snapshot path reparses the edited IFC. Unrelated products
are not tessellated again.

## In the browser

Nothing to install beyond the viewer. Build the package, serve the checkout
and open a model:

```sh
python scripts/build-wasm.py --target web
python -m http.server 8000 --bind 127.0.0.1
```

Open <http://127.0.0.1:8000/viewer/>, drop an IFC on it, open the **Session**
panel from the `</>` icon on the right rail, choose **Examples > Add a door to
a wall** and press **Run**. The wall gets an opening and a door, the tree
gains the door, and the status line reports the revision and the three
products that were rebuilt. The other examples raise, move, rename and delete
the selection, add a column, or list walls without changing anything. `Undo`
and `Redo` republish the source as it was.

```js
const wall = selected ?? ifc.byType("IfcWall")[0];
const solid = wall.Representation.Representations[0].Items[0];
solid.Depth = solid.Depth + 0.5;
print("raised", wall.Name);
```

For the **Assistant** tab, open **Settings > Assistant** and choose OpenRouter,
Anthropic or an OpenAI-compatible URL, enter the model and the key. Ask mode
answers questions from the model; Edit mode proposes a script that you review
and run from the card.

To let an agent outside the browser edit the model, or to build one from
nothing, see the [agent-building example](../agent-building/README.md): it
runs the same scripts from Node, from Claude Code over MCP and from Python.

## With Python and IfcOpenShell

Use a Python environment where IfcOpenShell is installed:

```sh
python scripts/build-wasm.py --target both
python examples/incremental-edit/edit.py demo.ifc init
python scripts/serve-edit-session.py demo.ifc
```

Open the local address printed by the server. The **Session** panel now runs
Python against the model held by the session, with its own examples:

```python
wall = selected or model.by_type("IfcWall")[0]
solid = wall.Representation.Representations[0].Items[0]
solid.Depth += 0.5
print("raised", wall.Name)
```

`Run` executes the script in a transaction, saves the file atomically and
reports what the viewer updated. `Undo` and `Redo` publish the previous or
restored content as new revisions. With `ANTHROPIC_API_KEY` set,
`python scripts/serve-edit-session.py demo.ifc --assistant anthropic` enables
the assistant on the session (`pip install anthropic` first).

The same file can still be edited from any other process:

```sh
python examples/incremental-edit/edit.py demo.ifc raise
python examples/incremental-edit/edit.py demo.ifc opening
python examples/incremental-edit/edit.py demo.ifc create
python examples/incremental-edit/edit.py demo.ifc delete
python examples/incremental-edit/edit.py demo.ifc rename
```

The wall-height edit changes its extrusion. The opening edit changes a profile
and also refreshes the host wall. Renaming updates metadata without tessellation.
The status bar reports the revision and affected geometry counts.

Saving through atomic replacement avoids partially written snapshots. The
server follows only the named IFC; scripts run in the session process, or in
the page's worker for browser scripts, without a sandbox, so run code you
trust and review assistant proposals. Stop the server with Ctrl+C.

Without the server, open the original model normally and choose
**File > Update IFC** with its edited version. The existing **Export IFC**
action downloads the committed source.

This implementation retains full-file parsing and conservative dependency
analysis. Units, contexts and uncertain identity changes can require a full
geometry refresh. Invalid candidate revisions leave the previous revision
available. Persistent model overlays and transform-only SDK updates are
subsequent work.
