// SPDX-License-Identifier: Apache-2.0
// The cluster plan and the software occlusion test, without a GPU: every
// rule that keeps a cluster visible is exercised on hand-built scenes.

import assert from "node:assert/strict";
import { CLUSTER_VERTEX_TARGET, planClusters, visibleRuns } from "../src/clusters.js";
import {
  OCCLUSION_GRID_WIDTH, clusterHidden, createOccluderSelection, createOcclusionGrid, rasteriseOccluders,
} from "../src/occlusion.js";

// ------------------------------------------------------------ matrices

function lookAt(eye, target, up) {
  const f = normalize(sub(target, eye));
  const s = normalize(cross(f, up));
  const u = cross(s, f);
  // Column-major view matrix: rows are s, u, -f.
  return [
    s[0], u[0], -f[0], 0,
    s[1], u[1], -f[1], 0,
    s[2], u[2], -f[2], 0,
    -dot(s, eye), -dot(u, eye), dot(f, eye), 1,
  ];
}

function perspective(fov, aspect, near, far) {
  const f = 1 / Math.tan(fov / 2);
  return [f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) / (near - far), -1, 0, 0, (2 * far * near) / (near - far), 0];
}

function orthographic(halfWidth, halfHeight, near, far) {
  return [1 / halfWidth, 0, 0, 0, 0, 1 / halfHeight, 0, 0, 0, 0, -2 / (far - near), 0, 0, 0, -(far + near) / (far - near), 1];
}

function multiply(a, b) {
  const out = new Array(16).fill(0);
  for (let column = 0; column < 4; column += 1) for (let row = 0; row < 4; row += 1) {
    let sum = 0;
    for (let k = 0; k < 4; k += 1) sum += a[k * 4 + row] * b[column * 4 + k];
    out[column * 4 + row] = sum;
  }
  return out;
}

const sub = (a, b) => a.map((v, i) => v - b[i]);
const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const normalize = (a) => { const l = Math.hypot(...a); return a.map((v) => v / l); };

/** A camera at `eye` looking at `target`, z up, with its matrices. */
function camera(eye, target, { fov = Math.PI * 42 / 180, aspect = 1.5, near = 0.1, far = 200, ortho = null } = {}) {
  const view = lookAt(eye, target, [0, 0, 1]);
  const projection = ortho ? orthographic(ortho, ortho / aspect, near, far) : perspective(fov, aspect, near, far);
  return { view, viewProjection: multiply(projection, view), near };
}

/** Occluders from a list of quads (twelve numbers each; a triangle repeats its last corner), all of record 0 unless given. */
function occludersOf(quads, records = null) {
  const vertices = new Float32Array(quads.length * 12);
  quads.forEach((quad, at) => vertices.set(quad.length === 9 ? [...quad, ...quad.slice(6, 9)] : quad, at * 12));
  return { vertices, records: Uint32Array.from(records ?? quads.map(() => 0)), count: quads.length };
}

/** A vertical wall in the plane x = at, spanning y and z in [-size, size], as two triangles. */
function wall(at, size = 10) {
  return [
    [at, -size, -size, at, size, -size, at, size, size],
    [at, -size, -size, at, size, size, at, -size, size],
  ];
}

/** The same wall as the quad the selection would join the triangles into. */
function wallQuad(at, size = 10) {
  return [[at, -size, -size, at, size, -size, at, size, size, at, -size, size]];
}

const visible = () => true;
const delta = 0.01;

// ------------------------------------------------------------ clusters

