// SPDX-License-Identifier: Apache-2.0
// The script runner: a worker thread with its own kernel runs the script, the
// session publishes its snapshot, and a script that never returns is stopped.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { pavilionIfc } from "../../../viewer/test/fixture.mjs";
import { createEditingSession } from "../src/index.js";
import { createScriptRunner } from "../src/script-runner.js";

let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

const pkg = fileURLToPath(new URL("../../wasm/pkg-node/tessifc_wasm.js", import.meta.url));
if (!existsSync(pkg)) {
  console.log("skip  build the Node package first (python scripts/build-wasm.py --target both)");
  process.exit(0);
}
const { Kernel } = createRequire(import.meta.url)(pkg);

const DOOR = `
  const wall = selected;
  const solid = wall.Representation.Representations[0].Items[0];
  const opening = ifc.addBox("IfcOpeningElement", "Door opening", { at: [1, 0, 0], size: [0.9, solid.SweptArea.YDim + 0.1, 2.1], relativeTo: wall });
  const door = ifc.addBox("IfcDoor", "New door", { at: [1, 0, 0], size: [0.9, 0.05, 2.1], relativeTo: wall });
  ifc.void(wall, opening);
  ifc.fill(opening, door);
  ifc.contain(door, ifc.container(wall));
  print(door.id);
`;

function openSession(kernel) {
  const modelId = kernel.openModel(Buffer.from(pavilionIfc()));
  const session = createEditingSession(kernel, modelId, { settings: { includeOpenings: true } });
  session.evaluate();
  return { modelId, session };
}

assert.throws(() => createScriptRunner({}), /kernel module/);
assert.throws(() => createScriptRunner({ kernelModule: pkg, timeoutMs: -1 }), /timeoutMs/);
ok(true, "the runner refuses a missing kernel module and a negative limit");

// The same script through the runner and in this thread gives the same report and delta.
const kernel = new Kernel();
const { modelId, session } = openSession(kernel);
const twin = openSession(new Kernel());
const wallId = kernel.getIdsOfType(modelId, "IfcWall")[0];
const runner = createScriptRunner({ kernelModule: pkg, timeoutMs: 5000 });
const started = performance.now();
const isolated = await session.runScriptWith(runner, DOOR, { ids: [wallId] });
const elapsedMs = performance.now() - started;
const direct = twin.session.runScript(DOOR, { ids: [wallId] });
ok(isolated.report.ok && isolated.report.changed && isolated.report.loaded === true && !("snapshot" in isolated.report),
  `the worker loaded the model and ran the script (${elapsedMs.toFixed(0)} ms)`);
ok(isolated.report.stdout === direct.report.stdout && JSON.stringify(isolated.report.operations) === JSON.stringify(direct.report.operations),
  "the isolated report matches the in-thread one");
ok(isolated.delta.kind === "selective" && isolated.delta.revision === "1" && isolated.delta.affectedProducts.length === 3
  && JSON.stringify(isolated.delta.affectedProducts) === JSON.stringify(direct.delta.affectedProducts),
  "the session published the worker's snapshot as the same selective delta");
// Generated GlobalIds differ between the two runs; everything else is the same text.
const masked = (bytes) => Buffer.from(bytes).toString("latin1").replace(/'[0-9A-Za-z_$]{22}'/g, "'GUID'");
ok(masked(session.export()) === masked(twin.session.export()), "the committed files are identical apart from generated GlobalIds");
ok(session.history.undo === 1 && runner.warm, "the run is in the undo history and the worker stays warm");

// The worker holds the committed snapshot, so the next run does not reload.
const second = await session.runScriptWith(runner, 'print(ifc.byType("IfcDoor").length)', null);
ok(second.report.ok && second.report.stdout === "1" && second.report.loaded === false && second.delta === null,
  "a read-only follow-up sees the door without reloading and publishes nothing");

// A changing script that is not committed leaves the model alone.
const peek = await session.runScriptWith(runner, 'selected.Name = "peek"; print("changed")', { ids: [wallId] }, { commit: false });
ok(peek.report.ok && peek.report.changed === false && peek.delta === null && session.revision === "1", "commit: false discards the edits");

// A throwing script fails without a timeout.
const failing = await session.runScriptWith(runner, "selected.Name = 'x'; nope();", { ids: [wallId] });
ok(!failing.report.ok && /ReferenceError/.test(failing.report.error) && !failing.report.timedOut && failing.delta === null && session.revision === "1",
  "a throwing script is ok: false without timedOut and publishes nothing");

// A script that never returns is stopped by ending the worker.
const short = createScriptRunner({ kernelModule: pkg, timeoutMs: 500 });
const before = session.export();
const loopStarted = performance.now();
const looping = await session.runScriptWith(short, "while (true) {}", null);
const loopMs = performance.now() - loopStarted;
ok(!looping.report.ok && looping.report.timedOut === true && /ScriptTimeout/.test(looping.report.error) && looping.report.changed === false,
  `an endless script resolves timedOut (${loopMs.toFixed(0)} ms)`);
ok(loopMs < 5000 && looping.delta === null && session.revision === "1" && Buffer.compare(Buffer.from(session.export()), Buffer.from(before)) === 0,
  "the stop came within the limit plus the worker start, and the revision is unchanged");
ok(!short.warm, "the stopped worker is gone");
const after = await session.runScriptWith(short, 'print(ifc.byType("IfcWall").length)', null);
ok(after.report.ok && after.report.stdout === "2" && after.report.loaded === true && short.warm, "the next run starts a fresh worker and works");

// An abort signal stops a script the same way.
const controller = new AbortController();
const aborting = session.runScriptWith(short, "while (true) {}", null, { signal: controller.signal });
setTimeout(() => controller.abort(), 100);
const aborted = await aborting;
ok(!aborted.report.ok && aborted.report.aborted === true && !aborted.report.timedOut && /AbortError/.test(aborted.report.error), "an abort signal cancels the script");

// Runs are serialised on one worker.
const race = await Promise.all([
  session.runScriptWith(runner, "print(1)", null),
  session.runScriptWith(runner, "print(2)", null),
  session.runScriptWith(runner, "print(3)", null),
]);
ok(race.map((item) => item.report.stdout).join("") === "123" && race.every((item) => item.report.ok), "concurrent calls run one after another");

// A closed runner refuses work and dispose does not hang.
await runner.dispose();
await short.dispose();
await assert.rejects(() => session.runScriptWith(runner, "print(1)", null), /disposed/);
ok(!runner.warm && !short.warm, "dispose ends the workers and further runs are refused");

session.close();
twin.session.close();
kernel.free();
console.log(`PASS ${passed} checks`);
