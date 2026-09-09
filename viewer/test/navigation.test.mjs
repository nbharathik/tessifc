// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { frameSphere, targetPlaneAnchor, wheelZoomFactor, zoomCamera } from "../src/navigation.js";

const camera = () => ({ mode: "perspective", position: [0, 0, 10], target: [0, 0, 0], distance: 10, orthoScale: 4, fov: Math.PI / 3 });
const close = (a, b) => assert(Math.abs(a - b) < 1e-9, `${a} differs from ${b}`);
const project = (point, c) => point.slice(0, 2).map((value, i) => (value - c.position[i]) / (c.position[2] - point[2]));
assert.equal(wheelZoomFactor(0), 1);
assert.equal(wheelZoomFactor(NaN), 1);
close(wheelZoomFactor(1, 1), wheelZoomFactor(16));
close(wheelZoomFactor(1, 2, 400), wheelZoomFactor(400));
assert(wheelZoomFactor(-1) > wheelZoomFactor(-100));
const c = camera(), anchor = [2, -1, 3], before = project(anchor, c);
assert(zoomCamera(c, 0.5, anchor));
project(anchor, c).forEach((value, i) => close(value, before[i]));
close(c.distance, 5);
for (let step = 0; step < 60; step++) zoomCamera(c, 0.5, anchor);
close(c.distance, 0.002);
assert(c.position.every(Number.isFinite));
assert.equal(zoomCamera(c, -1, anchor), false);
assert.equal(zoomCamera(c, Infinity, anchor), false);

const ortho = { ...camera(), mode: "top" };
const orthoBefore = anchor.slice(0, 2).map((value, i) => (value - ortho.target[i]) / ortho.orthoScale);
zoomCamera(ortho, 0.5, anchor);
anchor.slice(0, 2).forEach((value, i) => close((value - ortho.target[i]) / ortho.orthoScale, orthoBefore[i]));
close(ortho.position[2], 10);
assert.deepEqual(targetPlaneAnchor(camera(), { origin: [1, 2, 9], direction: [0, 0, -1] }), [1, 2, 0]);
assert.deepEqual(targetPlaneAnchor(camera(), { origin: [1, 2, 9], direction: [1, 0, 0] }), [0, 0, 0]);
const portrait = frameSphere(2, Math.PI / 3, 0.5);
const landscape = frameSphere(2, Math.PI / 3, 2);
assert(portrait.distance > landscape.distance);
close(portrait.orthoScale, landscape.orthoScale * 2);
assert(frameSphere(0.005, Math.PI / 3, 1).distance < 0.02);
console.log("PASS  cursor anchors, wheel units, zoom limits and portrait framing");
