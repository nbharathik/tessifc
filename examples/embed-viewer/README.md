<!-- SPDX-License-Identifier: Apache-2.0 -->
# embed-viewer

The [`@tessifc/viewer`](../../bindings/viewer/README.md) package in one HTML
page: a drop zone, a row of buttons and the viewer's own canvas. Parsing,
geometry and rendering all happen in the tab; nothing is uploaded anywhere.

## Running it

The WASM package has to exist first, and the page has to be served over HTTP
rather than opened from disk, because ES modules and `.wasm` both need real
MIME types:

```sh
python scripts/build-wasm.py --target web   # writes bindings/wasm/pkg
python -m http.server 8000 --bind 127.0.0.1
```

Then open <http://127.0.0.1:8000/examples/embed-viewer/> and drop a `.ifc`
file on it. The import map at the top of the page points the package names
at the checkout; with a bundler, install the packages and delete the map.

## What it shows

Streaming progress while the kernel works, the selected element's class and
id, and one button per API call: fit, focus, top and 3D views, display style,
hide, isolate, show all and a horizontal section. Click an element to select
it; the orbit and zoom centre move to the point you clicked.
