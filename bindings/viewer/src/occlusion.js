// SPDX-License-Identifier: Apache-2.0

//! Same-frame software occlusion: the largest opaque faces of a model are
//! drawn into a small depth grid with inner coverage, and a cluster whose
//! projected box lies entirely behind that grid is left out of a moving frame.
//! Every rule errs toward drawing: a pixel is filled only where one occluder
//! covers all of it, an occluder counts at its farthest vertex, a cluster at
//! its nearest corner, and anything near the near plane or off the grid stays.
//! Two triangles that make a planar convex quad are one occluder, so the
//! diagonal of a wall leaves no seam of uncovered pixels.

/** Occluder triangles kept per model, the largest by area. */
export const MAX_OCCLUDER_TRIANGLES = 8192;
/** Grid columns; rows follow the viewport's aspect. */
export const OCCLUSION_GRID_WIDTH = 256;
/** Grid pixels one frame may write while rasterising occluders; about a dozen fills of the grid. */
export const OCCLUSION_PIXEL_BUDGET = 250_000;
/** Occlusion runs while the camera is nearer the model's centre than this many radii; from farther away little is ever fully hidden. */
export const OCCLUSION_INSIDE_RADII = 1;
/** Pixels per side of the blocks whose farthest depth the cluster test reads first. */
const BLOCK = 8;
/** A triangle is an occluder when its area passes this fraction of the model radius squared. */
export const OCCLUDER_AREA_FRACTION = 4e-4;
/** Records an occluder selection step visits before it yields. */
const SELECTION_SLICE_RECORDS = 64;

/**
 * @typedef {object} Occluders
 * @property {Float32Array} vertices Twelve floats per occluder, a convex quad in render space, largest first; a triangle repeats its last corner.
 * @property {Uint32Array} records The record each occluder came from.
 * @property {number} count
 */

/** Vertices per occluder in the packed array. */
export const OCCLUDER_CORNERS = 4;

/**
 * A resumable selection of the largest opaque triangles. Call `step` with a
 * deadline until it returns true, then `result()`.
 * @param {{ instances: { count: number, geometryIds: Uint32Array, transforms: Float32Array, active?: Uint8Array | null }, geometry: Array<{ id: number, positions: ArrayLike<number>, indices: ArrayLike<number> }> }} pack
 * @param {Uint8Array} renderColors Display colours, four bytes per record; alpha 255 is opaque.
 * @param {number[]} renderOrigin What the renderer subtracts from world coordinates.
 * @param {{ radius: number, maxTriangles?: number, areaFraction?: number, isActive?: (record: number) => boolean }} options
 */
