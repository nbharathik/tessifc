// SPDX-License-Identifier: Apache-2.0

import assert from "node:assert/strict";
import { createPackAssembler, MAX_INSTANCE_SLOTS } from "../src/stream.js";

const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
const mesh = (id, height = 1) => ({
  id,
  bbox: [0, 0, 0, 1, height, 0],
  positions: Float32Array.from([0, 0, 0, 1, 0, 0, 0, height, 0]),
  indices: Uint16Array.from([0, 1, 2]),
});
const chunk = (geometry, records) => ({
  geometry,
  index: { classes: ["IfcWall"], model_offset: [0, 0, 0] },
  instances: {
    count: records.length,
    geometryIds: Uint32Array.from(records.map(([geometryId]) => geometryId)),
    expressIds: Uint32Array.from(records.map(([, expressId]) => expressId)),
    classIds: new Uint16Array(records.length),
    transforms: Float32Array.from(records.flatMap(() => identity)),
    colors: Uint8Array.from(records.flatMap(() => [200, 200, 200, 255])),
    flags: new Uint16Array(records.length),
  },
  flags: 0,
  bytes: 100,
});

function checkMemory(pack) {
  const geometryBytes = pack.geometry.reduce((total, item) => total + item.positions.byteLength + item.indices.byteLength, 0);
  const normalsBytes = pack.geometry.reduce((total, item) => total + item.positions.length * 4, 0);
  const instanceBytes = Object.values(pack.instances).reduce((total, column) => total + (column.byteLength ?? 0), 0);
  assert.deepEqual(pack.memory, {
    geometryBytes,
    instanceBytes,
    gpuBytes: geometryBytes + normalsBytes + pack.instances.count * (16 * 4 + 3 * 4),
  });
}

const assembler = createPackAssembler();
checkMemory(assembler.pack());
const initial = mesh(0);
assembler.append(chunk([initial], [[0, 10]]));
const before = assembler.pack();
checkMemory(before);
assembler.append(chunk([initial, mesh(1)], [[1, 11], [0, 12]]));
assert.equal(assembler.geometryCount, 2, "repeated geometry does not increase memory accounting");
checkMemory(assembler.pack());
assert.equal(before.geometry.length, 1, "a prior pack keeps its mesh list");
checkMemory(before);

const firstPatch = assembler.replaceProducts([11], chunk([mesh(2, 2)], [[2, 11]]));
assert.deepEqual(firstPatch.removedRecords, [1]);
assert.deepEqual(firstPatch.appended, { from: 3, to: 4 });
assert.deepEqual([...assembler.pack().instances.active], [1, 0, 1, 1]);
assert.equal(assembler.pack().instances.activeCount, 3);
assert.equal(assembler.pack().instances.expressIds[2], 12, "an unrelated slot never moves");
assert.equal(assembler.geometryCount, 3, "source geometry stays available to future stream chunks");
checkMemory(assembler.pack());
assembler.replaceProducts([11], chunk([mesh(3, 3)], [[3, 11]]));
assert.equal(assembler.geometryCount, 3, "superseded patch geometry is released");
checkMemory(assembler.pack());
assembler.replaceProducts([11], chunk([], []));
assert.equal(assembler.geometryCount, 2, "deleting patched geometry releases its memory");
checkMemory(assembler.pack());

const resized = new Float32Array(18);
resized.set(initial.positions);
assembler.append(chunk([{ id: 4, positions: resized, indices: new Uint32Array([0, 1, 2]), bbox: initial.bbox }], [[4, 13]]));
checkMemory(assembler.pack());
assert.deepEqual([...assembler.pack().instances.expressIds], [10, 11, 12, 11, 11, 13]);
assert.deepEqual([...assembler.pack().instances.active], [1, 0, 1, 0, 0, 1]);
assert.equal(assembler.pack().instances.activeCount, 3);
console.log("ok    streamed memory totals follow deduplication, patch replacement, and deletion");

