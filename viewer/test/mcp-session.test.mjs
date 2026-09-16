// SPDX-License-Identifier: Apache-2.0
// The viewer follows a tessifc-mcp session: it opens the empty model, shows
// every edit an MCP client makes, runs the panel's JavaScript examples on the
// host, reports its selection back, and reopens when the host starts a new
// model. One browser, one child process.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { instrumentViewer } from "./harness.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
const mcpDir = resolve(root, "bindings/mcp");
if (!existsSync(resolve(mcpDir, "node_modules/@modelcontextprotocol/sdk"))) {
  console.log("skip  install the MCP package first (npm ci --prefix bindings/mcp)");
  process.exit(0);
}
const require = createRequire(resolve(mcpDir, "package.json"));
const { Client } = require("@modelcontextprotocol/sdk/client/index.js");
const { StdioClientTransport } = require("@modelcontextprotocol/sdk/client/stdio.js");

const directory = mkdtempSync(join(tmpdir(), "tessifc-mcp-viewer-"));
const file = join(directory, "house.ifc");
const transport = new StdioClientTransport({
  command: process.execPath,
  args: [resolve(mcpDir, "src/cli.js"), file, "--new", "--port", "0", "--storeys", "Ground floor:0,Upper floor:3"],
  stderr: "pipe",
  cwd: root,
});
let stderr = "";
const viewerUrl = new Promise((done) => {
  transport.stderr.on("data", (chunk) => {
    stderr += chunk;
    const match = /Viewer: (http:\/\/127\.0\.0\.1:\d+\/viewer\/\?session=file)/.exec(stderr);
    if (match) done(match[1]);
  });
});
const client = new Client({ name: "tessifc-viewer-test", version: "0" });
const text = (response) => JSON.parse(response.content[0].text);
let browser;
try {
  await client.connect(transport);
  const url = await viewerUrl;
  browser = await chromium.launch({ channel: process.env.TESSIFC_BROWSER_CHANNEL || undefined,
    args: ["--use-angle=swiftshader", "--use-gl=angle", "--enable-unsafe-swiftshader"] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await instrumentViewer(page);
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto(url);
  await page.waitForFunction(() => window.__tessifc?.ready() && window.__tessifc.loadStatus().finished, null, { timeout: 60000 });
  const revision = (value) => page.waitForFunction((expected) => window.__tessifc.state.model.revision === expected, value, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "0");
  assert.equal(await page.evaluate(() => window.__tessifc.pack().instances.count), 0);
  assert.match(await page.textContent("#status-text"), /Empty model/);
  console.log("ok the viewer opens the empty model the host created");

  const wall = text(await client.callTool({ name: "edit_model", arguments: { script: 'const w = ifc.addWall({ from: [0, 0], to: [6, 0], height: 3, thickness: 0.3, name: "South wall" }); print(w.id);', summary: "South wall" } }));
  assert.equal(wall.revision, "1");
  await revision("1");
  await page.waitForFunction(() => !window.__tessifc.state.revisionPending, null, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts.length), 1);
  assert.equal(await page.evaluate(() => window.__tessifc.pack().instances.active.reduce((sum, value) => sum + value, 0)), 1);
  console.log("ok an MCP edit reaches the viewer as revision 1 with one affected product");

  await page.click("#rail-script");
  await page.waitForFunction(() => !document.querySelector("#session").classList.contains("collapsed"));
  assert.match(await page.textContent("#session-status"), /house\.ifc · JavaScript · tessifc-mcp/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.fileSession.status().capabilities.authoring), "javascript");
  const titles = await page.evaluate(() => [...document.querySelectorAll("#script-example option")].map((option) => option.textContent));
  assert.ok(titles.includes("Build a small house") && titles.includes("Add a column"));
  console.log("ok the session panel shows the JavaScript session and its examples");

  await page.selectOption("#script-example", { label: "Add a column" });
  await page.click("#script-run");
  await revision("2");
  await page.waitForFunction(() => /View updated: revision 2/.test(document.querySelector("#script-output").textContent), null, { timeout: 60000 });
  const described = text(await client.callTool({ name: "describe_model", arguments: {} }));
  assert.equal(described.revision, "2");
  assert.equal(described.history.undo, 2);
  assert.equal(described.products.IfcColumn, 1);
  console.log("ok a panel script runs on the host and the MCP client sees the revision");

  const wallId = Number(wall.stdout.trim());
  await page.evaluate((id) => window.__tessifc.selectExpressId(id), wallId);
  await page.waitForFunction(() => window.__tessifc.state.selection?.infoState === "ready");
  await page.waitForFunction(() => window.__tessifc.state.fileSession.status()?.capabilities?.selection, null, { timeout: 10000 });
  let selection = null;
  for (let attempt = 0; attempt < 40 && !selection?.ids?.length; attempt += 1) {
    await new Promise((done) => setTimeout(done, 100));
    selection = text(await client.callTool({ name: "get_selection", arguments: {} }));
  }
  assert.deepEqual(selection.ids, [wallId]);
  assert.equal(selection.className, "IfcWall");
  const withViewer = text(await client.callTool({ name: "describe_model", arguments: {} }));
  assert.equal(withViewer.viewer.applied?.revision, "2");
  console.log("ok the viewer reports its selection and the applied revision to the host");

  const fresh = text(await client.callTool({ name: "new_model", arguments: { name: "Second", path: join(directory, "second.ifc"), storeys: [{ name: "Only floor", elevation: 0 }] } }));
  assert.equal(fresh.generation, 2);
  await page.waitForFunction(() => window.__tessifc.state.model?.revision === "0" && window.__tessifc.state.fileSession.status()?.generation === 2
    && window.__tessifc.loadStatus().finished, null, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.pack().instances.count), 0);
  assert.match(await page.evaluate(() => document.querySelector("#model-name").textContent), /second\.ifc/);
  console.log("ok a new model on the host reopens the viewer at revision 0");
  assert.deepEqual(problems, []);
} finally {
  if (browser) await browser.close();
  await client.close().catch(() => {});
  rmSync(directory, { recursive: true, force: true });
}
