<!-- SPDX-License-Identifier: Apache-2.0 -->
# @tessifc/edit

The editing session over the TessIFC kernel: scripts, attribute edits and
external snapshots become revisions, each revision becomes a scene delta that
names the affected products and carries their replacement geometry, and a
small set of agent tools lets any model drive the same loop. Pure JavaScript,
browser and Node, no dependency beyond `@tessifc/core`.

```js
import { Kernel } from "@tessifc/core/node";
import { createEditingSession } from "@tessifc/edit";

const kernel = new Kernel();
const id = kernel.openModel(bytes);
const session = createEditingSession(kernel, id, { settings: { includeOpenings: true } });
const { pack } = session.evaluate();                       // the initial scene as a parsed IGP pack

const { report, delta } = session.runScript(`
  const wall = ifc.byType("IfcWall")[0];
  const solid = wall.Representation.Representations[0].Items[0];
  solid.Depth = solid.Depth + 0.5;
  print("raised", wall.Name);
`);
console.log(report.stdout, delta.revision, delta.affectedProducts);   // one product
myScene.applyDelta(delta);                                            // your renderer, or @tessifc/three
```

`session.setAttributes`, `session.applySnapshot`, `session.refreshProducts`,
`session.undo` and `session.redo` return the same delta shape.

## From nothing to a building

`createModel` writes a minimal IFC file (project, units, contexts, site,
building and storeys) so a session can start empty, and the script API's
building helpers add the rest:

```js
import { createModel } from "@tessifc/edit";

const session = createEditingSession(kernel, kernel.openModel(createModel({ schema: "IFC4", name: "House" })));
session.evaluate();
const { delta } = session.runScript(`
  const wall = ifc.addWall({ from: [0, 0], to: [6, 0], height: 3, thickness: 0.3, name: "South wall" });
  ifc.addDoor({ in: wall, at: [0, 0, 0], size: [0.9, 2.1] });
  ifc.addSlab({ size: [6, 4], at: [3, 2, 0], thickness: 0.2, type: "BASESLAB" });
  ifc.addProperties(wall, "Pset_WallCommon", { IsExternal: true });
  ifc.setColor(wall, [0.9, 0.85, 0.8, 1]);
`);
console.log(delta.affectedProducts);                       // the wall, the opening, the door and the slab
```

## For an agent

`createAgentTools(session, { policy, onDelta })` executes the three tools
(`inspect_model`, `propose_edit`, `undo_edit`) against a session, and
`runAgentTurn({ complete, tools, prompt, mode, context })` drives any
provider until the model answers. `chatCompletions` and `anthropicMessages`
from `@tessifc/edit/providers` are `complete` functions for the two common
wire formats, `describeModel(session)` builds the context, and
`verifyRevision` from `@tessifc/edit/verify` checks the scene built from the
deltas against a fresh evaluation of the exported file. The [SDK
page](https://github.com/nbharathik/tessifc/blob/main/docs/sdk.md) documents
every module and the script API; [Agents and
pipelines](https://github.com/nbharathik/tessifc/blob/main/docs/agents.md) shows the loop, the MCP server that
packages it and how a renderer applies a delta.

Scripts run with your program's permissions and without a sandbox.
`runScript` runs them in the calling thread without a time limit; in Node,
`createScriptRunner({ kernelModule, timeoutMs })` from
`@tessifc/edit/script-runner` with `session.runScriptWith(runner, source)`
runs each script in a worker thread with its own kernel and stops one that
overruns by ending the thread.
