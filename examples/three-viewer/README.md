<!-- SPDX-License-Identifier: Apache-2.0 -->
# three-viewer

A drag-and-drop IFC viewer in one HTML page and one script. Parsing,
geometry and rendering all happen in the tab; nothing is uploaded anywhere.
`main.js` runs under `// @ts-check` against the packages' declarations.

## Running it

The WASM package has to exist first, and the page has to be served over HTTP
rather than opened from disk, because ES modules and `.wasm` both need real
MIME types:

```sh
python scripts/build-wasm.py --target web   # writes bindings/wasm/pkg
python -m http.server 8000 --bind 127.0.0.1
```

Then open <http://127.0.0.1:8000/examples/three-viewer/> and drop a `.ifc` file
on it. The import map at the top of the page points the package names at the
checkout and three.js at a CDN; with a bundler, install the packages and
delete the map.

## What it shows

Element count, triangle count, draw calls, and the parse and geometry time
measured live in the browser. Click an element to see its express id and IFC
class.

## What it is not

This is a compact v0.2 integration example. It has no
tree, no properties panel, no section planes and no measurement. `three.js`
comes from a CDN through an import map so that there is nothing to install.

The example converts synchronously and can block its tab on a large file.
Use a Web Worker for an application, as the reference viewer does. Replacing
a model disposes the previous group's GPU resources; a failed replacement
keeps the previous model available. Check the summary and product outcomes
before using geometry downstream.

The parts worth copying are in [`adapters/three`](../../adapters/three), which
is tested; this page is the wiring around them.
