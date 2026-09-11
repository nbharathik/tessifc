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
`session.undo` and `session.redo` return the same delta shape. The
[SDK page](../../docs/sdk.md) documents the session and the script API;
[Agents and pipelines](../../docs/agents.md) shows the agent tools and how a
renderer applies a delta.

Scripts run with your program's permissions and without a sandbox.
