// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { buildBoundsTree, queryBoundsTree } from "../src/picking.js";

function boxDistance(box, origin, direction) {
  let near = 0, far = Infinity;
  for (let axis = 0; axis < 3; axis++) {
    if (Math.abs(direction[axis]) < 1e-12) {
      if (origin[axis] < box.min[axis] || origin[axis] > box.max[axis]) return null;
      continue;
    }
    const first = (box.min[axis] - origin[axis]) / direction[axis];
    const second = (box.max[axis] - origin[axis]) / direction[axis];
    near = Math.max(near, Math.min(first, second));
    far = Math.min(far, Math.max(first, second));
    if (far < near) return null;
  }
  return near;
}

let seed = 731;
const random = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 2 ** 32);
const boxes = Array.from({ length: 2049 }, (_, index) => {
  const min = Array.from({ length: 3 }, () => random() * 100 - 50);
  return { min, max: min.map((value) => value + (index % 9 === 0 ? 0 : random() * 3)) };
});
const tree = buildBoundsTree(boxes.length, (index) => boxes[index]);
const all = boxes.map((_, index) => index);
const visible = (index) => index % 5 !== 0;
const exact = (ids, origin, direction) => ids
  .filter(visible)
  .map((index) => ({ index, distance: boxDistance(boxes[index], origin, direction) }))
  .filter((hit) => hit.distance !== null)
  .sort((a, b) => a.distance - b.distance || a.index - b.index);
for (let sample = 0; sample < 250; sample++) {
  const origin = Array.from({ length: 3 }, () => random() * 120 - 60);
  const direction = Array.from({ length: 3 }, (_, axis) => sample % 3 === axis ? 0 : random() * 2 - 1);
  const candidates = queryBoundsTree(tree, origin, direction);
  assert.deepEqual(exact(candidates, origin, direction), exact(all, origin, direction));
  assert.deepEqual(candidates, [...new Set(candidates)].sort((a, b) => a - b));
}

const boundary = [
  { min: [0, 0, 0], max: [1, 1, 0] },
  { min: [0, 0, -2], max: [1, 1, -1] },
  { min: [NaN, 0, 0], max: [1, 1, 1] },
  null,
];
const boundaryTree = buildBoundsTree(boundary.length, (index) => boundary[index], 1);
assert.deepEqual(queryBoundsTree(boundaryTree, [0, 0, 0], [0, 0, 1]), [0, 2]);
assert.deepEqual(queryBoundsTree(buildBoundsTree(0, () => null), [0, 0, 0], [0, 0, 1]), []);
for (const count of [1, 7, 8, 9, 17, 64, 65, 513]) {
  const repeated = buildBoundsTree(count, () => ({ min: [0, 0, 0], max: [1, 1, 1] }));
  assert.deepEqual(queryBoundsTree(repeated, [.5, .5, -1], [0, 0, 1]), Array.from({ length: count }, (_, index) => index));
}

const grid = [];
for (let row = 0; row < 32; row++) {
  for (let column = 0; column < 32; column++) {
    for (let layer = 0; layer < 3; layer++) {
      grid.push({ min: [column * 2, row * 2, layer], max: [column * 2 + 1, row * 2 + 1, layer + .25] });
    }
  }
}
// Coincident records preserve source order even when their hierarchy traversal differs.
grid.push(grid[0]);
const gridTree = buildBoundsTree(grid.length, (index) => grid[index]);
const gridIds = grid.map((_, index) => index);
function sectionHits(ids, x, y, section) {
  return ids.filter((index) => grid[index].max[2] >= section && boxDistance(grid[index], [x, y, -1], [0, 0, 1]) !== null)
    .sort((a, b) => grid[a].min[2] - grid[b].min[2] || a - b);
}
for (let sample = 0; sample < 80; sample++) {
  const x = Math.floor(random() * 32) * 2 + .25, y = Math.floor(random() * 32) * 2 + .25;
  const candidates = queryBoundsTree(gridTree, [x, y, -1], [0, 0, 1]);
  for (const section of [0, 1, 2, 3]) {
    assert.deepEqual(sectionHits(candidates, x, y, section), sectionHits(gridIds, x, y, section));
  }
  assert.ok(candidates.length < gridIds.length / 4, "spatially local rays reject unrelated records");
}
assert.deepEqual(sectionHits(queryBoundsTree(gridTree, [.25, .25, -1], [0, 0, 1]), .25, .25, 0), [0, gridIds.length - 1, 1, 2]);
console.log("ok    bounds hierarchy preserves exact candidates, ties, visibility and clipped hits");
