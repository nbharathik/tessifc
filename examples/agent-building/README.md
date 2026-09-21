<!-- SPDX-License-Identifier: Apache-2.0 -->
# An agent builds a house

A small two-storey house, built from an empty IFC model one step at a time:
exterior and interior walls, slabs, doors and windows with their openings, a
roof, columns and a beam, property sets and colours. Each step is one script
against the [script API](../../docs/sdk.md#browser-scripts); after each step
the kernel reports which products it rebuilt, and a viewer that follows the
session shows the house grow.

There are four ways to run it. The first three need the Node kernel:

```sh
python scripts/build-wasm.py --target both
npm ci
```

## 1. Scripted, no model needed

```sh
node examples/agent-building/build-house.mjs house.ifc --serve
```

Open the address it prints. The build starts after a short pause and pauses
between steps so the viewer's refresh is visible: only the products of the
current step are tessellated, everything else keeps its GPU buffers. The
script ends with a verification that the scene assembled from the eleven
deltas equals a fresh evaluation of the saved file. Drop `--serve` to build
without the viewer.

The steps live in [`steps.mjs`](steps.mjs), so the same house is what the
test and the agent brief expect.

## 2. From Claude Code or any MCP client

```sh
claude mcp add tessifc -- node bindings/mcp/src/cli.js --new house.ifc
```

Start a conversation, open the viewer address the server prints on start, and
ask for the `build-a-building` prompt (or say: "build a small two-storey house
in the open model, one step at a time, and verify it at the end"). The server
exposes `describe_model`, `inspect_model`, `edit_model`, `undo`, `redo`,
`export_model`, `new_model`, `open_model`, `find_products`, `product_info`,
`get_selection`, `verify_revision` and `list_examples`; the model reads the
script API from the `tessifc://script-api` resource. Every `edit_model` result
carries the kernel's revision and affected products, so the agent reports what
was rebuilt rather than what it intended.

For Claude Desktop, add to its configuration:

```json
{
  "mcpServers": {
    "tessifc": { "command": "node", "args": ["<checkout>/bindings/mcp/src/cli.js", "--new", "house.ifc"] }
  }
}
```

Click a product in the viewer and the agent can ask `get_selection` for it.

## 3. Through a provider of your choice

```sh
OPENROUTER_API_KEY=... node examples/agent-building/agent.mjs house.ifc --serve
ANTHROPIC_API_KEY=... node examples/agent-building/agent.mjs house.ifc --provider anthropic --serve
```

`agent.mjs` drives `runAgentTurn` from `@tessifc/edit` with the chat-completions
adapter (OpenRouter, or any compatible server through `--model` and the code)
or the Messages adapter. Proposals run automatically; the transcript with every
script, the kernel's report for it and the token usage is written to
`transcript.json`. The model must support tool calling.

## 4. With Python and IfcOpenShell

```sh
pip install -e "adapters/ifcopenshell[authoring,mcp]"
python examples/agent-building/build_house.py house.ifc --serve
```

`build_house.py` builds the same eleven steps with `ifcopenshell.api`
(walls, slabs, openings and fillings, columns, a beam, property sets and
styles), saving the file after each so a viewer that follows it shows the
house grow; `--serve` starts the Python session server beside it. For an
agent, `claude mcp add tessifc-py -- python scripts/serve-edit-session.py
house.ifc --mcp --new` offers the same tools with Python scripts, and the
`build-a-building` prompt works unchanged. `adapters/ifcopenshell/tests/test_build_house.py`
checks the counts in both IFC4 and IFC2X3.

## In the viewer only

Without any of the above, open the reference viewer, load a model, and pick
**Examples > Build a small house** in the Session panel: the same helpers add
a house beside whatever is loaded, in the browser, with nothing installed.

## What to look at

* The status line after each step: `Revision N: k geometry updates`. A rename
  or a property set reports zero geometry updates.
* `verify_revision` (MCP) or the end of `build-house.mjs`: the scene built from
  deltas equals a fresh evaluation of the exported file, product by product.
* `house.ifc` opens in any IFC tool; it is plain IFC4 (or IFC2X3 with
  `--schema IFC2X3`).
