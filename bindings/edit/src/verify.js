// SPDX-License-Identifier: Apache-2.0

//! Proof that a chain of deltas produced the scene a fresh evaluation would:
//! a small scene mirror that applies deltas, a normalised per-product
//! triangle set, and the comparison against the exported file.

import { readIgp } from "./igp.js";

const QUANTUM = 10000;

/** Every active product's triangles in world space, quantised and sorted, keyed by express id. */
export function normalizeScene(pack) {
  const meshes = new Map(pack.geometry.map((mesh) => [mesh.id, mesh]));
  const products = new Map();
  const { instances } = pack;
  const offset = pack.index?.model_offset ?? [0, 0, 0];
  for (let record = 0; record < instances.count; record += 1) {
    if (instances.active && !instances.active[record]) continue;
    const mesh = meshes.get(instances.geometryIds[record]);
    if (!mesh) continue;
    const matrix = instances.transforms.slice(record * 16, record * 16 + 16);
    const triangles = products.get(instances.expressIds[record]) ?? [];
    const color = Array.from(instances.colors.slice(record * 4, record * 4 + 4)).join(",");
    const point = (index) => {
      const [x, y, z] = mesh.positions.slice(index * 3, index * 3 + 3);
      return [0, 1, 2].map((axis) => Math.round((matrix[axis] * x + matrix[axis + 4] * y + matrix[axis + 8] * z + matrix[axis + 12] + offset[axis]) * QUANTUM)).join(",");
    };
    for (let i = 0; i < mesh.indices.length; i += 3) {
      triangles.push(`${color}:${[point(mesh.indices[i]), point(mesh.indices[i + 1]), point(mesh.indices[i + 2])].sort().join("/")}`);
    }
    products.set(instances.expressIds[record], triangles);
  }
  return [...products].sort(([a], [b]) => a - b).map(([id, triangles]) => [id, triangles.sort()]);
}

/** A fresh whole-model evaluation of `bytes` in its own kernel; returns the parsed pack. */
export function evaluateScene(Kernel, bytes, settings = {}) {
  const kernel = new Kernel();
  try {
    const id = kernel.openModel(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
    kernel.evaluateGeometry(id, JSON.stringify(settings));
    const pack = readIgp(kernel.takePack(id));
    return pack;
  } finally {
    kernel.free();
  }
}

/**
 * A scene that follows deltas without a renderer: the records of every
 * product and the meshes they use. `pack()` returns a pack-shaped view for
 * `normalizeScene`.
 */
export function createSceneMirror(initial = null) {
  const records = new Map();
  const meshes = new Map();
  let offset = [0, 0, 0];

  function addPack(pack) {
    if (pack.index?.model_offset) offset = Array.from(pack.index.model_offset);
    for (const mesh of pack.geometry) meshes.set(mesh.id, mesh);
    const { instances } = pack;
    for (let record = 0; record < instances.count; record += 1) {
      if (instances.active && !instances.active[record]) continue;
      const id = instances.expressIds[record];
      const list = records.get(id) ?? [];
      list.push({
        geometryId: instances.geometryIds[record],
        transform: Array.from(instances.transforms.slice(record * 16, record * 16 + 16)),
        color: Array.from(instances.colors.slice(record * 4, record * 4 + 4)),
      });
      records.set(id, list);
    }
  }

  function removeProducts(ids) {
    for (const id of ids) records.delete(id);
  }

  function prune() {
    const used = new Set();
    for (const list of records.values()) for (const record of list) used.add(record.geometryId);
    for (const id of [...meshes.keys()]) if (!used.has(id)) meshes.delete(id);
  }

  function applyDelta(delta) {
    if (!delta?.pack) throw new Error("applyDelta needs the delta's parsed pack.");
    if (delta.kind === "full") {
      records.clear();
      meshes.clear();
    } else {
      removeProducts([...(delta.affectedProducts ?? []), ...(delta.removedProducts ?? [])]);
    }
    addPack(delta.pack);
    prune();
    return { products: records.size, meshes: meshes.size };
  }

  function pack() {
    const list = [...records.entries()].flatMap(([id, items]) => items.map((item) => ({ id, ...item })));
    const count = list.length;
    const expressIds = new Uint32Array(count);
    const geometryIds = new Uint32Array(count);
    const transforms = new Float32Array(count * 16);
    const colors = new Uint8Array(count * 4);
    list.forEach((item, record) => {
      expressIds[record] = item.id;
      geometryIds[record] = item.geometryId;
      transforms.set(item.transform, record * 16);
      colors.set(item.color, record * 4);
    });
    return { index: { model_offset: offset.slice() }, geometry: [...meshes.values()], instances: { count, expressIds, geometryIds, transforms, colors, active: null } };
  }

  if (initial) addPack(initial);
  return { applyDelta, pack, productIds: () => [...records.keys()], get products() { return records.size; }, get meshCount() { return meshes.size; } };
}

/**
 * Compare a mirror that followed the session's deltas with a fresh evaluation
 * of the session's exported file, product by product.
 */
export function verifyRevision({ Kernel, session, mirror, settings = null }) {
  const started = typeof performance !== "undefined" ? performance.now() : Date.now();
  const fresh = normalizeScene(evaluateScene(Kernel, session.export(), settings ?? session.settings()));
  const mirrored = normalizeScene(mirror.pack());
  const mismatches = [];
  const freshById = new Map(fresh);
  const mirroredById = new Map(mirrored);
  for (const [id, triangles] of freshById) {
    const seen = mirroredById.get(id);
    if (!seen) mismatches.push({ id, reason: "missing from the scene" });
    else if (seen.length !== triangles.length) mismatches.push({ id, reason: `${seen.length} triangles in the scene, ${triangles.length} fresh` });
    else if (seen.some((triangle, index) => triangle !== triangles[index])) mismatches.push({ id, reason: "different triangles" });
  }
  for (const id of mirroredById.keys()) if (!freshById.has(id)) mismatches.push({ id, reason: "not in a fresh evaluation" });
  const ended = typeof performance !== "undefined" ? performance.now() : Date.now();
  return { ok: mismatches.length === 0, revision: session.revision, products: fresh.length, mismatches, elapsedMs: ended - started };
}
