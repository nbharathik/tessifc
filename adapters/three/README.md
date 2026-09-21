<!-- SPDX-License-Identifier: Apache-2.0 -->
# @tessifc/three

**v0.2 developer preview.** Build three.js meshes from an evaluated TessIFC
model. Pass your application's `THREE` namespace; the adapter does not bundle
a second copy. three.js is a peer dependency.

After publication, install `@tessifc/core@preview`, `@tessifc/three@preview`
and `three`. In a checkout, import `src/index.js` directly.

```js
import init, { Kernel } from "@tessifc/core/web";
import * as THREE from "three";
import { loadModel, frameCamera, expressIdAt, disposeModel } from "@tessifc/three";

await init();
const kernel = new Kernel();
const modelId = kernel.openModel(new Uint8Array(await file.arrayBuffer()));
const { group, bounds, summary, outcomes } = loadModel(THREE, kernel, modelId);
console.log(summary, outcomes); // inspect warnings before accepting geometry
scene.add(group);
frameCamera(camera, bounds, controls);

// While the model is still open:
const hit = raycaster.intersectObjects(group.children, false)[0];
const elementId = expressIdAt(hit);
if (elementId !== null) console.log(kernel.getClassName(modelId, elementId));

// When closing this model:
disposeModel(group);
kernel.closeModel(modelId);
kernel.free();
```

## API and ownership

* `loadModel(THREE, kernel, modelId, options?)` evaluates and returns
  `{ group, bounds, shapes, summary, outcomes, batches }`. Pass `settings` for geometry
  options, or `evaluate: false` to reuse an evaluation (summary is then null).
* `frameCamera(camera, bounds, controls?)` fits a perspective camera against
  both viewport dimensions and updates optional orbit controls.
* `expressIdAt(intersection)` recovers the IFC element ID, or returns null.
* `disposeModel(group)` disposes the group's geometries and materials,
  detaches it and clears its children. It does not dispose textures or
  other resources added by your application.

The group owns copies of its arrays. You can close the kernel model after
loading if no attribute queries or edits remain. Use `try/finally` in an
application so exceptions also release kernel and GPU resources.

## Batching

Opaque parts with the same colour share a draw batch. Transparent parts
remain separate so three.js can sort them by camera depth. Oversized parts
split at triangle boundaries, with at most 260,000 vertices per batch.
An `expressId` attribute preserves selection through batching.

`@tessifc/three/build` exports `buildBatches` and `frame` without requiring
three.js. Batches are typed arrays, with compact 16-bit indices where possible.
The adapter copies shape positions with product transforms already applied,
in metres relative to `summary.modelOffset`. Add that offset in double
precision for source coordinates. The reference viewer's IGP path also
supports shared instancing and progressive streaming.

## A scene that follows edits

`createRetainedModel(THREE, pack)` builds a scene from a parsed IGP pack, one
mesh per placed instance with a `BufferGeometry` shared per IGP geometry, and
`applyDelta(delta)` replaces only the products a revision touched. The pack
and the deltas come from `@tessifc/edit`:

```js
import { createEditingSession, readIgp } from "@tessifc/edit";
import { createRetainedModel } from "@tessifc/three";

const session = createEditingSession(kernel, modelId, { settings: { includeOpenings: true } });
const model = createRetainedModel(THREE, session.evaluate().pack);
scene.add(model.group);

const { delta } = session.runScript('ifc.byType("IfcWall")[0].Name = "Renamed";');
if (delta) model.applyDelta(delta);   // affected and removed products swap, nothing else moves
model.setVisible(expressId, false);
model.dispose();
```

Meshes carry `userData.expressId`, `class` and `flags`; helper geometry
(openings, spaces, references) starts invisible. This scene favours
correctness over draw-call count; `loadModel` stays the merged static path.

A pack evaluated with the `textures` setting carries materials, textures and
texture coordinates. `createRetainedModel(THREE, pack, { textures: true })`
gives each instance with a material row a `MeshStandardMaterial` (diffuse
colour, roughness from the style's roughness or shininess, metal and mirror
reflectance as metal) with the texture as its colour map, and puts the pack's
`uv` on the geometry. Pixel textures become `DataTexture`s, embedded images
decode through `createImageBitmap`, and image paths load through
`TextureLoader` only from the page's own origin or `textureBaseUrl` unless
`allowRemoteTextures` is set; `onTexture(id)` fires when an image arrives so
a host that renders on demand can draw again. `materialParameters(row,
texture)` is the pure mapping for a host that builds its own materials.
Without the option every mesh keeps its flat `MeshLambertMaterial`.

Intersecting transparent surfaces still have normal object-sorting limits.
The adapter does not repair unsupported IFC geometry. See the
[coverage guide](https://github.com/nbharathik/tessifc/blob/main/docs/coverage.md).

## Test from a checkout

```sh
node adapters/three/test/build.test.mjs
node adapters/three/test/retained.test.mjs
```

Tests use first-party synthetic inputs. Build the Node WASM package to also
exercise the actual kernel. `TESSIFC_TEST_MODEL` can supply an additional model.
