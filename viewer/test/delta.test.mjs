// SPDX-License-Identifier: Apache-2.0

import assert from "node:assert/strict";
import { IfcRenderer, planRenderBatches, planDepthRanks, displayColors } from "../src/renderer.js";
import { createPackAssembler } from "../src/stream.js";
import { deltaChunk, deltaTriangle, deltaIdentity } from "./delta-fixture.mjs";

// A resource-owning GL double exercises the actual buffer/batch lifecycle.
// Browser pixel and real-context checks live in delta-render.mjs.
let serial = 0, uploads = 0;
const deleted = new Set();
const gl = new Proxy({
  UNSIGNED_INT: 0x1405, UNSIGNED_SHORT: 0x1403,
  createBuffer: () => ({ buffer: ++serial }), createVertexArray: () => ({ vao: ++serial }),
  createTexture: () => ({ texture: ++serial }), getParameter: () => 4096,
  bufferData: () => { uploads++; }, deleteBuffer: (buffer) => deleted.add(buffer),
}, { get: (target, key) => key in target ? target[key] : /^[A-Z_0-9]+$/.test(key) ? key : () => {} });
const assembler = createPackAssembler();
const renderer = Object.assign(Object.create(IfcRenderer.prototype), {
  gl, pack: assembler.pack(), contextLost: false, renderOrigin: [10, 20, 30],
  camera: { position: [1, 2, 3], target: [4, 5, 6], mode: "perspective" },
  preparedByGeometry: new WeakMap(), batches: [], opaqueBatches: [], transparentBatches: [],
  contestedOpaqueBatches: [], sharedGeometryBuffers: [], selected: [], recordLocations: [], pickCandidates: [],
  renderColors: new Uint8Array(0), depthRanks: new Uint32Array(0), depthContested: new Uint8Array(0),
  visibility: new Uint8Array(0), baseVisible: new Uint8Array(0), visibilityCapacity: 0, visibilityTexture: null,
  visibilityVersion: 0, selectionVersion: 0, minimumAlpha: 0, minimumLuminance: 0,
  suppressCoplanar: false, depthTieBreak: true, gpuBufferBytes: 0,
  section: { active: false, world: 0, axis: 2, sign: 1 },
  cancelWirePreparation() {}, scheduleWirePreparation() {}, currentGpuBytes() { return this.gpuBufferBytes; },
});
const camera = JSON.stringify(renderer.camera), origin = renderer.renderOrigin.slice();
const first = assembler.append(deltaChunk([deltaTriangle(0), deltaTriangle(1, 20)], [
  { geometry: 0, id: 10 }, { geometry: 1, id: 11, color: [80, 80, 200, 255] },
]));
renderer.applyDelta(assembler.pack(), { changed: true, appended: first, removedRecords: [] });
assert.equal(renderer.batches.length, 2);
const untouched = renderer.batches.find((batch) => batch.records.includes(1));
const untouchedBuffers = untouched.buffers.slice();
renderer.select([0, 1]);
const patch = assembler.replaceProducts([10], deltaChunk([deltaTriangle(2, 5, 2)], [{ geometry: 2, id: 10 }]));
const result = renderer.applyDelta(assembler.pack(), patch);
assert.equal(result.patchStats.reusedBatches, 1);
assert.equal(result.patchStats.rebuiltBatches, 1);
assert.ok(renderer.batches.includes(untouched), "unrelated batch object survives");
assert.ok(untouchedBuffers.every((buffer) => !deleted.has(buffer)), "unrelated GPU buffers survive");
assert.deepEqual(renderer.selected, [1, 2], "selection follows surviving products into their fresh slots");
assert.equal(JSON.stringify(renderer.camera), camera);
assert.deepEqual(renderer.renderOrigin, origin);
assert.equal(renderer.recordLocations[0], null);
const ray = (x) => ({ origin: [x - origin[0], .2 - origin[1], 2 - origin[2]], direction: [0, 0, -1] });
assert.equal(renderer.pickRecord(ray(.2)), null, "superseded geometry cannot be picked");
assert.equal(renderer.pickRecord(ray(5.2))?.record, 2, "the replacement is pickable at its new position");
assert.equal(renderer.pickRecord(ray(20.2))?.record, 1, "unrelated geometry remains pickable");
const beforeNoop = uploads;
const noChange = assembler.replaceProducts([10], deltaChunk([deltaTriangle(3, 5, 2)], [{ geometry: 3, id: 10 }]));
assert.equal(noChange.changed, false);
renderer.applyDelta(assembler.pack(), noChange);
assert.equal(uploads, beforeNoop, "no-op changes upload no geometry");
const deletion = assembler.replaceProducts([10], deltaChunk([], []));
renderer.applyDelta(assembler.pack(), deletion, () => true);
renderer.setVisibility(() => true);
assert.equal(renderer.pickRecord(ray(5.2)), null, "show-all cannot resurrect a deleted product");
assert.deepEqual(renderer.selected, [1]);
assert.equal(renderer.batches.length, 1);
assert.equal(renderer.batches[0], untouched);
assert.equal(renderer.sharedGeometryBuffers.length, 0);
assert.equal(planRenderBatches(assembler.pack()).drawCalls, 1, "load and context restore batch only active slots");
assert.ok(result.patchStats.uploadedBytes > 0);
console.log("ok    selective GPU resource reuse, exact no-op, stable picking, deletion, selection and coordinate frame");

