// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { findContestedTriangles, planeKey } from "../src/depth-planes.js";

/** A unit box as 12 triangles, corners at `origin` plus `size`. */
function box() {
  const positions = new Float32Array([
    0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0,
    0, 0, 1, 1, 0, 1, 1, 1, 1, 0, 1, 1,
  ]);
  const indices = new Uint32Array([
    0, 2, 1, 0, 3, 2, // bottom, z = 0
    4, 5, 6, 4, 6, 7, // top, z = 1
    0, 1, 5, 0, 5, 4, // front, y = 0
    2, 3, 7, 2, 7, 6, // back, y = 1
    0, 4, 7, 0, 7, 3, // left, x = 0
    1, 2, 6, 1, 6, 5, // right, x = 1
  ]);
  return { positions, indices };
}

function translation(x, y, z) {
  return [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, y, z, 1];
}

/** A pack with one box geometry placed once per `placements`, coloured by `colors`. */
function pack(placements, colors) {
  const count = placements.length;
  const transforms = new Float32Array(count * 16);
  const colorBytes = new Uint8Array(count * 4);
  placements.forEach((offset, record) => {
    transforms.set(translation(...offset), record * 16);
    colorBytes.set(colors[record], record * 4);
  });
  return {
    instances: { count, transforms, colors: colorBytes, geometryIds: new Uint32Array(count).fill(1), active: null },
  };
}

const geometries = new Map([[1, box()]]);
const red = [200, 40, 40, 255];
const blue = [40, 40, 200, 255];
const glass = [40, 40, 200, 90];

// Two boxes side by side share the face at x = 1: both its triangles on each side.
{
  const result = findContestedTriangles(pack([[0, 0, 0], [1, 0, 0]], [red, blue]), geometries);
  assert.deepEqual([...result.records], [0, 1]);
  assert.deepEqual([...result.offsets], [0, 2, 4]);
  assert.deepEqual([...result.triangles], [10, 11, 8, 9], "the right face of the first box and the left face of the second");
  assert.equal(result.pairs, 1);
}

// A gap wider than the tolerance, the same colour, or a translucent partner: nothing contested.
{
  assert.equal(findContestedTriangles(pack([[0, 0, 0], [1.01, 0, 0]], [red, blue]), geometries).records.length, 0);
  assert.equal(findContestedTriangles(pack([[0, 0, 0], [1, 0, 0]], [red, red]), geometries).records.length, 0);
  assert.equal(findContestedTriangles(pack([[0, 0, 0], [1, 0, 0]], [red, glass]), geometries).records.length, 0);
}

// Stacked boxes share the plane z = 1; a third box touching only along an edge does not count.
{
  const result = findContestedTriangles(pack([[0, 0, 0], [0, 0, 1], [1, 1, 0]], [red, blue, blue]), geometries);
  assert.deepEqual([...result.records], [0, 1]);
  assert.deepEqual([...result.triangles], [2, 3, 0, 1]);
}

// Half-overlapping boxes share a plane with overlapping extents: still the whole face, since a triangle is atomic.
{
  const result = findContestedTriangles(pack([[0, 0, 0], [0.5, 0, 1]], [red, blue]), geometries);
  assert.deepEqual([...result.records], [0, 1]);
  assert.equal(result.triangles.length, 4);
}

// Past the pair-test budget the analysis reports that it gave up rather than a half answer.
{
  const result = findContestedTriangles(pack([[0, 0, 0], [1, 0, 0], [0, 0, 1]], [red, blue, blue]), geometries, { maxPairTests: 1 });
  assert.equal(result.exhausted, true);
  assert.equal(result.records.length, 0, "an exhausted analysis names no records, so a caller cannot mistake it for a refined one");
  assert.deepEqual([...result.offsets], [0]);
}

// Both sides of a face share one key; a degenerate triangle has none; a shifted plane differs.
{
  const key = planeKey(0, 0, 0, 1, 0, 0, 0, 1, 0);
  assert.equal(planeKey(0, 0, 0, 0, 1, 0, 1, 0, 0), key, "winding does not change the key");
  assert.ok(Number.isNaN(planeKey(0, 0, 0, 1, 0, 0, 2, 0, 0)), "a sliver has no plane");
  assert.notEqual(planeKey(0, 0, 0.01, 1, 0, 0.01, 0, 1, 0.01), key, "a centimetre apart is another plane");
}

console.log("PASS  contested triangles: shared faces only, colour and translucency rules, plane keys");
