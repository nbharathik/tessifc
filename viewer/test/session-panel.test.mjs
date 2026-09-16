// SPDX-License-Identifier: Apache-2.0
// The session panel without any Python: browser scripts, undo, examples, and
// the assistant against a scripted OpenAI-compatible server on the loopback.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { chromium } from "playwright";
import { serveViewer, instrumentViewer } from "./harness.mjs";
import { pavilionFile } from "./fixture.mjs";

/** Answers like a chat-completions endpoint: one inspect call, then a proposal or a text answer. */
function fakeProvider() {
  const requests = [];
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      const payload = JSON.parse(body || "{}");
      requests.push({ url: request.url, headers: request.headers, payload });
      // Earlier text turns come back as history; only tool-call turns belong to this exchange.
      const assistantTurns = payload.messages.filter((message) => message.role === "assistant" && message.tool_calls).length;
      const tools = (payload.tools ?? []).map((tool) => tool.function.name);
      const prompt = [...payload.messages].reverse().find((message) => message.role === "user" && typeof message.content === "string")?.content ?? "";
      let message;
      if (assistantTurns === 0) {
        message = { role: "assistant", content: null, tool_calls: [{ id: "call_1", type: "function",
          function: { name: "inspect_model", arguments: JSON.stringify({ code: 'print("walls", ifc.byType("IfcWall").length)' }) } }] };
      } else if (assistantTurns === 1 && tools.includes("propose_edit")) {
        const script = /rename/i.test(prompt)
          ? 'const target = selected ?? ifc.byType("IfcWall")[0];\ntarget.Name = "Assistant renamed wall";\nprint("renamed", target);\n'
          : 'const target = selected ?? ifc.byType("IfcWall")[0];\nconst solid = target.Representation.Representations[0].Items[0];\nsolid.Depth = solid.Depth + 0.5;\nprint("raised", target.Name);\n';
        message = { role: "assistant", content: null, tool_calls: [{ id: "call_2", type: "function",
          function: { name: "propose_edit", arguments: JSON.stringify({ script, summary: /rename/i.test(prompt) ? "Renames the wall." : "Raises the wall by 0.5." }) } }] };
      } else {
        const toolOutputs = payload.messages.filter((item) => item.role === "tool").map((item) => item.content).join(" | ");
        message = { role: "assistant", content: `Fake assistant: ${toolOutputs}` };
      }
      const answer = JSON.stringify({ id: "chatcmpl-1", choices: [{ index: 0, message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }],
        usage: { prompt_tokens: 10, completion_tokens: 5 } });
      response.writeHead(200, { "content-type": "application/json", "access-control-allow-origin": "*",
        "access-control-allow-headers": "authorization, content-type" });
      response.end(answer);
    });
  });
  server.on("request", (request, response) => {
    if (request.method === "OPTIONS") {
      response.writeHead(204, { "access-control-allow-origin": "*", "access-control-allow-headers": "authorization, content-type",
        "access-control-allow-methods": "POST" });
      response.end();
    }
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve({
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, requests, close: () => new Promise((done) => server.close(done)) })));
}

