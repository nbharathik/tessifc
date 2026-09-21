<!-- SPDX-License-Identifier: Apache-2.0 -->
# embed-viewer

The [`@tessifc/viewer`](../../bindings/viewer/README.md) package in one HTML
page and one script: a drop zone, a row of buttons and the viewer's own
canvas. Parsing, geometry and rendering all happen in the tab; nothing is
uploaded anywhere. `main.js` runs under `// @ts-check` against the packages'
declarations, so it doubles as a typed example.

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
`?worker=1` runs the kernel in the package's worker instead of on the page
(`&scriptTimeoutMs=<ms>` sets the script limit), which is what an
application should do for large files.

## What it shows

Streaming progress while the kernel works, the selected element's class and
id, and one button per API call: fit, focus, top and 3D views, display style,
hide, isolate, show all and a horizontal section. Click an element to select
it; the orbit and zoom centre move to the point you clicked.

## Following a session

Start a session host instead of the static server, for example an MCP server
with a new model:

```sh
npm ci
node bindings/mcp/src/cli.js house.ifc --new
```

Open <http://127.0.0.1:8000/examples/embed-viewer/?session=file>, or press
**Follow the local session** on the page. The viewer opens the host's model
(empty at first) and applies every revision the host publishes as a delta;
the status line reports the kernel's affected-product count after each one.
Edit through an MCP client, or run `node examples/agent-building/build-house.mjs
house.ifc --serve` and watch the house grow. The Python session server
(`python scripts/serve-edit-session.py model.ifc`) is followed the same way.
