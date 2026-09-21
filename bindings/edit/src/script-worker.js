// SPDX-License-Identifier: Apache-2.0

//! The script worker: a kernel of its own over a copy of the model, so the
//! host can stop a script that never returns by ending this thread.

import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { parentPort, workerData } from "node:worker_threads";
import { createScriptEngine, runScript } from "./script-engine.js";

const { Kernel } = createRequire(import.meta.url)(workerData.kernelModule);
const kernel = new Kernel();
let modelId = null;
let held = null;

parentPort.on("message", (message) => {
  if (message?.type === "run") run(message);
  else if (message?.type === "close") close();
});

/** Open `bytes` as the current model; `hash` names it for later runs. */
function open(bytes, hash) {
  const id = kernel.openModel(bytes);
  const info = JSON.parse(kernel.getModelInfo(id));
  if (!info.entities) {
    const diagnostics = JSON.parse(kernel.getDiagnostics(id) ?? "[]");
    kernel.closeModel(id);
    throw new Error(diagnostics[0]?.message ?? "No IFC entities were found in the model.");
  }
  if (modelId !== null) kernel.closeModel(modelId);
  modelId = id;
  held = hash;
}

function run({ id, hash, buffer, source, selection, commit }) {
  let adopt = null;
  try {
    const loaded = held !== hash;
    if (loaded) {
      if (!buffer) throw new Error("The script worker holds another model.");
      open(new Uint8Array(buffer), hash);
    }
    const engine = createScriptEngine(kernel, modelId);
    // The host's time limit starts here: opening the model is the kernel's bounded work.
    parentPort.postMessage({ type: "started", id });
    const report = runScript(engine, source, selection);
    report.loaded = loaded;
    let snapshot = null;
    if (report.ok && report.changed && commit) {
      snapshot = engine.snapshotBytes();
      adopt = snapshot.slice();
    }
    parentPort.postMessage({ type: "result", id, report, snapshot }, snapshot ? [snapshot.buffer] : []);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const report = { ok: false, error: message, traceback: "", stdout: "", changed: false, operations: { created: 0, modified: 0, deleted: 0 } };
    parentPort.postMessage({ type: "result", id, report, snapshot: null });
  }
  // The host commits that snapshot next; holding it saves the following run a reload.
  if (adopt) {
    try {
      open(adopt, createHash("sha256").update(adopt).digest("hex"));
    } catch {
      held = null;
    }
  }
}

function close() {
  if (modelId !== null) kernel.closeModel(modelId);
  modelId = null;
  held = null;
  kernel.free?.();
  parentPort.close();
}
