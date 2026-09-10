// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import {
  IfcRenderer, MOTION_SCALES, MOTION_SLOW_FRAMES, MOTION_SLOW_MS, MOTION_VERY_SLOW_MS,
} from "../src/renderer.js";

const motionStub = () => ({
  adaptiveResolution: true,
  interactionScale: 1,
  gestureScale: 1,
  motionStep: 0,
  slowFrames: 0,
  slowStep: 0,
  motionSteady: true,
  lastMotionFrameAt: 0,
  interacting: false,
  adaptMotionScale: IfcRenderer.prototype.adaptMotionScale,
  chooseGestureScale: IfcRenderer.prototype.chooseGestureScale,
  beginInteraction: IfcRenderer.prototype.beginInteraction,
  setAdaptiveResolution: IfcRenderer.prototype.setAdaptiveResolution,
});
/** Feed `count` frames of `period` and report the scale a gesture would use. */
const drive = (stub, period, count) => {
  let now = stub.lastMotionFrameAt || 1000;
  for (let frame = 0; frame < count; frame += 1) {
    now += period;
    stub.adaptMotionScale(now);
  }
  return stub.gestureScale;
};

// A scene holding 60 Hz never softens.
const fast = motionStub();
fast.beginInteraction();
assert.equal(drive(fast, 16.7, 12), 1, "a scene at 60 Hz keeps every pixel");
assert.equal(fast.motionStep, 0);

// A slow gesture drops one step, a very slow one drops two.
const slow = motionStub();
slow.beginInteraction();
assert.equal(drive(slow, MOTION_SLOW_MS + 12, 12), MOTION_SCALES[1]);
const crawling = motionStub();
crawling.beginInteraction();
assert.equal(drive(crawling, MOTION_VERY_SLOW_MS + 20, 12), MOTION_SCALES[2]);

// Within one gesture the scale only ever falls, so it cannot flicker.
assert.equal(drive(crawling, 16.7, 20), MOTION_SCALES[2], "a gesture never sharpens mid-drag");

// A gesture that went slow does not hand the next one a sharper start, or a
// heavy model would stutter at the beginning of every drag.
crawling.interacting = false;
crawling.beginInteraction();
assert.equal(crawling.gestureScale, MOTION_SCALES[2], "a slow gesture starts the next one where it left off");

// One gesture that never goes slow earns a step back, and so does the next.
assert.equal(drive(crawling, 16.7, 20), MOTION_SCALES[2], "a gesture never sharpens mid-drag either");
crawling.interacting = false;
crawling.beginInteraction();
assert.equal(crawling.gestureScale, MOTION_SCALES[1], "a steady gesture earns one step back");
drive(crawling, 16.7, 20);
crawling.interacting = false;
crawling.beginInteraction();
assert.equal(crawling.gestureScale, MOTION_SCALES[0], "two steady gestures reach full size again");

// One stalled frame among fast ones cannot soften the view.
const blip = motionStub();
blip.beginInteraction();
drive(blip, 16.7, 10);
drive(blip, 120, 1);
assert.equal(blip.gestureScale, 1, "a single stalled frame does not soften the view");
for (let frame = 1; frame < MOTION_SLOW_FRAMES; frame += 1) drive(blip, 120, 1);
assert.equal(blip.gestureScale, MOTION_SCALES[2], "a run of stalled frames does");

// Off pins every gesture to full size, and clears any step already taken.
const off = motionStub();
off.beginInteraction();
drive(off, MOTION_VERY_SLOW_MS + 20, 12);
assert.equal(off.motionStep, 2);
off.setAdaptiveResolution(false);
assert.equal(off.gestureScale, 1);
assert.equal(drive(off, MOTION_VERY_SLOW_MS + 20, 12), 1, "off never trades pixels for frames");

// A settled view is never measured, so the next gesture starts from nothing.
const settled = motionStub();
settled.beginInteraction();
drive(settled, 16.7, 3);
settled.interacting = false;
settled.adaptMotionScale(9999);
assert.equal(settled.lastMotionFrameAt, 0, "a settled view keeps no frame period");

// A caller's own reduced scale still wins over the adaptive one.
const pinned = motionStub();
pinned.interactionScale = 0.5;
pinned.beginInteraction();
assert.equal(drive(pinned, 16.7, 12), 0.5, "an explicit request is the ceiling");

// Zooming keeps the orbit target, so rotating after a zoom stays centred.
const zoomStub = {
  pack: {}, dirty: false, cameraTouched: false,
  camera: { mode: "perspective", position: [0, 0, 10], target: [1, 2, 3], distance: 10, orthoScale: 5 },
  zoomAt: IfcRenderer.prototype.zoomAt,
};
zoomStub.zoomAt(4);
assert.deepEqual(zoomStub.camera.target, [1, 2, 3], "zooming out leaves the orbit target alone");
assert.ok(zoomStub.camera.distance > 10, "zooming out moves the camera away");
zoomStub.zoomAt(0.25);
assert.deepEqual(zoomStub.camera.target, [1, 2, 3], "zooming in leaves the orbit target alone");

console.log("PASS  motion resolution steps, gesture recovery and target-preserving zoom");
