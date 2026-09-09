<!-- SPDX-License-Identifier: Apache-2.0 -->
<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
    <img src="docs/assets/banner-light.svg" alt="TessIFC" width="480">
  </picture>
</p>

<p align="center"><b>IFC files in. Render-ready meshes out.</b><br>
<b>v0.1 developer preview</b></p>

<p align="center">
  <a href="docs/getting-started.md">Get started</a> &middot;
  <a href="docs/coverage.md">Geometry coverage</a> &middot;
  <a href="https://github.com/nbharathik/tessifc/releases">Releases</a>
</p>

![The TessIFC viewer inspecting a first-party pavilion model](docs/assets/viewer.png)

## What it does

* Reads IFC-SPF with IFC2X3, IFC4 and IFC4X3 schema tables.
* Tessellates supported extrusions, sweeps, tessellated sets, BReps and boolean operations.
* Preserves element IDs, colours, placements and reusable geometry in the IGP mesh container.
* Streams geometry and reports unsupported, repaired and degraded results per product.
* Inspects attributes and exports source-preserving edits.

Schema recognition is broader than geometry support. This preview is for
integration and evaluation: check the conversion report before using a mesh
downstream. Complex trims, booleans and malformed topology still have limits.
See [coverage](docs/coverage.md) and the [preview contract](docs/preview.md).

## Run the viewer

Install Rust through rustup and Python 3.11 or newer, then run from this checkout:

```sh
rustup target add wasm32-unknown-unknown
python scripts/build-wasm.py --target web
python -m http.server 8000 --bind 127.0.0.1
```

The build script reports the exact `wasm-bindgen-cli` version to install if
it is missing. Open <http://127.0.0.1:8000/viewer/> and choose an `.ifc` file.
Files stay in the browser.

Scroll over a detail to zoom toward it, double-click to frame an element, or
use the **+ / −** controls. Inspect, section, measure, hide and isolate using
the ribbon. [Viewer guide and shortcuts](viewer/README.md).

## Use the kernel

Build and run the CLI locally:

```sh
cargo run --locked --release -p tessifc-cli -- info model.ifc
cargo run --locked --release -p tessifc-cli -- convert model.ifc -o model.igp
```

Use `--strict` when a pipeline must reject missing, degraded or repaired
geometry. The [getting started guide](docs/getting-started.md) covers local
browser imports, Node, Rust and the `@tessifc/three` adapter. npm package
installation instructions apply once the preview packages are published.

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

## Contributing and licensing

[CONTRIBUTING.md](CONTRIBUTING.md) explains how to build, test and add
evaluators. Report security issues through [SECURITY.md](SECURITY.md).

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). TessIFC is an
independent implementation, not endorsed or certified by buildingSMART.
