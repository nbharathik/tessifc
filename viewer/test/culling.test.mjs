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

// Projected-size level of detail, against the real method on a stub renderer.
const { IfcRenderer, DEFAULT_LOD_PIXELS } = await import("../src/renderer.js");
const box = (centre, half) => ({
  min: centre.map((value, axis) => value - half[axis]),
  max: centre.map((value, axis) => value + half[axis]),
});
const lodStub = (records) => {
  const uploads = [];
  const stub = {
    pack: { instances: { count: records.length } },
    visibilityTexture: {},
    recordLocations: records.map((record) => ({ bounds: record })),
    baseVisible: new Uint8Array(records.length).fill(255),
    visibility: new Uint8Array(records.length * 2),
    lodHidden: new Uint8Array(records.length),
    lodHiddenCount: 0,
    lodState: null,
    lodPixels: DEFAULT_LOD_PIXELS,
    visibilityVersion: 0,
    renderBounds: { center: [0, 0, 0], radius: 40 },
    target: null,
    canvas: { width: 1810, height: 1413 },
    camera: { mode: "perspective", fov: Math.PI * 42 / 180, orthoScale: 5, position: [0, 0, 90] },
    applyLod: IfcRenderer.prototype.applyLod,
    pixelsPerMetre: IfcRenderer.prototype.pixelsPerMetre,
    uploadVisibility() { this.visibilityVersion += 1; uploads.push(this.visibility.slice()); },
  };
  for (let record = 0; record < records.length; record += 1) stub.visibility[record * 2] = 255;
  return { stub, uploads };
};

// A 4 cm product 90 m away is well under two pixels; a 4 m one is not.
const { stub, uploads } = lodStub([box([0, 0, 0], [0.02, 0.02, 0.02]), box([0, 0, 0], [2, 2, 2])]);
assert.equal(stub.applyLod(), true);
assert.equal(stub.visibility[0], 0, "a product under two pixels is not drawn");
assert.equal(stub.visibility[2], 255, "a product wider than two pixels is drawn");
assert.equal(stub.lodHiddenCount, 1);
assert.equal(uploads.length, 1);
assert.equal(stub.applyLod(), false, "a settled camera re-uploads nothing");

// Selection always wins, so a tree click can never highlight nothing.
stub.visibility[1] = 255;
stub.lodState = null;
assert.equal(stub.applyLod(), true);
assert.equal(stub.visibility[0], 255, "a selected product is drawn however small");

// The caller's own hide is never overridden by the level of detail pass.
const hidden = lodStub([box([0, 0, 0], [2, 2, 2])]);
hidden.stub.baseVisible[0] = 0;
hidden.stub.visibility[0] = 0;
hidden.stub.applyLod();
assert.equal(hidden.stub.visibility[0], 0, "a hidden product stays hidden");

// Zero draws everything, and restores what the pass had dropped.
const off = lodStub([box([0, 0, 0], [0.02, 0.02, 0.02])]);
off.stub.applyLod();
assert.equal(off.stub.visibility[0], 0);
off.stub.lodPixels = 0;
off.stub.lodState = null;
assert.equal(off.stub.applyLod(), true);
assert.equal(off.stub.visibility[0], 255, "zero pixels restores every product");
assert.equal(off.stub.lodHiddenCount, 0);

// Orthographic size does not depend on distance.
const ortho = lodStub([box([0, 0, 0], [0.02, 0.02, 0.02])]);
ortho.stub.camera = { mode: "orthographic", fov: 1, orthoScale: 40, position: [0, 0, 90] };
ortho.stub.applyLod();
assert.equal(ortho.stub.visibility[0], 0);
console.log("ok    products under the projected-size threshold are left out");
