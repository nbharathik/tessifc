// SPDX-License-Identifier: Apache-2.0
// The MCP server end to end over a spawned CLI: the tools match the contract,
// a model is created, edited, undone, exported and verified.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

const here = fileURLToPath(new URL(".", import.meta.url));
const root = resolve(here, "../../..");
if (!existsSync(resolve(root, "bindings/wasm/pkg-node/tessifc_wasm.js"))) {
  console.log("skip  build the Node package first (python scripts/build-wasm.py --target both)");
  process.exit(0);
}
const contract = JSON.parse(readFileSync(resolve(here, "../contract.json"), "utf8"));
const dir = mkdtempSync(join(tmpdir(), "tessifc-mcp-"));
const file = join(dir, "house.ifc");

const transport = new StdioClientTransport({
  command: process.execPath,
  args: [resolve(here, "../src/cli.js"), file, "--new", "--port", "0", "--storeys", "Ground floor:0,Upper floor:3"],
  stderr: "pipe",
  cwd: root,
});
const stderrLines = [];
const viewerUrl = new Promise((done) => {
  transport.stderr.on("data", (chunk) => {
    for (const line of String(chunk).split(/\r?\n/)) {
      if (!line) continue;
      stderrLines.push(line);
      const match = /Viewer: (http:\/\/127\.0\.0\.1:\d+)\//.exec(line);
      if (match) done(match[1]);
    }
  });
});
const client = new Client({ name: "tessifc-test", version: "0" });
const text = (response) => JSON.parse(response.content[0].text);
try {
  await client.connect(transport);
  const base = await viewerUrl;
  ok(/^http:\/\/127\.0\.0\.1:\d+$/.test(base), "the CLI prints the viewer address on stderr");

  const { tools } = await client.listTools();
  const names = tools.map((tool) => tool.name).sort();
  assert.deepEqual(names, Object.keys(contract.tools).sort());
  for (const tool of tools) {
    const expected = contract.tools[tool.name];
    const properties = Object.keys(tool.inputSchema?.properties ?? {}).sort();
    assert.deepEqual(properties, [...expected.input.properties].sort(), `${tool.name} input properties`);
    assert.deepEqual([...(tool.inputSchema?.required ?? [])].sort(), [...expected.input.required].sort(), `${tool.name} required inputs`);
  }
  ok(true, "the tools and their inputs match contract.json");
  const { resources } = await client.listResources();
  const { prompts } = await client.listPrompts();
  ok(contract.resources.every((uri) => resources.some((resource) => resource.uri === uri)) && prompts.some((prompt) => prompt.name === "build-a-building"),
    "the resources and the prompt are listed");
  const api = await client.readResource({ uri: "tessifc://script-api" });
  ok(/ifc\.addWall/.test(api.contents[0].text), "the script API resource documents the building helpers");

  const described = text(await client.callTool({ name: "describe_model", arguments: {} }));
  ok(described.open && described.revision === "0" && described.storeys.length === 2 && described.lengthUnit === "m" && described.viewer.url.startsWith(base),
    "describe_model reports the new model, its storeys and the viewer");
  for (const key of contract.tools.describe_model.result) assert.ok(key in described, `describe_model result has ${key}`);

  const inspected = text(await client.callTool({ name: "inspect_model", arguments: { code: 'print(ifc.storeys().map((s) => s.Name).join("|"))' } }));
  ok(inspected.ok && inspected.stdout === "Ground floor|Upper floor", "inspect_model prints without changing anything");

  const before = readFileSync(file);
  const edited = await client.callTool({ name: "edit_model", arguments: { script: 'const w = ifc.addWall({ from: [0, 0], to: [6, 0], height: 3, thickness: 0.3, name: "South wall" }); print(w.id);', summary: "Adds the south wall." } });
  const edit = text(edited);
  ok(!edited.isError && edit.ok && edit.changed && edit.revision === "1" && edit.affectedProducts.length === 1 && edit.saved && edit.history.undo === 1,
    "edit_model publishes revision 1 with one affected product and saves");
  for (const key of contract.tools.edit_model.result) assert.ok(key in edit, `edit_model result has ${key}`);
  ok(edited.structuredContent?.revision === "1", "the structured content carries the same record");
  const after = readFileSync(file);
  ok(after.length > before.length && after.toString("latin1").includes("'South wall'"), "the file on disk carries the wall");
  const status = await (await fetch(`${base}/__tessifc/session`)).json();
  ok(status.revision === "1" && status.capabilities.authoring === "javascript", "the viewer route reports the new revision");

  const failing = await client.callTool({ name: "edit_model", arguments: { script: "missing()" } });
  ok(failing.isError && /ReferenceError/.test(text(failing).error) && text(failing).revision === "1", "a failing script is an error result and publishes nothing");
  const rejected = await client.callTool({ name: "edit_model", arguments: { script: 'ifc.byType("IfcWall")[0].Representation.Representations[0].Items[0].SweptArea = null;' } });
  ok(rejected.isError && /rejected/.test(text(rejected).error), "a candidate the kernel refuses is an error result with the reason");

  const found = text(await client.callTool({ name: "find_products", arguments: { class: "IfcWall" } }));
  ok(found.total === 1 && found.products[0].name === "South wall" && found.products[0].storey === "Ground floor", "find_products lists the wall with its storey");
  const info = text(await client.callTool({ name: "product_info", arguments: { id: found.products[0].id } }));
  ok(info.class === "IfcWall" && info.container === "Ground floor" && info.representations.length === 1 && info.placement.location.length === 3, "product_info describes the wall");

  const undone = text(await client.callTool({ name: "undo", arguments: {} }));
  ok(undone.ok && undone.revision === "2" && undone.removedProducts.length === 1 && undone.history.redo === 1, "undo publishes revision 2 and removes the wall");
  const redone = text(await client.callTool({ name: "redo", arguments: {} }));
  ok(redone.revision === "3" && redone.affectedProducts.length === 1, "redo brings it back as revision 3");

  const exported = text(await client.callTool({ name: "export_model", arguments: { path: join(dir, "copy.ifc") } }));
  ok(existsSync(join(dir, "copy.ifc")) && exported.bytes === readFileSync(join(dir, "copy.ifc")).length, "export_model writes a copy");
  const verified = text(await client.callTool({ name: "verify_revision", arguments: {} }));
  ok(verified.ok && verified.products === 1 && verified.revision === "3", "verify_revision agrees with a fresh evaluation");

  const selection = text(await client.callTool({ name: "get_selection", arguments: {} }));
  ok(selection.ids.length === 0 && selection.reportedAt === null, "no selection until the viewer reports one");
  const examples = text(await client.callTool({ name: "list_examples", arguments: {} }));
  ok(examples.examples.some((example) => example.title === "Build a small house"), "the examples include the house");

  const fresh = text(await client.callTool({ name: "new_model", arguments: { name: "Second", path: join(dir, "second.ifc") } }));
  ok(fresh.revision === "0" && fresh.generation === 2 && existsSync(join(dir, "second.ifc")), "new_model starts a second generation saved to its file");
  const reopened = text(await client.callTool({ name: "open_model", arguments: { path: file } }));
  ok(reopened.generation === 3 && reopened.products === 1, "open_model reopens the first file with its wall");
} finally {
  await client.close().catch(() => {});
  rmSync(dir, { recursive: true, force: true });
}
ok(stderrLines.some((line) => /ready/.test(line)) && !stderrLines.some((line) => /^\{/.test(line)), "diagnostics went to stderr");
console.log(`PASS ${passed} checks`);
