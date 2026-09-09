<!-- SPDX-License-Identifier: Apache-2.0 -->
# TessIFC viewer

A v0.1 developer preview viewer for IFC-SPF files. Drop an `.ifc` file into the page to
review the model, cut live sections, measure between corners, edges and
surfaces, inspect and edit element attributes, and export the edited source.

The file never leaves the browser. A Web Worker runs the TessIFC WASM kernel
and streams the model to the page as IGP chunks. The building fills in while the
kernel keeps tessellating, behind a progress card that counts products. When
the stream ends the scene is rebuilt once from the assembled pack, so the
final state is exactly what a whole load produces. The source and the compact
model stay in the worker so edits and export remain possible, and the
renderer keeps typed-array views into the chunks rather than a second CPU
copy.

Repeated family geometry can be shared and placed by transform. Editing an element
re-evaluates only that element; if its triangles did not change, nothing is
rebuilt.

The renderer is first-party WebGL2. There is no npm runtime package, no CDN
script and no font service, and the page's Content Security Policy forbids
remote code, so it keeps working offline once the repository and the WASM
artefacts are present.

## Run it

```sh
python scripts/build-wasm.py --target web
python -m http.server 8000 --bind 127.0.0.1
```

Open <http://127.0.0.1:8000/viewer/>.

The viewer needs a parent-directory scope to reach `bindings/wasm/pkg/`, so
the server exposes the whole repository. Keep it bound to localhost.

## Layout

```
viewer/index.html        the page
viewer/src/main.js       worker lifecycle, model load, selection, visibility
viewer/src/shell.js      ribbon, panels, dialogs, toasts, commands, shortcuts
viewer/src/tree.js       the spatial and type trees
viewer/src/inspector.js  properties, statistics, diagnostics, the editor
viewer/src/tools.js      standard views, display style, measure, section
viewer/src/gizmo.js      the orientation gizmo in the corner of the viewport
viewer/src/measure.js    snapping and measurement maths, pure and unit-tested
viewer/src/navigation.js cursor anchors, wheel scaling and camera framing
viewer/src/culling.js    conservative visibility tests for render batches
viewer/src/scheduler.js  frame coalescing and responsive input tasks
viewer/src/renderer.js   the WebGL2 renderer
viewer/src/worker.js     the geometry worker
viewer/src/igp.js        the IGP reader
viewer/src/stream.js     assembles streamed chunks into one pack
viewer/src/styles.css    the design system
```

Every action is a registered command, so a ribbon button, the dock, the
command palette and the keyboard all reach the same code.

The Home ribbon separates model helpers from the physical building. Spaces,
openings and guides each have their own show or hide toggle. They start hidden
but remain in the pack for inspection. Hidden helpers are also excluded from
camera fitting, picking and the overlapping-material plan, so room volumes and
void tools cannot interfere with orbiting the building.

The structure panel and the inspector fold away from the chevron in their
own header. The folded structure panel leaves a thin strip whose icon
brings it back; the inspector comes back from any tab on its rail, and the
open tab folds it again.

On narrow screens, the panels start closed and open one at a time. Zoom
controls stay reachable beside the viewport. The UI follows browser text
zoom; render scale in Settings controls canvas sharpness separately.

## Navigation

Scroll over a surface to zoom toward it. Trackpad deltas keep their magnitude,
and the surface stays under the pointer during a wheel gesture. Double-click
to frame an element or use the **+ / −** buttons and shortcuts. **F** restores
the whole model; **Shift F** frames the current selection. Framing accounts
for portrait viewports and millimetre-scale parts.

Left drag orbits. Right drag or Shift-drag pans in screen pixels. On a touch
screen, one finger orbits; two fingers pan and pinch to zoom. A model still
converting preserves a camera you have moved.

## The structure panel

Two trees over the same model. **Spatial** follows the file's own containment,
project to site to building to storey, and **Types** groups every class under a
readable role. Both descend the same way: a container holds one group per IFC
class, and a class group holds its elements by name with their express ids.

Clicking an element selects it and fills the inspector; selecting in the
viewport opens the branches above that element and scrolls its row into view,
so a click in the model always answers where the element lives. Double-click
frames whatever the row covers, from a single door to a whole storey.

Every row carries an eye. Hiding a container hides everything under it, and a
container whose contents are only partly visible shows a dimmed eye. Elements
appear a page at a time, so one class with tens of thousands of members costs
nothing until it is opened, and then adds rows in bounded steps. Expanding
every branch fills the lists in over a few frames rather than in one step, and
a list that has scrolled out of view is neither laid out nor painted, nor are
its rows rewritten when visibility changes, until it comes back into view.

The search box filters both trees on names, classes and `#123` express ids,
opening whatever it has to in order to show a match. Members on pages that
are not built yet are searched too, and the pages up to a match are added.

| Key | Action |
| --- | --- |
| Up, Down | Move between rows |
| Right, Left | Open or close a branch, or step to its parent |
| Home, End | First or last visible row |
| Enter | Select the element |
| Space | Show or hide |
| `F` | Frame the row |

## Theme

