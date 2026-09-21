// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { createRequire } from "node:module";
import { instrumentViewer, serveViewer } from "./harness.mjs";
import { pavilionIfc } from "./fixture.mjs";
import { readIgp } from "../src/igp.js";
import { normalizeScene as normalized } from "../../bindings/edit/src/verify.js";

const require = createRequire(import.meta.url);
const { Kernel } = require("../../bindings/wasm/pkg-node/tessifc_wasm.js");
const settings = JSON.stringify({ includeSpaces: true, includeOpenings: true, includeAnnotations: true, includeReferences: true });

function records(source) {
  return new Map([...source.matchAll(/^#(\d+)=(.*);$/gm)].map((match) => [Number(match[1]), match[2]]));
}

const refs = (record) => [...record.matchAll(/#(\d+)/g)].map((match) => Number(match[1]));

function product(source, name) {
  const entities = records(source);
  const [id, record] = [...entities].find(([, value]) => value.includes(`'${name}'`));
  const representation = refs(record).at(-1);
  const shape = refs(entities.get(representation)).at(-1);
  const solid = refs(entities.get(shape)).at(-1);
  return { id, solid, profile: refs(entities.get(solid))[0], representation, record };
}

function replaceRecord(source, id, change) {
  return source.replace(new RegExp(`^#${id}=(.*);$`, "m"), (_, text) => `#${id}=${change(text)};`);
}

function fullScene(source) {
  const kernel = new Kernel();
  try {
    const model = kernel.openModel(Buffer.from(source));
    kernel.evaluateGeometry(model, settings);
    return normalized(readIgp(kernel.getPack(model)));
  } finally { kernel.free(); }
}

const server = await serveViewer();
let browser;
try {
  browser = await chromium.launch({ channel: process.env.TESSIFC_BROWSER_CHANNEL || undefined,
    args: ["--use-angle=swiftshader", "--use-gl=angle", "--enable-unsafe-swiftshader"] });
  const page = await browser.newPage({ viewport: { width: 1024, height: 768 } });
  await instrumentViewer(page);
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  await page.goto(`${server.origin}/viewer/`);
  let source = pavilionIfc();
  await page.setInputFiles("#file-input", { name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: Buffer.from(source) });
  await page.waitForFunction(() => window.__tessifc?.ready(), null, { timeout: 60000 });
  await page.evaluate(() => {
    const test = window.__tessifc;
    test.revisionReloads = 0;
    const reload = test.renderer.reload.bind(test.renderer);
    test.renderer.reload = (...args) => { test.revisionReloads += 1; return reload(...args); };
  });

  async function update(next, expectedProducts) {
    const before = await page.evaluate(() => window.__tessifc.state.model.revision);
    await page.setInputFiles("#revision-input", { name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: Buffer.from(next) });
    await page.waitForFunction((revision) => !window.__tessifc.state.revisionPending &&
      (window.__tessifc.state.model.revision !== revision || document.querySelector("#status-text").textContent.includes("rejected")), before, { timeout: 60000 });
    const state = await page.evaluate(() => ({ revision: window.__tessifc.state.model.revision,
      update: window.__tessifc.state.model.lastUpdate, status: document.querySelector("#status-text").textContent }));
    assert.equal(state.revision, String(BigInt(before) + 1n), state.status);
    assert.deepEqual(state.update.affectedProducts, expectedProducts.toSorted((a, b) => a - b));
    const packed = await page.evaluate(() => {
      const pack = window.__tessifc.pack();
      return { index: { model_offset: pack.index.model_offset ?? [0, 0, 0] },
        geometry: pack.geometry.map((mesh) => ({ id: mesh.id, positions: Array.from(mesh.positions), indices: Array.from(mesh.indices) })),
        instances: Object.fromEntries(Object.entries(pack.instances).map(([key, value]) => [key, ArrayBuffer.isView(value) ? Array.from(value) : value])) };
    });
    assert.deepEqual(normalized(packed), fullScene(next), "incremental triangles equal a fresh full evaluation");
    assert.equal(await page.evaluate(() => window.__tessifc.revisionReloads), 0, "selective updates never reload the whole scene");
    source = next;
    return state.update;
  }

  const wall = product(source, "Gallery wall");
  const opening = product(source, "Gallery window opening");
  let info = await update(replaceRecord(source, wall.solid, (record) => record.replace(/,3\.4\)$/, ",4.)")), [wall.id]);
  assert.ok(info.renderer.reusedBatches > 0, "unrelated GPU batches survive a wall edit");
  assert.ok(Number.isFinite(info.stages.totalMs) && info.stages.totalMs >= info.stages.rendererMs, "main-thread stages are timed");
  assert.ok(Number.isFinite(info.kernel?.prepareMs) && Number.isFinite(info.commitMs), "kernel and worker stages ride along");
  console.log("ok wall resize, selective GPU reuse and full-evaluation equivalence");

  info = await update(replaceRecord(source, opening.profile, (record) => record.replace(",2.6,", ",3.,")), [wall.id, opening.id]);
  assert.ok(info.reasons.some((reason) => reason.expressId === wall.id));
  console.log("ok opening edit invalidates its host");

  await page.evaluate(() => { window.__tessifc.savedBatches = window.__tessifc.renderer.batches.slice(); });
  await update(replaceRecord(source, wall.id, (record) => record.replace("'Gallery wall'", "'Renamed gallery'")), []);
  assert.ok(await page.evaluate(() => window.__tessifc.savedBatches.every((batch, i) => batch === window.__tessifc.renderer.batches[i])),
    "metadata edit retains every GPU batch");
  console.log("ok metadata-only edit skips tessellation and retains GPU resources");

  const added = `#9000=IFCCOLUMN('0000000000000000009000',$,'New column',$,$,$,#${wall.representation},$,.NOTDEFINED.);\n`;
  await update(source.replace("ENDSEC;\nEND-ISO-10303-21;", `${added}ENDSEC;\nEND-ISO-10303-21;`), [9000]);
  await update(source.replace(added, ""), []);
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.index.recordsByExpressId.has(9000)), false);
  console.log("ok create and delete maintain the active scene");

  const revision = await page.evaluate(() => window.__tessifc.state.model.revision);
  await page.setInputFiles("#revision-input", { name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: Buffer.from(source.slice(0, -40)) });
  await page.waitForFunction(() => !window.__tessifc.state.revisionPending && document.querySelector("#status-text").textContent.includes("rejected"));
  assert.equal(await page.evaluate(() => window.__tessifc.state.model.revision), revision);
  assert.deepEqual(problems, []);
  console.log("ok truncated revision rejected without changing the committed model");

  await page.evaluate((id) => window.__tessifc.selectExpressId(id), wall.id);
  await page.waitForFunction(() => document.querySelector('#edit-fields [data-attribute="Name"]'));
  const undoEntries = await page.evaluate(() => window.__tessifc.state.scriptHistory.undo);
  await page.evaluate(() => {
    window.__tessifc.savedBatches = window.__tessifc.renderer.batches.slice();
    const input = document.querySelector('#edit-fields [data-attribute="Name"]');
    input.value = "Edited from the inspector";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    document.querySelector("#edit-apply").click();
  });
  await page.waitForFunction((before) => !window.__tessifc.state.revisionPending &&
    window.__tessifc.state.model.revision !== before, revision, { timeout: 60000 });
  assert.deepEqual(await page.evaluate(() => window.__tessifc.state.model.lastUpdate.affectedProducts), []);
  assert.ok(await page.evaluate(() => window.__tessifc.state.dirty &&
    window.__tessifc.state.selection.info.fields.some((field) => field.name === "Name" && field.value === "Edited from the inspector") &&
    window.__tessifc.savedBatches.every((batch, i) => batch === window.__tessifc.renderer.batches[i])));
  assert.deepEqual(problems, []);
  assert.equal(await page.evaluate(() => window.__tessifc.state.scriptHistory.undo), undoEntries + 1, "an inspector save is an undo entry");
  console.log("ok inspector saves use staged revisions, update properties and retain GPU batches");

  // A revision message that reports the current revision again releases the pending gate.
  await page.evaluate(() => {
    const test = window.__tessifc;
    test.state.revisionPending = { requestId: 424242 };
    const model = test.state.model;
    test.receiveRevision({ modelId: model.modelId, requestId: 424242, revision: model.revision, baseRevision: model.revision });
  });
  assert.equal(await page.evaluate(() => window.__tessifc.state.revisionPending), null);
  assert.notEqual(await page.evaluate(() => window.__tessifc.state.model.stale), true);
  assert.deepEqual(problems, []);
  console.log("ok a repeated revision releases the pending gate");

  // Products that share one GlobalId keep their hidden state through a selective update.
  page.on("dialog", (dialog) => dialog.accept());
  source = pavilionIfc({ duplicateGuids: true });
  await page.setInputFiles("#file-input", { name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: Buffer.from(source) });
  await page.waitForFunction(() => window.__tessifc?.ready() && window.__tessifc.state.model.revision === "0" &&
    window.__tessifc.loadStatus().finished, null, { timeout: 60000 });
  const column = product(source, "Steel column");
  await page.evaluate((id) => window.__tessifc.selectExpressId(id), column.id);
  await page.waitForFunction(() => window.__tessifc.state.selection?.infoState === "ready");
  await page.evaluate(() => window.__tessifc.shell.run("hide"));
  assert.equal(await page.evaluate(() => window.__tessifc.state.hiddenRecords.size), 1);
  const duplicateWall = product(source, "Gallery wall");
  info = await update(replaceRecord(source, duplicateWall.solid, (record) => record.replace(/,3\.4\)$/, ",3.6)")), [duplicateWall.id]);
  assert.equal(info.fullRebuild, false, "duplicated GlobalIds do not force a full rebuild");
  assert.deepEqual(await page.evaluate(() => [...window.__tessifc.state.hiddenRecords].map((record) => window.__tessifc.pack().instances.expressIds[record])), [column.id]);
  console.log("ok duplicated GlobalIds keep the hidden column through a selective update");

  // A patch of instanced products carries their family once, like the initial stream.
  source = pavilionIfc({ family: true });
  await page.setInputFiles("#file-input", { name: "pavilion.ifc", mimeType: "application/octet-stream", buffer: Buffer.from(source) });
  await page.waitForFunction(() => window.__tessifc?.ready() && window.__tessifc.state.model.revision === "0" &&
    window.__tessifc.state.model.info.products.IfcFurnishingElement === 4 && window.__tessifc.loadStatus().finished, null, { timeout: 60000 });
  const seatA = product(source, "Mapped seat A");
  const seatB = product(source, "Mapped seat B");
  const familyProfile = [...records(source)].find(([, value]) => value.startsWith("IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.6,0.6)"))[0];
  await update(replaceRecord(source, familyProfile, (record) => record.replace("0.6,0.6", "0.8,0.6")), [seatA.id, seatB.id]);
  const familyPack = await page.evaluate(() => {
    const pack = window.__tessifc.pack();
    const live = new Set();
    for (let record = 0; record < pack.instances.count; record += 1) if (pack.instances.active[record]) live.add(pack.instances.geometryIds[record]);
    return { live: live.size, records: pack.instances.count };
  });
  const fresh = (() => {
    const kernel = new Kernel();
    try {
      const model = kernel.openModel(Buffer.from(source));
      kernel.evaluateGeometry(model, settings);
      return readIgp(kernel.getPack(model)).geometry.length;
    } finally { kernel.free(); }
  })();
  assert.equal(familyPack.live, fresh, "the family mesh is shared inside the patch");
  assert.deepEqual(problems, []);
  console.log("ok a patch keeps shared families");
} finally {
  if (browser) await browser.close();
  await server.close();
}
