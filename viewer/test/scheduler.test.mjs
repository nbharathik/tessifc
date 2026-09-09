// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { createFrameScheduler } from "../src/scheduler.js";

let time = 0, hidden = false, draws = 0, nextId = 0;
const frames = new Map(), tasks = [];
const scheduler = createFrameScheduler(() => draws++, {
  requestFrame(callback) { const id = nextId++; frames.set(id, callback); return id; },
  cancelFrame(id) { frames.delete(id); },
  postTask(callback) { tasks.push(callback); },
  now: () => time,
  hidden: () => hidden,
});
const runFrame = () => { const [id, callback] = frames.entries().next().value; frames.delete(id); callback(); };
const runTasks = () => { while (tasks.length) tasks.shift()(); };
scheduler.request(); scheduler.request(); scheduler.request();
assert.equal(frames.size, 1);
assert.equal(tasks.length, 0);
runFrame(); assert.equal(draws, 1, "normal updates share one frame, including frame ID zero");
scheduler.request(); time = 80; scheduler.request();
runTasks(); assert.equal(draws, 2, "continued input flushes an overdue frame");
assert.equal(frames.size, 0);
scheduler.request(true); scheduler.request(true);
assert.equal(tasks.length, 1);
runFrame(); scheduler.request(); runTasks();
assert.equal(draws, 3, "a stale urgent task cannot consume a newer normal frame");
runFrame(); assert.equal(draws, 4);
hidden = true; scheduler.request(); runTasks();
assert.equal(draws, 5, "a hidden embedded view does not wait for animation callbacks");
scheduler.request(true); scheduler.cancel(); runTasks();
assert.equal(draws, 5, "cancelled work cannot redraw");
assert.equal(frames.size + tasks.length, 0, "idle scheduling creates no recurring work");
console.log("PASS  frame coalescing, urgent input, throttled callbacks and cancellation");
