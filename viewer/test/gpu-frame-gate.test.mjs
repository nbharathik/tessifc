// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { createGpuFrameGate } from "../src/gpu-frame-gate.js";
import { IfcRenderer } from "../src/renderer.js";

function harness({ app = false, supported = true, idleGpuLimit = 2 } = {}) {
  let time = 0, serial = 0, latest = 0, deletes = 0, flushes = 0, lost = false;
  const frames = new Map(), timers = new Map(), syncs = [], waits = [], submitted = [], appTasks = [], overlays = [], commands = [];
  const gl = {
    SYNC_GPU_COMMANDS_COMPLETE: 1, ALREADY_SIGNALED: 2, CONDITION_SATISFIED: 3, TIMEOUT_EXPIRED: 4, WAIT_FAILED: 5,
    FRAMEBUFFER: 1, COLOR_BUFFER_BIT: 1, DEPTH_BUFFER_BIT: 2, STENCIL_BUFFER_BIT: 4,
    fenceSync() { const sync = { ready: false, deleted: false }; syncs.push(sync); commands.push("fence"); return sync; },
    clientWaitSync(sync, flags, timeout) {
      waits.push({ flags, timeout, time });
      return sync.failed ? this.WAIT_FAILED : sync.ready ? this.ALREADY_SIGNALED : this.TIMEOUT_EXPIRED;
    },
    deleteSync(sync) { assert.equal(sync.deleted, false, "each completion marker is released once"); sync.deleted = true; deletes++; },
    flush() { flushes++; commands.push("flush"); },
    finish() { assert.fail("frame pacing must never wait synchronously for GPU completion"); },
    isContextLost: () => lost,
    bindFramebuffer() {}, viewport() {}, clear() {},
  };
  if (!supported) { delete gl.fenceSync; delete gl.clientWaitSync; }
  const renderer = {
    gl, canvas: { width: 640, height: 480 }, gpuPacingWidth: 0, gpuPacingHeight: 0,
    dirty: true, contextLost: false, resizeDirty: false, resizeSettleTimer: 0, devicePixelRatio: 1,
    interactionScale: 1, interacting: false, streaming: false, idleGpuLimit, selected: [], pack: null,
    drag: null, pointers: new Map(), wheelQualityTimer: 0,
    ensureRenderTarget: () => false, updateCameraMatrices() {}, applyLod: () => false, adaptMotionScale() {},
    chooseGestureScale: IfcRenderer.prototype.chooseGestureScale, adaptiveResolution: false, motionStep: 0, motionSteady: true,
    draw() { commands.push("draw"); submitted.push({ latest, selected: this.selected.slice(), width: this.canvas.width, frameWidth: this.target?.frameWidth ?? this.canvas.width, interacting: this.interacting }); },
    render: IfcRenderer.prototype.render,
    onFrameReady: app ? () => appTasks.push(() => { renderer.render(); overlays.push(submitted.at(-1)?.latest); }) : null,
  };
  renderer.gpuPacing = createGpuFrameGate(gl, () => {
    if (!renderer.dirty || renderer.contextLost) return;
    if (renderer.onFrameReady) renderer.onFrameReady();
    else renderer.render();
  }, {
    requestFrame: (fn) => { frames.set(++serial, fn); return serial; }, cancelFrame: (id) => frames.delete(id),
    setTimer: (fn, delay) => { timers.set(++serial, { fn, delay }); return serial; }, clearTimer: (id) => timers.delete(id),
    now: () => time,
  });
  return { renderer, gate: renderer.gpuPacing, gl, frames, timers, syncs, waits, submitted, appTasks, overlays, commands,
    setTime(value) { time = value; }, update(value) { latest = value; renderer.dirty = true; renderer.render(); },
    select(record) { renderer.selected = [record]; renderer.dirty = true; renderer.render(); },
    setLost(value) { lost = value; renderer.contextLost = value; },
    complete() { for (const sync of syncs) sync.ready = true; },
    timer() { timers.values().next().value?.fn(); }, frame() { frames.values().next().value?.(); },
    drainApp() { while (appTasks.length) appTasks.shift()(); }, deletes: () => deletes, flushes: () => flushes };
}