export function createOccluderSelection(pack, renderColors, renderOrigin, options) {
  const maxTriangles = options.maxTriangles ?? MAX_OCCLUDER_TRIANGLES;
  const threshold = Math.max(options.radius, 1e-3) ** 2 * (options.areaFraction ?? OCCLUDER_AREA_FRACTION);
  const geometryById = new Map(pack.geometry.map((mesh) => [mesh.id, mesh]));
  const isActive = options.isActive ?? (() => true);
  /** @type {Array<{ area: number, record: number, vertices: number[], corners: number[] }>} */
  let candidates = [];
  let record = 0;
  let done = false;

  function trim() {
    if (candidates.length <= maxTriangles * 4) return;
    candidates.sort((left, right) => right.area - left.area);
    candidates.length = maxTriangles;
  }

  function visit(at) {
    if (!isActive(at) || renderColors[at * 4 + 3] !== 255) return;
    if (pack.instances.active && !pack.instances.active[at]) return;
    const mesh = geometryById.get(pack.instances.geometryIds[at]);
    if (!mesh) return;
    const matrix = pack.instances.transforms.subarray(at * 16, at * 16 + 16);
    const positions = mesh.positions;
    const indices = mesh.indices;
    const world = new Float64Array(9);
    for (let triangle = 0; triangle + 2 < indices.length; triangle += 3) {
      for (let corner = 0; corner < 3; corner += 1) {
        const index = indices[triangle + corner] * 3;
        const x = positions[index], y = positions[index + 1], z = positions[index + 2];
        world[corner * 3] = matrix[0] * x + matrix[4] * y + matrix[8] * z + matrix[12] - renderOrigin[0];
        world[corner * 3 + 1] = matrix[1] * x + matrix[5] * y + matrix[9] * z + matrix[13] - renderOrigin[1];
        world[corner * 3 + 2] = matrix[2] * x + matrix[6] * y + matrix[10] * z + matrix[14] - renderOrigin[2];
      }
      const ax = world[3] - world[0], ay = world[4] - world[1], az = world[5] - world[2];
      const bx = world[6] - world[0], by = world[7] - world[1], bz = world[8] - world[2];
      const cx = ay * bz - az * by, cy = az * bx - ax * bz, cz = ax * by - ay * bx;
      const area = Math.sqrt(cx * cx + cy * cy + cz * cz) / 2;
      if (!(area > threshold)) continue;
      candidates.push({ area, record: at, vertices: Array.from(world), corners: [indices[triangle], indices[triangle + 1], indices[triangle + 2]] });
    }
    trim();
  }

  /** Join pairs of a record's triangles that share an edge into planar convex quads. */
  function merged(list) {
    const byEdge = new Map();
    const key = (record, a, b) => `${record}:${Math.min(a, b)}:${Math.max(a, b)}`;
    for (const candidate of list) {
      const [a, b, c] = candidate.corners;
      for (const [p, q] of [[a, b], [b, c], [c, a]]) {
        const edge = key(candidate.record, p, q);
        const found = byEdge.get(edge);
        if (found) found.push(candidate);
        else byEdge.set(edge, [candidate]);
      }
    }
    const taken = new Set();
    const out = [];
    for (const candidate of list) {
      if (taken.has(candidate)) continue;
      let quad = null;
      const [a, b, c] = candidate.corners;
      for (const [p, q, r] of [[a, b, c], [b, c, a], [c, a, b]]) {
        const partners = byEdge.get(key(candidate.record, p, q)) ?? [];
        for (const other of partners) {
          if (other === candidate || taken.has(other)) continue;
          const s = other.corners.find((corner) => corner !== p && corner !== q);
          if (s === undefined) continue;
          quad = convexQuad(candidate, other, p, q, r, s);
          if (quad) break;
        }
        if (quad) break;
      }
      taken.add(candidate);
      if (quad) {
        taken.add(quad.other);
        out.push({ area: candidate.area + quad.other.area, record: candidate.record, vertices: quad.vertices });
      } else {
        out.push({ area: candidate.area, record: candidate.record, vertices: [...candidate.vertices, ...candidate.vertices.slice(6, 9)] });
      }
    }
    return out;
  }

  return {
    /**
     * Visit records until `deadline` (a `performance.now()` value) or the end.
     * @param {number} deadline
     * @returns {boolean} true when every record has been visited
     */
    step(deadline) {
      if (done) return true;
      const count = pack.instances.count;
      while (record < count) {
        const stop = Math.min(count, record + SELECTION_SLICE_RECORDS);
        for (; record < stop; record += 1) visit(record);
        if (performance.now() >= deadline) break;
      }
      done = record >= count;
      return done;
    },
    /** The selection so far, quads joined, largest first. */
    result() {
      candidates.sort((left, right) => right.area - left.area);
      const kept = merged(candidates.slice(0, maxTriangles));
      kept.sort((left, right) => right.area - left.area);
      const vertices = new Float32Array(kept.length * 12);
      const records = new Uint32Array(kept.length);
      kept.forEach((candidate, at) => {
        vertices.set(candidate.vertices, at * 12);
        records[at] = candidate.record;
      });
      return { vertices, records, count: kept.length };
    },
    get done() {
      return done;
    },
  };
}

/**
 * The planar convex quad two triangles sharing the edge `p q` make, in the
 * order `p, r, q, s`, or null when they are not coplanar or the quad bends.
 */
