<!-- SPDX-License-Identifier: Apache-2.0 -->
# @tessifc/viewer

A minimal IFC viewer over the TessIFC kernel: one container, one kernel and a
small API. It is the same WebGL2 renderer the reference viewer uses, without
the ribbon, panels and tree, so you can build your own interface on top of
it. No runtime dependency beyond `@tessifc/core` and the IGP reader in
`@tessifc/edit`.

```js
import init, { Kernel } from "@tessifc/core/web";
import { createViewer } from "@tessifc/viewer";

await init();
const viewer = createViewer(document.getElementById("host"), { kernel: new Kernel(), theme: "dark" });

viewer.on("progress", ({ done, total }) => console.log(`${done} of ${total} products`));
viewer.on("select", (selection) => console.log(selection?.expressIds ?? "nothing selected"));

const { info, summary, hierarchy } = await viewer.open(file);   // File, Blob, ArrayBuffer or Uint8Array
viewer.select(hierarchy.nodes[0].expressId);
viewer.focus();                                                  // frame the selection
viewer.setSection({ axis: "z", fraction: 0.6 });
```

Geometry streams into the view while the kernel is still working, so a large
model appears within the first second. The kernel runs on the page's thread
here; `createViewer(host, { worker: true })` runs it in a Web Worker instead
(see below), and `loadPack` accepts an IGP pack produced anywhere, including
by a worker of your own or the `tessifc` CLI.

## API