const server = await serveViewer();
const provider = await fakeProvider();
const browser = await chromium.launch({ channel: process.env.TESSIFC_BROWSER_CHANNEL || undefined,
  args: ["--use-angle=swiftshader", "--use-gl=angle", "--enable-unsafe-swiftshader"] });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await instrumentViewer(page);
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto(`${server.origin}/viewer/`);
  await page.evaluate((settings) => localStorage.setItem("tessifc.assistant", JSON.stringify(settings)),
    { provider: "compatible", model: "fake-model", baseUrl: provider.baseUrl, key: "test-key" });
  await page.reload();
  await page.setInputFiles("#file-input", pavilionFile());
  await page.waitForFunction(() => window.__tessifc?.ready() && window.__tessifc.loadStatus().finished, null, { timeout: 60000 });
  const revision = (value) => page.waitForFunction((expected) => window.__tessifc.state.model.revision === expected, value, { timeout: 60000 });
  const products = () => page.evaluate(() => window.__tessifc.state.model.index.recordsByExpressId.size);
  const metaCount = () => page.evaluate(() => document.querySelectorAll("#script-output .meta").length);
  const nextMeta = async (before, pattern) => {
    await page.waitForFunction((count) => document.querySelectorAll("#script-output .meta").length > count, before, { timeout: 60000 });
    const lines = await page.evaluate(() => [...document.querySelectorAll("#script-output .meta")].map((node) => node.textContent));
    assert.match(lines.slice(before).join("\n"), pattern);
    return lines.length;
  };

  await page.click("#rail-script");
  await page.waitForFunction(() => !document.querySelector("#session").classList.contains("collapsed"));
  assert.match(await page.textContent("#session-status"), /JavaScript · in this browser · assistant fake-model/);
  assert.equal(await page.$eval("#script-run", (button) => button.disabled), false);
  console.log("ok the panel runs browser scripts as soon as a model is open");

  // The wall is selected and its class group open, so the refresh can be checked for stability.
  const wallId = await page.evaluate(() => {
    const pack = window.__tessifc.pack();
    for (let record = 0; record < pack.instances.count; record += 1) {
      if (String(pack.index.classes[pack.instances.classIds[record]]) === "IfcWall") return pack.instances.expressIds[record];
    }
    return null;
  });
  await page.click("#tree-expand");
  await page.evaluate((id) => window.__tessifc.selectExpressId(id), wallId);
  await page.waitForFunction(() => window.__tessifc.state.selection?.infoState === "ready");
  const openBranches = await page.evaluate(() => document.querySelectorAll(".tnode.branch.open").length);
  await page.evaluate(() => {
    window.__loadingSeen = false;
    const list = document.querySelector("#property-list");
    new MutationObserver(() => {
      if (/Reading IFC attributes/.test(list.textContent)) window.__loadingSeen = true;
    }).observe(list, { childList: true, subtree: true, characterData: true });
  });

  // The door example adds an opening and a door into the first wall: three products change.
  const before = await products();
  await page.selectOption("#script-example", { label: "Add a door to a wall" });
  let seen = await metaCount();
  await page.click("#script-run");
  await revision("1");
  const flashing = await page.evaluate(() => window.__tessifc.renderer.animating);
  seen = await nextMeta(seen, /View updated: revision 1, 3 geometry updates, 0 removed/);
  assert.equal(await products(), before + 2);
  assert.match(await page.textContent("#script-output"), /added #\d+ IfcDoor into Gallery wall/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.fullRebuild), false);
  assert.equal(await page.evaluate(() => window.__tessifc.state.dirty), true);
  console.log("ok the door example cuts an opening and adds a door through the selective path");

  // The refresh keeps the panels steady: a highlight fade, the inspector and the
  // tree unchanged, and the download chip as the only unsaved-edit cue.
  assert.equal(flashing, true, "changed products are highlighted after the update");
  await page.waitForFunction(() => !window.__tessifc.renderer.animating, null, { timeout: 10000 });
  assert.equal(await page.evaluate(() => window.__loadingSeen), false, "the inspector never showed a loading gap");
  assert.equal(await page.evaluate(() => window.__tessifc.state.selection?.expressId), wallId);
  assert.equal(await page.evaluate(() => document.querySelectorAll("#property-list .prop-row").length), 9);
  assert.equal(await page.evaluate(() => document.querySelectorAll(".tnode.branch.open").length), openBranches);
  assert.equal(await page.evaluate(() => document.querySelector("#save-revision").classList.contains("hidden")), false);
  assert.equal(await page.evaluate(() => document.querySelector("#pending-bar")), null);
  await page.evaluate(() => window.__tessifc.shell.run("clear-selection"));
  console.log("ok the refresh highlights the change and keeps the inspector, tree and top bar steady");

  await page.click("#script-undo");
  await revision("2");
  seen = await nextMeta(seen, /View updated: revision 2/);
  assert.equal(await products(), before);
  await page.click("#script-redo");
  await revision("3");
  seen = await nextMeta(seen, /View updated: revision 3/);
  assert.equal(await products(), before + 2);
  console.log("ok undo and redo publish the stored source as new revisions");

  // Inspector saves and external updates enter the same history as scripts, so undo never
  // silently reverts them, and a new change clears the redo stack.
  const historyAt = () => page.evaluate(() => ({ ...window.__tessifc.state.scriptHistory }));
  const nameOf = () => page.evaluate(() => window.__tessifc.state.selection?.info?.fields.find((field) => field.name === "Name")?.value ?? null);
  const undoBefore = (await historyAt()).undo;
  await page.evaluate((id) => window.__tessifc.selectExpressId(id), wallId);
  await page.waitForFunction(() => document.querySelector('#edit-fields [data-attribute="Name"]'));
  await page.evaluate(() => {
    const input = document.querySelector('#edit-fields [data-attribute="Name"]');
    input.value = "Inspector name";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    document.querySelector("#edit-apply").click();
  });
  await revision("4");
  assert.equal((await historyAt()).undo, undoBefore + 1, "an inspector save is an undo entry");
  assert.equal(await nameOf(), "Inspector name");
  await page.click("#script-undo");
  await revision("5");
  await page.waitForFunction(() => window.__tessifc.state.selection?.infoState === "ready");
  assert.equal(await nameOf(), "Gallery wall", "undo restores the name the inspector changed");
  assert.deepEqual(await historyAt(), { undo: undoBefore, redo: 1 });
  page.once("dialog", (dialog) => dialog.accept());
  const external = Buffer.from(pavilionFile().buffer.toString("latin1").replace("'Roof'", "'Roof edited'"), "latin1");
  await page.setInputFiles("#revision-input", { name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: external });
  await revision("6");
  assert.deepEqual(await historyAt(), { undo: undoBefore + 1, redo: 0 }, "an external update is an undo entry and clears redo");
  const hasDoor = () => page.evaluate(() => {
    const pack = window.__tessifc.pack();
    return Array.from(pack.instances.classIds).some((classId, record) => pack.instances.active[record] && String(pack.index.classes[classId]) === "IfcDoor");
  });
  assert.equal(await hasDoor(), false, "the pristine file has no door");
  await page.click("#script-undo");
  await revision("7");
  assert.equal(await hasDoor(), true, "undoing the external update brings the door back");
  assert.deepEqual(await historyAt(), { undo: undoBefore, redo: 1 });
  assert.deepEqual(problems, []);
  console.log("ok inspector saves and external updates share the undo history");

  await page.fill("#script-editor", 'print("before");\nmissing();\n');
  await page.keyboard.press("Control+Enter");
  await page.waitForFunction(() => /ReferenceError: missing is not defined/.test(document.querySelector("#script-output").textContent));
  assert.match(await page.textContent("#script-output"), /line 2: missing\(\);/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "7");
  console.log("ok a failing script reports its line and publishes nothing");

  // A read-only script publishes nothing either.
  await page.selectOption("#script-example", { label: "List walls (read only)" });
  seen = await metaCount();
  await page.click("#script-run");
  seen = await nextMeta(seen, /No changes/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "7");
  console.log("ok a read-only script publishes no revision");

  // A script the kernel refuses (the wall loses its profile) reports the reason and publishes nothing.
  await page.evaluate((id) => window.__tessifc.selectExpressId(id), wallId);
  await page.waitForFunction(() => window.__tessifc.state.selection?.infoState === "ready");
  await page.fill("#script-editor", "selected.Representation.Representations[0].Items[0].SweptArea = null;");
  await page.keyboard.press("Control+Enter");
  await page.waitForFunction(() => /rejected/.test(document.querySelector("#script-output").textContent), null, { timeout: 60000 });
  assert.match(await page.textContent("#script-output"), /rejected: (E_|#\d+)/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "7");
  assert.equal(await page.evaluate(() => window.__tessifc.state.revisionPending), null);
  console.log("ok a rejected script names the kernel's reason and publishes nothing");

  // Deleting the selected door removes it and its relationships; the selection clears.
  const doorId = await page.evaluate(() => {
    const pack = window.__tessifc.pack();
    for (let record = 0; record < pack.instances.count; record += 1) {
      if (String(pack.index.classes[pack.instances.classIds[record]]) === "IfcDoor") return pack.instances.expressIds[record];
    }
    return null;
  });
  await page.evaluate((id) => window.__tessifc.selectExpressId(id), doorId);
  await page.waitForFunction(() => window.__tessifc.state.selection?.infoState === "ready");
  await page.selectOption("#script-example", { label: "Delete the selection" });
  await page.click("#script-run");
  await revision("8");
  assert.deepEqual(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.removedProducts), [doorId]);
  assert.equal(await page.evaluate(() => window.__tessifc.state.selection), null);
  console.log("ok deleting the selection removes the product and clears the selection");

  // Ask mode inspects through the worker and answers; nothing is published.
  await page.click("#rail-assistant");
  await page.fill("#assistant-prompt", "How many walls are there?");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => /Fake assistant: walls 2/.test(document.querySelector("#assistant-log").textContent), null, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "8");
  const askRequest = provider.requests[0];
  assert.equal(askRequest.headers.authorization, "Bearer test-key");
  assert.equal(askRequest.payload.model, "fake-model");
  assert.deepEqual(askRequest.payload.tools.map((tool) => tool.function.name), ["inspect_model"]);
  assert.match(askRequest.payload.messages[0].content, /ifc\.byType/);
  console.log("ok ask mode inspects the model and answers without a revision");

  // Edit mode under review: a proposal card, run from the card.
  await page.click('#assistant-mode [data-mode="edit"]');
  await page.fill("#assistant-prompt", "Rename the first wall");
  await page.click("#assistant-send");
  await page.waitForSelector("#assistant-log .proposal", { timeout: 60000 });
  assert.match(await page.textContent("#assistant-log .proposal-code"), /Assistant renamed wall/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), "8");
  seen = await metaCount();
  await page.click("#assistant-log .proposal .btn.accent");
  await revision("9");
  seen = await nextMeta(seen, /View updated: revision 9, 0 geometry updates/);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts.length), 0);
  console.log("ok an edit proposal is reviewed and then run through the script path");

  // Edit mode under the automatic policy runs immediately.
  await page.click("#rail-assistant");
  await page.check("#assistant-auto");
  await page.fill("#assistant-prompt", "Raise the first wall");
  await page.click("#assistant-send");
  await revision("10");
  await page.waitForFunction(() => document.querySelectorAll("#assistant-log .proposal button.accent[disabled]").length === 2, null, { timeout: 60000 });
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts.length), 1);
  console.log("ok the automatic policy runs the proposal and reports the impact");

  assert.deepEqual(problems, []);
} finally {
  await browser.close();
  await provider.close();
  await server.close();
}
