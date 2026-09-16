<!-- SPDX-License-Identifier: Apache-2.0 -->
# Editing IFC without rewriting it

TessIFC edits IFC-SPF at the source boundary, not by serializing the geometry
model back into a new file. This matters because a conventional rewrite loses
or normalizes information the geometry engine does not understand: comments,
whitespace, record order, vendor classes, newer-schema attributes, and the
authoring application's exact spelling of unchanged values.

## Invariants

An accepted attribute edit has these properties:

1. Only the requested top-level argument byte span changes.
2. Every untouched source byte remains identical.
3. Strings are escaped by TessIFC, including apostrophes and UTF-16 STEP
   escapes. Raw values must parse as exactly one STEP value.
4. All edit spans are resolved before any replacement and applied back-to-front.
5. The source length and per-record hash must match the parsed image.
6. The edited file is reparsed before it replaces the open model or is written.
7. Entity count and every edited express id must survive verification.

Unknown simple classes remain editable by zero-based argument index. Complex
instances are edited by named schema leaf and leaf-local argument index; a flat
numeric edit is refused because it would be ambiguous.

## CLI

Text values are encoded safely:

```sh
tessifc edit model.ifc --id 219 --attribute Name --value "External wall" -o edited.ifc
```

Lists, enumerations, references and typed values use validated raw STEP syntax:

```sh
tessifc edit model.ifc --id 9001 --argument 2 --raw --value "(1.,2.,3.)" -o edited.ifc
```

The CLI refuses to overwrite the input path. Write a new file, inspect it, and
replace the original through the user's normal version-control workflow.

## WASM and viewer

`Kernel.getEntityInfo` exposes schema names, exact raw spelling and decoded text.
`Kernel.setAttributes` applies a form save as one transaction and one reparse.
These immediate edits advance the revision and discard a pending staged
candidate, so a host that stages revisions should not mix them in.
`Kernel.exportModel` returns the current source. The viewer keeps the model in a
worker, edits scalar text fields, and downloads an `.edited.ifc` revision with
**Save IFC** or `Ctrl+S`.

The viewer's attribute form exposes text fields. **Update IFC** accepts an
externally edited snapshot, including geometry changes, additions and deletions.
Both paths use the staged revision API described in the [SDK](sdk.md).

## Incremental geometry revisions

A revision is prepared separately from the committed model. The kernel compares
decoded entities and follows dependencies in both the old and new models. For
example, changing an opening also invalidates its host; changing a shared profile
invalidates its consumers. Physical proximity alone does not imply a dependency.
Entity IDs address a particular snapshot. Product GlobalIds help preserve viewer
selection and visibility across a conservative rebuild.

The kernel evaluates the affected products with the established geometry settings
and coordinate offset before allowing commit. Rejected candidates leave the
committed source and scene intact. Metadata-only changes skip tessellation.
Unknown or global effects, including units, context and product identity changes,
can require a full geometry rebuild. The impact result reports `fullRebuild` and
per-product reasons.

After commit, the viewer retires replaced instance slots and appends new ones.
Unchanged slots keep their IDs; identical product geometry keeps its existing
resources. The renderer rebuilds affected batches and preserves unrelated GPU
buffers. A shared batch can include more than the edited product, and CPU spatial
and depth indexes are currently rebuilt. Selection, camera, clipping and
visibility are retained where their identities remain valid; measurements are
cleared after a geometry change.

The first implementation reparses the full candidate file and scans dependencies.
It selectively tessellates and uploads geometry; it is not an incremental text
parser. External snapshots retain exactly the submitted bytes, so the attribute
edit byte-preservation guarantees above do not extend to an external tool's
serialization. Deleted and replaced meshes and GPU resources are reclaimed, but
retired CPU instance slots accumulate until the model is reopened. Very long edit
sessions should use an explicit reopen checkpoint.

The worker commits before the main thread applies its prepared scene patch. If
the renderer then fails, the viewer marks the scene stale and requires reopening
the exported committed IFC. Durable patch replay and cross-process transactions
are outside this preview.

## Scripts in the viewer

The **Session** panel (the `</>` icon on the right rail) runs scripts against
the open model. With nothing else installed, scripts are JavaScript and run in
the browser: the worker executes them next to the kernel, collects the edits,
writes a new IFC snapshot and stages it through the revision path above. Only
the affected products are tessellated and uploaded, and the report under the
script comes from the kernel's impact analysis, not from the script.

