// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { createRequire } from "node:module";
import { instrumentViewer, serveViewer } from "./harness.mjs";
import { pavilionIfc } from "./fixture.mjs";
import { readIgp } from "../src/igp.js";

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

function normalized(pack) {
  const meshes = new Map(pack.geometry.map((mesh) => [mesh.id, mesh]));
  const products = new Map();
  const { instances } = pack;
  for (let record = 0; record < instances.count; record += 1) {
    if (instances.active && !instances.active[record]) continue;
    const mesh = meshes.get(instances.geometryIds[record]);
    const matrix = instances.transforms.slice(record * 16, record * 16 + 16);
    const triangles = products.get(instances.expressIds[record]) ?? [];
    const color = Array.from(instances.colors.slice(record * 4, record * 4 + 4)).join(",");
    const point = (index) => {
      const [x, y, z] = mesh.positions.slice(index * 3, index * 3 + 3);
      return [0, 1, 2].map((axis) => Math.round((matrix[axis] * x + matrix[axis + 4] * y + matrix[axis + 8] * z + matrix[axis + 12]) * 10000)).join(",");
    };
    for (let i = 0; i < mesh.indices.length; i += 3) {
      triangles.push(`${color}:${[point(mesh.indices[i]), point(mesh.indices[i + 1]), point(mesh.indices[i + 2])].sort().join("/")}`);
    }
    products.set(instances.expressIds[record], triangles);
  }
  return [...products].sort(([a], [b]) => a - b).map(([id, triangles]) => [id, triangles.sort()]);
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
      return { geometry: pack.geometry.map((mesh) => ({ id: mesh.id, positions: Array.from(mesh.positions), indices: Array.from(mesh.indices) })),
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
  console.log("ok inspector saves use staged revisions, update properties and retain GPU batches");
} finally {
  if (browser) await browser.close();
  await server.close();
}
