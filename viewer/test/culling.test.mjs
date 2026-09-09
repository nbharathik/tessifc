// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { boxInView, viewSidePlanes } from "../src/culling.js";

const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
const planes = viewSidePlanes(identity, 1000, 1000);
assert.ok(boxInView([0, 0, 0], [.1, .1, .1], planes));
for (const center of [[2, 0, 0], [-2, 0, 0], [0, 2, 0], [0, -2, 0]]) {
  assert.equal(boxInView(center, [.1, .1, .1], planes), false);
}
assert.ok(boxInView([2, 0, 0], [1, .1, .1], planes), "intersecting boxes stay visible");
assert.ok(boxInView([1.001, 0, 0], [0, 0, 0], planes), "retain raster-edge margin");
assert.ok(boxInView([0, 0, 1000], [.1, .1, .1], planes), "GPU retains depth clipping and overlay decisions");
assert.ok(boxInView([NaN, 0, 0], [.1, .1, .1], planes), "uncertain bounds are retained");
assert.ok(boxInView(null, null, planes));
const perspective = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, -1, -1, 0, 0, -1, 0];
viewSidePlanes(perspective, 1000, 1000, planes);
assert.ok(boxInView([0, 0, -5], [1, 1, 1], planes));
assert.equal(boxInView([10, 0, -5], [1, 1, 1], planes), false);
assert.ok(boxInView([0, 0, 0], [10, 10, 10], planes), "a box surrounding the camera cannot be culled");
console.log("ok    conservative lateral view culling");