function convexQuad(first, second, p, q, r, s) {
  const point = (candidate, corner) => {
    const at = candidate.corners.indexOf(corner) * 3;
    return [candidate.vertices[at], candidate.vertices[at + 1], candidate.vertices[at + 2]];
  };
  const P = point(first, p), Q = point(first, q), R = point(first, r), S = point(second, s);
  const n1 = normal(P, Q, R);
  const n2 = normal(P, S, Q);
  const l1 = Math.hypot(...n1), l2 = Math.hypot(...n2);
  if (!(l1 > 0) || !(l2 > 0)) return null;
  // Same plane, same facing: the two triangles lie on the shared edge's two sides.
  if ((n1[0] * n2[0] + n1[1] * n2[1] + n1[2] * n2[2]) / (l1 * l2) < 1 - 1e-6) return null;
  // Convex when every corner turns the same way around the plane normal.
  const ring = [P, R, Q, S];
  let sign = 0;
  for (let at = 0; at < 4; at += 1) {
    const a = ring[at], b = ring[(at + 1) % 4], c = ring[(at + 2) % 4];
    const turn = normal(a, b, c);
    const along = turn[0] * n1[0] + turn[1] * n1[1] + turn[2] * n1[2];
    if (Math.abs(along) <= 1e-9 * l1 * l1) return null;
    if (sign === 0) sign = Math.sign(along);
    else if (Math.sign(along) !== sign) return null;
  }
  return { other: second, vertices: [...P, ...R, ...Q, ...S] };
}

function normal(a, b, c) {
  const ux = b[0] - a[0], uy = b[1] - a[1], uz = b[2] - a[2];
  const vx = c[0] - a[0], vy = c[1] - a[1], vz = c[2] - a[2];
  return [uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx];
}

/**
 * @typedef {object} OcclusionGrid
 * @property {number} width
 * @property {number} height
 * @property {Float32Array} depth The nearest occluder's farthest depth per pixel; Infinity where nothing is drawn.
 * @property {number} blocksWide Blocks per row.
 * @property {number} blocksHigh Block rows.
 * @property {Float32Array} blockMax The farthest depth in each block, so a box test can skip whole blocks.
 * @property {number} filled Pixels written in the last rasterisation.
 * @property {number} used Occluders drawn in the last rasterisation.
 */

/**
 * A grid of `width` columns and rows in the viewport's aspect.
 * @param {number} width
 * @param {number} aspect viewport width over height
 * @returns {OcclusionGrid}
 */
export function createOcclusionGrid(width = OCCLUSION_GRID_WIDTH, aspect = 1.5) {
  const columns = Math.max(4, Math.round(width));
  const rows = Math.max(4, Math.round(columns / Math.max(aspect, 1e-3)));
  const blocksWide = Math.ceil(columns / BLOCK), blocksHigh = Math.ceil(rows / BLOCK);
  return {
    width: columns, height: rows, depth: new Float32Array(columns * rows),
    blocksWide, blocksHigh, blockMax: new Float32Array(blocksWide * blocksHigh), filled: 0, used: 0,
  };
}

/** View-space distance along the camera's forward axis, positive in front. */
function depthOf(view, x, y, z) {
  return -(view[2] * x + view[6] * y + view[10] * z + view[14]);
}

/**
 * Draw the occluders into the grid. A pixel takes an occluder only when the
 * occluder covers the whole pixel; it keeps the nearer of what it has and the
 * occluder's farthest corner. Occluders with a corner at or before `near`,
 * with a non-finite coordinate or of a hidden record are skipped, and the
 * pixel budget stops the pass once it is spent.
 * @param {OcclusionGrid} grid
 * @param {Float32Array | Float64Array | number[]} viewProjection column-major
 * @param {Float32Array | Float64Array | number[]} view column-major
 * @param {number} near
 * @param {Occluders} occluders
 * @param {(record: number) => boolean} isRecordVisible
 * @param {number} [pixelBudget]
 */
