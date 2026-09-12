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
here; the reference viewer runs it in a worker, and `loadPack` accepts an IGP
pack produced anywhere, including by a worker of your own or the `tessifc`
CLI.

## API

| Call | What it does |
|---|---|
| `createViewer(container, options)` | Creates the canvas inside `container`. Options: `kernel`, `hiddenFlags`, `lodPixels`, `theme`, `background`, `selectOnClick`, `focusOnDoubleClick`, `coincidence` (`false` skips the worker that refines the coincident-surface overlay). |
| `open(source, { settings, modelSettings })` | Parses the IFC with the kernel and streams its geometry. Resolves with `modelId`, `info`, `summary` and `hierarchy`. |
| `loadPack(bytes)` | Shows an IGP pack directly, without a kernel. |
| `close()` | Drops the model from the view and the kernel. |
| `select(ids)`, `selection()` | Selects express ids (`null` clears); every part of a product is selected together. |
| `hide(ids)`, `show(ids)`, `isolate(ids)`, `showAll()` | Visibility by express id; `isolate(null)` ends an isolation. |
| `setHiddenFlags(flags)` | Which helper categories (spaces, openings, references) stay hidden by default. |
| `fit()`, `focus(ids?)`, `setView(mode)`, `setStyle(style)` | Camera and display: views `perspective`, `top`, `front`, `right`; styles `shaded`, `xray`, `wire`. |
| `setSection({ axis, value or fraction, flipped, cap })`, `setSection(null)` | A cut along one axis, in IFC coordinates or as a fraction of the bounds. |
| `pick(clientX, clientY)` | The product and surface point under a pointer. |
| `setPivot(point)`, `zoom(factor)` | The orbit and zoom centre, and a programmatic zoom step. |
| `on(event, listener)` | `load`, `progress`, `select`, `visibility`, `camera`, `overlay`, `close`; returns the unsubscribe function. |
| `pack()`, `hierarchy()`, `modelId()`, `overlayState()`, `renderer` | The assembled pack, the kernel's spatial tree, the model id, whether the coincident-surface overlay has been refined (`pending`, `ready`, `failed`, `off`) and the renderer itself for anything not covered above. |
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

`@tessifc/viewer/renderer` exports the `IfcRenderer` class for hosts that want
to drive the renderer directly, `@tessifc/viewer/stream` the pack assembler
that applies incremental deltas from `@tessifc/edit`, and
`@tessifc/viewer/depth-planes` the plane analysis on its own.

## Example

`examples/embed-viewer/index.html` is a complete page that uses the package
through an import map; serve the checkout and open
`/examples/embed-viewer/`. `bindings/viewer/test/embed.test.mjs` drives the
same page in a browser.