// A changed product can share a bounded baked batch. Rebuild that batch while
// retaining its unedited record, and avoid leaving tombstoned triangles behind.
const addition = assembler.replaceProducts([12, 13], deltaChunk([deltaTriangle(4, 40), deltaTriangle(5, 42)], [
  { geometry: 4, id: 12 }, { geometry: 5, id: 13 },
]));
renderer.applyDelta(assembler.pack(), addition);
const owning = renderer.batches.find((batch) => batch.records.includes(3));
assert.ok(owning.records.includes(4));
const removal = assembler.replaceProducts([12], deltaChunk([], []));
renderer.applyDelta(assembler.pack(), removal);
assert.ok(owning.buffers.every((buffer) => deleted.has(buffer)), "the retired bounded batch releases its owned buffers");
assert.ok(renderer.batches.some((batch) => batch.records.includes(4)));
assert.ok(renderer.batches.every((batch) => !batch.records.includes(3)));
assert.ok(renderer.batches.includes(untouched));
assert.equal(renderer.pickRecord(ray(42.2))?.record, 4);
console.log("ok    bounded shared-batch replacement preserves unrelated slots and releases obsolete buffers");

const sharedMesh = deltaTriangle(6);
const widePositions = new Float32Array(2048 * 3);
widePositions.set(sharedMesh.positions);
sharedMesh.positions = widePositions;
const translation = (x) => { const matrix = deltaIdentity.slice(); matrix[12] = x; return matrix; };
const sharedAddition = assembler.replaceProducts([100, 101], deltaChunk([sharedMesh], [
  { geometry: 6, id: 100, transform: translation(100), color: [100, 90, 80, 255] },
  { geometry: 6, id: 101, transform: translation(200), color: [120, 90, 80, 255] },
]));
renderer.applyDelta(assembler.pack(), sharedAddition);
const sharedBatches = renderer.batches.filter((batch) => batch.sharedGeometry?.geometry.id === 6);
assert.equal(sharedBatches.length, 2);
assert.equal(sharedBatches[0].sharedGeometry, sharedBatches[1].sharedGeometry);
renderer.ensureWireBuffer(sharedBatches[0]);
const sharedOwner = sharedBatches[0].sharedGeometry;
const sharedSurvivor = sharedBatches.find((batch) => batch.records.includes(sharedAddition.to - 1));
const sharedEdit = assembler.replaceProducts([100], deltaChunk([deltaTriangle(7, 110)], [{ geometry: 7, id: 100 }]));
renderer.applyDelta(assembler.pack(), sharedEdit);
assert.ok(renderer.batches.includes(sharedSurvivor));
assert.ok(sharedOwner.buffers.every((buffer) => !deleted.has(buffer)), "shared vertex/index/wire buffers survive another occurrence's edit");
const sharedRemoval = assembler.replaceProducts([101], deltaChunk([], []));
renderer.applyDelta(assembler.pack(), sharedRemoval);
assert.ok(sharedOwner.buffers.every((buffer) => deleted.has(buffer)), "the last shared occurrence releases all its buffers");
assert.ok(renderer.batches.includes(untouched));
const colors = displayColors(assembler.pack(), 0, 0);
const rankPlan = planDepthRanks(colors);
const activeColors = new Set();
for (let record = 0; record < assembler.pack().instances.count; record++) {
  if (assembler.pack().instances.active[record]) activeColors.add([...colors.subarray(record * 4, record * 4 + 4)].join(","));
}
assert.equal(rankPlan.colors, activeColors.size, "retired styles cannot accumulate depth priorities");
const beforeLimit = renderer.batches.slice();
assert.throws(() => renderer.applyDelta({ ...assembler.pack(), instances: { ...assembler.pack().instances, count: 2 ** 24 + 1 } }, { changed: true }), /stable IFC slots/);
assert.deepEqual(renderer.batches, beforeLimit, "slot-limit rejection occurs before retiring any GPU resource");
console.log("ok    instanced geometry and wire reference lifetimes, active depth ranks, and early slot limits");