export function rasteriseOccluders(grid, viewProjection, view, near, occluders, isRecordVisible, pixelBudget = OCCLUSION_PIXEL_BUDGET) {
  const { width, height, depth } = grid;
  depth.fill(Infinity);
  grid.filled = 0;
  grid.used = 0;
  const sx = new Float64Array(4), sy = new Float64Array(4);
  const ax = new Float64Array(4), bx = new Float64Array(4), cx0 = new Float64Array(4);
  const ox = new Uint8Array(4), oy = new Uint8Array(4);
  let budget = pixelBudget;
  for (let at = 0; at < occluders.count; at += 1) {
    if (!isRecordVisible(occluders.records[at])) continue;
    let key = -Infinity;
    let usable = true;
    for (let corner = 0; corner < OCCLUDER_CORNERS; corner += 1) {
      const base = at * 12 + corner * 3;
      const x = occluders.vertices[base], y = occluders.vertices[base + 1], z = occluders.vertices[base + 2];
      const w = viewProjection[3] * x + viewProjection[7] * y + viewProjection[11] * z + viewProjection[15];
      const d = depthOf(view, x, y, z);
      if (!(w > 0) || !(d > near) || !Number.isFinite(d)) { usable = false; break; }
      const cx = (viewProjection[0] * x + viewProjection[4] * y + viewProjection[8] * z + viewProjection[12]) / w;
      const cy = (viewProjection[1] * x + viewProjection[5] * y + viewProjection[9] * z + viewProjection[13]) / w;
      if (!Number.isFinite(cx) || !Number.isFinite(cy)) { usable = false; break; }
      sx[corner] = (cx * 0.5 + 0.5) * width;
      sy[corner] = (cy * 0.5 + 0.5) * height;
      if (d > key) key = d;
    }
    if (!usable) continue;
    // Counter-clockwise on screen, so every edge function is positive inside.
    let doubleArea = 0;
    for (let corner = 0; corner < OCCLUDER_CORNERS; corner += 1) {
      const next = (corner + 1) % OCCLUDER_CORNERS;
      doubleArea += sx[corner] * sy[next] - sx[next] * sy[corner];
    }
    if (!(Math.abs(doubleArea) > 1e-9)) continue;
    if (doubleArea < 0) {
      // Reverse the ring in place.
      [sx[1], sx[3]] = [sx[3], sx[1]];
      [sy[1], sy[3]] = [sy[3], sy[1]];
    }
    let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (let corner = 0; corner < OCCLUDER_CORNERS; corner += 1) {
      if (sx[corner] < minX) minX = sx[corner];
      if (sx[corner] > maxX) maxX = sx[corner];
      if (sy[corner] < minY) minY = sy[corner];
      if (sy[corner] > maxY) maxY = sy[corner];
    }
    minX = Math.max(0, Math.floor(minX));
    maxX = Math.min(width, Math.ceil(maxX));
    minY = Math.max(0, Math.floor(minY));
    maxY = Math.min(height, Math.ceil(maxY));
    if (minX >= maxX || minY >= maxY) continue;
    grid.used += 1;
    // Edge functions E(x, y) = A x + B y + C, positive on the inside; the
    // pixel square is inside when its most outward corner is. A repeated
    // corner makes a zero edge, which every pixel passes.
    for (let corner = 0; corner < OCCLUDER_CORNERS; corner += 1) {
      const next = (corner + 1) % OCCLUDER_CORNERS;
      ax[corner] = sy[corner] - sy[next];
      bx[corner] = sx[next] - sx[corner];
      cx0[corner] = sx[corner] * sy[next] - sx[next] * sy[corner];
      ox[corner] = ax[corner] < 0 ? 1 : 0;
      oy[corner] = bx[corner] < 0 ? 1 : 0;
    }
    // Per row, the edges bound a span of whole pixels; only that span is written.
    for (let py = minY; py < maxY && budget > 0; py += 1) {
      let lo = minX, hi = maxX;
      for (let corner = 0; corner < OCCLUDER_CORNERS && lo < hi; corner += 1) {
        const a = ax[corner];
        const k = bx[corner] * (py + oy[corner]) + cx0[corner] + a * ox[corner];
        if (a > 0) {
          const bound = Math.ceil(-k / a + 1e-7);
          if (bound > lo) lo = bound;
        } else if (a < 0) {
          const bound = Math.floor(-k / a - 1e-7) + 1;
          if (bound < hi) hi = bound;
        } else if (k < 0) {
          hi = lo;
        }
      }
      if (lo >= hi) continue;
      budget -= hi - lo;
      const row = py * width;
      for (let px = lo; px < hi; px += 1) {
        const cell = row + px;
        if (key < depth[cell]) {
          if (depth[cell] === Infinity) grid.filled += 1;
          depth[cell] = key;
        }
      }
    }
    if (budget <= 0) break;
  }
  // The farthest depth per block: a box whose blocks all pass needs no pixel read.
  const { blocksWide, blocksHigh, blockMax } = grid;
  for (let by = 0; by < blocksHigh; by += 1) {
    for (let bx = 0; bx < blocksWide; bx += 1) {
      let farthest = -Infinity;
      const yEnd = Math.min(height, (by + 1) * BLOCK), xEnd = Math.min(width, (bx + 1) * BLOCK);
      for (let py = by * BLOCK; py < yEnd; py += 1) {
        const row = py * width;
        for (let px = bx * BLOCK; px < xEnd; px += 1) if (depth[row + px] > farthest) farthest = depth[row + px];
      }
      blockMax[by * blocksWide + bx] = farthest;
    }
  }
  return grid;
}

