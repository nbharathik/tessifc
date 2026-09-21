// SPDX-License-Identifier: Apache-2.0

//! The IGP v0 reader: header, JSON index and typed-array views over the
//! binary chunk, plus the instance flags and class helpers viewers share.

const IGP_MAGIC = 0x00504749;
const IGP_VERSION = 0;
const HEADER_BYTES = 24;
export const INSTANCE_OPENING = 1 << 1;
export const INSTANCE_SPACE = 1 << 2;
export const INSTANCE_REFERENCE = 1 << 4;
export const DEFAULT_HIDDEN_INSTANCE_FLAGS = INSTANCE_OPENING | INSTANCE_SPACE | INSTANCE_REFERENCE;

/**
 * Read an IGP v0 buffer; the returned typed arrays view the input without copying.
 * @param {Uint8Array | ArrayBuffer} input
 * @returns {import("./types.js").Pack}
 */
export function readIgp(input) {
  const bytes = asBytes(input);
  if (bytes.byteLength < HEADER_BYTES) {
    throw new Error("The geometry pack is shorter than its 24-byte header.");
  }

  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const magic = view.getUint32(0, true);
  const version = view.getUint32(4, true);
  const jsonLength = view.getUint32(8, true);
  const binaryLength = readU64(view, 12);
  const flags = view.getUint32(20, true);

  if (magic !== IGP_MAGIC) throw new Error("The bytes are not IGP geometry.");
  if (version !== IGP_VERSION) throw new Error(`IGP version ${version} is not supported.`);

  const binaryStart = HEADER_BYTES + Math.ceil(jsonLength / 8) * 8;
  if (binaryStart > bytes.byteLength || binaryLength > bytes.byteLength - binaryStart) {
    throw new Error("The geometry pack is truncated.");
  }

  /** @type {import("./types.js").PackIndex} */
  let index;
  try {
    index = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(HEADER_BYTES, HEADER_BYTES + jsonLength)));
  } catch (error) {
    throw new Error(`The IGP index is not valid JSON: ${error.message}`);
  }

  if (
    index.igp !== IGP_VERSION ||
    !Array.isArray(index.geometries) ||
    !index.instances ||
    !Array.isArray(index.classes)
  ) {
    throw new Error("The geometry pack index is missing required IGP v0 fields.");
  }

  const absoluteBinaryStart = bytes.byteOffset + binaryStart;
  const geometry = index.geometries.map((entry) => {
    // File-supplied, so the shape is checked before any view is built.
    if (!entry || typeof entry !== "object" || !entry.positions || !entry.indices) {
      throw new Error("The geometry pack index is missing required IGP v0 fields.");
    }
    if (!safeCount(entry.positions.count) || !safeCount(entry.indices.count)) {
      throw new Error("An IGP geometry entry declares an invalid element count.");
    }
    const PositionArray = flags & 2 ? Float64Array : Float32Array;
    const IndexArray = entry.indices.type === "u32" ? Uint32Array : Uint16Array;
    // Optional: texture coordinates, one pair per vertex.
    const uv = entry.uv && safeCount(entry.uv.count) && entry.uv.count === entry.positions.count
      ? typedView(bytes.buffer, absoluteBinaryStart, binaryLength, entry.uv, Float32Array, entry.uv.count * 2)
      : null;
    // Optional: a coarse level of another entry, over that entry's positions.
    const lod = entry.lod && typeof entry.lod === "object" && safeCount(entry.lod.of) && safeCount(entry.lod.level)
      ? { of: entry.lod.of, level: entry.lod.level }
      : null;
    return {
      id: entry.id,
      primitive: entry.primitive ?? "triangles",
      bbox: entry.bbox,
      // Optional: absent on a pack written before the writer answered it.
      closed: typeof entry.closed === "boolean" ? entry.closed : null,
      positions: typedView(
        bytes.buffer,
        absoluteBinaryStart,
        binaryLength,
        entry.positions,
        PositionArray,
        entry.positions.count * 3,
      ),
      indices: typedView(
        bytes.buffer,
        absoluteBinaryStart,
        binaryLength,
        entry.indices,
        IndexArray,
        entry.indices.count,
      ),
      uv,
      lod,
    };
  });

  const count = index.instances.count;
  if (!safeCount(count)) throw new Error("The IGP instance count is invalid.");
  // Optional; a whole pack reads as a stream of one final chunk.
  const stream = index.stream;
  if (stream !== undefined) {
    if (
      !stream ||
      typeof stream !== "object" ||
      !safeCount(stream.chunk) ||
      typeof stream.final !== "boolean" ||
      !safeCount(stream.products_done) ||
      !safeCount(stream.products_total)
    ) {
      throw new Error("The IGP stream position is invalid.");
    }
  }
  const columns = index.instances;
  const instances = {
    count,
    geometryIds: typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.geometry_id, Uint32Array, count),
    expressIds: typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.express_id, Uint32Array, count),
    classIds: typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.class_id, Uint16Array, count),
    transforms: typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.transform, Float32Array, count * 16),
    colors: typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.color, Uint8Array, count * 4),
    flags: typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.flags, Uint16Array, count),
    // Optional: absent from a pack written before the writer produced it.
    provenance: columns.provenance
      ? typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.provenance, Uint32Array, count)
      : null,
    // Optional: only a pack written with textures on carries materials.
    material: columns.material
      ? typedView(bytes.buffer, absoluteBinaryStart, binaryLength, columns.material, Uint32Array, count)
      : null,
  };
  // Optional tables; a texture's bytes are viewed where the pack embeds them.
  if (index.materials !== undefined && !Array.isArray(index.materials)) {
    throw new Error("The IGP materials table is invalid.");
  }
  if (index.textures !== undefined) {
    if (!Array.isArray(index.textures)) throw new Error("The IGP textures table is invalid.");
    index.textures = index.textures.map((texture) => readTexture(texture, bytes.buffer, absoluteBinaryStart, binaryLength));
  }

  let geometryBytes = 0;
  for (const mesh of geometry) {
    // A level shares its base's positions and uv; only its indices are new bytes.
    geometryBytes += mesh.indices.byteLength + (mesh.lod ? 0 : mesh.positions.byteLength + (mesh.uv ? mesh.uv.byteLength : 0));
  }
  const instanceBytes =
    instances.geometryIds.byteLength +
    instances.expressIds.byteLength +
    instances.classIds.byteLength +
    instances.transforms.byteLength +
    instances.colors.byteLength +
    instances.flags.byteLength +
    (instances.provenance ? instances.provenance.byteLength : 0) +
    (instances.material ? instances.material.byteLength : 0);
  const normalsBytes = geometry.reduce((sum, mesh) => sum + (mesh.lod ? 0 : mesh.positions.length * 4), 0);
  const gpuBytes = geometryBytes + normalsBytes + count * (16 * 4 + 3 * 4);

  return {
    index,
    geometry,
    instances,
    flags,
    stream: stream ?? { chunk: 0, final: true, products_done: count, products_total: count },
    bytes: bytes.byteLength,
    memory: { geometryBytes, instanceBytes, gpuBytes },
  };
}