{
  // Coarse levels: added over a base the assembler holds, counted by their indices, gone with the base.
  const levelled = createPackAssembler();
  const big = mesh(7);
  const whole = chunk([big], [[7, 70]]);
  whole.stream = { final: true };
  levelled.append(whole);
  const ids = levelled.addLodLevels([{ of: 7, level: 1, indices: Uint16Array.from([0, 1, 2]) }]);
  assert.equal(ids.length, 1);
  assert.equal(levelled.geometryCount, 2);
  const level = levelled.pack().geometry.find((item) => item.id === ids[0]);
  assert.deepEqual(level.lod, { of: 7, level: 1 });
  assert.equal(level.positions, big.positions, "a level views the base's positions");
  assert.equal(levelled.pack().memory.geometryBytes, big.positions.byteLength + big.indices.byteLength + 6);
  assert.equal(levelled.nextGeometryId(), ids[0] + 1, "a level takes a fresh id above every issued one");
  assert.deepEqual(levelled.addLodLevels([{ of: 7, level: 1, indices: Uint16Array.from([0, 1, 2]) }]), [], "the same level is not added twice");
  const fresh = levelled.nextGeometryId();
  levelled.replaceProducts([70], chunk([mesh(fresh, 2)], [[fresh, 70]]));
  assert.deepEqual(levelled.pack().geometry.map((item) => item.id), [fresh], "the base's level is pruned with the base");
  checkMemory(levelled.pack());
  console.log("ok    coarse levels are added over their base and pruned with it");
}

const selective = createPackAssembler();
const source = chunk([mesh(0), mesh(1)], [[0, 10], [1, 11]]);
source.stream = { final: true };
source.index.diagnostics = [{ id: 10, code: "W_OLD" }, { id: 11, code: "W_UNRELATED" }];
selective.append(source);
const mixed = chunk([mesh(2, 2), mesh(3)], [[2, 10], [3, 11]]);
mixed.index.diagnostics = [{ id: 10, code: "W_NEW" }, { id: 11, code: "W_UNRELATED" }];
const replacement = selective.replaceProducts([10, 11], mixed);
assert.deepEqual(replacement.changedProducts, [10], "a conservative affected set still retains identical products");
assert.deepEqual(replacement.removedRecords, [0]);
assert.deepEqual([...selective.pack().instances.expressIds], [10, 11, 10]);
assert.deepEqual(selective.pack().geometry.map((item) => item.id), [1, 2], "retired source meshes are released after streaming finishes");
assert.deepEqual(selective.pack().index.diagnostics, [{ id: 10, code: "W_NEW" }, { id: 11, code: "W_UNRELATED" }]);

const metadata = chunk([mesh(4)], [[4, 11]]);
metadata.index.classes = ["IfcDoor"];
metadata.instances.flags[0] = 4;
metadata.index.provenance = [{ rep: 99, item: 100, evaluator: "test" }];
metadata.instances.provenance = Uint32Array.of(0);
assert.equal(selective.replaceProducts([11], metadata).changed, true, "class, helper flags and provenance cannot be suppressed by equal triangles");
const current = selective.pack();
const slot = current.instances.count - 1;
assert.equal(current.index.classes[current.instances.classIds[slot]], "IfcDoor");
assert.equal(current.instances.flags[slot], 4);
assert.equal(current.index.provenance[current.instances.provenance[slot]].rep, 99);
const closed = { ...metadata, geometry: [{ ...mesh(5), closed: true }] };
closed.instances = { ...metadata.instances, geometryIds: Uint32Array.of(5) };
assert.equal(selective.replaceProducts([11], closed).changed, true, "closedness changes section behavior even with equal triangles");
const same = { ...closed, geometry: [{ ...mesh(6), closed: true }], instances: { ...closed.instances, geometryIds: Uint32Array.of(6) } };
const sameResult = selective.replaceProducts([11], same);
assert.equal(sameResult.changed, false);
assert.equal(sameResult.removedRecords.length, 0);
const slotsBeforeInvalid = selective.pack().instances.count;
assert.throws(() => selective.replaceProducts([11], chunk([mesh(5, 9)], [[5, 11]])), /reuses geometry ID/);
assert.throws(() => selective.replaceProducts([11], chunk([], [[900, 11]])), /missing geometry/);
assert.throws(() => selective.replaceProducts([11], chunk([mesh(10)], [[10, 99]])), /unrequested/);
assert.throws(() => selective.replaceProducts([11], { ...same, index: { ...same.index, model_offset: [1, 0, 0] } }), /model offset/);
assert.equal(selective.pack().instances.count, slotsBeforeInvalid, "invalid patches do not consume slots or retire live products");
assert.equal(selective.pack().instances.activeCount, 2);
checkMemory(selective.pack());
const tooMany = { ...chunk([], []), instances: { count: MAX_INSTANCE_SLOTS + 1 } };
assert.throws(() => selective.append(tooMany), /stable-slot limit/);
assert.equal(selective.pack().instances.count, slotsBeforeInvalid);
console.log("ok    stable slots, per-product equality, metadata, diagnostics and patch validation");