const previousWindow = globalThis.window;
globalThis.window = { devicePixelRatio: 1 };
try {
  const h = harness();
  h.renderer.render(); h.update(1);
  for (let camera = 2; camera <= 5; camera++) h.update(camera);
  assert.deepEqual(h.submitted.map((item) => item.latest), [0, 1]);
  assert.equal(h.gate.info().limit, 2); assert.equal(h.gate.info().peak, 2);
  assert.equal(h.frames.size, 1); assert.equal(h.timers.size, 1);
  h.renderer.interacting = false;
  h.complete(); h.setTime(16); h.timer();
  assert.deepEqual(h.submitted.map((item) => item.latest), [0, 1, 5]);
  assert.equal(h.renderer.dirty, false);
  assert.equal(h.frames.size + h.timers.size, 0, "idle frames schedule no polling");
  assert.ok(h.waits.every(({ flags, timeout }) => flags === 0 && timeout === 0));
  assert.deepEqual(h.commands, Array.from({ length: 3 }, () => ["draw", "fence", "flush"]).flat());
  const idleFlushes = h.flushes(), idlePolls = h.gate.info().polls;
  h.renderer.render(); h.renderer.render();
  assert.equal(h.flushes(), idleFlushes, "clean frames submit no commands");
  assert.equal(h.gate.info().polls, idlePolls, "clean frames do not poll the GPU");

  h.update(6); h.update(7);
  const staleFrame = h.frames.values().next().value;
  const staleTimer = h.timers.values().next().value.fn;
  const beforeForce = h.deletes();
  h.renderer.render(true);
  assert.ok(h.deletes() > beforeForce);
  assert.equal(h.gate.info().pending, 1);
  h.update(8); h.update(9);
  const beforeStale = h.submitted.length, activeTimer = h.timers.keys().next().value;
  staleFrame(); staleTimer();
  assert.equal(h.submitted.length, beforeStale);
  assert.equal(h.timers.keys().next().value, activeTimer, "stale callbacks cannot cancel a newer retry");
  h.complete(); h.setTime(32); h.timer();
  assert.equal(h.submitted.at(-1).latest, 9);

  h.update(10); h.update(11);
  h.renderer.canvas.width = 800;
  h.renderer.render();
  assert.equal(h.submitted.at(-1).width, 800, "a reset backing store renders immediately despite pending work");
  assert.equal(h.renderer.gpuPacingWidth, 800);
  assert.equal(h.gate.info().pending, 1);

  const a = harness({ app: true });
  a.renderer.render(); a.update(1); a.update(2);
  a.complete(); a.setTime(16); a.timer();
  assert.equal(a.submitted.at(-1).latest, 1, "retry requests the application frame instead of rendering alone");
  assert.equal(a.appTasks.length, 1);
  a.drainApp();
  assert.equal(a.submitted.at(-1).latest, 2);
  assert.equal(a.overlays.at(-1), 2, "the final admitted frame also updates overlays");

  const quality = harness();
  quality.renderer.interactionScale = .5;
  quality.renderer.interacting = true;
  quality.renderer.ensureRenderTarget = () => {
    quality.renderer.target = { frameWidth: quality.renderer.interacting ? 320 : 640, frameHeight: 480, framebuffer: {} };
    return true;
  };
  quality.renderer.presentTarget = () => true;
  quality.renderer.render(); quality.update(1);
  quality.renderer.interacting = false; quality.update(2);
  assert.equal(quality.submitted.at(-1).frameWidth, 320);
  quality.complete(); quality.setTime(16); quality.timer();
  assert.deepEqual(quality.submitted.map((item) => item.frameWidth), [320, 320, 640]);
  assert.equal(quality.renderer.dirty, false, "the deferred release restores the full target");

  const presented = harness();
  presented.renderer.target = { frameWidth: 640, frameHeight: 480, framebuffer: {} };
  presented.renderer.ensureRenderTarget = () => true;
  presented.renderer.presentTarget = () => { presented.commands.push("present"); return true; };
  presented.renderer.render();
  assert.deepEqual(presented.commands, ["draw", "present", "fence", "flush"], "the completion marker includes presentation commands");

  const fallback = harness();
  fallback.renderer.target = { key: "unsupported", frameWidth: 640, frameHeight: 480, framebuffer: {} };
  fallback.renderer.targetFailures = new Set();
  fallback.renderer.ensureRenderTarget = () => Boolean(fallback.renderer.target);
  fallback.renderer.presentTarget = () => { fallback.commands.push("failed-present"); return false; };
  fallback.renderer.deleteRenderTarget = () => { fallback.renderer.target = null; };
  fallback.renderer.render();
  assert.deepEqual(fallback.commands, ["draw", "failed-present", "draw", "fence", "flush"]);
  assert.equal(fallback.gate.info().pending, 1, "presentation fallback fences only the successful frame");
  assert.equal(fallback.renderer.dirty, false);

  const cleared = harness();
  cleared.renderer.render(); cleared.update(1); cleared.update(2);
  const pendingBeforeClear = cleared.gate.info().pending;
  cleared.gate.cancelRetry();
  assert.equal(cleared.frames.size + cleared.timers.size, 0);
  assert.equal(cleared.gate.info().pending, pendingBeforeClear, "model clearing retains fences for earlier GPU work");
  cleared.complete(); cleared.setTime(16); cleared.update(3);
  assert.equal(cleared.submitted.at(-1).latest, 3);

  const slow = harness();
  slow.renderer.render(); slow.update(1); slow.update(2);
  slow.setTime(350); slow.timer();
  assert.equal(slow.frames.size, 0, "long waits back off without requesting every animation frame");
  assert.ok(slow.timers.values().next().value.delay > 16);
  const polls = slow.gate.info().polls;
  slow.setTime(351); slow.update(3);
  assert.equal(slow.gate.info().polls, polls, "input during backoff adds no GL poll");
  slow.setTime(2050); slow.timer();
  assert.equal(slow.submitted.at(-1).latest, 3);
  assert.equal(slow.gate.info().watchdogTrips, 1);
  assert.equal(slow.gate.info().failed, true);
  assert.equal(slow.frames.size + slow.timers.size, 0);

  const idle = harness();
  idle.renderer.render(); idle.complete(); idle.setTime(5000); idle.update(1);
  assert.equal(idle.gate.info().failed, false, "an old completed fence does not trip the watchdog");
  idle.update(2);
  const beforeLoss = idle.deletes();
  const oldCallback = idle.timers.values().next().value?.fn;
  idle.setLost(true);
  idle.gate.reset(true);
  assert.equal(idle.deletes(), beforeLoss);
  assert.equal(idle.gate.allow(), false);
  const drawsBeforeLoss = idle.submitted.length, flushesBeforeLoss = idle.flushes();
  idle.renderer.render(true);
  assert.equal(idle.submitted.length, drawsBeforeLoss);
  assert.equal(idle.flushes(), flushesBeforeLoss);
  idle.setLost(false);
  idle.gate.reset(); idle.update(3);
  oldCallback?.();
  assert.equal(idle.submitted.at(-1).latest, 3);
  idle.syncs.at(-1).failed = true; idle.setTime(5016); idle.update(4);
  assert.equal(idle.gate.info().failed, true);
  assert.equal(idle.submitted.at(-1).latest, 4);

  const off = harness();
  off.renderer.render(); off.update(1); off.update(2);
  off.gate.setLimit(0); off.timer();
  assert.equal(off.submitted.at(-1).latest, 2);
  assert.equal(off.gate.info().pending, 0);
  off.gate.setLimit(1); off.update(3); off.update(4);
  const canceled = off.timers.values().next().value.fn;
  off.gate.dispose(); canceled();
  assert.equal(off.gate.allow(true), false);
  assert.equal(off.frames.size + off.timers.size, 0);
  assert.equal(off.gate.info().pending, 0);
  assert.equal(off.submitted.at(-1).latest, 3);

  const discrete = harness({ idleGpuLimit: 1 });
  discrete.renderer.render();
  discrete.renderer.interacting = true;
  discrete.renderer.drag = { moved: false };
  discrete.renderer.pointers.set(1, { x: 0, y: 0 });
  discrete.select(10); discrete.select(20); discrete.select(30);
  assert.equal(discrete.submitted.length, 1, "rapid selections retain one pending frame during a stationary pointer press");
  assert.equal(discrete.gate.info().pending, 1);
  assert.equal(discrete.timers.size, 1);
  discrete.syncs[0].ready = true; discrete.setTime(16); discrete.timer();
  assert.deepEqual(discrete.submitted.at(-1).selected, [30]);
  assert.equal(discrete.renderer.dirty, false);
  assert.equal(discrete.frames.size + discrete.timers.size, 0);

  discrete.renderer.drag.moved = true; discrete.update(1); discrete.update(2);
  assert.equal(discrete.submitted.at(-1).latest, 1, "motion retains room for a second pending frame");
  assert.equal(discrete.gate.info().pending, 2);
  discrete.renderer.interacting = false; discrete.renderer.drag = null; discrete.renderer.pointers.clear(); discrete.select(40);
  discrete.syncs[1].ready = true; discrete.setTime(32); discrete.timer();
  assert.equal(discrete.submitted.at(-1).latest, 2, "release retains the motion allowance until outstanding work drains");
  assert.deepEqual(discrete.submitted.at(-1).selected, [40]);
  assert.equal(discrete.renderer.dirty, false);
  assert.equal(discrete.gate.info().pending, 2);
  discrete.select(50);
  discrete.syncs[2].ready = true; discrete.setTime(48); discrete.timer();
  assert.equal(discrete.submitted.at(-1).latest, 2, "the final camera is retained across the limit transition");
  assert.deepEqual(discrete.submitted.at(-1).selected, [50], "the final selection is retained independently of camera changes");
  assert.equal(discrete.renderer.dirty, false);
  assert.equal(discrete.frames.size + discrete.timers.size, 0);

  discrete.complete(); discrete.setTime(64); discrete.select(55); discrete.select(56);
  assert.deepEqual(discrete.submitted.at(-1).selected, [55], "a new idle burst returns to one pending frame after the queue drains");
  assert.equal(discrete.gate.info().pending, 1);

  discrete.renderer.streaming = true; discrete.update(3); discrete.update(4);
  assert.equal(discrete.submitted.at(-1).latest, 3, "streaming also retains two pending frames");
  assert.equal(discrete.gate.info().pending, 2);
  discrete.renderer.streaming = false; discrete.renderer.idleGpuLimit = 2; discrete.select(60);
  discrete.syncs[5].ready = true; discrete.setTime(80); discrete.timer();
  assert.equal(discrete.submitted.at(-1).latest, 4, "a two-frame idle setting admits when only the oldest marker completes");
  assert.deepEqual(discrete.submitted.at(-1).selected, [60]);

  discrete.renderer.idleGpuLimit = 1; discrete.select(70);
  const staleIdleRetry = discrete.timers.values().next().value.fn;
  discrete.renderer.render(true);
  assert.deepEqual(discrete.submitted.at(-1).selected, [70], "force redraw bypasses the idle limit");
  assert.equal(discrete.gate.info().pending, 1);
  discrete.select(80); discrete.renderer.canvas.width = 800; discrete.renderer.render();
  assert.deepEqual(discrete.submitted.at(-1).selected, [80]);
  assert.equal(discrete.submitted.at(-1).width, 800, "a cleared backing store bypasses the idle limit");
  const idleDraws = discrete.submitted.length;
  staleIdleRetry();
  assert.equal(discrete.submitted.length, idleDraws);

  discrete.select(90); discrete.gate.setLimit(0); discrete.setTime(96); discrete.timer();
  assert.deepEqual(discrete.submitted.at(-1).selected, [90], "disabling pacing releases a deferred idle selection");
  assert.equal(discrete.gate.info().pending, 0);
  assert.equal(discrete.frames.size + discrete.timers.size, 0);
  discrete.gate.setLimit(2); discrete.select(100); discrete.select(110);
  const disposedIdleRetry = discrete.timers.values().next().value.fn;
  discrete.gate.dispose(); disposedIdleRetry();
  assert.deepEqual(discrete.submitted.at(-1).selected, [100]);
  assert.equal(discrete.gate.info().pending, 0);
  assert.equal(discrete.frames.size + discrete.timers.size, 0);

  const bounded = harness({ idleGpuLimit: 8 });
  bounded.renderer.render(); bounded.select(10); bounded.select(20);
  assert.equal(bounded.gate.info().pending, 2, "a requested limit cannot exceed the configured maximum");
  assert.equal(bounded.gate.info().peak, 2);
  assert.deepEqual(bounded.submitted.at(-1).selected, [10]);
  bounded.complete(); bounded.setTime(16); bounded.timer();
  bounded.gate.setLimit(1); bounded.renderer.interacting = true; bounded.update(1);
  assert.equal(bounded.gate.info().pending, 1, "a smaller configured maximum still applies during motion");
  assert.equal(bounded.submitted.at(-1).latest, 0);
  bounded.complete(); bounded.setTime(32); bounded.timer();
  assert.equal(bounded.submitted.at(-1).latest, 1);

  for (const motion of ["pinch", "wheel"]) {
    const gesture = harness({ idleGpuLimit: 1 });
    gesture.renderer.render();
    gesture.renderer.interacting = true; gesture.renderer.drag = { moved: false };
    gesture.renderer.pointers.set(1, { x: 0, y: 0 });
    if (motion === "pinch") gesture.renderer.pointers.set(2, { x: 1, y: 0 });
    else gesture.renderer.wheelQualityTimer = 1;
    gesture.update(1); gesture.update(2);
    assert.equal(gesture.submitted.at(-1).latest, 1, `${motion} retains motion admission while a pointer is held`);
    assert.equal(gesture.gate.info().pending, 2);
    gesture.gate.dispose();
    assert.equal(gesture.frames.size + gesture.timers.size, 0);
  }

  const requestedLimits = [[undefined, 2], [NaN, 2], [Infinity, 2], [-Infinity, 2], [1.9, 1], [.25, 1], [2.75, 2], [0, 1], [-3, 1], [8, 2]];
  for (const configured of [1, 2]) for (const [requested, expected] of requestedLimits) {
    const limits = harness();
    limits.gate.setLimit(configured);
    for (let attempt = 0; attempt < 5; attempt++) {
      if (limits.gate.allow(false, requested)) limits.gate.committed();
    }
    assert.equal(limits.gate.info().pending, Math.min(configured, expected), `requested ${String(requested)} preserves configured admission ${configured}`);
    assert.ok(limits.gate.info().peak <= configured, "invalid requests cannot accumulate completion markers");
    assert.equal(limits.gate.allow(false, 1), false, "a later valid request remains bounded after an invalid one");
    limits.gate.dispose();
    assert.equal(limits.frames.size + limits.timers.size, 0);
  }
  const invalidOff = harness();
  invalidOff.gate.setLimit(0);
  for (let attempt = 0; attempt < 3; attempt++) {
    assert.equal(invalidOff.gate.allow(false, NaN), true, "disabled pacing ignores per-frame requests");
    invalidOff.gate.committed();
  }
  assert.equal(invalidOff.gate.info().pending, 0);
  assert.equal(invalidOff.frames.size + invalidOff.timers.size, 0);
  invalidOff.gate.dispose();

  const unsupported = harness({ supported: false });
  unsupported.renderer.render(); unsupported.update(1); unsupported.update(2);
  assert.equal(unsupported.gate.info().supported, false);
  assert.deepEqual(unsupported.submitted.map((item) => item.latest), [0, 1, 2]);
  assert.equal(unsupported.frames.size + unsupported.timers.size, 0);
  assert.equal(unsupported.flushes(), 0);

  const allocation = harness();
  allocation.renderer.render();
  allocation.gl.fenceSync = () => null;
  allocation.update(1); allocation.update(2);
  assert.deepEqual(allocation.submitted.map((item) => item.latest), [0, 1, 2]);
  assert.equal(allocation.gate.info().failed, true, "failed marker creation leaves rendering available");
  assert.equal(allocation.gate.info().pending, 0);
  assert.equal(allocation.deletes(), 1);
  assert.equal(allocation.frames.size + allocation.timers.size, 0);
  for (const item of [h, a, quality, presented, fallback, cleared, slow, idle, off, discrete, bounded, unsupported, allocation]) item.gate.dispose();
  console.log("PASS  GPU frame pacing, idle/motion transitions, final camera and selection, quality, presentation and lifecycle");
} finally {
  if (previousWindow === undefined) delete globalThis.window;
  else globalThis.window = previousWindow;
}
