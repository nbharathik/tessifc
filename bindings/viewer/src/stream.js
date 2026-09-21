// SPDX-License-Identifier: Apache-2.0

//! Assembles streamed IGP chunks into one pack. Geometry ids are global and a
//! mesh is written once; class ids are chunk-local. The assembler keeps one
//! growing set of columns and hands out a pack-shaped view of them.

/** @type {Record<string, [any, number]>} */
const COLUMNS = {
  geometryIds: [Uint32Array, 1],
  expressIds: [Uint32Array, 1],
  classIds: [Uint16Array, 1],
  transforms: [Float32Array, 16],
  colors: [Uint8Array, 4],
  flags: [Uint16Array, 1],
  provenance: [Uint32Array, 1],
  material: [Uint32Array, 1],
  active: [Uint8Array, 1],
};

const NO_MATERIAL = 0xffffffff;

const IDENTITY = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
// Slots are encoded in Float32 GPU attributes. Retired slots are not recycled:
// column storage grows with total revision parts until an explicit reopen.
export const MAX_INSTANCE_SLOTS = 2 ** 24;

/**
 * The assembled scene: a pack whose instance table also carries `active` and
 * `activeCount`, since retired revision slots stay in the columns.
 * @typedef {import("@tessifc/edit/types").Pack & { instances: { activeCount: number, active: Uint8Array }, chunks: number, sharedRecords: number }} AssembledPack
 */

/** @typedef {ReturnType<typeof createPackAssembler>} PackAssembler */