| Call | What it does |
|---|---|
| `createViewer(container, options)` | Creates the canvas inside `container`. Options: `kernel`, `hiddenFlags`, `lodPixels`, `theme`, `background`, `selectOnClick`, `focusOnDoubleClick`, `coincidence` (`false` skips the worker that refines the coincident-surface overlay), `requireGeometry` (`true` refuses a model without drawable geometry), `textures` (`true` asks the kernel for materials and textures and paints them in the shaded style), `allowRemoteTextures` (`true` also fetches image textures from other origins), `textureBaseUrl` (where a texture's relative path resolves; the page's URL by default) `worker` (`true`, or `{ url, wasmUrl, editUrl, scriptTimeoutMs }`: run the kernel in a Web Worker; no `kernel` is needed then), `occlusion` (`false` draws every product group on moving frames too) and `motionLod` (`false` draws the full meshes on moving frames instead of the pack's coarse levels). |
| `open(source, { settings, modelSettings })` | Parses the IFC with the kernel and streams its geometry. Resolves with `modelId`, `info`, `summary` and `hierarchy`, or `null` when a later `open`, `loadPack`, `close` or `dispose` superseded it. A model with no product geometry yet opens empty; the `load` event says `empty: true`. |
| `loadPack(bytes)` | Shows an IGP pack directly, without a kernel. |
| `close()` | Drops the model from the view and the kernel. |
| `select(ids)`, `selection()` | Selects express ids (`null` clears); every part of a product is selected together. |
| `hide(ids)`, `show(ids)`, `isolate(ids)`, `showAll()` | Visibility by express id; `isolate(null)` ends an isolation. |
| `setHiddenFlags(flags)` | Which helper categories (spaces, openings, references) stay hidden by default. |
| `fit()`, `focus(ids?)`, `setView(mode)`, `setStyle(style)` | Camera and display: views `perspective`, `top`, `front`, `right`; styles `shaded`, `xray`, `wire`. |
| `setTextures(active)` | Paint the pack's textures, or every product in its flat colour. A model opened while textures were off carries none until it is opened again. |
| `setOcclusion(active)` | Leave product groups hidden behind the model's largest faces out of moving frames, or draw everything. |
| `setMotionLod(active)` | Draw the pack's coarse mesh levels on moving frames, or the full meshes always. |
| `setSection({ axis, value or fraction, flipped, cap })`, `setSection(null)` | A cut along one axis, in IFC coordinates or as a fraction of the bounds. |
| `pick(clientX, clientY)` | The product and surface point under a pointer. |
| `setPivot(point)`, `zoom(factor)` | The orbit and zoom centre, and a programmatic zoom step. |
| `on(event, listener)` | `load`, `progress`, `select`, `visibility`, `camera`, `overlay`, `close`, `revision`, `session`, `reopen`; returns the unsubscribe function. |
| `session()` | The `@tessifc/edit` editing session over the open model, adopted to the streamed scene: `runScript`, `setAttributes`, `applySnapshot`, `undo`, `redo`, `export`. In worker mode the same calls return promises. |
| `applyDelta(delta)` | Applies a session delta: retires the affected and removed products, adds their replacements (or rebuilds everything for a `full` delta), keeps selection and visibility by GlobalId, flashes the changed products and emits `revision`. |
| `follow(baseUrl)`, `unfollow()` | Follows a local session host (`tessifc-mcp` or the Python session server) at `baseUrl` (`""` is the page's own origin): every published version is opened or applied as a delta; `session` events carry the host's status. `follow` returns the client, with `run`, `undo`, `redo` and `reportSelection`. |
| `worker()` | Worker mode's state: whether its worker runs, the kernel version and the script limit; `null` on the page-thread path. |
| `lodState()` | How many coarse levels the pack carries, how many batches draw them, whether the switch is on and whether the last frame used them. |
| `pack()`, `hierarchy()`, `modelId()`, `overlayState()`, `renderer` | The assembled pack, the kernel's spatial tree, the model id, whether the coincident-surface overlay has been refined (`pending`, `ready`, `exhausted` when the model was too large for the analysis budget and whole products stay in the overlay, `failed`, `off`) and the renderer itself for anything not covered above. |
| `dispose()` | Releases the GPU resources and removes the canvas. |

Left-drag orbits, right-drag or Shift-drag pans, the wheel zooms toward the
pivot, a click selects and moves the pivot to the surface it hit, and a
double-click frames the product. Everything the mouse does is also reachable
through the API, so a host can replace the built-in gestures by passing
`selectOnClick: false` and `focusOnDoubleClick: false`.

Touching faces of different products would flicker against each other; the
renderer redraws the triangles that share a plane with another product in a
fixed order so one always wins. Finding those triangles runs in a worker
(`contested-worker.js`, referenced with `new URL(..., import.meta.url)` so
bundlers pick it up) after the model is on screen; until it answers, whole
products near each other are redrawn instead.

While the camera moves, batches are drawn in clusters of nearby products and
a cluster outside the view is left out; once the camera is inside the model,
clusters entirely behind its largest opaque faces are left out too, tested
against a small depth grid on the page so nothing is ever a frame late. The
frame at rest always draws everything, so a still image never depends on
the cull. `displayInfo().occlusionState` reports what the last moving frame
left out.

A pack converted with the kernel's `lodLevels` setting carries a coarse
level of each large mesh: a second index array over the same vertices, so
the level costs index bytes only. Moving frames draw the level in place of
the mesh and the frame at rest draws the full mesh, so a still image is
never simplified; wireframe, picking, measuring and the coincident-surface
overlay only ever see the full meshes. Levels that arrive after a load
(the reference viewer computes them in its worker once the model is on
screen) go to the GPU through `renderer.applyLodLevels(pack, ids)` without
uploading the vertices again. `motionLod: false` or `setMotionLod(false)`
draws the full meshes on every frame; `lodState()` says what the pack and
the GPU hold.

Textures are off by default: a pack then carries no materials and every
product draws in its style colour. With `textures: true` the kernel reads
the file's surface styles and texture maps, image textures given as a path
load only from the page's own origin or `textureBaseUrl` unless
`allowRemoteTextures` is set, embedded and pixel textures decode from the
pack, and a product shows white where its texture has not arrived yet. The
x-ray and wireframe styles keep the flat colours.

## Editing and following a session

```js
const session = viewer.session();
const { delta } = session.runScript(`ifc.addWall({ from: [0, 0], to: [6, 0], height: 3, thickness: 0.3 })`);
viewer.applyDelta(delta);                    // only the wall is tessellated and uploaded
viewer.on("revision", ({ revision, affectedProducts }) => console.log(revision, affectedProducts.length));

viewer.follow("");                           // a tessifc-mcp or Python session serving this page
```

`@tessifc/viewer/renderer` exports the `IfcRenderer` class for hosts that want
to drive the renderer directly, `@tessifc/viewer/stream` the pack assembler
that applies incremental deltas from `@tessifc/edit`,
`@tessifc/viewer/session-client` the client of a local session host on its
own, `@tessifc/viewer/kernel-client` the promise API over the kernel worker,
and `@tessifc/viewer/depth-planes` the plane analysis on its own.

## Running the kernel in a worker

```js
const viewer = createViewer(document.getElementById("host"), { worker: true, theme: "dark" });
await viewer.open(file);                     // parsed and tessellated off the page
const session = viewer.session();            // the same calls, each returning a promise
const { report, delta } = await session.runScript(`ifc.addWall({ from: [0, 0], to: [6, 0], height: 3 })`);
if (delta) viewer.applyDelta(delta);
```

With `worker: true` the page loads no kernel: the package's own worker
(`kernel-worker.js`) parses, tessellates and runs the editing session, chunks
arrive on the page as transferred buffers, and the page only assembles and
draws them, so no task on it outlasts a chunk. The worker finds the kernel
and `@tessifc/edit` next to this package, in a checkout (`bindings/wasm`,
`bindings/edit`) or an installed tree (`@tessifc/core`, `@tessifc/edit`);
`worker: { wasmUrl }` names another kernel module and `editUrl` another
source directory. A bundler that cannot follow those URLs gets its own entry:

```js
// tessifc.worker.js, a module worker the bundler builds
import * as glue from "@tessifc/core/web";
import { createEditingSession } from "@tessifc/edit/session";
import { createScriptEngine, runScript } from "@tessifc/edit/script-engine";
import { lengthUnitOf, storeysOf } from "@tessifc/edit/describe";
import { startKernelWorker } from "@tessifc/viewer/kernel-worker-core";
startKernelWorker({ glue, edit: { createEditingSession, createScriptEngine, runScript, storeysOf, lengthUnitOf } });
```

and passes it as `worker: { url: new URL("./tessifc.worker.js", import.meta.url) }`.

A script that runs past `scriptTimeoutMs` (30 seconds by default, 0 for no
limit) is stopped by ending the worker: `runScript` resolves with
`report.timedOut`, the model is reopened in a fresh worker from its last
committed revision, the undo history is cleared and a `reopen` event names
the model id in view. The scene on the page is untouched throughout.
`close()` drops the model but keeps the worker for the next open;
`dispose()` ends it. `worker()` reports whether the worker runs, the kernel
version and the limit. The reference viewer's own worker is the same
implementation.

## Example

`examples/embed-viewer/index.html` is a complete page that uses the package
through an import map; serve the checkout and open
`/examples/embed-viewer/`. Its "Follow the local session" button (or
`?session=file`) shows a `tessifc-mcp` or Python session at work.
`bindings/viewer/test/embed.test.mjs` drives the same page in a browser.
