// SPDX-License-Identifier: Apache-2.0

/** Queue separate event-loop tasks without depending on an animation callback. */
export function createTaskQueue() {
  const channel = new MessageChannel();
  const tasks = [];
  channel.port1.onmessage = () => tasks.shift()?.();
  return {
    post(callback) { tasks.push(callback); channel.port2.postMessage(null); },
    dispose() { tasks.length = 0; channel.port1.close(); channel.port2.close(); },
  };
}

/** Coalesce normal frames, but let input finish and overdue frames make progress. */
export function createFrameScheduler(draw, {
  requestFrame, cancelFrame, postTask, now, hidden,
}) {
  let pending = false, frame = null, version = 0, taskVersion = -1, requestedAt = 0;
  const cancel = () => {
    if (frame !== null) cancelFrame(frame);
    frame = null;
    pending = false;
  };
  const flush = (expected) => {
    if (!pending || expected !== version) return;
    cancel();
    draw();
  };
  return {
    request(urgent = false) {
      if (!pending) {
        pending = true;
        requestedAt = now();
        const expected = ++version;
        frame = requestFrame(() => flush(expected));
      }
      if ((urgent || hidden() || now() - requestedAt >= 50) && taskVersion !== version) {
        const expected = version;
        taskVersion = expected;
        postTask(() => flush(expected));
      }
    },
    cancel,
  };
}