/** A growing pack; `pack()` views point into assembler storage and last until the next append. */
export function createPackAssembler() {
  let geometry = [];
  const geometryIds = new Set();
  const patchMeshes = new Set();
  let maxGeometryId = -1;
  let geometryBytes = 0;
  let normalsBytes = 0;
  let appendingPatch = false;
  const classes = [];
  const classIndex = new Map();
  // Provenance rows are chunk-local, exactly like classes, so they are merged
  // and every chunk's column is remapped onto the merged table.
  const provenance = [];
  const provenanceIndex = new Map();
  // Materials are chunk-local like classes; textures are global by id and
  // arrive once, so they are kept by id.
  const materials = [];
  const materialIndex = new Map();
  const textures = new Map();
  let diagnostics = [];
  let columns = allocate(1024);
  let count = 0;
  let bytes = 0;
  let index = null;
  let stats = {};
  let headerFlags = 0;
  let chunks = 0;
  let sharedRecords = 0;
  let activeCount = 0;
  let streamFinished = false;

  function allocate(capacity) {
    const out = { capacity };
    for (const [name, [Type, width]] of Object.entries(COLUMNS)) out[name] = new Type(capacity * width);
    return out;
  }

  function ensureCapacity(needed) {
    if (needed > MAX_INSTANCE_SLOTS) throw new Error("The scene reached its stable-slot limit; reopen a saved revision to reclaim retired slots.");
    if (needed <= columns.capacity) return;
    let capacity = columns.capacity;
    while (capacity < needed) capacity *= 2;
    const grown = allocate(capacity);
    for (const name of Object.keys(COLUMNS)) grown[name].set(columns[name].subarray(0, count * COLUMNS[name][1]));
    columns = grown;
  }

  function classId(name) {
    let id = classIndex.get(name);
    if (id === undefined) {
      id = classes.length;
      classes.push(name);
      classIndex.set(name, id);
    }
    return id;
  }

  function provenanceId(row) {
    const key = JSON.stringify(row ?? null);
    let id = provenanceIndex.get(key);
    if (id === undefined) {
      id = provenance.length;
      provenance.push(row);
      provenanceIndex.set(key, id);
    }
    return id;
  }

  function materialId(row) {
    const key = JSON.stringify(row ?? null);
    let id = materialIndex.get(key);
    if (id === undefined) {
      id = materials.length;
      materials.push(row);
      materialIndex.set(key, id);
    }
    return id;
  }

  /** Add one chunk. Returns the record range it occupies. */
  function append(chunk) {
    ensureCapacity(count + chunk.instances.count);
    if (!index) {
      // Model-wide index fields come from the first chunk; later ones repeat them.
      index = {
        ...chunk.index,
        classes: undefined,
        provenance: undefined,
        materials: undefined,
        textures: undefined,
        diagnostics: undefined,
        stats: undefined,
        stream: undefined,
      };
    }
    for (const mesh of chunk.geometry) {
      if (geometryIds.has(mesh.id)) continue;
      geometryIds.add(mesh.id);
      geometry.push(mesh);
      geometryBytes += meshBytes(mesh);
      normalsBytes += mesh.lod ? 0 : mesh.positions.length * 4;
      if (appendingPatch) patchMeshes.add(mesh.id);
      if (Number.isFinite(mesh.id) && mesh.id > maxGeometryId) maxGeometryId = mesh.id;
    }
    const remap = chunk.index.classes.map((name) => classId(String(name)));
    const provenanceRemap = (chunk.index.provenance ?? []).map((row) => provenanceId(row));
    const materialRemap = (chunk.index.materials ?? []).map((row) => materialId(row));
    for (const texture of chunk.index.textures ?? []) if (!textures.has(texture.id)) textures.set(texture.id, texture);
    const added = chunk.instances.count;
    for (const [name, [, width]] of Object.entries(COLUMNS)) {
      // An optional column is absent from a pack written before it existed.
      const source = chunk.instances[name];
      if (!source) continue;
      columns[name].set(source.subarray(0, added * width), count * width);
    }
    for (let record = 0; record < added; record += 1) {
      columns.active[count + record] = chunk.instances.active?.[record] ?? 1;
      if (columns.active[count + record]) activeCount += 1;
      const local = chunk.instances.classIds[record];
      columns.classIds[count + record] = remap[local] ?? classId("IfcUnknown");
      columns.provenance[count + record] = 0xffffffff;
      if (chunk.instances.provenance) {
        const row = chunk.instances.provenance[record];
        columns.provenance[count + record] = provenanceRemap[row] ?? 0xffffffff;
      }
      columns.material[count + record] = NO_MATERIAL;
      if (chunk.instances.material) {
        const row = chunk.instances.material[record];
        columns.material[count + record] = row === NO_MATERIAL ? NO_MATERIAL : materialRemap[row] ?? NO_MATERIAL;
      }
      if (columns.active[count + record] && !isIdentity(chunk.instances.transforms, record * 16)) sharedRecords += 1;
    }
    for (const item of chunk.index.diagnostics ?? []) diagnostics.push(item);
    if (chunk.index.stats && !appendingPatch) stats = { ...stats, ...chunk.index.stats };
    headerFlags = chunk.flags;
    const from = count;
    count += added;
    bytes += chunk.bytes;
    chunks += 1;
    if (!appendingPatch && (chunk.stream?.final || chunk.index.stream?.final)) streamFinished = true;
    return { from, to: count };
  }

  /** Replace every record of these products with those of `chunk`; reports whether drawn geometry changed. */
  function replaceProducts(expressIds, chunk) {
    const requested = new Set(expressIds);
    const oldMeshes = new Map(geometry.map((mesh) => [mesh.id, mesh]));
    const newMeshes = new Map(chunk.geometry.map((mesh) => [mesh.id, mesh]));
    const offset = index?.model_offset ?? [0, 0, 0];
    if (!equalArray(offset, chunk.index.model_offset ?? [0, 0, 0])) {
      throw new Error("A product patch must use the existing model offset.");
    }
    const before = new Map(), after = new Map();
    for (let record = 0; record < count; record++) {
      if (!columns.active[record] || !requested.has(columns.expressIds[record])) continue;
      const id = columns.expressIds[record];
      if (!before.has(id)) before.set(id, []);
      before.get(id).push(record);
    }
    for (let record = 0; record < chunk.instances.count; record++) {
      const id = chunk.instances.expressIds[record];
      if (!requested.has(id)) throw new Error(`Product patch contains unrequested IFC entity #${id}.`);
      if (chunk.instances.active?.[record] === 0) continue;
      const geometryId = chunk.instances.geometryIds[record];
      if (!newMeshes.has(geometryId) && !oldMeshes.has(geometryId)) throw new Error("Product patch references missing geometry.");
      if (!after.has(id)) after.set(id, []);
      after.get(id).push(record);
    }
    // Match complete part sets, including render metadata. Hashes only select
    // candidates; exact equality confirms reuse, so collisions cannot hide edits.
    const previous = { instances: columns, index: { classes, provenance } };
    const changedProducts = [...requested].filter((id) => !sameParts(
      previous, before.get(id) ?? [], oldMeshes,
      chunk, after.get(id) ?? [], new Map([...oldMeshes, ...newMeshes]),
    ));
    const changed = new Set(changedProducts);
    const empty = { from: count, to: count };
    const nextDiagnostics = [...new Map([
      ...diagnostics.filter((item) => !requested.has(item.expressId ?? item.express_id ?? item.id)),
      ...(chunk.index.diagnostics ?? []),
    ].map((item) => [JSON.stringify(item), item])).values()];
    const diagnosticsChanged = JSON.stringify(diagnostics) !== JSON.stringify(nextDiagnostics);
    if (!changed.size) {
      if (diagnosticsChanged) diagnostics = nextDiagnostics;
      return { ...empty, appended: empty, removed: 0, removedRecords: [], changedProducts, changed: diagnosticsChanged, metadataOnly: diagnosticsChanged };
    }

    const records = [];
    for (let record = 0; record < chunk.instances.count; record++) {
      if (changed.has(chunk.instances.expressIds[record]) && chunk.instances.active?.[record] !== 0) records.push(record);
    }
    const replacement = selectRecords(chunk, records);
    replacement.index = { ...replacement.index, diagnostics: [] };
    for (const mesh of replacement.geometry) {
      if (oldMeshes.has(mesh.id) && !sameMesh(oldMeshes.get(mesh.id), mesh)) {
        throw new Error(`Product patch reuses geometry ID ${mesh.id} for different geometry.`);
      }
    }
    // Allocate before deactivating anything. Existing rows are never compacted
    // or reused: GPU IDs and pending picks continue to identify the same slot.
    ensureCapacity(count + records.length);
    diagnostics = nextDiagnostics;
    const removedRecords = [];
    for (const id of changedProducts) for (const record of before.get(id) ?? []) {
      columns.active[record] = 0;
      activeCount -= 1;
      if (!isIdentity(columns.transforms, record * 16)) sharedRecords -= 1;
      removedRecords.push(record);
    }
    appendingPatch = true;
    let range;
    try {
      range = append(replacement);
    } finally {
      appendingPatch = false;
    }
    prunePatchMeshes();
    const meshes = new Map(geometry.map((mesh) => [mesh.id, mesh]));
    const products = new Set();
    let triangles = 0;
    for (let record = 0; record < count; record++) if (columns.active[record]) {
      products.add(columns.expressIds[record]);
      triangles += (meshes.get(columns.geometryIds[record])?.indices.length ?? 0) / 3;
    }
    stats = { ...stats, products: products.size, triangles };
    return { ...range, appended: range, removed: removedRecords.length, removedRecords, changedProducts, changed: true };
  }

  /** Retire unused meshes; initial-stream geometry stays until the final chunk arrives. */
  function prunePatchMeshes() {
    if (!patchMeshes.size && !streamFinished) return;
    const live = new Set();
    for (let record = 0; record < count; record++) if (columns.active[record]) live.add(columns.geometryIds[record]);
    const dead = new Set();
    for (const mesh of geometry) if (!mesh.lod && !live.has(mesh.id) && (streamFinished || patchMeshes.has(mesh.id))) dead.add(mesh.id);
    // A coarse level lives exactly as long as its base.
    for (const mesh of geometry) if (mesh.lod && (dead.has(mesh.lod.of) || !geometryIds.has(mesh.lod.of))) dead.add(mesh.id);
    if (!dead.size) return;
    for (const id of dead) {
      patchMeshes.delete(id);
      geometryIds.delete(id);
    }
    geometry = geometry.filter((mesh) => {
      if (!dead.has(mesh.id)) return true;
      geometryBytes -= meshBytes(mesh);
      normalsBytes -= mesh.lod ? 0 : mesh.positions.length * 4;
      return false;
    });
  }

  /**
   * Add coarse levels computed for meshes of this pack, each `{ of, level,
   * indices }` over the base's positions; returns the ids given to them. A
   * level of a base the pack no longer holds, or that it already has, is skipped.
   * @param {Array<{ of: number, level: number, indices: Uint16Array | Uint32Array }>} levels
   * @returns {number[]}
   */
  function addLodLevels(levels) {
    const byId = new Map(geometry.map((mesh) => [mesh.id, mesh]));
    const ids = [];
    for (const level of levels) {
      const base = byId.get(level.of);
      if (!base || base.lod || !(level.level === 1 || level.level === 2)) continue;
      if (geometry.some((mesh) => mesh.lod && mesh.lod.of === level.of && mesh.lod.level === level.level)) continue;
      const indices = level.indices;
      const vertices = base.positions.length / 3;
      if (!indices || indices.length % 3 !== 0 || !indices.length) continue;
      let valid = true;
      for (let at = 0; at < indices.length; at += 1) if (indices[at] >= vertices) { valid = false; break; }
      if (!valid) continue;
      const id = maxGeometryId + 1;
      maxGeometryId = id;
      const mesh = { id, primitive: base.primitive ?? "triangles", bbox: base.bbox, closed: base.closed ?? null, positions: base.positions,
        indices, uv: base.uv ?? null, lod: { of: level.of, level: level.level } };
      geometry.push(mesh);
      geometryIds.add(id);
      byId.set(id, mesh);
      geometryBytes += meshBytes(mesh);
      ids.push(id);
    }
    return ids;
  }

  /** The pack so far, shaped exactly like `readIgp`'s result. */
  /** @returns {AssembledPack} */
  function pack() {
    /** @type {Record<string, any>} */
    const instances = { count, activeCount };
    for (const [name, [, width]] of Object.entries(COLUMNS)) instances[name] = columns[name].subarray(0, count * width);
    let instanceBytes = 0;
    for (const name of Object.keys(COLUMNS)) instanceBytes += instances[name].byteLength;
    return /** @type {AssembledPack} */ ({
      index: {
        ...(index ?? {}),
        classes: classes.slice(),
        provenance: provenance.slice(),
        materials: materials.slice(),
        textures: [...textures.values()],
        diagnostics: diagnostics.slice(),
        stats: { ...stats },
      },
      geometry: geometry.slice(),
      instances,
      flags: headerFlags,
      bytes,
      chunks,
      sharedRecords,
      memory: { geometryBytes, instanceBytes, gpuBytes: geometryBytes + normalsBytes + count * (16 * 4 + 3 * 4) },
    });
  }

  return {
    append,
    replaceProducts,
    pack,
    get count() {
      return count;
    },
    get geometryCount() {
      return geometry.length;
    },
    get chunks() {
      return chunks;
    },
    // Never lowered by pruning, so a patch id can never collide with one already issued.
    nextGeometryId: () => maxGeometryId + 1,
    addLodLevels,
  };
}

