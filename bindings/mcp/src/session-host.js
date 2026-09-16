// SPDX-License-Identifier: Apache-2.0

//! The model behind the MCP server: one kernel, one editing session, the current
//! snapshot with its content version, the file it is saved to, and a scene
//! mirror for verification. Every change goes through the session.

import { createHash } from "node:crypto";
import { readFile, rename, unlink, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { createEditingSession } from "../../edit/src/session.js";
import { createModel } from "../../edit/src/create-model.js";
import { createSceneMirror, verifyRevision } from "../../edit/src/verify.js";
import { createScriptEngine } from "../../edit/src/script-engine.js";
import { JAVASCRIPT_EXAMPLES } from "../../edit/src/examples.js";
import { describeModel, describeSelection, lengthUnitOf, storeysOf } from "../../edit/src/describe.js";

export const GEOMETRY_SETTINGS = { includeSpaces: true, includeOpenings: true, includeAnnotations: true, includeReferences: true };
const RENAME_RETRIES = 3;

/** Another script is still running; the caller retries. */
export class SessionBusy extends Error {
  constructor(message = "A script is still running; wait for it to finish.") {
    super(message);
    this.name = "SessionBusy";
  }
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

/** Replace `target` atomically; a locked target on Windows is retried a few times. */
async function atomicWrite(target, bytes) {
  const temp = join(dirname(target), `.tessifc-${process.pid}-${Date.now()}.ifc`);
  await writeFile(temp, bytes);
  for (let attempt = 1; ; attempt += 1) {
    try {
      await rename(temp, target);
      return;
    } catch (error) {
      if ((error.code !== "EPERM" && error.code !== "EBUSY") || attempt >= RENAME_RETRIES) {
        await unlink(temp).catch(() => {});
        throw error;
      }
      await new Promise((done) => setTimeout(done, 50 * attempt));
    }
  }
}

/**
 * Create the host. `Kernel` is the Node kernel class; `save` writes the file
 * after every commit; `log` receives one line per event (stderr in the CLI).
 */
export function createModelHost({ Kernel, settings = GEOMETRY_SETTINGS, save = true, log = () => {}, version: engineVersion = "" } = {}) {
  if (!Kernel) throw new Error("createModelHost needs the kernel class.");
  const kernel = new Kernel();
  let modelId = null;
  let session = null;
  let mirror = null;
  let path = null;
  let name = null;
  let bytes = null;
  let version = null;
  let generation = 0;
  let busy = false;
  let selection = null;
  let applied = null;
  let savedVersion = null;
  const waiters = new Set();

  function requireSession() {
    if (!session) throw new Error("No model is open; call new_model or open_model first.");
  }

  function notify() {
    for (const waiter of waiters) waiter();
    waiters.clear();
  }

  function refreshSnapshot() {
    bytes = session.export();
    version = sha256(bytes);
    notify();
  }

  async function persist() {
    if (!save || !path || savedVersion === version) return savedVersion === version && Boolean(path);
    await atomicWrite(path, bytes);
    savedVersion = version;
    log(`saved ${name} at revision ${session.revision}`);
    return true;
  }

  function close() {
    session?.close();
    if (modelId !== null) kernel.closeModel(modelId);
    session = null;
    mirror = null;
    modelId = null;
    bytes = null;
    version = null;
    savedVersion = null;
    selection = null;
    applied = null;
  }

  /** Open model bytes; `label` names it and `file` is where commits are saved. */
  function openBytes(input, { name: label = "model.ifc", path: file = null } = {}) {
    const data = input instanceof Uint8Array ? input : new Uint8Array(input);
    const id = kernel.openModel(data);
    const info = JSON.parse(kernel.getModelInfo(id));
    if (!info.entities) {
      const diagnostics = JSON.parse(kernel.getDiagnostics(id) ?? "[]");
      kernel.closeModel(id);
      throw new Error(diagnostics[0]?.message ?? "No IFC entities were found in this file.");
    }
    close();
    modelId = id;
    session = createEditingSession(kernel, modelId, { settings, label });
    const initial = session.evaluate();
    mirror = createSceneMirror(initial.pack);
    path = file;
    name = label;
    generation += 1;
    refreshSnapshot();
    savedVersion = file ? version : null;
    log(`opened ${label}: ${info.entities} entities, ${initial.pack.instances.count} placed instances`);
    return { revision: session.revision, version, generation, info, products: initial.pack.instances.count };
  }

  async function openFile(file) {
    const target = resolve(file);
    const data = await readFile(target);
    return openBytes(new Uint8Array(data), { name: basename(target), path: target });
  }

  /** A model from nothing (see `createModel`); `path` is where it will be saved. */
  async function newModel(options = {}, { path: file = null, force = false } = {}) {
    if (session && !path && session.revision !== "0" && !force) {
      throw new Error("The current model is not saved to a file; pass force to drop its changes.");
    }
    const target = file ? resolve(file) : null;
    const created = createModel(options);
    const opened = openBytes(created, { name: target ? basename(target) : `${options.name ?? "New project"}.ifc`, path: target });
    savedVersion = null;
    await persist();
    return { ...opened, storeys: storeysOf(session) };
  }

  async function guard(work) {
    if (busy) throw new SessionBusy();
    busy = true;
    try {
      return await work();
    } finally {
      busy = false;
    }
  }

  async function published(delta) {
    mirror.applyDelta(delta);
    refreshSnapshot();
    const saved = await persist();
    return saved;
  }

  /** Run a script; a failing or read-only script publishes nothing. */
  async function run(source, scriptSelection = null, { commit = true, label = "script" } = {}) {
    requireSession();
    return guard(async () => {
      const started = performance.now();
      let outcome;
      try {
        outcome = session.runScript(String(source ?? ""), scriptSelection ?? selectionFor(), { commit });
      } catch (error) {
        if (error?.committed) {
          refreshSnapshot();
          await persist();
        }
        throw error;
      }
      const { report, delta } = outcome;
      report.elapsedMs = performance.now() - started;
      report.label = label;
      const saved = delta ? await published(delta) : false;
      return { report, delta, saved, version, revision: session.revision };
    });
  }

  async function restore(action) {
    requireSession();
    return guard(async () => {
      const started = performance.now();
      const delta = action === "redo" ? session.redo() : session.undo();
      const saved = await published(delta);
      const report = { ok: true, changed: true, stdout: "", operations: { created: 0, modified: 0, deleted: 0 }, label: action, elapsedMs: performance.now() - started };
      return { report, delta, saved, version, revision: session.revision };
    });
  }

  async function exportTo(file) {
    requireSession();
    const target = resolve(file);
    await atomicWrite(target, bytes);
    if (target === path) savedVersion = version;
    return { path: target, bytes: bytes.length, version };
  }

  function selectionFor() {
    return selection ? { ids: selection.ids ?? [], guids: selection.guids ?? [] } : null;
  }

  function snapshot() {
    requireSession();
    return { name, version, bytes, revision: session.revision, generation };
  }

  /** Resolve with the current version once it differs from `after`, or after `timeoutMs`. */
  function waitForChange(after, timeoutMs) {
    if (!session || version !== after || timeoutMs <= 0) return Promise.resolve(version);
    return new Promise((done) => {
      const timer = setTimeout(() => {
        waiters.delete(wake);
        done(version);
      }, timeoutMs);
      const wake = () => {
        clearTimeout(timer);
        done(version);
      };
      waiters.add(wake);
    });
  }

  function describe() {
    return {
      name: name ?? null,
      version,
      revision: session?.revision ?? null,
      generation,
      busy,
      undo: session?.history.undo ?? 0,
      redo: session?.history.redo ?? 0,
      capabilities: {
        authoring: session ? "javascript" : false,
        authoringError: session ? null : "No model is open.",
        engine: `tessifc-mcp ${engineVersion}`.trim(),
        assistant: null,
        selection: true,
        applied: true,
      },
      examples: JAVASCRIPT_EXAMPLES.map((example) => ({ title: example.title, source: example.source })),
      path,
      saved: savedVersion === version && Boolean(path),
    };
  }

  /** A read-only script API over the committed model, for queries. */
  function engine() {
    requireSession();
    return createScriptEngine(kernel, modelId).api;
  }

  function verify() {
    requireSession();
    return verifyRevision({ Kernel, session, mirror, settings });
  }

  return {
    get kernel() {
      return kernel;
    },
    get session() {
      return session;
    },
    get mirror() {
      return mirror;
    },
    get modelId() {
      return modelId;
    },
    get path() {
      return path;
    },
    get name() {
      return name;
    },
    get version() {
      return version;
    },
    get generation() {
      return generation;
    },
    get busy() {
      return busy;
    },
    get selection() {
      return selection;
    },
    set selection(value) {
      selection = value ? { ...value, reportedAt: new Date().toISOString() } : null;
    },
    get applied() {
      return applied;
    },
    set applied(value) {
      applied = value ? { ...value, reportedAt: new Date().toISOString() } : null;
    },
    settings: () => ({ ...settings }),
    openBytes,
    openFile,
    newModel,
    run,
    undo: () => restore("undo"),
    redo: () => restore("redo"),
    exportTo,
    snapshot,
    waitForChange,
    describe,
    engine,
    verify,
    context: (scriptSelection = null) => (session ? describeModel(session, { selection: scriptSelection ?? selectionFor() }) : "No model is open."),
    facts: () => (session ? { storeys: storeysOf(session), lengthUnit: lengthUnitOf(session), selection: describeSelection(session, selectionFor()) } : null),
    close,
    dispose() {
      close();
      kernel.free();
    },
  };
}
