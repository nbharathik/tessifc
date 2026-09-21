<!-- SPDX-License-Identifier: Apache-2.0 -->
# @tessifc/mcp

An MCP server over the TessIFC kernel. An agent (Claude Code, Claude Desktop,
or any client that speaks the Model Context Protocol) opens, creates and edits
IFC models through it, and the reference viewer follows every change at a
loopback address. No Python, no IfcOpenShell: the kernel runs in Node through
WebAssembly and scripts use the same JavaScript API as the viewer's Session
panel.

## Run it

From a checkout, after `python scripts/build-wasm.py --target both`:

```sh
npm ci                                                # at the repository root
node bindings/mcp/src/cli.js house.ifc --new          # a new model, saved to house.ifc after every edit
node bindings/mcp/src/cli.js model.ifc                # an existing file
```

Installed from npm beside `@tessifc/core`, `npx tessifc-mcp model.ifc
--root <checkout>` is the same server; `@tessifc/edit` comes with it, so
only the viewer files need the checkout.

The server speaks MCP on stdin and stdout and prints the viewer address on
stderr, for example `Viewer: http://127.0.0.1:8000/viewer/?session=file`.
Open it in a browser: the page follows the file, shows every revision, and
its Session panel runs JavaScript on the same host.

Register it with Claude Code:

```sh
claude mcp add tessifc -- node bindings/mcp/src/cli.js --new house.ifc
```

Options: `--new` (start from a project, site, building and storeys; refuses
to overwrite an existing file without `--force`), `--schema IFC2X3|IFC4|IFC4X3`,
`--units m|mm`, `--storeys "Ground floor:0,Upper floor:3"`, `--port 8000`
(`0` picks a free port), `--no-viewer`, `--no-save` (keep the model in memory
only), `--script-timeout-ms 30000` (stop a script that runs longer; `0` for
no limit), `--root <checkout>` (where the viewer and the WASM package are).

## Tools

| Tool | What it does |
|---|---|
| `describe_model` | File, schema, revision, unit, product counts, storeys, history and the viewer's selection |
| `find_products` | Products of a class, filtered by name or storey |
| `product_info` | One entity: attributes, container, property sets, representation items, placement |
| `inspect_model` | Run read-only JavaScript and return what it prints |
| `edit_model` | Run a script; its edits become one revision. Returns the kernel's affected and removed products, diagnostics and whether the file was saved; `timedOut` when the script was stopped at the limit |
| `undo`, `redo` | New revisions that restore earlier content |
| `export_model` | Write the committed IFC to a path |
| `new_model`, `open_model` | Start a model from nothing, or follow another file |
| `get_selection` | What the user selected in the viewer |
| `verify_revision` | Evaluate the exported file from scratch and compare it with the scene built from the deltas |
| `list_examples` | Ready-made scripts, including a small house |

Resources: `tessifc://script-api` (the script names) and
`tessifc://model/summary`. Prompt: `build-a-building`.

Every result is JSON in the text content and, for the editing tools, as
structured content with a declared schema. A script that throws is an error
result with the traceback; a candidate the kernel refuses is an error result
with the diagnostics, and the committed model is unchanged.

## As a library

```js
import { createModelHost, createTessifcServer, createViewerServer } from "@tessifc/mcp";
```

`createModelHost({ Kernel, kernelModule, scriptTimeoutMs })` holds the
kernel, the editing session, the current snapshot and its content version,
the file it saves to, and a scene mirror for verification; with
`kernelModule` (the path of the Node kernel) scripts run in a worker thread
under the limit, without it they run in the process without one. `createViewerServer(host, { root, port })` is the
loopback server the viewer follows; `createTessifcServer(host)` returns the
MCP server for the transport of your choice. `examples/agent-building/` uses
the host and the viewer server without MCP.

## Boundary

Scripts run with the server's permissions and without a sandbox, in a worker
thread that is ended when a script passes the time limit; the limit catches a
script that never returns and is not a security boundary. Review what an
agent proposes before granting it automatic edits elsewhere.
The viewer server binds to the loopback interface only, checks the Host and
Origin headers, and requires a per-process token on every command.
