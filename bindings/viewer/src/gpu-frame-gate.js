// SPDX-License-Identifier: Apache-2.0

/** Bound submitted GPU work while retaining the caller's latest dirty state. */
export function createGpuFrameGate(gl, retry, {
  requestFrame = requestAnimationFrame, cancelFrame = cancelAnimationFrame,
  setTimer = setTimeout, clearTimer = clearTimeout, now = () => performance.now(),
  limit = 2, watchdogMs = 2000,
} = {}) {
  const pending = [];
  const metrics = { admitted: 0, submitted: 0, deferred: 0, forced: 0, polls: 0, timeouts: 0, peak: 0, failures: 0, watchdogTrips: 0 };
  const supported = typeof gl.fenceSync === "function" && typeof gl.clientWaitSync === "function";
  const watchdog = Math.max(16, Number(watchdogMs) || 2000);
  let maximum = Math.min(2, Math.max(0, Math.floor(Number(limit) || 0)));
  let frame = null, timer = null, generation = 0, nextPollAt = 0;
  let paused = false, disposed = false, failed = false;
  let burstLimit = 0;
  const cancelRetry = () => {
    generation++;
    if (frame !== null) cancelFrame(frame);
    if (timer !== null) clearTimer(timer);
    frame = timer = null;
  };
  const clearFences = (lost = false) => {
    for (const item of pending) if (!lost) gl.deleteSync(item.sync);
    pending.length = 0;
    burstLimit = 0;
    nextPollAt = 0;
  };
  const scheduleRetry = () => {
    if (disposed || paused || frame !== null || timer !== null) return;
    const expected = ++generation;
    const run = () => {
      if (expected !== generation) return;
      cancelRetry();
      if (!disposed && !paused) retry();
    };
    const delay = Math.max(1, nextPollAt - now());
    if (delay <= 16) frame = requestFrame(run);
    timer = setTimer(run, delay);
  };
  const stopOnFailure = (expired = false) => {
    failed = true;
    metrics.failures++;
    if (expired) metrics.watchdogTrips++;
    clearFences(paused || Boolean(gl.isContextLost?.()));
    cancelRetry();
  };
  const poll = () => {
    const time = now();
    if (time < nextPollAt) return;
    while (pending.length) {
      metrics.polls++;
      const state = gl.clientWaitSync(pending[0].sync, 0, 0);
      if (state === gl.TIMEOUT_EXPIRED) {
        metrics.timeouts++;
        const age = time - pending[0].submittedAt;
        if (age >= watchdog) stopOnFailure(true);
        else nextPollAt = time + Math.min(100, 16 * (1 + Math.floor(age / 250)));
        return;
      }
      if (state !== gl.ALREADY_SIGNALED && state !== gl.CONDITION_SATISFIED) {
        stopOnFailure();
        return;
      }
      gl.deleteSync(pending.shift().sync);
    }
    nextPollAt = 0;
  };
  return {
    /** Explicit redraws replace old fence handles and bypass the queue limit. */
    allow(force = false, requestedLimit = maximum) {
      if (disposed || paused) return false;
      if (!maximum || !supported || failed) return true;
      const value = Number(requestedLimit);
      const requested = Number.isFinite(value) ? Math.min(maximum, Math.max(1, Math.floor(value))) : maximum;
      if (force) {
        metrics.forced++;
        clearFences();
        burstLimit = requested;
        cancelRetry();
        return true;
      }
      poll();
      if (failed) return true;
      // Keep motion admission until all earlier frame markers complete.
      burstLimit = pending.length ? Math.min(maximum, Math.max(burstLimit, requested)) : requested;
      if (pending.length >= burstLimit) {
        metrics.deferred++;
        scheduleRetry();
        return false;
      }
      metrics.admitted++;
      cancelRetry();
      return true;
    },
    /** Submit a completion marker without waiting for the GPU. */
    committed() {
      if (disposed || paused || !maximum || !supported || failed) return;
      const sync = gl.fenceSync(gl.SYNC_GPU_COMMANDS_COMPLETE, 0);
      if (!sync) { stopOnFailure(); return; }
      pending.push({ sync, submittedAt: now() });
      metrics.submitted++;
      metrics.peak = Math.max(metrics.peak, pending.length);
      gl.flush();
    },
    /** Zero disables pacing; one or two bounds the pending completion markers. */
    setLimit(value) {
      const waiting = frame !== null || timer !== null;
      maximum = Math.min(2, Math.max(0, Math.floor(Number(value) || 0)));
      if (!maximum) { clearFences(paused || Boolean(gl.isContextLost?.())); cancelRetry(); }
      if (waiting) scheduleRetry();
    },
    cancelRetry,
    /** Cancel retries and release markers; lost contexts require no GL calls. */
    reset(lost = false) {
      cancelRetry();
      clearFences(lost);
      paused = lost;
      failed = false;
    },
    dispose() {
      cancelRetry();
      clearFences(paused || Boolean(gl.isContextLost?.()));
      disposed = true;
    },
    info() { return { ...metrics, pending: pending.length, limit: maximum, burstLimit, supported, failed, paused, disposed }; },
  };
}
