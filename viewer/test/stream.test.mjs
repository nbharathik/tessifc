// SPDX-License-Identifier: Apache-2.0

import assert from "node:assert/strict";
import { createPackAssembler } from "../src/stream.js";

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

assembler.replaceProducts([11], chunk([mesh(2, 2)], [[2, 11]]));
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
assert.deepEqual([...assembler.pack().instances.expressIds], [10, 12, 13]);
console.log("ok    streamed memory totals follow deduplication, patch replacement, and deletion");
