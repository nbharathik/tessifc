// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, rename, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { once } from "node:events";
import { chromium } from "playwright";
import { instrumentViewer } from "./harness.mjs";
import { pavilionIfc } from "./fixture.mjs";

const directory = await mkdtemp(join(tmpdir(), "tessifc-session-"));
const path = join(directory, "model.ifc");
const script = fileURLToPath(new URL("../../scripts/serve-edit-session.py", import.meta.url));
let child;
let browser;
try {
  let source = pavilionIfc();
  await writeFile(path, source);
  child = spawn(process.env.PYTHON || "python", [script, path, "--port", "0", "--assistant", "fake"], { windowsHide: true });
  let errors = "";
  let output = "";
  child.stderr.on("data", (chunk) => { errors += chunk; });
  const origin = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`File session did not start: ${errors}`)), 30000);
    child.once("error", (error) => { clearTimeout(timeout); reject(error); });
    child.once("exit", (code) => { clearTimeout(timeout); reject(new Error(`File session exited ${code}: ${errors}`)); });
    child.stdout.on("data", (chunk) => {
      output += chunk;
      const match = output.match(/Open (http:\/\/127\.0\.0\.1:\d+)/);
      if (match) { clearTimeout(timeout); resolve(match[1]); }
    });
  });
  const authoring = /Scripts run with IfcOpenShell/.test(output);
  browser = await chromium.launch({ channel: process.env.TESSIFC_BROWSER_CHANNEL || undefined,
    args: ["--use-angle=swiftshader", "--use-gl=angle", "--enable-unsafe-swiftshader"] });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await instrumentViewer(page);
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto(`${origin}/viewer/?session=file`);
  await page.waitForFunction(() => window.__tessifc?.ready(), null, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "0");
  const revision = (value) => page.waitForFunction((expected) => window.__tessifc.state.model.revision === expected, value, { timeout: 60000 });

  async function save(next) {
    const staging = join(directory, "next.ifc");
    await writeFile(staging, next);
    await rename(staging, path);
  }
  source = source.replace("'Gallery wall'", "'Externally renamed wall'");
  await save(source);
  await revision("1");
  assert.deepEqual(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts), []);
  console.log("ok local file session automatically applies an external metadata edit");

  await save(source.slice(0, -40));
  await page.waitForFunction(() => document.querySelector("#status-dot").classList.contains("err") &&
    !window.__tessifc.state.revisionPending, null, { timeout: 10000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "1");
  source = source.replace("'Externally renamed wall'", "'Recovered external wall'");
  await save(source);
  await revision("2");
  console.log("ok a rejected saved snapshot preserves the model and the next valid save recovers");

  let releaseFetch;
  const gate = new Promise((resolve) => { releaseFetch = resolve; });
  let intercepted = false;
  await page.route("**/__tessifc/model.ifc?*", async (route) => {
    if (!intercepted) {
      intercepted = true;
      await gate;
    }
    await route.continue();
  });
  const requested = page.waitForRequest((request) => request.url().includes("/__tessifc/model.ifc?"));
  source = source.replace("'Recovered external wall'", "'Retried external wall'");
  await save(source);
  await requested;
  await page.evaluate(() => { window.__tessifc.state.converting = true; });
  releaseFetch();
  await page.waitForTimeout(200);
  await page.evaluate(() => { window.__tessifc.state.converting = false; });
  await revision("3");
  await page.unroute("**/__tessifc/model.ifc?*");
  console.log("ok a temporary busy state during snapshot fetch does not consume the saved revision");

  // The session panel reports the connection and its capabilities.
  await page.click("#rail-script");
  await page.waitForFunction(() => !document.querySelector("#session").classList.contains("collapsed"));
  const status = await page.evaluate(() => window.__tessifc.state.fileSession.status());
  assert.equal(status.name, "model.ifc");
  assert.equal(status.capabilities.authoring, authoring);
  assert.equal(status.capabilities.assistant.provider, "fake");
  assert.match(await page.textContent("#session-status"), authoring ? /model\.ifc · Python · IfcOpenShell/ : /model\.ifc · Python · scripts unavailable/);
  assert.equal(await page.getAttribute("#rail-script", "aria-pressed"), "true");
  console.log("ok the session panel shows the connected session");

  if (authoring) {
    const wallGuid = source.match(/IFCWALL\('([^']+)'/)[1];
    // Waits for a new status line in the script output that matches `pattern`.
    const metaCount = () => page.evaluate(() => document.querySelectorAll("#script-output .meta").length);
    const nextMeta = async (before, pattern) => {
      await page.waitForFunction((count) => document.querySelectorAll("#script-output .meta").length > count, before, { timeout: 60000 });
      const lines = await page.evaluate(() => [...document.querySelectorAll("#script-output .meta")].map((node) => node.textContent));
      assert.match(lines.slice(before).join("\n"), pattern);
      return lines.length;
    };
    // A metadata script publishes a revision without any geometry work.
    await page.fill("#script-editor", `wall = model.by_guid(${JSON.stringify(wallGuid)})\nwall.Name = "Scripted wall"\nprint("renamed", wall.Name)\n`);
    let seen = await metaCount();
    await page.click("#script-run");
    await revision("4");
    seen = await nextMeta(seen, /View updated: revision 4, 0 geometry updates/);
    const outputText = await page.textContent("#script-output");
    assert.match(outputText, /renamed Scripted wall/);
    assert.match(outputText, /Saved in .*\(1 modified\)/);
    assert.deepEqual(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts), []);
    console.log("ok a metadata script publishes a revision with no tessellation");

    // A geometry script updates exactly the edited wall through the selective path.
    await page.fill("#script-editor", `wall = model.by_guid(${JSON.stringify(wallGuid)})\nsolid = wall.Representation.Representations[0].Items[0]\nsolid.Depth = solid.Depth + 0.5\nprint("depth", solid.Depth)\n`);
    await page.keyboard.press("Control+Enter");
    await revision("5");
    seen = await nextMeta(seen, /View updated: revision 5, 1 geometry updates, 0 removed/);
    const affected = await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts);
    assert.equal(affected.length, 1);
    assert.equal(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.fullRebuild), false);
    console.log("ok a geometry script updates only the edited wall");

    // A failing script leaves the model and the revision alone.
    await page.fill("#script-editor", `wall = model.by_guid(${JSON.stringify(wallGuid)})\nwall.Name = "Broken"\nraise ValueError("stop here")\n`);
    await page.click("#script-run");
    await page.waitForFunction(() => /ValueError: stop here/.test(document.querySelector("#script-output").textContent));
    await page.waitForFunction(() => !document.querySelector("#script-run").disabled);
    assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "5");
    console.log("ok a failing script is rolled back without a revision");

    // Undo restores the previous content as a new revision.
    await page.click("#script-undo");
    await revision("6");
    seen = await nextMeta(seen, /View updated: revision 6, 1 geometry updates/);
    console.log("ok undo publishes the previous content as a new revision");

    // Edit mode proposes a script under review; running it goes through the same path.
    await page.click("#rail-assistant");
    await page.click('#assistant-mode [data-mode="edit"]');
    await page.fill("#assistant-prompt", "raise the gallery wall");
    await page.click("#assistant-send");
    await page.waitForSelector("#assistant-log .proposal", { timeout: 60000 });
    assert.match(await page.textContent("#assistant-log .proposal-code"), /Depth/);
    assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "6");
    await page.click("#assistant-log .proposal .btn.accent");
    await revision("7");
    seen = await nextMeta(seen, /View updated: revision 7, 1 geometry updates/);
    console.log("ok the assistant proposal runs through the script path after review");

    // Ask mode answers from the model without changing it.
    await page.click("#rail-assistant");
    await page.click('#assistant-mode [data-mode="ask"]');
    await page.fill("#assistant-prompt", "how many products are there?");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => /products \d+/.test(document.querySelector("#assistant-log").textContent), null, { timeout: 60000 });
    assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "7");
    console.log("ok ask mode answers without a revision");
  } else {
    console.log("skip script and assistant flows: the session Python has no IfcOpenShell");
  }

  await page.evaluate((text) => {
    const transfer = new DataTransfer();
    transfer.items.add(new File([text], "separate.ifc"));
    window.dispatchEvent(new DragEvent("drop", { dataTransfer: transfer }));
  }, pavilionIfc());
  await page.waitForFunction(() => window.__tessifc.state.model?.file.name === "separate.ifc" &&
    !window.__tessifc.state.converting, null, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.fileSession), null);
  assert.match(await page.textContent("#session-status"), /JavaScript · in this browser/);
  await save(source.replace("'Retried external wall'", "'Detached external wall'"));
  await page.waitForTimeout(1500);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "0");
  assert.deepEqual(problems, []);
  console.log("ok dropping another model detaches the previous file session");
} finally {
  if (browser) await browser.close();
  if (child && child.exitCode === null) {
    const exited = once(child, "exit");
    child.kill();
    await exited;
  }
  await rm(directory, { recursive: true, force: true });
}
