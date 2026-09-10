// SPDX-License-Identifier: Apache-2.0

//! Assembles streamed IGP chunks into one pack. Geometry ids are global and a
//! mesh is written once; class ids are chunk-local. The assembler keeps one
//! growing set of columns and hands out a pack-shaped view of them.

const COLUMNS = {
  geometryIds: [Uint32Array, 1],
  expressIds: [Uint32Array, 1],
  classIds: [Uint16Array, 1],
  transforms: [Float32Array, 16],
  colors: [Uint8Array, 4],
  flags: [Uint16Array, 1],
  provenance: [Uint32Array, 1],
};

const IDENTITY = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];

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
  const diagnostics = [];
  let columns = allocate(1024);
  let count = 0;
  let bytes = 0;
  let index = null;
  let stats = {};
  let headerFlags = 0;
  let chunks = 0;
  let sharedRecords = 0;

  function allocate(capacity) {
    const out = { capacity };
    for (const [name, [Type, width]] of Object.entries(COLUMNS)) out[name] = new Type(capacity * width);
    return out;
  }

  function ensureCapacity(needed) {
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
    const key = `${row?.rep}|${row?.item}|${row?.evaluator}|${row?.boolean}`;
    let id = provenanceIndex.get(key);
    if (id === undefined) {
      id = provenance.length;
      provenance.push(row);
      provenanceIndex.set(key, id);
    }
    return id;
  }

  /** Add one chunk. Returns the record range it occupies. */
  function append(chunk) {
    if (!index) {
      // Model-wide index fields come from the first chunk; later ones repeat them.
      index = {
        ...chunk.index,
        classes: undefined,
        provenance: undefined,
        diagnostics: undefined,
        stats: undefined,
        stream: undefined,
      };
    }
    for (const mesh of chunk.geometry) {
      if (geometryIds.has(mesh.id)) continue;
      geometryIds.add(mesh.id);
      geometry.push(mesh);
      geometryBytes += mesh.positions.byteLength + mesh.indices.byteLength;
      normalsBytes += mesh.positions.length * 4;
      if (appendingPatch) patchMeshes.add(mesh.id);
      if (Number.isFinite(mesh.id) && mesh.id > maxGeometryId) maxGeometryId = mesh.id;
    }
    const remap = chunk.index.classes.map((name) => classId(String(name)));
    const provenanceRemap = (chunk.index.provenance ?? []).map((row) => provenanceId(row));
    const added = chunk.instances.count;
    ensureCapacity(count + added);
    for (const [name, [, width]] of Object.entries(COLUMNS)) {
      // An optional column is absent from a pack written before it existed.
      const source = chunk.instances[name];
      if (!source) continue;
      columns[name].set(source.subarray(0, added * width), count * width);
    }
    for (let record = 0; record < added; record += 1) {
      const local = chunk.instances.classIds[record];
      columns.classIds[count + record] = remap[local] ?? classId("IfcUnknown");
      if (chunk.instances.provenance) {
        const row = chunk.instances.provenance[record];
        columns.provenance[count + record] = provenanceRemap[row] ?? 0;
      }
      if (!isIdentity(chunk.instances.transforms, record * 16)) sharedRecords += 1;
    }
    for (const item of chunk.index.diagnostics ?? []) diagnostics.push(item);
    if (chunk.index.stats) stats = { ...stats, ...chunk.index.stats };
    headerFlags = chunk.flags;
    const from = count;
    count += added;
    bytes += chunk.bytes;
    chunks += 1;
    return { from, to: count };
  }

  /** Replace every record of these products with those of `chunk`; reports whether drawn geometry changed. */
  function replaceProducts(expressIds, chunk) {
    const removing = new Set(expressIds);
    const before = signature(removing);
    // Unchanged records stay where they are, so nothing downstream has to be rebuilt.
    if (before === chunkSignature(chunk, removing)) return { from: count, to: count, removed: 0, changed: false };
    let write = 0;
    for (let record = 0; record < count; record += 1) {
      if (removing.has(columns.expressIds[record])) continue;
      if (write !== record) {
        for (const [name, [, width]] of Object.entries(COLUMNS)) {
          columns[name].copyWithin(write * width, record * width, (record + 1) * width);
        }
      }
      write += 1;
    }
    const removed = count - write;
    count = write;
    prunePatchMeshes();
    appendingPatch = true;
    let range;
    try {
      range = append(chunk);
    } finally {
      appendingPatch = false;
    }
    const after = signature(removing);
    return { ...range, removed, changed: before !== after };
  }

  /** Drop meshes an earlier patch left unreferenced. Streamed meshes stay: a later chunk may still use them. */
  function prunePatchMeshes() {
    if (!patchMeshes.size) return;
    const live = new Set(columns.geometryIds.subarray(0, count));
    const dead = new Set();
    for (const id of patchMeshes) if (!live.has(id)) dead.add(id);
    if (!dead.size) return;
    for (const id of dead) {
      patchMeshes.delete(id);
      geometryIds.delete(id);
    }
    geometry = geometry.filter((mesh) => {
      if (!dead.has(mesh.id)) return true;
      geometryBytes -= mesh.positions.byteLength + mesh.indices.byteLength;
      normalsBytes -= mesh.positions.length * 4;
      return false;
    });
  }

  /** The signature `chunk` would have for these products, so a no-op patch is recognised. */
  function chunkSignature(chunk, expressIds) {
    const byId = new Map(chunk.geometry.map((mesh) => [mesh.id, mesh]));
    const parts = [];
    const { instances } = chunk;
    for (let record = 0; record < instances.count; record += 1) {
      if (!expressIds.has(instances.expressIds[record])) continue;
      const mesh = byId.get(instances.geometryIds[record]);
      const at = record * 16;
      parts.push(
        `${instances.expressIds[record]}:${instances.colors.subarray(record * 4, record * 4 + 4).join(",")}:` +
          `${Array.from(instances.transforms.subarray(at, at + 16)).join(",")}:` +
          (mesh ? hashTyped(mesh.positions) + ":" + hashTyped(mesh.indices) : "none"),
      );
    }
    return parts.sort().join("|");
  }

  /** A string that changes when the drawn triangles or colours of these products change. */
  function signature(expressIds) {
    const byId = new Map(geometry.map((mesh) => [mesh.id, mesh]));
    const parts = [];
    for (let record = 0; record < count; record += 1) {
      if (!expressIds.has(columns.expressIds[record])) continue;
      const mesh = byId.get(columns.geometryIds[record]);
      const at = record * 16;
      parts.push(
        `${columns.expressIds[record]}:${columns.colors.subarray(record * 4, record * 4 + 4).join(",")}:` +
          `${Array.from(columns.transforms.subarray(at, at + 16)).join(",")}:` +
          (mesh ? hashTyped(mesh.positions) + ":" + hashTyped(mesh.indices) : "none"),
      );
    }
    return parts.sort().join("|");
  }

  /** The pack so far, shaped exactly like `readIgp`'s result. */
  function pack() {
    const instances = { count };
    for (const [name, [, width]] of Object.entries(COLUMNS)) instances[name] = columns[name].subarray(0, count * width);
    let instanceBytes = 0;
    for (const name of Object.keys(COLUMNS)) instanceBytes += instances[name].byteLength;
    return {
      index: {
        ...(index ?? {}),
        classes: classes.slice(),
        provenance: provenance.slice(),
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
    };
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
  };
}

/** Whether a column-major 4x4 at `offset` is the identity. */
export function isIdentity(transforms, offset) {
  for (let index = 0; index < 16; index += 1) {
    if (transforms[offset + index] !== IDENTITY[index]) return false;
  }
  return true;
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