/**
 * Whether a box lies entirely behind the grid's occluders: every pixel its
 * projection touches holds an occluder nearer than the box's nearest corner
 * by more than `delta`. Any corner at or before `near`, any part of the
 * projection off the grid, or any non-finite value keeps the box visible.
 * @param {OcclusionGrid} grid
 * @param {Float32Array | Float64Array | number[]} viewProjection
 * @param {Float32Array | Float64Array | number[]} view
 * @param {number} near
 * @param {number[]} center
 * @param {number[]} halfExtents
 * @param {number} delta
 */
export function clusterHidden(grid, viewProjection, view, near, center, halfExtents, delta) {
  if (grid.filled === 0) return false;
  const { width, height, depth } = grid;
  let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity, nearest = Infinity;
  for (let corner = 0; corner < 8; corner += 1) {
    const x = center[0] + (corner & 1 ? halfExtents[0] : -halfExtents[0]);
    const y = center[1] + (corner & 2 ? halfExtents[1] : -halfExtents[1]);
    const z = center[2] + (corner & 4 ? halfExtents[2] : -halfExtents[2]);
    const w = viewProjection[3] * x + viewProjection[7] * y + viewProjection[11] * z + viewProjection[15];
    const d = depthOf(view, x, y, z);
    if (!(w > 0) || !(d > near) || !Number.isFinite(d)) return false;
    const cx = (viewProjection[0] * x + viewProjection[4] * y + viewProjection[8] * z + viewProjection[12]) / w;
    const cy = (viewProjection[1] * x + viewProjection[5] * y + viewProjection[9] * z + viewProjection[13]) / w;
    if (!Number.isFinite(cx) || !Number.isFinite(cy)) return false;
    const px = (cx * 0.5 + 0.5) * width;
    const py = (cy * 0.5 + 0.5) * height;
    if (px < minX) minX = px;
    if (px > maxX) maxX = px;
    if (py < minY) minY = py;
    if (py > maxY) maxY = py;
    if (d < nearest) nearest = d;
  }
  // Whole pixels, and nothing off the grid: what is not tested is not hidden.
  const x0 = Math.floor(minX), x1 = Math.ceil(maxX), y0 = Math.floor(minY), y1 = Math.ceil(maxY);
  if (x0 < 0 || y0 < 0 || x1 > width || y1 > height || x0 >= x1 || y0 >= y1) return false;
  const limit = nearest - delta;
  // Blocks first: a block whose farthest depth passes needs no pixel read,
  // and one that fails settles the box at once.
  const { blocksWide, blockMax } = grid;
  for (let by = Math.floor(y0 / BLOCK); by * BLOCK < y1; by += 1) {
    for (let bx = Math.floor(x0 / BLOCK); bx * BLOCK < x1; bx += 1) {
      const farthest = blockMax[by * blocksWide + bx];
      const px0 = Math.max(x0, bx * BLOCK), px1 = Math.min(x1, (bx + 1) * BLOCK);
      const py0 = Math.max(y0, by * BLOCK), py1 = Math.min(y1, (by + 1) * BLOCK);
      if (farthest < limit) continue;
      const whole = px0 === bx * BLOCK && py0 === by * BLOCK && px1 === (bx + 1) * BLOCK && py1 === (by + 1) * BLOCK;
      if (whole) return false;
      for (let py = py0; py < py1; py += 1) {
        const row = py * width;
        for (let px = px0; px < px1; px += 1) {
          if (!(depth[row + px] < limit)) return false;
        }
      }
    }
  }
  return true;
}
