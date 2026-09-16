<!-- SPDX-License-Identifier: Apache-2.0 -->
# Getting started

Start from the checkout for the v0.2 developer preview. Rust is pinned in
`rust-toolchain.toml`; the helper scripts use Python 3.11 or newer. Node 20
or newer is needed for Node integrations and JavaScript tests.

## Build and open the viewer

```sh
rustup target add wasm32-unknown-unknown
python scripts/build-wasm.py --target both
python -m http.server 8000 --bind 127.0.0.1
```

If `wasm-bindgen-cli` is missing or mismatched, the build script prints the
exact installation command. It uses the version in `Cargo.lock`.

Open <http://127.0.0.1:8000/viewer/> and choose an `.ifc` file. Scroll over a
detail to zoom; **F** frames the model and **Shift F** frames the selection.
The [viewer guide](../viewer/README.md) covers all tools and shortcuts.

## Browser API

From a page served at the checkout root, use the generated browser module:

```js
import init, { Kernel } from "./bindings/wasm/pkg/tessifc_wasm.js";

await init();
const kernel = new Kernel();
const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));
try {
  const info = JSON.parse(kernel.getModelInfo(id));
  const summary = JSON.parse(kernel.evaluateGeometry(id, "{}"));
  console.log(info.schema, summary, JSON.parse(kernel.getProductOutcomes(id)));
  console.log(JSON.parse(kernel.getDiagnostics(id))); // parse diagnostics
  const pack = kernel.takePack(id); // owned Uint8Array containing IGP
  // Save or consume pack here. Check diagnostics before accepting the result.
} finally {
  kernel.closeModel(id);
  kernel.free();
}
```

After npm publication, `npm install @tessifc/core@preview` provides the same
browser API through `@tessifc/core/web`. The explicit `/web` path also avoids
Node's export condition when building a server-rendered application.
Run parsing and evaluation in a worker to keep the page responsive. See the
[SDK worker example](sdk.md#running-in-a-worker).

## three.js

Build the browser package first. In a local integration, import the adapter
from `./adapters/three/src/index.js`; after publication install
`@tessifc/three@preview` and `three` and use the package import below.

```js
import { loadModel, frameCamera, disposeModel } from "@tessifc/three";

// THREE, scene, camera, controls and file are owned by your application.
const kernel = new Kernel();
const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));
let loaded;
try {
  loaded = loadModel(THREE, kernel, id);
  console.log(loaded.summary, loaded.outcomes); // decide whether to accept the geometry
  scene.add(loaded.group);
  frameCamera(camera, loaded.bounds, controls);
} finally {
  kernel.closeModel(id);
  kernel.free();
}
// When removing or replacing the model:
disposeModel(loaded.group);
```

The adapter owns copies of the mesh arrays, so closing the kernel model
after loading is safe. Keep it open if your app still needs IFC attributes
or editing. The [adapter guide](../adapters/three/README.md) explains picking
and GPU cleanup.

## Node

The generated CommonJS module loads WASM synchronously; it does not need `init()`.
Run from the checkout root:

```js
const { Kernel } = require("./bindings/wasm/pkg-node/tessifc_wasm.js");
const fs = require("node:fs");

const kernel = new Kernel();
const id = kernel.openModel(fs.readFileSync("model.ifc"));
try {
  const summary = JSON.parse(kernel.evaluateGeometry(id, "{}"));
  console.log(summary, JSON.parse(kernel.getProductOutcomes(id)));
  console.log(JSON.parse(kernel.getDiagnostics(id))); // parse diagnostics
  fs.writeFileSync("model.igp", kernel.takePack(id));
} finally {
  kernel.closeModel(id);
  kernel.free();
}
```

After publication, use `require("@tessifc/core/node")` or
`require("@tessifc/core")`. Use a worker thread or child process for workloads
that must not block a Node server.

To edit rather than only read, wrap the open model in a session from
`bindings/edit` (`@tessifc/edit`): scripts, attribute edits, undo and a
model built from nothing all come back as deltas that name the affected
products. To let an agent do the editing while the viewer follows, start
`node bindings/mcp/src/cli.js --new house.ifc` (after `npm ci --prefix
bindings/mcp`) and register it with your MCP client; see
[Agents and pipelines](agents.md).

## Command line

```sh
cargo build --locked --release -p tessifc-cli
```

Run `target/release/tessifc` (`tessifc.exe` on Windows), or use
`cargo run --locked --release -p tessifc-cli --` before each command:

```sh
tessifc info model.ifc --json
tessifc convert model.ifc -o model.igp
tessifc convert model.ifc -o checked.igp --strict
tessifc convert model.ifc -o coarse.igp --jobs 1 --no-spaces --circle-segments 16
tessifc edit model.ifc --id 42 --attribute Name --value "Wall A" -o edited.ifc
tessifc coverage --markdown
```

Inspect diagnostics even when conversion succeeds. Strict conversion refuses
unacceptable geometry before writing the destination. Parallel evaluation
preserves product order; runtime timing metadata is not deterministic.

## Rust

Use local path dependencies on the crates you need; registry versions apply
after publication. For a sibling checkout:

```toml
[dependencies]
tessifc-step = { path = "../tessifc/crates/ifc-step" }
tessifc-model = { path = "../tessifc/crates/ifc-model" }
tessifc-engine = { path = "../tessifc/crates/ifc-engine" }
```

```rust
use tessifc_engine::Engine;
use tessifc_model::Model;
use tessifc_step::{ParseOptions, parse};

let bytes = std::fs::read("model.ifc")?;
let model = Model::new(parse(&bytes, &ParseOptions::default()));
let result = Engine::new().evaluate(&model);
println!("{} shapes, {} triangles", result.shapes.len(), result.triangles());
for diagnostic in &result.diagnostics {
    println!("{diagnostic}");
}
# Ok::<(), std::io::Error>(())
```

Enable the `parallel` feature of `tessifc-engine` for its parallel API.

## Static website

```sh
python -m pip install -r requirements-site.txt
python scripts/build-site.py
python -m http.server 8000 --bind 127.0.0.1 --directory dist/site
```

This packages the landing page, docs, viewer and previously built browser WASM.
Pass `--base /name/` for a subdirectory deployment. `--out` must name a new or
empty directory, or one marked as generated by this builder. A failed build
keeps the previous site. Serve the generated directory when hosting publicly.

The website uses MkDocs Material for navigation, search, light and dark themes,
and syntax highlighting. Edit the guides in `docs/`, configure navigation and
Markdown extensions in `mkdocs.yml`, and keep website styles and template
overrides in `docs/site/`. Rebuild with `scripts/build-site.py` so the viewer,
WASM package and redirects are included. Run the website regression checks with
`python -m unittest discover -s scripts -p 'test_build_site.py'`.

Continue with the [SDK](sdk.md), [coverage](coverage.md),
[preview contract](preview.md), [IGP format](igp-format.md), [editing](editing.md)
and [agents and pipelines](agents.md).
