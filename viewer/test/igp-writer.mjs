// SPDX-License-Identifier: Apache-2.0
// A small IGP v0 writer for tests: enough of the container to hand-build a
// pack with the optional members a reader must cope with.

const IGP_MAGIC = 0x00504749;
const HEADER_BYTES = 24;

/**
 * @typedef {object} WriterGeometry
 * @property {number} id
 * @property {Float32Array} positions
 * @property {Uint16Array | Uint32Array} indices
 * @property {number[]} [bbox]
 * @property {boolean} [closed]
 * @property {Float32Array} [uv]
 * @property {{ of: number, level: number }} [lod] A coarse level: its positions are the base entry's section.
 */

/**
 * @typedef {object} WriterInstance
 * @property {number} geometry
 * @property {number} expressId
 * @property {number} classId
 * @property {number[]} [transform] column-major, identity by default
 * @property {number[]} [color]
 * @property {number} [flags]
 * @property {number} [material] an index into `materials`
 */

/**
 * Write a pack. Textures with `blob` or `pixels` bytes land in the binary
 * chunk after the instance columns, as the kernel writes them.
 * @param {{
 *   geometries: WriterGeometry[],
 *   instances: WriterInstance[],
 *   classes?: string[],
 *   materials?: Array<Record<string, unknown>>,
 *   textures?: Array<Record<string, any>>,
 *   index?: Record<string, unknown>,
 * }} spec
 * @returns {Uint8Array}
 */
export function writeIgp(spec) {
  const sections = [];
  let cursor = 0;
  const place = (bytes) => {
    const off = cursor;
    sections.push({ off, bytes });
    cursor = Math.ceil((off + bytes.byteLength) / 8) * 8;
    return off;
  };
  const placedPositions = new Map();
  const geometries = spec.geometries.map((geometry) => {
    const base = geometry.lod ? placedPositions.get(geometry.lod.of) : null;
    const positions = base ?? { off: place(view(geometry.positions)), count: geometry.positions.length / 3, type: "f32" };
    if (!geometry.lod) placedPositions.set(geometry.id, positions);
    const entry = {
      id: geometry.id,
      primitive: "triangles",
      bbox: geometry.bbox ?? [0, 0, 0, 1, 1, 1],
      closed: geometry.closed ?? false,
      positions,
      indices: {
        off: place(view(geometry.indices)),
        count: geometry.indices.length,
        type: geometry.indices instanceof Uint32Array ? "u32" : "u16",
      },
    };
    if (geometry.uv) entry.uv = { off: place(view(geometry.uv)), count: geometry.uv.length / 2 };
    if (geometry.lod) entry.lod = { of: geometry.lod.of, level: geometry.lod.level };
    return entry;
  });
  const count = spec.instances.length;
  const geometryIds = new Uint32Array(count);
  const expressIds = new Uint32Array(count);
  const classIds = new Uint16Array(count);
  const transforms = new Float32Array(count * 16);
  const colors = new Uint8Array(count * 4);
  const flags = new Uint16Array(count);
  const materials = new Uint32Array(count).fill(0xffffffff);
  let anyMaterial = false;
  spec.instances.forEach((instance, record) => {
    geometryIds[record] = instance.geometry;
    expressIds[record] = instance.expressId;
    classIds[record] = instance.classId ?? 0;
    transforms.set(instance.transform ?? [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1], record * 16);
    colors.set(instance.color ?? [200, 200, 200, 255], record * 4);
    flags[record] = instance.flags ?? 0;
    if (typeof instance.material === "number") {
      materials[record] = instance.material;
      anyMaterial = true;
    }
  });
  const instances = {
    count,
    geometry_id: { off: place(view(geometryIds)), type: "u32" },
    express_id: { off: place(view(expressIds)), type: "u32" },
    class_id: { off: place(view(classIds)), type: "u16" },
    transform: { off: place(view(transforms)), type: "f32x16" },
    color: { off: place(view(colors)), type: "u8x4" },
    flags: { off: place(view(flags)), type: "u16" },
  };
  if (anyMaterial) instances.material = { off: place(view(materials)), type: "u32" };
  const textures = (spec.textures ?? []).map((texture) => {
    const entry = { id: texture.id, mime: texture.mime ?? null, repeat: texture.repeat ?? [true, true], transform: texture.transform ?? null };
    if (typeof texture.uri === "string") entry.uri = texture.uri;
    else if (texture.blob) entry.blob = { off: place(texture.blob), len: texture.blob.byteLength };
    else if (texture.pixels) {
      const { width, height, components, bytes } = texture.pixels;
      entry.pixels = { off: place(bytes), len: bytes.byteLength, width, height, components };
    } else entry.omitted = true;
    return entry;
  });
  const index = {
    igp: 0,
    schema: "IFC4",
    model_offset: [0, 0, 0],
    classes: spec.classes ?? ["IfcWall"],
    geometries,
    instances,
    ...(spec.materials ? { materials: spec.materials } : {}),
    ...(spec.textures ? { textures } : {}),
    stats: { products: count, triangles: 0 },
    diagnostics: [],
    ...(spec.index ?? {}),
  };
  const json = new TextEncoder().encode(JSON.stringify(index));
  const jsonPadded = Math.ceil(json.byteLength / 8) * 8;
  const binaryLength = cursor;
  const out = new Uint8Array(HEADER_BYTES + jsonPadded + binaryLength);
  const header = new DataView(out.buffer);
  header.setUint32(0, IGP_MAGIC, true);
  header.setUint32(4, 0, true);
  header.setUint32(8, json.byteLength, true);
  header.setUint32(12, binaryLength, true);
  header.setUint32(16, 0, true);
  header.setUint32(20, 0, true);
  out.set(json, HEADER_BYTES);
  for (const { off, bytes } of sections) out.set(bytes, HEADER_BYTES + jsonPadded + off);
  return out;
}

function view(array) {
  return new Uint8Array(array.buffer, array.byteOffset, array.byteLength);
}
