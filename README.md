<!-- SPDX-License-Identifier: Apache-2.0 -->

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
    <img src="docs/assets/banner-light.svg" alt="TessIFC" width="480">
  </picture>
</p>

<p align="center">
  <a href="https://nbharathik.github.io/tessifc/">website</a>
  |
  <a href="https://nbharathik.github.io/tessifc/docs/">documentation</a>
  |
  <a href="https://nbharathik.github.io/tessifc/viewer/">demo viewer</a>
  |
  <a href="https://github.com/nbharathik/tessifc/releases">releases</a>
</p>

# TessIFC

**tessifc** is an IFC geometry kernel: IFC files in, render-ready triangle
meshes out. One Rust codebase runs in the browser through WebAssembly, in Node
and as a command line tool, and it ships with a viewer that uses it.

This is the **v0.1 developer preview**.

![The TessIFC viewer inspecting a pavilion model](docs/assets/viewer.png)

## Install

Preview packages are not on npm yet, so build the browser package from this
checkout:

```sh
rustup target add wasm32-unknown-unknown
python scripts/build-wasm.py --target both
```

That writes the browser module to `bindings/wasm/pkg` and the Node module to
`bindings/wasm/pkg-node`. Once the preview is published, `npm install
@tessifc/core` gives you the same API.

## Quick setup

```js
import init, { Kernel } from "./bindings/wasm/pkg/tessifc_wasm.js";

// initialize the wasm module
await init();

// create a kernel
const kernel = new Kernel();

// open a model from data
const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));

// the model is now loaded, use id to tessellate it and read what happened
const summary = JSON.parse(kernel.evaluateGeometry(id, "{}"));
const outcomes = JSON.parse(kernel.getProductOutcomes(id));
const pack = kernel.takePack(id); // meshes in the IGP container

// close the model, all memory is freed
kernel.closeModel(id);
kernel.free();
```

See [getting started](docs/getting-started.md) for the Node and Rust entry
points and the [`@tessifc/three`](adapters/three/README.md) adapter.

## Run the viewer

Serve the checkout and open the viewer in a browser:

```sh
python -m http.server 8000 --bind 127.0.0.1
```

Open <http://127.0.0.1:8000/viewer/> and choose an `.ifc` file. Files stay in
the browser, nothing is uploaded. Scroll over a detail to zoom toward it,
double-click to frame an element, and use the ribbon to inspect, section,
measure, hide and isolate. See the [viewer guide](viewer/README.md) for the
full tool list and shortcuts.

## Command line

```sh
cargo run --locked --release -p tessifc-cli -- info model.ifc
cargo run --locked --release -p tessifc-cli -- convert model.ifc -o model.igp
```

Add `--strict` when a pipeline must reject geometry that is missing, degraded
or repaired.

## What it does

* Reads IFC-SPF with IFC2X3, IFC4 and IFC4X3 schema tables.
* Tessellates extrusions, sweeps, tessellated sets, BReps and boolean
  operations.
* Keeps element IDs, colours, placements and reused geometry in the IGP mesh
  container.
* Streams geometry and reports per product what was unsupported, repaired or
  degraded.
* Reads attributes and writes source-preserving edits.

Schema recognition is broader than geometry support, and complex trims,
booleans and malformed topology still have limits. Read the conversion report
before using a mesh downstream. See [geometry coverage](docs/coverage.md) and
the [preview contract](docs/preview.md).

## Requirements

These are needed only to build from source.

1. Rust, the version pinned in `rust-toolchain.toml`, installed through rustup
2. The `wasm32-unknown-unknown` target, for the browser and Node packages
3. Python 3.11 or later, for the build scripts
4. Node 20 or later, for the Node package and the JavaScript tests

`scripts/build-wasm.py` checks `wasm-bindgen-cli` against the lockfile and
prints the exact command to install the matching version. A current
`wasm-opt` on the path is optional; it runs an extra optimisation pass.

## Testing

```sh
cargo test --workspace
npm --prefix bindings/wasm test
node viewer/test/igp.test.mjs
```

`igp.test.mjs` needs only Node. The viewer's pixel tests need a headless
browser:

```sh
npm ci --prefix viewer
cd viewer && npx playwright install chromium && npm test
```

## Documentation

| Guide | Contents |
|---|---|
| [Getting started](docs/getting-started.md) | Build, load and convert |
| [SDK](docs/sdk.md) | API, settings, workers and memory ownership |
| [Preview contract](docs/preview.md) | Scope, limitations and validation |
| [Geometry coverage](docs/coverage.md) | Supported representations and conditions |
| [Architecture](docs/architecture.md) | Pipeline and extension points |
| [IGP format](docs/igp-format.md) | Mesh container and readers |
| [Editing](docs/editing.md) | Source-preserving attribute changes |

## Licence

Apache-2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE). TessIFC is an
independent implementation, not endorsed or certified by buildingSMART.