One setting drives the panels and the viewport: **System**, **Light** or
**Dark**. System follows the operating system and changes with it while the
page is open. The Display group on the Home ribbon cycles the three, and
Settings has the same choice.

The viewport can opt out. Appearance on the View ribbon, and Settings, offer a
viewport background that matches the theme or is pinned light or dark, for
anyone who wants a dark model under a light interface.

## Orientation

A gizmo sits under the view badge in the top right corner. Its three solid
handles are the positive X, Y and Z axes and the three hollow ones are the
negatives, drawn in painter's order so the near handles cover the far ones.
Click one and the camera looks along that axis, keeping the current zoom.
The gizmo is keyboard reachable: tab to a handle and press Enter.

## Measuring

Press `M` or the ruler in the dock. The pointer shows what it will take: a
square on a corner, a diamond on an edge, a circle on a surface. The snap
radius is twelve CSS pixels, wide enough for a trackpad and narrow enough not
to grab the wrong corner. Click twice and the measurement stays on the
viewport with its number; the card lists the straight length and the axis
deltas of each one, and the rubber band shows the length before the second
click lands.

| Key or control | Action |
|---|---|
| Click | Take a point |
| `Backspace` or `Delete` | Remove the point in progress, or the last measurement |
| `Esc` | Drop the point in progress, then leave the tool |
| Copy | Every measurement as tab-separated text, in IFC coordinates |
| Clear | Remove every measurement |

Measurements are in metres, shown in millimetres below a centimetre. They
are drawn on a 2D canvas above the WebGL one and cost the GPU nothing.
Leaving the tool keeps them on screen; Clear removes them, as does opening
another file.

## Selecting and editing

Click an element and the Properties panel shows its name, class and IFC
attributes. The Element tab beside it shows where the element sits in the
spatial structure, its centre, size and extent, and what its geometry
costs to draw. A small dock under the viewport tools
offers Focus, Isolate and Hide for the selection, and its pencil opens the
edit panel, which slides in beside the properties only when you ask for it.
Text attributes are patched in place; every other byte of the file is left
as it was.

## Shortcuts