/** The bytes a mesh adds to the pack; a level shares its base's positions and uv. */
function meshBytes(mesh) {
  return mesh.indices.byteLength + (mesh.lod ? 0 : mesh.positions.byteLength + (mesh.uv?.byteLength ?? 0));
}

/** Whether a column-major 4x4 at `offset` is the identity. */
export function isIdentity(transforms, offset) {
  for (let index = 0; index < 16; index += 1) {
    if (transforms[offset + index] !== IDENTITY[index]) return false;
  }
  return true;
}

function equalArray(left, right) {
  if (left === right) return true;
  if (!left || !right || left.length !== right.length) return false;
  for (let i = 0; i < left.length; i++) if (!Object.is(left[i], right[i])) return false;
  return true;
}

function sameMesh(left, right) {
  return left === right || Boolean(left && right &&
    (left.primitive ?? "triangles") === (right.primitive ?? "triangles") &&
    (left.closed ?? null) === (right.closed ?? null) &&
    equalArray(left.bbox, right.bbox) && equalArray(left.positions, right.positions) && equalArray(left.indices, right.indices) &&
    equalArray(left.uv ?? null, right.uv ?? null));
}

function partMetadata(pack, record) {
  const { instances, index } = pack;
  return JSON.stringify([
    index.classes[instances.classIds[record]], instances.flags?.[record] ?? 0,
    Array.from(instances.colors.subarray(record * 4, record * 4 + 4)),
    Array.from(instances.transforms.subarray(record * 16, record * 16 + 16)),
    index.provenance?.[instances.provenance?.[record]] ?? null,
    instances.material && instances.material[record] !== NO_MATERIAL ? index.materials?.[instances.material[record]] ?? null : null,
  ]);
}