Pick an example from the **Examples** menu, such as *Add a door to a wall*,
and press Run or Ctrl Enter. Scripts see `ifc`, `selected`, `selection` and
`print`:

```js
const wall = selected ?? ifc.byType("IfcWall")[0];
const solid = wall.Representation.Representations[0].Items[0];
solid.Depth = solid.Depth + 0.5;                       // an attribute edit
const column = ifc.addBox("IfcColumn", "New column",   // placed geometry
  { at: [2, 1, 0], size: [0.3, 0.3, 3] });
ifc.contain(column, ifc.container(wall));
print("raised", wall.Name, "and added", column);
```

Entities expose their IFC attributes by name, references resolve to entities
and lists to arrays. `ifc.add(className, attributes)` creates a record from
attributes by name using the model's schema, `ifc.remove(entity)` deletes one
and detaches every reference, `ifc.addBox`, `ifc.contain`, `ifc.void`,
`ifc.fill` and `ifc.aggregate` cover the common authoring steps, and
`ifc.inverses`, `ifc.container` and `ifc.byGuid` answer the common queries.
Building helpers go one level up: `ifc.addWall`, `ifc.addSlab`,
`ifc.addDoor`, `ifc.addWindow`, `ifc.addColumn`, `ifc.addBeam`,
`ifc.addStorey`, `ifc.addProperties` and `ifc.setColor` write the profiles,
placements, openings, relationships, property sets and styles a building
needs in the model's schema, and **Examples > Build a small house** shows
them together: it adds a house beside whatever model is open. The
[SDK](sdk.md#browser-scripts) lists the whole API and [Agents and
pipelines](agents.md) shows the same loop outside the viewer. A script that
throws publishes nothing; a script that changes nothing publishes nothing.
`Undo` and `Redo` republish the source as it was before or after the last
change, whether a script, an attribute save or an update from a file, as new
revisions. Browser scripts make the model dirty: the download icon in the top
bar exports the result.

The viewer also opens a model that has no product geometry yet, such as one
written by `createModel` or `tessifc-mcp --new`: the status line says so,
the tree shows the storeys, and the first script that adds a product
publishes an ordinary selective revision.

**JavaScript from another process** is the same engine outside the browser.
`node bindings/mcp/src/cli.js model.ifc` (or `--new house.ifc`) holds the
model in a Node kernel, saves it after every accepted edit and serves the
viewer at a loopback address; open the printed address and the Session panel
runs its scripts on that host, whose undo and redo the panel shares, while an
agent connected over MCP edits the same model. The viewer follows every
revision with the kernel's affected-product report.

**Python with IfcOpenShell** is the third engine. Start a local session,
`python scripts/serve-edit-session.py model.ifc`, and open the address it
prints. The session process owns the file, parses it once with an installed
IfcOpenShell, runs scripts with `model`, `ifcopenshell`, `api`, `element`,
`guid`, `selection` and `selected` defined inside a transaction, and publishes
each accepted change by writing the file atomically. The viewer follows the
file, so any other process may save it too. The panel switches its examples
and its language when the session is connected. `--new` creates the file
first and `--mcp` puts the same tools on stdio for an agent.

## The assistant

The **Assistant** tab has Ask and Edit modes. Ask reads the model through a
read-only script tool and answers. Edit proposes one script with a summary;
by default you review it and run it from the card, and *Run edits
automatically* runs it at once. The model summary, the selection and its
attributes are sent as context; model text is treated as data, never as
instructions. The reply is followed by the kernel's own report of what the
viewer changed.

In the browser, choose the provider under **Settings > Assistant**:
OpenRouter, Anthropic, or any OpenAI-compatible chat-completions URL such as
Ollama or LM Studio on your machine. The key is stored in this browser only
and sent only to that provider. With a Python session the assistant runs on
the session process instead (`--assistant anthropic` with `ANTHROPIC_API_KEY`).
An agent of your own, Claude Code or Claude Desktop connects through the MCP
servers described in [Agents and pipelines](agents.md) and the viewer shows
its work the same way.

Scripts, whether typed or generated, run with your user's permissions and
without a sandbox: in the page's worker for JavaScript, in the session
process for Python. Review generated code before running it.
