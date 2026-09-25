<!-- SPDX-License-Identifier: Apache-2.0 -->
# @tessifc/core

**v0.3 developer preview.** The TessIFC IFC geometry kernel as WebAssembly,
with browser and Node builds and generated TypeScript declarations.
IFC-SPF in, render-ready meshes out. Apache-2.0.

After publication:

```sh
npm install @tessifc/core@preview
```

## Browser

```js
import init, { Kernel } from "@tessifc/core/web";

await init();
const kernel = new Kernel();
const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));
try {
  const info = JSON.parse(kernel.getModelInfo(id));
  const summary = JSON.parse(kernel.evaluateGeometry(id, "{}"));
  console.log(info, summary, JSON.parse(kernel.getProductOutcomes(id)));
  console.log(JSON.parse(kernel.getDiagnostics(id))); // parse diagnostics
  const pack = kernel.takePack(id); // owned Uint8Array, IGP v0
  // Save or consume the pack after checking the result.
} finally {
  kernel.closeModel(id);
  kernel.free();
}
```

## Node

Node loads the CommonJS build synchronously, without `init()`:

```js
const { Kernel, version } = require("@tessifc/core/node");
```

The root import selects the Node build under Node's export condition and
the browser build otherwise. Use the explicit subpaths when a bundler also
builds server code. Target-specific declarations match each runtime.

## Lifecycle and geometry

For TypeScript, the generated declarations use `Symbol.dispose`. Include
`ESNext.Disposable` in `compilerOptions.lib` alongside your normal libraries
(for example `ES2022` and `DOM`). The preview consumer checks use TypeScript 5.9.

Keep a model open while querying its attributes or editing. `closeModel`
releases a model; `free()` releases the kernel instance. `takePack` returns
an owned IGP buffer and releases the evaluation, while `getPack` retains it.
Shape-array getters return copies. `releaseGeometry` retains the parsed model.

For progressive display, use `beginGeometryStream`, `nextGeometryChunk` and
`streamProgress`. Chunks are complete IGP containers and share geometry IDs
across the stream. Time and triangle chunk limits are checked between
products; a single complex product can exceed a chunk's requested budget.
Use a worker and terminate it for cancellation or a host-enforced deadline.

Recoverable parse and geometry problems are diagnostics. Invalid settings
can throw, and host resource failures can trap. Inspect product outcomes,
warnings and errors before accepting output. Reading IFC2X3, IFC4 and IFC4X3
schemas does not imply support for every representation. `openModel` takes
plain IFC or an IFCZIP archive; whole-model triangle, vertex and time
budgets are opt-in settings that stop a run between products and say so.
The `lodLevels` setting adds coarse levels of large meshes to the pack, and
the module-level `simplifyMesh` computes one for a mesh a host already holds.

The [SDK guide](https://github.com/nbharathik/tessifc/blob/main/docs/sdk.md)
documents settings, streaming, editing, memory and workers.
See [coverage](https://github.com/nbharathik/tessifc/blob/main/docs/coverage.md)
and the [preview contract](https://github.com/nbharathik/tessifc/blob/main/docs/preview.md).

## Build from a checkout

From the repository root:

```sh
rustup target add wasm32-unknown-unknown
python scripts/build-wasm.py --target both
node bindings/wasm/test/smoke.mjs
```

The script checks the installed tools and reports the matching
`wasm-bindgen-cli` installation command. Cargo may fetch locked dependencies;
the script does not install tools. Optional Binaryen optimisation uses an
already installed `wasm-opt`. Serve `.wasm` as `application/wasm`; allow local
modules, workers and `'wasm-unsafe-eval'` in your Content Security Policy.
Cross-origin isolation and SharedArrayBuffer are not required.

A build with one schema is smaller. Every schema feature must be dropped
and the wanted one named, and the kernel then refuses files declaring another
schema:

```sh
python scripts/build-wasm.py --target both --no-default-features --features tessifc-wasm/schema-ifc4
```

The `edit` feature (on by default) carries attribute edits, revisions and
patches: `setAttribute`, `exportModel`, `getModelRevision`,
`prepareRevision`, `prepareAttributeEdits`, `evaluatePreparedRevision`,
`commitRevision`, `discardRevision` and `evaluateProducts`. A viewer that
only reads and draws can leave it out, together with `ifczip`; the methods
are then absent and `createEditingSession` from `@tessifc/edit` says so:

```sh
python scripts/build-wasm.py --target both --no-default-features --features schema-ifc2x3,schema-ifc4,schema-ifc4x3
```

`--sections` prints what the module's bytes are spent on, `--keep-names`
keeps symbol names for a profiler such as twiggy, and `--budget-bytes` and
`--budget-raw-bytes` fail the build over a gzipped or raw size. Shipped
modules carry no name or producers section.