| Key | Action |
| --- | --- |
| `1` `2` `3` `4` | Front, right, top, isometric |
| `F` / `Shift F` | Frame the model / frame the selection |
| `+` / `-` | Zoom in / out |
| Double-click | Frame an element under the pointer |
| `P` | Sectioned plan view, again to return to 3D |
| `D` | Cycle shaded, x-ray and wireframe |
| `M` | Measure |
| `Backspace` | Remove the last measurement, while measuring |
| `X` | Section plane |
| `I` `H` `A` | Isolate, hide, show everything |
| `E` | Open or close the edit panel |
| `Esc` | Clear the selection, or drop the active tool |
| `Ctrl K` | Command palette |
| `Ctrl O` / `Ctrl S` | Open a file / export the IFC |
| `Ctrl F` | Search the structure |
| `Ctrl B` / `\` | Toggle the structure panel / the inspector |
| `Ctrl F1` | Collapse the ribbon |
| `Ctrl ,` / `?` | Settings / shortcuts |

Supported schemas are IFC2X3, IFC4 and IFC4X3. Geometry coverage is the
kernel's, in [`docs/coverage.md`](../docs/coverage.md). An unsupported element
is diagnosed. Read the [preview contract](../docs/preview.md) before accepting
geometry for downstream use.

## Rendering decisions

The renderer keeps the exported IGP geometry unchanged and prepares a
render-only index view. It drops exact duplicate triangles and, for opaque
geometry only, one side of a fully covered opposite-wound patch on the same
represented axis plane. Partial overlaps, nearby planes, sloped patches,
transparent layers and faces owned by different products are preserved. Both
filters are skipped above a triangle budget, since they are quality passes.

GPU transforms are rebased around the scene centre, clipping planes stay
tight to the model, equal-depth fragments use strict depth testing, and
two-sided shading follows the visible geometric plane rather than IFC
winding.

### Coincident surfaces

Two products that share a face, a slab edge flush with a wall, a finish on a
floor, a column face in a facade panel, would otherwise fight for the pixel
and flicker while the camera moves. The viewer settles them in three passes:

1. An unbiased opaque pass writes canonical depth.
2. Materials whose bounding boxes overlap another opaque material of a
   different colour are redrawn in one deterministic colour order, with
   depth writes off and a polygon offset clamped to a 0.75 mm world-space
   envelope, so the same material wins at every angle and a real gap of a
   millimetre or more is never overridden.
3. Translucent products blend back to front.

This works on both depth conventions the browser can grant. With
`EXT_clip_control` and `EXT_polygon_offset_clamp` the scene is drawn into a
reversed 32-bit float target; otherwise into a forward 24-bit target, where
the clamp is floored at two depth units because the buffer cannot express
less. A browser with no polygon offset clamp at all gets a bounded
whole-unit step. The overlay is planned again while a model streams in, a few times a second,
and once more when the stream finishes, so the first seconds of a large model
are as steady as the last. A model
with so many overlapping products that the pair analysis exceeds its budget
has every opaque material contested, which costs one extra draw per batch
rather than the stability. Clamp values are quantised to a few steps per
octave so a frame cannot ask the driver for more rasterizer states than it
caches.

CPU picking applies the same winner inside a render-space tolerance capped
at 0.750 mm, so a real gap stays nearer while a coincident click agrees with
the visible material. The overlay switches off entirely when the model's
render radius exceeds what f32 can hold within that envelope. The Model
panel reports which path is in use.

What remains is sampling noise on thin geometry at silhouette edges, which
is antialiasing, not depth.

### Budgets

`{depth: true}` guarantees only 16 bits, so the viewer uses an offscreen
target where the browser permits, down a 4x, 2x, 1x sample ladder within a
sample-pixel budget. The default device pixel ratio is capped at 1.5, with
separate ceilings on rendered pixels and sample pixels. Rendering runs only
after model, camera or interface state changes. Selection uses a CPU bounds
broad phase with a budget, then triangle tests, so it never stalls on a
synchronous GPU readback. Opaque singletons sharing a colour are submitted as
bounded batches while repeated geometry stays instanced once its copies
would cost more than a draw. Wire indices are prepared in idle time after a
load while they stay small, and otherwise on the first wireframe frame. The render view of each mesh, with its duplicate and covered
triangles removed, is prepared once and reused when the finished pack is
rebuilt after streaming. Hidden batches and batches outside the view are skipped before GPU
submission; hidden records in mixed batches are clipped before rasterisation.
Transparent parts retain their independent sorting order.

Complex scenes draw into a smaller offscreen target during orbit, pan and zoom,
then return to full resolution when the gesture ends. Both targets are prepared
before interaction and reused, so a gesture neither resizes the canvas nor
reallocates its GPU buffers. The cache is bounded and included in the reported
GPU memory estimate. The sampling grid stays fixed throughout each gesture.
Geometry and picking accuracy are unchanged. The orientation gizmo updates once
per rendered frame, and an empty measurement overlay does no drawing work.

Normal motion shares animation frames. Discrete actions and overdue input frames
can also render through a queued task when animation callbacks are delayed.
A click selects inside its own release handler, so the release and the
selection share one frame. The viewer schedules no recurring rendering work
while idle.

The render targets keep the largest frame they have drawn, so folding a panel
back and forth allocates nothing after the first time; a smaller frame draws
into a corner of them. A run of canvas size changes, a window being dragged,
changes the drawing buffer once the size has settled, and the browser scales
the last frame into the new box meanwhile. The gesture target is prepared after
the frame that follows a resize, not inside it. Hiding, isolating and showing
read the overlapping-material plan off the record pairs found once at load, so
a visibility change does not sweep the model again. The frame after a click or
a visibility change carries the model and the selection card; the structure
panel and the attribute list follow in the next frame.

## What a file can hide from you

Two things in a real IFC remove products from view, and neither is a
conversion fault:

* `IfcSurfaceStyleRendering.Transparency` of 1.0, which exporters apply to a
  whole window rather than to its glass.
* A black surface colour against a darker viewport. Shading is
  multiplicative, so a black surface stays black under any light.

The kernel carries the file's colour through unchanged, so the properties
panel, any measurement and the exported IFC all see exactly what the file
says. The viewer draws such products at a minimum opacity and luminance so
they can still be seen, selected and inspected, and the Quality panel says
how many were floored. Set `renderer.minimumAlpha` and
`renderer.minimumLuminance` to zero to draw the file's own values exactly.

## Tests

```sh
python scripts/build-wasm.py --target both
npm ci --prefix viewer
cd viewer
npx playwright install chromium
npm test
```

`igp.test.mjs` needs only Node. It covers the IGP reader, batching, depth
planning, the triangle filters, measurement snapping and readouts, an edit
round trip, and guards the renderer source against regressions of every
rendering rule above. It also asserts that the page has no remote
dependency.

`navigation.test.mjs` checks camera maths without a browser.
`scheduler.test.mjs` checks frame coalescing, delayed callbacks and cancellation.
`render.test.mjs` generates a first-party pavilion in memory, loads it in
headless Chromium and reads the frame
buffer back. It asserts that every product with geometry paints at least one
pixel when isolated, that the coincident-face filter changes no visible
pixel, and that coplanar multi-colour fixtures keep one winner through
sixteen orbit angles under both depth conventions. It also verifies cursor
zoom, pan scaling, controls and mobile panels. Culling is compared against
unculled pixels across display modes, sections and views; hidden and offscreen
geometry must issue no mesh draws. Gesture tests verify reduced offscreen
resolution, a fixed canvas, full-resolution restoration and target reuse.
Pointer and selection tests also run with animation callbacks held. The structure
panel is checked for a one-frame click selection that reveals its row, sliced
expansion, correct row states in lists the browser had skipped, and keyboard
movement. Required tools and WASM must
be present; missing prerequisites fail the suite. Set
`TESSIFC_BROWSER_CHANNEL=chrome` to use an installed Chrome, or
`TESSIFC_TEST_MODEL` to add a model of your own (with an opening for the
opening-toggle checks). Test state is injected by the test harness and is
not exposed by the published viewer.
