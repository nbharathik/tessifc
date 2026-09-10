<!-- SPDX-License-Identifier: Apache-2.0 -->
# TessIFC viewer

A browser viewer for IFC files, powered by the [TessIFC](../README.md) geometry
kernel. Open a model to inspect, section, measure and edit it. Files stay in
your browser; nothing is uploaded.

**v0.1 developer preview.** Built with WebAssembly and WebGL2, with no runtime
npm or CDN dependencies.

![The TessIFC viewer inspecting a pavilion model](../docs/assets/viewer.png)

## What it does

- Browse and search elements by spatial structure, IFC class, name or ID.
- Inspect attributes, hide or isolate elements, and switch display styles.
- Cut live sections and measure between corners, edges and surfaces.
- Edit text attributes and export the IFC while preserving unrelated source.

Reads IFC2X3, IFC4 and IFC4X3. Geometry support varies by representation; see
[geometry coverage](../docs/coverage.md) and the [preview contract](../docs/preview.md).

## Run locally

Requires Rust via rustup, Python 3.11+ and a browser with WebGL2 support.
Run from the repository root:

```sh
rustup target add wasm32-unknown-unknown
python scripts/build-wasm.py --target web
python -m http.server 8000 --bind 127.0.0.1
```

If `wasm-bindgen-cli` is missing or mismatched, the build script prints the
installation command. See [getting started](../docs/getting-started.md) for details.

Open <http://127.0.0.1:8000/viewer/> and drop an `.ifc` file onto the page.
Serve the repository root so the viewer can load `bindings/wasm/pkg/`.

## Controls

Left-drag to orbit, right-drag or Shift-drag to pan, and scroll to zoom.
Double-click an element to frame it. On touch screens, use one finger to
orbit and two fingers to pan or pinch to zoom.

| Key | Action |
| --- | --- |
| `F` / `Shift F` | Frame model / selection |
| `1` `2` `3` `4` | Front, right, top, isometric |
| `M` / `X` / `P` | Measure / section plane / plan view |
| `I` / `H` / `A` | Isolate / hide / show all |
| `E` | Toggle the attribute editor |
| `Ctrl O` / `Ctrl S` | Open IFC / export IFC |
| `Ctrl K` / `?` | Command palette / all shortcuts |

## Tests

Requires Node 20+. From the repository root:

```sh
python scripts/build-wasm.py --target both
npm ci --prefix viewer
cd viewer
npx playwright install chromium
npm test
```

Runs unit tests and browser rendering checks. Set
`TESSIFC_BROWSER_CHANNEL=chrome` to use an installed Chrome.

## Licence

Apache-2.0. See [LICENSE](../LICENSE) and [NOTICE](../NOTICE).