{
  const items = Array.from({ length: 40 }, (_, at) => ({ vertices: 1000, indices: 1500, bounds: { min: [at, 0, 0], max: [at + 1, 1, 1] } }));
  const clusters = planClusters(items, (item) => item.vertices, (item) => item.indices, (item) => item.bounds, 5000);
  assert.equal(clusters.length, 8, "forty items of a thousand vertices make eight clusters of five");
  let next = 0, offset = 0;
  for (const cluster of clusters) {
    assert.equal(cluster.first, next, "clusters are contiguous");
    assert.equal(cluster.count, 5);
    assert.equal(cluster.indexOffset, offset, "index ranges follow the items");
    assert.equal(cluster.indexCount, 7500);
    assert.deepEqual(cluster.center, [cluster.first + 2.5, 0.5, 0.5]);
    assert.deepEqual(cluster.halfExtents, [2.5, 0.5, 0.5]);
    assert.ok(cluster.bounded);
    next += cluster.count;
    offset += cluster.indexCount;
  }
  const again = planClusters(items, (item) => item.vertices, (item) => item.indices, (item) => item.bounds, 5000);
  assert.deepEqual(again, clusters, "the plan is deterministic");
  const huge = planClusters([{ vertices: 100_000, indices: 9, bounds: null }], (i) => i.vertices, (i) => i.indices, (i) => i.bounds);
  assert.equal(huge.length, 1);
  assert.equal(huge[0].bounded, false, "an item without bounds leaves its cluster unbounded");
  const many = planClusters(Array.from({ length: 20_000 }, () => ({ vertices: 1, indices: 3, bounds: null })), (i) => i.vertices, (i) => i.indices, (i) => i.bounds, 1, 64);
  assert.ok(many.length <= 64, `the cluster count is capped by doubling the target (${many.length})`);
  assert.equal(planClusters([], () => 1, () => 3, () => null).length, 0);
  assert.ok(CLUSTER_VERTEX_TARGET > 0);
  assert.deepEqual(visibleRuns(Uint8Array.from([0, 0, 1, 0, 2, 2, 0])), [[0, 2], [3, 1], [6, 1]]);
  assert.deepEqual(visibleRuns(Uint8Array.from([1, 1])), []);
  assert.deepEqual(visibleRuns(Uint8Array.from([0, 0, 0])), [[0, 3]], "every cluster visible is one run");
  console.log("ok    clusters are contiguous runs with index ranges, bounds and a cap");
}

// ----------------------------------------------------------- occlusion

{
  // The camera at x = -20 looks down +x at a wall at x = 0; a box behind it at x = 5 is hidden.
  const cam = camera([-20, 0, 0], [0, 0, 0]);
  const grid = createOcclusionGrid(OCCLUSION_GRID_WIDTH, 1.5);
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(wall(0)), visible);
  assert.ok(grid.filled > 0 && grid.used === 2, `the wall fills the grid (${grid.filled} pixels)`);
  const seam = grid.depth[Math.floor(grid.height / 2) * grid.width + Math.floor(grid.width / 2)];
  assert.equal(seam, Infinity, "two triangles leave their shared diagonal uncovered, since neither covers those pixels whole");
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(wallQuad(0)), visible);
  assert.ok(Number.isFinite(grid.depth[Math.floor(grid.height / 2) * grid.width + Math.floor(grid.width / 2)]), "the joined quad covers the diagonal");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [5, 0, 0], [1, 1, 1], delta), true, "a box behind the wall is hidden");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [-5, 0, 0], [1, 1, 1], delta), false, "a box in front of the wall is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [0, 0, 0], [2, 1, 1], delta), false, "a box straddling the wall is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [0.5 + delta / 2, 0, 0], [0.5, 1, 1], delta), false, "a box within the bias of the wall is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [5, 0, 0], [1, 30, 1], delta), false, "a box reaching outside the grid is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [-19, 0, 0], [2, 1, 1], delta), false, "a box with a corner behind the near plane is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [5, Number.NaN, 0], [1, 1, 1], delta), false, "a box with a non-finite coordinate is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [5, 0, 0], [1, 1, 1], 100), false, "a bias larger than the gap keeps the box visible");
  console.log("ok    a wall hides a box behind it, and nothing in front, straddling, near the bias, off the grid or at the near plane");
}

{
  // An edge-on wall (in the plane y = 0, seen along y) covers no whole pixel.
  const cam = camera([-20, 0, 0], [0, 0, 0]);
  const grid = createOcclusionGrid(OCCLUSION_GRID_WIDTH, 1.5);
  const edgeOn = [[0, 0, -10, 10, 0, -10, 10, 0, 10], [0, 0, -10, 10, 0, 10, 0, 0, 10]];
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(edgeOn), visible);
  assert.equal(grid.filled, 0, "an edge-on wall fills nothing");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [5, 0, 0], [1, 1, 1], delta), false);
  // A wall with a vertex behind the near plane is skipped whole.
  const behind = [[-30, -10, -10, 0, 10, -10, 0, 10, 10]];
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(behind), visible);
  assert.equal(grid.used, 0, "an occluder with a vertex behind the near plane is not drawn");
  // A hidden record's occluders are skipped; a visible one's count.
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(wall(0), [7, 7]), (record) => record !== 7);
  assert.equal(grid.used, 0, "a hidden record occludes nothing");
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(wall(0), [7, 7]), (record) => record === 7);
  assert.equal(grid.used, 2);
  // A spent pixel budget stops the pass: at most one row past the budget is written.
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(wall(0)), visible, 1);
  assert.ok(grid.used === 1 && grid.filled <= grid.width, `a budget of one pixel draws one row of the first triangle (${grid.filled} pixels)`);
  const half = createOcclusionGrid(OCCLUSION_GRID_WIDTH, 1.5);
  rasteriseOccluders(half, cam.viewProjection, cam.view, cam.near, occludersOf(wall(0)), visible, 10_000);
  assert.ok(half.used === 1 && half.filled <= 10_000 + half.width, `a budget under the first triangle's area stops before the second (${half.filled} pixels)`);
  // Nearer occluders win a pixel; a farther one behind them changes nothing.
  const near = createOcclusionGrid(OCCLUSION_GRID_WIDTH, 1.5);
  rasteriseOccluders(near, cam.viewProjection, cam.view, cam.near, occludersOf([...wallQuad(2), ...wallQuad(0)]), visible);
  assert.equal(clusterHidden(near, cam.viewProjection, cam.view, cam.near, [1, 0, 0], [0.5, 1, 1], delta), true, "the nearer wall at x = 0 hides a box between the two walls");
  console.log("ok    edge-on, near-plane, hidden-record and budget cases keep everything visible; the nearest occluder wins");
}

