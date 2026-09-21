// SPDX-License-Identifier: Apache-2.0

//! Which triangles of which records lie on a plane another record also
//! occupies. The tie-break overlay redraws only those, not whole records.

// Normals are quantised to a thousandth and plane offsets to a millimetre.
const NORMAL_STEPS = 500;
const OFFSET_QUANTUM = 0.001;
const OFFSET_BIAS = 2 ** 22;
const KEY_SCALE = 2 ** 23;
// Two planes whose quantised offsets differ by one bin still count as one plane.
const SLACK = OFFSET_QUANTUM * 2;

/** The quantised key of the triangle plane through world points `a`, `b`, `c`, or NaN for a sliver. */
export function planeKey(ax, ay, az, bx, by, bz, cx, cy, cz) {
  const ux = bx - ax, uy = by - ay, uz = bz - az;
  const vx = cx - ax, vy = cy - ay, vz = cz - az;
  let nx = uy * vz - uz * vy;
  let ny = uz * vx - ux * vz;
  let nz = ux * vy - uy * vx;
  const length = Math.hypot(nx, ny, nz);
  if (!(length > 1e-12)) return NaN;
  nx /= length;
  ny /= length;
  nz /= length;
  // Both sides of a shared face get the same key.
  if (nx < -1e-9 || (nx <= 1e-9 && (ny < -1e-9 || (ny <= 1e-9 && nz < 0)))) {
    nx = -nx;
    ny = -ny;
    nz = -nz;
  }
  const qx = Math.round(nx * NORMAL_STEPS) + 512;
  const qy = Math.round(ny * NORMAL_STEPS) + 512;
  const qz = Math.round(nz * NORMAL_STEPS) + 512;
  const offset = nx * ax + ny * ay + nz * az;
  const qd = Math.round(offset / OFFSET_QUANTUM) + OFFSET_BIAS;
  if (qd < 0 || qd >= KEY_SCALE) return NaN;
  return ((qx * 1024 + qy) * 1024 + qz) * KEY_SCALE + qd;
}

function transformPositions(positions, transforms, record) {
  const at = record * 16;
  const m = transforms;
  const out = new Float64Array(positions.length);
  for (let v = 0; v < positions.length; v += 3) {
    const x = positions[v], y = positions[v + 1], z = positions[v + 2];
    out[v] = m[at] * x + m[at + 4] * y + m[at + 8] * z + m[at + 12];
    out[v + 1] = m[at + 1] * x + m[at + 5] * y + m[at + 9] * z + m[at + 13];
    out[v + 2] = m[at + 2] * x + m[at + 6] * y + m[at + 10] * z + m[at + 14];
  }
  return out;
}

/** Plane keys of every triangle of `record` in pack space, NaN where a triangle is degenerate. */
function triangleKeys(world, indices) {
  const count = Math.floor(indices.length / 3);
  const keys = new Float64Array(count);
  const vertexCount = Math.floor(world.length / 3);
  for (let t = 0; t < count; t += 1) {
    const a = indices[t * 3] * 3, b = indices[t * 3 + 1] * 3, c = indices[t * 3 + 2] * 3;
    if (a >= world.length || b >= world.length || c >= world.length || vertexCount === 0) {
      keys[t] = NaN;
      continue;
    }
    keys[t] = planeKey(world[a], world[a + 1], world[a + 2], world[b], world[b + 1], world[b + 2], world[c], world[c + 1], world[c + 2]);
  }
  return keys;
}