/**
 * Turn an IFC class into a short UI label without losing its exact name.
 * @param {string} name
 */
export function humanizeIfcClass(name) {
  return String(name ?? "Unknown")
    .replace(/^Ifc/, "")
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .replace(/^./, (letter) => letter.toUpperCase());
}

/** Stable semantic colour used by the class-label rail. */
export function classLabelColor(name) {
  const value = String(name).toLowerCase();
  if (/(wall|slab|roof|beam|column|footing|member|plate)/.test(value)) return "#6ecbc4";
  if (/(door|window|opening)/.test(value)) return "#ff7147";
  if (/(stair|ramp|railing)/.test(value)) return "#f0b15b";
  if (/(flow|distribution|pipe|duct|cable|terminal|sanitary)/.test(value)) return "#7ca6d8";
  if (/(site|building|storey|space|zone)/.test(value)) return "#9d8bc5";
  if (/(furnishing|equipment|proxy)/.test(value)) return "#c49a79";
  return "#829195";
}

/** Class ids whose every instance is helper geometry, hidden by default. */
export function defaultHiddenClassIds(pack) {
  const classes = new Map();
  for (let record = 0; record < pack.instances.count; record += 1) {
    const classId = pack.instances.classIds[record];
    const semantic = Boolean(pack.instances.flags[record] & DEFAULT_HIDDEN_INSTANCE_FLAGS);
    const state = classes.get(classId) ?? { semantic: 0, ordinary: 0 };
    if (semantic) state.semantic += 1;
    else state.ordinary += 1;
    classes.set(classId, state);
  }
  return new Set(
    [...classes.entries()]
      .filter(([, counts]) => counts.semantic > 0 && counts.ordinary === 0)
      .map(([classId]) => classId),
  );
}

function safeCount(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

/** One `textures` entry with its blob or pixel bytes viewed, not copied. */
function readTexture(texture, buffer, binaryStart, binaryLength) {
  if (!texture || typeof texture !== "object" || !safeCount(texture.id)) {
    throw new Error("An IGP texture entry is invalid.");
  }
  /** @type {import("./types.js").PackTexture} */
  const out = {
    id: texture.id,
    mime: typeof texture.mime === "string" ? texture.mime : null,
    repeat: Array.isArray(texture.repeat) ? texture.repeat.map(Boolean) : [true, true],
    transform: Array.isArray(texture.transform) && texture.transform.length === 6 ? texture.transform : null,
  };
  const bytesOf = (section) => {
    if (!section || !safeCount(section.len)) throw new Error("An IGP texture section is invalid.");
    return typedView(buffer, binaryStart, binaryLength, section, Uint8Array, section.len);
  };
  if (typeof texture.uri === "string") out.uri = texture.uri;
  else if (texture.blob) out.blob = bytesOf(texture.blob);
  else if (texture.pixels) {
    const { width, height, components } = texture.pixels;
    if (!safeCount(width) || !safeCount(height) || !safeCount(components) || width * height * components !== texture.pixels.len) {
      throw new Error("An IGP pixel texture does not match its size.");
    }
    out.pixels = { width, height, components, bytes: bytesOf(texture.pixels) };
  } else out.omitted = true;
  return out;
}

function asBytes(input) {
  if (input instanceof Uint8Array) return input;
  if (input instanceof ArrayBuffer) return new Uint8Array(input);
  throw new TypeError("readIgp expects an ArrayBuffer or Uint8Array.");
}

function readU64(view, offset) {
  const low = view.getUint32(offset, true);
  const high = view.getUint32(offset + 4, true);
  const value = low + high * 2 ** 32;
  if (!Number.isSafeInteger(value)) throw new Error("The IGP binary chunk is too large for this browser.");
  return value;
}

function typedView(buffer, binaryStart, binaryLength, section, Type, length) {
  if (!section || !Number.isSafeInteger(section.off) || section.off < 0) {
    throw new Error("The IGP index contains an invalid binary offset.");
  }
  const bytesNeeded = length * Type.BYTES_PER_ELEMENT;
  if (section.off + bytesNeeded > binaryLength) {
    throw new Error("An IGP binary column runs past the end of the pack.");
  }
  try {
    return new Type(buffer, binaryStart + section.off, length);
  } catch (error) {
    throw new Error(`An IGP binary column is misaligned: ${error.message}`);
  }
}
