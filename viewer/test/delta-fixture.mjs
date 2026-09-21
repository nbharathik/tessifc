// SPDX-License-Identifier: Apache-2.0

export const deltaIdentity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];

export function deltaTriangle(id, x = 0, height = 1) {
  return {
    id, primitive: "triangles", closed: false, bbox: [x, 0, 0, x + 1, height, 0],
    positions: Float32Array.from([x, 0, 0, x + 1, 0, 0, x, height, 0]),
    indices: Uint16Array.of(0, 1, 2),
  };
}

export function deltaChunk(geometry, records) {
  return {
    geometry, index: { classes: ["IfcWall"], model_offset: [0, 0, 0] }, stream: { final: true },
    instances: {
      count: records.length,
      geometryIds: Uint32Array.from(records.map((record) => record.geometry)),
      expressIds: Uint32Array.from(records.map((record) => record.id)),
      classIds: new Uint16Array(records.length),
      transforms: Float32Array.from(records.flatMap((record) => record.transform ?? deltaIdentity)),
      colors: Uint8Array.from(records.flatMap((record) => record.color ?? [200, 80, 80, 255])),
      flags: new Uint16Array(records.length),
    },
    flags: 0, bytes: 100,
  };
}