{
  // Orthographic: depth comes from the view matrix, so the same wall hides the same box.
  const cam = camera([-20, 0, 0], [0, 0, 0], { ortho: 15 });
  const grid = createOcclusionGrid(OCCLUSION_GRID_WIDTH, 1.5);
  rasteriseOccluders(grid, cam.viewProjection, cam.view, cam.near, occludersOf(wallQuad(0)), visible);
  assert.ok(grid.filled > 0);
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [5, 0, 0], [1, 1, 1], delta), true, "orthographic: a box behind the wall is hidden");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [-5, 0, 0], [1, 1, 1], delta), false, "orthographic: a box in front is visible");
  assert.equal(clusterHidden(grid, cam.viewProjection, cam.view, cam.near, [-25, 0, 0], [1, 1, 1], delta), false, "orthographic: a box behind the camera is visible");
  // An empty grid hides nothing.
  const empty = createOcclusionGrid(OCCLUSION_GRID_WIDTH, 1.5);
  assert.equal(clusterHidden(empty, cam.viewProjection, cam.view, cam.near, [5, 0, 0], [1, 1, 1], delta), false);
  console.log("ok    orthographic views test depth through the view matrix");
}

{
  // Occluder selection: the largest opaque triangles, transformed and shifted, across idle steps.
  const square = (size) => ({
    positions: Float32Array.from([0, 0, 0, size, 0, 0, size, size, 0, 0, size, 0]),
    indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
  });
  const pack = {
    geometry: [{ id: 1, ...square(10) }, { id: 2, ...square(0.1) }, { id: 3, ...square(4) }],
    instances: {
      count: 4,
      geometryIds: Uint32Array.from([1, 2, 3, 3]),
      transforms: Float32Array.from([
        1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 100, 0, 0, 1,
        1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1,
        1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 5, 0, 1,
        1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 5, 1,
      ]),
      active: null,
    },
  };
  const colors = Uint8Array.from([200, 200, 200, 255, 200, 200, 200, 255, 200, 200, 200, 255, 90, 140, 200, 120]);
  const selection = createOccluderSelection(pack, colors, [100, 0, 0], { radius: 20, maxTriangles: 3 });
  let steps = 0;
  while (!selection.step(performance.now() + 1000)) steps += 1;
  const occluders = selection.result();
  assert.equal(occluders.count, 2, "three triangles fit the cap; the large square's two join into one quad, then the opaque medium triangle");
  assert.deepEqual([...occluders.records], [0, 2], "the translucent copy is out");
  const quad = [...occluders.vertices.subarray(0, 12)];
  const corners = new Set([0, 3, 6, 9].map((at) => quad.slice(at, at + 3).join(",")));
  assert.deepEqual([...corners].sort(), ["0,0,0", "0,10,0", "10,0,0", "10,10,0"], "the quad's corners are the square's, transformed and shifted by the render origin");
  const tri = [...occluders.vertices.subarray(12, 24)];
  assert.deepEqual(tri.slice(6, 9), tri.slice(9, 12), "a lone triangle repeats its last corner");
  const tiny = createOccluderSelection(pack, colors, [0, 0, 0], { radius: 20, maxTriangles: 8 });
  while (!tiny.step(Infinity));
  assert.ok(![...tiny.result().records].includes(1), "a triangle under the area threshold is not an occluder");
  const inactive = createOccluderSelection(pack, colors, [0, 0, 0], { radius: 20, isActive: (record) => record !== 0 });
  while (!inactive.step(Infinity));
  assert.ok(![...inactive.result().records].includes(0), "an inactive record contributes no occluder");
  console.log("ok    occluders are the largest opaque triangles in render space, chosen in resumable steps");
}

console.log("PASS  clusters and software occlusion");
