#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0

//! tessifc-mcp: an MCP server on stdio over one IFC model, with a loopback
//! viewer server beside it so the reference viewer follows every edit.
//!
//!   tessifc-mcp [model.ifc] [--new] [--force] [--schema IFC4] [--units m]
//!               [--storeys "Ground floor:0,Upper floor:3"] [--port 8000]
//!               [--no-viewer] [--no-save] [--script-timeout-ms 30000]
//!               [--root <checkout>]

import { createRequire } from "node:module";
import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { createTessifcServer } from "./server.js";
import { createModelHost } from "./session-host.js";
import { createViewerServer } from "./viewer-server.js";

// Stdout carries the protocol; every other line goes to stderr.
const log = (...parts) => console.error("[tessifc-mcp]", ...parts);
console.log = console.info = console.debug = log;

const here = dirname(fileURLToPath(import.meta.url));
const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    new: { type: "boolean", default: false },
    force: { type: "boolean", default: false },
    schema: { type: "string", default: "IFC4" },
    units: { type: "string", default: "m" },
    storeys: { type: "string" },
    port: { type: "string", default: "8000" },
    "no-viewer": { type: "boolean", default: false },
    "no-save": { type: "boolean", default: false },
    "script-timeout-ms": { type: "string", default: "30000" },
    root: { type: "string" },
    help: { type: "boolean", default: false },
  },
});

if (values.help) {
  console.error(`usage: tessifc-mcp [model.ifc] [--new] [--force] [--schema IFC4] [--units m] [--storeys "Ground floor:0,Upper floor:3"] [--port 8000] [--no-viewer] [--no-save] [--script-timeout-ms 30000] [--root <checkout>]`);
  process.exit(0);
}
const scriptTimeoutMs = Number(values["script-timeout-ms"]);
if (!Number.isInteger(scriptTimeoutMs) || scriptTimeoutMs < 0) {
  log("--script-timeout-ms takes a whole number of milliseconds; 0 disables the limit");
  process.exit(1);
}

const root = resolve(values.root ?? resolve(here, "../../.."));
const packageVersion = JSON.parse(readFileSync(resolve(here, "../package.json"), "utf8")).version;
const require = createRequire(import.meta.url);
// The installed package first; a checkout that has not run npm ci falls back to the built files.
const kernelModule = resolveKernel();
function resolveKernel() {
  try {
    return require.resolve("@tessifc/core/node");
  } catch {
    const built = resolve(root, "bindings/wasm/pkg-node/tessifc_wasm.js");
    if (existsSync(built)) return built;
    log(`The Node kernel is missing: install @tessifc/core, or build it in a checkout with: python scripts/build-wasm.py --target both`);
    process.exit(1);
  }
}
const { Kernel } = require(kernelModule);

function parseStoreys(text) {
  if (!text) return undefined;
  return String(text).split(",").map((item, index) => {
    const [name, elevation] = item.split(":");
    return { name: name?.trim() || `Storey ${index + 1}`, elevation: Number(elevation ?? 0) || 0 };
  });
}

const host = createModelHost({ Kernel, save: !values["no-save"], log, version: packageVersion, scriptTimeoutMs, kernelModule });
const file = positionals[0] ? resolve(positionals[0]) : null;
if (file && (values.new || !existsSync(file))) {
  if (existsSync(file) && !values.force) {
    log(`${file} exists; pass --force to overwrite it, or drop --new to open it`);
    process.exit(1);
  }
  await host.newModel({ schema: values.schema, units: values.units, storeys: parseStoreys(values.storeys), name: positionals[0].replace(/\.ifc$/i, "") }, { path: file, force: true });
} else if (file) {
  await host.openFile(file);
}

let viewer = null;
if (!values["no-viewer"]) {
  viewer = createViewerServer(host, { root, port: Number(values.port) || 0, log });
  await viewer.listen();
  log(`Viewer: ${viewer.url}/viewer/?session=file`);
}

const server = createTessifcServer(host, { viewerUrl: viewer ? `${viewer.url}/viewer/?session=file` : null, version: packageVersion });
const transport = new StdioServerTransport();
let closing = false;
async function shutdown() {
  if (closing) return;
  closing = true;
  await viewer?.close();
  host.dispose();
  process.exit(0);
}
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
await server.connect(transport);
// The client closing its end of the pipe ends the session.
server.server.onclose = shutdown;
log(`ready${host.session ? `: ${host.name} at revision ${host.session.revision}` : ", no model open"}`);
