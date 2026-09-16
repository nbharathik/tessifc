// SPDX-License-Identifier: Apache-2.0

//! Draw batches from a TessIFC kernel, with no three.js in sight: kernel in,
//! typed arrays out, so the batching can be tested. `index.js` wraps them.

/** How many vertices before a batch is closed and a new one started. */
export const BATCH_VERTEX_LIMIT = 65_000 * 4;

/**
 * Build draw batches from an evaluated model, grouped by colour so one batch is one material.
 * @param kernel a TessIFC `Kernel` with `evaluateGeometry` already called
 * @param modelId the model id
 * @returns `{ batches, shapes, bounds }`
 */
export function buildBatches(kernel, modelId) {
  const count = kernel.shapeCount(modelId);
  const byMaterial = new Map();
  const groups = [];
  const shapes = [];
  const bounds = { min: [Infinity, Infinity, Infinity], max: [-Infinity, -Infinity, -Infinity] };

  for (let index = 0; index < count; index += 1) {
    const expressId = kernel.shapeExpressId(modelId, index);
    const ifcClass = kernel.shapeClass(modelId, index);
    // A product has one or more coloured parts; merging them would recolour the glass.
    const partCount = kernel.shapePartCount?.(modelId, index) ?? 1;
    let shapeTriangles = 0;

    for (let part = 0; part < partCount; part += 1) {
      const positions = kernel.shapePositions(modelId, index, part);
      const indices = kernel.shapeIndices(modelId, index, part);
      if (!positions?.length || !indices?.length) continue;

      const color = kernel.shapeColor(modelId, index, part);
      const key = `${color[0]},${color[1]},${color[2]},${color[3]}`;
      for (const piece of splitPart(positions, indices)) {
        // Transparent parts stay separate so three.js can sort their depths.
        let group = color[3] === 255 ? byMaterial.get(key) : null;
        if (!group || group.vertexCount + piece.positions.length / 3 > BATCH_VERTEX_LIMIT) {
          group = { color: Array.from(color), parts: [], vertexCount: 0, indexCount: 0 };
          if (color[3] === 255) byMaterial.set(key, group);
          groups.push(group);
        }
        group.parts.push({ ...piece, expressId });
        group.vertexCount += piece.positions.length / 3;
        group.indexCount += piece.indices.length;
      }

      for (let i = 0; i < positions.length; i += 3) {
        for (let axis = 0; axis < 3; axis += 1) {
          const value = positions[i + axis];
          if (value < bounds.min[axis]) bounds.min[axis] = value;
          if (value > bounds.max[axis]) bounds.max[axis] = value;
        }
      }

      shapeTriangles += indices.length / 3;
    }

    if (shapeTriangles) shapes.push({ expressId, class: ifcClass, triangles: shapeTriangles });
  }

  const batches = groups.map((group) => {
    const positions = new Float32Array(group.vertexCount * 3);
    const indices = new (group.vertexCount > 65_535 ? Uint32Array : Uint16Array)(group.indexCount);
    const expressIds = new Uint32Array(group.vertexCount);
    let vertex = 0, triangleIndex = 0;
    for (const part of group.parts) {
      positions.set(part.positions, vertex * 3);
      expressIds.fill(part.expressId, vertex, vertex + part.positions.length / 3);
      for (const index of part.indices) indices[triangleIndex++] = index + vertex;
      vertex += part.positions.length / 3;
    }
    return { color: group.color, transparent: group.color[3] < 255, positions, indices, expressIds, vertexCount: group.vertexCount };
  });

  // Opaque first, so a renderer ignoring the flag still draws solids before glass.
  batches.sort((a, b) => Number(a.transparent) - Number(b.transparent));

  if (!Number.isFinite(bounds.min[0])) {
    bounds.min = [0, 0, 0];
    bounds.max = [0, 0, 0];
  }
  return { batches, shapes, bounds };
}

/** Split oversized parts at triangle boundaries without losing vertices or ids. */
function* splitPart(positions, indices) {
  if (positions.length / 3 <= BATCH_VERTEX_LIMIT) {
    yield { positions, indices };
    return;
  }
  let vertices = new Map(), triangleIndices = [];
  const finish = () => {
    const output = new Float32Array(vertices.size * 3);
    for (const [source, target] of vertices) output.set(positions.subarray(source * 3, source * 3 + 3), target * 3);
    return { positions: output, indices: Uint32Array.from(triangleIndices) };
  };
  for (let index = 0; index < indices.length; index += 3) {
    const triangle = indices.subarray(index, index + 3);
    const missing = new Set(Array.from(triangle).filter((vertex) => !vertices.has(vertex))).size;
    if (vertices.size + missing > BATCH_VERTEX_LIMIT) {
      yield finish();
      vertices = new Map();
      triangleIndices = [];
    }
    for (const vertex of triangle) {
      if (!vertices.has(vertex)) vertices.set(vertex, vertices.size);
      triangleIndices.push(vertices.get(vertex));
    }
  }
  if (triangleIndices.length) yield finish();
}

/** The centre and radius of a bounds, for framing a camera. */
export function frame(bounds) {
  const centre = [0, 1, 2].map((axis) => (bounds.min[axis] + bounds.max[axis]) / 2);
  const size = [0, 1, 2].map((axis) => bounds.max[axis] - bounds.min[axis]);
  const radius = Math.max(Math.hypot(...size) / 2, 1e-3);
  return { centre, size, radius };
}