/** The planes at least two of the record's triangles share, sorted by key, with their extents. */
function planeTable(world, indices, keys) {
  const counts = new Map();
  for (const key of keys) {
    if (Number.isNaN(key)) continue;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  const shared = [];
  for (const [key, count] of counts) if (count >= 2) shared.push(key);
  shared.sort((left, right) => left - right);
  const slot = new Map(shared.map((key, index) => [key, index]));
  const extents = new Float64Array(shared.length * 6);
  for (let i = 0; i < shared.length; i += 1) {
    extents[i * 6] = extents[i * 6 + 1] = extents[i * 6 + 2] = Infinity;
    extents[i * 6 + 3] = extents[i * 6 + 4] = extents[i * 6 + 5] = -Infinity;
  }
  for (let t = 0; t < keys.length; t += 1) {
    const index = slot.get(keys[t]);
    if (index === undefined) continue;
    for (let corner = 0; corner < 3; corner += 1) {
      const v = indices[t * 3 + corner] * 3;
      for (let axis = 0; axis < 3; axis += 1) {
        const value = world[v + axis];
        if (value < extents[index * 6 + axis]) extents[index * 6 + axis] = value;
        if (value > extents[index * 6 + 3 + axis]) extents[index * 6 + 3 + axis] = value;
      }
    }
  }
  return { keys: Float64Array.from(shared), extents };
}

/** Faces that meet along an edge cannot fight; the extents must overlap in area, so on two axes. */
function extentsOverlap(a, i, b, j) {
  let wide = 0;
  for (let axis = 0; axis < 3; axis += 1) {
    const low = Math.max(a[i * 6 + axis], b[j * 6 + axis]);
    const high = Math.min(a[i * 6 + 3 + axis], b[j * 6 + 3 + axis]);
    if (high + SLACK < low) return false;
    if (high - low > SLACK) wide += 1;
  }
  return wide >= 2;
}

function boxesOverlap(a, b) {
  for (let axis = 0; axis < 3; axis += 1) {
    if (a.max[axis] < b.min[axis] || b.max[axis] < a.min[axis]) return false;
  }
  return true;
}

/** Mark, in both records, every plane the two share within one offset bin whose extents meet. */
function markSharedPlanes(a, b) {
  let i = 0, j = 0;
  let any = false;
  const ka = a.table.keys, kb = b.table.keys;
  while (i < ka.length && j < kb.length) {
    const difference = ka[i] - kb[j];
    if (difference < -1) i += 1;
    else if (difference > 1) j += 1;
    else {
      // Neighbouring bins of the same normal are candidates too; scan the small window.
      let hit = false;
      for (let jj = j; jj < kb.length && kb[jj] - ka[i] <= 1; jj += 1) {
        if (extentsOverlap(a.table.extents, i, b.table.extents, jj)) {
          b.shared[jj] = 1;
          hit = true;
        }
      }
      if (hit) {
        a.shared[i] = 1;
        any = true;
      }
      i += 1;
    }
  }
  return any;
}

/**
 * Find, for every opaque record, the triangles on a plane that another opaque record of a
 * different colour also occupies. `geometries` maps geometry id to `{ positions, indices }`.
 * Returns CSR arrays: `records`, `offsets` (length records + 1) and `triangles` (indices into
 * the geometry's triangle list, ascending), plus counts for reporting.
 */
export function findContestedTriangles(pack, geometries, options = {}) {
  const { instances } = pack;
  const count = instances.count;
  const maxPairTests = options.maxPairTests ?? 4_000_000;
  const entries = [];
  for (let record = 0; record < count; record += 1) {
    if (instances.active && !instances.active[record]) continue;
    if (instances.colors[record * 4 + 3] < 255) continue;
    const geometry = geometries.get(instances.geometryIds[record]);
    if (!geometry || geometry.indices.length < 3) continue;
    const world = transformPositions(geometry.positions, instances.transforms, record);
    const min = [Infinity, Infinity, Infinity], max = [-Infinity, -Infinity, -Infinity];
    for (let v = 0; v < world.length; v += 3) {
      for (let axis = 0; axis < 3; axis += 1) {
        const value = world[v + axis];
        if (value < min[axis]) min[axis] = value;
        if (value > max[axis]) max[axis] = value;
      }
    }
    if (!Number.isFinite(min[0]) || !Number.isFinite(max[0])) continue;
    const keys = triangleKeys(world, geometry.indices);
    const table = planeTable(world, geometry.indices, keys);
    if (!table.keys.length) continue;
    const c = instances.colors;
    entries.push({
      record,
      color: (c[record * 4] << 16) | (c[record * 4 + 1] << 8) | c[record * 4 + 2],
      min,
      max,
      table,
      shared: new Uint8Array(table.keys.length),
      keys,
    });
  }

  entries.sort((left, right) => left.min[0] - right.min[0] || left.record - right.record);
  const active = [];
  let tests = 0;
  let pairs = 0;
  let exhausted = false;
  for (const current of entries) {
    let write = 0;
    for (const other of active) if (other.max[0] >= current.min[0]) active[write++] = other;
    active.length = write;
    tests += active.length;
    if (tests > maxPairTests) {
      exhausted = true;
      break;
    }
    for (const other of active) {
      if (other.color === current.color || !boxesOverlap(other, current)) continue;
      if (markSharedPlanes(other, current)) pairs += 1;
    }
    active.push(current);
  }

  const records = [];
  const offsets = [0];
  const triangles = [];
  if (!exhausted) {
    for (const entry of entries) {
      if (!entry.shared.some((flag) => flag === 1)) continue;
      const wanted = new Set();
      for (let i = 0; i < entry.shared.length; i += 1) if (entry.shared[i]) wanted.add(entry.table.keys[i]);
      let added = 0;
      for (let t = 0; t < entry.keys.length; t += 1) {
        const key = entry.keys[t];
        if (wanted.has(key) || wanted.has(key - 1) || wanted.has(key + 1)) {
          triangles.push(t);
          added += 1;
        }
      }
      if (!added) continue;
      records.push(entry.record);
      offsets.push(triangles.length);
    }
  }
  return {
    records: Uint32Array.from(records),
    offsets: Uint32Array.from(offsets),
    triangles: Uint32Array.from(triangles),
    pairs,
    exhausted,
    opaqueRecords: entries.length,
  };
}