function sameParts(left, leftRecords, leftMeshes, right, rightRecords, rightMeshes) {
  if (leftRecords.length !== rightRecords.length) return false;
  const candidates = new Map();
  const keyOf = (pack, record, mesh) => partMetadata(pack, record) +
    (mesh ? `${hashTyped(mesh.positions)}:${hashTyped(mesh.indices)}` : "none");
  for (const record of leftRecords) {
    const mesh = leftMeshes.get(left.instances.geometryIds[record]);
    const key = keyOf(left, record, mesh);
    if (!candidates.has(key)) candidates.set(key, []);
    candidates.get(key).push(mesh);
  }
  for (const record of rightRecords) {
    const mesh = rightMeshes.get(right.instances.geometryIds[record]);
    const matches = candidates.get(keyOf(right, record, mesh));
    const match = matches?.findIndex((candidate) => sameMesh(candidate, mesh)) ?? -1;
    if (match < 0) return false;
    matches.splice(match, 1);
  }
  return true;
}

function selectRecords(chunk, records) {
  const instances = { count: records.length };
  for (const [name, [Type, width]] of Object.entries(COLUMNS)) {
    const source = chunk.instances[name];
    if (!source) continue;
    const target = new Type(records.length * width);
    for (let i = 0; i < records.length; i++) target.set(source.subarray(records[i] * width, (records[i] + 1) * width), i * width);
    instances[name] = target;
  }
  const used = new Set(instances.geometryIds);
  return { ...chunk, instances, geometry: chunk.geometry.filter((mesh) => used.has(mesh.id)) };
}

/** A cheap content hash of a typed array, for change detection only. */
function hashTyped(array) {
  const bytes = new Uint8Array(array.buffer, array.byteOffset, array.byteLength);
  let hash = 2166136261;
  for (let index = 0; index < bytes.length; index += 1) {
    hash ^= bytes[index];
    hash = Math.imul(hash, 16777619);
  }
  return `${bytes.length}:${hash >>> 0}`;
}
