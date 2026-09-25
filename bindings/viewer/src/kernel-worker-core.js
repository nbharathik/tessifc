// SPDX-License-Identifier: Apache-2.0

//! The kernel worker's body: parses, tessellates and packs off the page
//! thread, streams IGP chunks and runs the editing session. The reference
//! application's worker and `createViewer`'s worker mode share it.

import { findContestedTriangles } from "./depth-planes.js";
import { lockdownScriptScope, scriptRefusal } from "./script-lockdown.js";

/** Helper geometry stays in the pack so a host can reveal it without converting again. */
export const GEOMETRY_SETTINGS = {
  includeSpaces: true,
  includeOpenings: true,
  includeAnnotations: true,
  includeReferences: true,
};

// A small first chunk for an early paint; later chunks grow to amortise overhead.
const FIRST_CHUNK_MS = 45;
const CHUNK_MS = 220;
// Checked between products; an indivisible product can exceed this budget.
const CHUNK_TRIANGLES = 600_000;

/**
 * The `@tessifc/edit` functions the worker runs the session with; an entry
 * imports them from wherever the package sits and hands them over, since a
 * module worker resolves no bare specifier of its own.
 * @typedef {object} WorkerEdit
 * @property {typeof import("@tessifc/edit/session").createEditingSession} createEditingSession
 * @property {typeof import("@tessifc/edit/script-engine").createScriptEngine} createScriptEngine
 * @property {typeof import("@tessifc/edit/script-engine").runScript} runScript
 * @property {typeof import("@tessifc/edit/describe").storeysOf} storeysOf
 * @property {typeof import("@tessifc/edit/describe").lengthUnitOf} lengthUnitOf
 */

/**
 * Run the kernel in this worker. `glue` is the imported `@tessifc/core/web`
 * module (its default export initialises the WASM); `settings` are the
 * geometry settings a `convert` message merges its own over. Once the kernel
 * is loaded, `lockdown` (on by default) takes the network, module loading and
 * code generation away from the worker's global so scripts cannot send the
 * model anywhere; a host that needs the network in this worker later turns
 * it off. Posts `ready` with the kernel version, or `boot-error`.
 * @param {{ glue: { default: () => Promise<unknown>, Kernel: new () => any, version: () => string, simplifyMesh?: (positions: Float32Array, indices: Uint32Array, settings?: string) => Uint32Array | undefined }, edit: WorkerEdit, settings?: Record<string, unknown>, scope?: any, lockdown?: boolean }} options
 */
export async function startKernelWorker({ glue, edit, settings = GEOMETRY_SETTINGS, scope = self, lockdown = true }) {
  /** @type {any} */
  let kernel = null;
  /** @type {number | undefined} */
  let activeModelId;
  // The editing session over the active model: every revision, undo and redo goes through it.
  /** @type {any} */
  let session = null;
  // The settings the active model was evaluated with; its session uses the same ones.
  let activeSettings = settings;
  // Only a worker's global is locked, never a page's.
  const workerGlobal = /** @type {any} */ (globalThis).WorkerGlobalScope;
  const locking = lockdown && typeof workerGlobal === "function" && globalThis instanceof workerGlobal;
  // Globals the lockdown could not remove; scripts are refused while any remain.
  let lockdownGaps = [];
  const post = (message, transfer) => scope.postMessage(message, transfer);

  scope.addEventListener("message", ({ data }) => {
    if (!kernel || !data?.type) return;
    if (data.type === "convert") convert(data);
    else if (data.type === "entity-info") inspectEntity(data);
    else if (data.type === "edit") editEntity(data);
    else if (data.type === "set-attributes") setAttributes(data);
    else if (data.type === "update-revision") updateRevision(data);
    else if (data.type === "run-script") runScript(data);
    else if (data.type === "script-history") scriptHistory(data);
    else if (data.type === "reopen") reopenModel(data);
    else if (data.type === "export") exportModel(data);
    else if (data.type === "query") query(data);
    else if (data.type === "mesh-levels") meshLevels(data);
    else if (data.type === "contested-triangles") contestedTriangles(data);
    else if (data.type === "close") closeActiveModel();
  });

  try {
    await glue.default();
    kernel = new glue.Kernel();
    // The realm's own global, whatever channel `scope` is; no message is handled before this.
    if (locking) lockdownGaps = lockdownScriptScope(globalThis);
    post({ type: "ready", version: glue.version(), streaming: typeof kernel.beginGeometryStream === "function" });
  } catch (error) {
    post({ type: "boot-error", message: readableError(error) });
  }

  function convert({ jobId, buffer, settings: overrides }) {
    let modelId;
    const previousModelId = activeModelId;
    const started = performance.now();
    const geometrySettings = { ...settings, ...(overrides ?? {}) };

    try {
      postPhase(jobId, "Parsing IFC", "Building the compact schema-aware model image and editable source map.");
      const parseStarted = performance.now();
      // The local view is dropped once the model is open so evaluation does not
      // run against a third live copy of a large file.
      let source = new Uint8Array(buffer);
      modelId = kernel.openModel(source);
      source = null;
      const parseMs = performance.now() - parseStarted;
      const info = JSON.parse(kernel.getModelInfo(modelId));

      if (!info.entities) {
        const diagnostics = JSON.parse(kernel.getDiagnostics(modelId) ?? "[]");
        // Without a diagnostic the file parsed and simply holds nothing.
        if (!diagnostics[0]?.message) throw Object.assign(new Error("No IFC entities were found in this file."), { empty: true });
        throw new Error(diagnostics[0].message);
      }

      // Keep helper geometry in the pack so the viewer can reveal each group
      // without converting the IFC again. The main thread hides it initially.
      const settingsJson = JSON.stringify(geometrySettings);
      const outcome =
        typeof kernel.beginGeometryStream === "function"
          ? streamGeometry(jobId, modelId, settingsJson)
          : wholeGeometry(jobId, modelId, settingsJson);
      const hierarchy = JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');

      // The model stays open for inspection and edits.
      if (previousModelId !== undefined) kernel.closeModel(previousModelId);
      activeModelId = modelId;
      activeSettings = geometrySettings;
      session?.close();
      session = edit.createEditingSession(kernel, modelId, { settings: activeSettings });
      const message = {
        type: "result",
        jobId,
        modelId,
        revision: kernel.getModelRevision?.(modelId) ?? "0",
        info,
        facts: modelFacts(),
        summary: outcome.summary,
        hierarchy,
        streamed: outcome.streamed,
        timings: {
          parseMs,
          geometryMs: outcome.geometryMs,
          packMs: outcome.packMs,
          firstChunkMs: outcome.firstChunkMs,
          chunks: outcome.chunks,
          workerMs: performance.now() - started,
        },
      };
      if (outcome.pack) {
        message.pack = outcome.pack.buffer;
        post(message, [outcome.pack.buffer]);
      } else {
        post(message);
      }
    } catch (error) {
      if (modelId !== undefined) kernel.closeModel(modelId);
      activeModelId = previousModelId;
      post({ type: "conversion-error", jobId, message: readableError(error), empty: Boolean(error?.empty) });
    }
  }

  /** Which triangles share a plane with another product, for the renderer's tie-break overlay. */
  function contestedTriangles({ requestId, modelId, instances, geometries }) {
    const started = performance.now();
    try {
      const byId = new Map(geometries.map((geometry) => [geometry.id, geometry]));
      const result = findContestedTriangles({ instances }, byId);
      post(
        { type: "contested-triangles", requestId, modelId, records: result.records, offsets: result.offsets,
          triangles: result.triangles, pairs: result.pairs, exhausted: result.exhausted, elapsedMs: performance.now() - started },
        [result.records.buffer, result.offsets.buffer, result.triangles.buffer],
      );
    } catch (error) {
      post({ type: "contested-triangles", requestId, modelId, error: readableError(error) });
    }
  }

  /**
   * Coarse levels of the meshes a host sends (`{ id, positions, indices }`
   * each), computed as the kernel's `lodLevels` setting would, posted back
   * with their index arrays transferred.
   */
  function meshLevels({ requestId, modelId, geometries, chordToleranceM, levels: wanted }) {
    const started = performance.now();
    try {
      if (typeof glue.simplifyMesh !== "function") throw new Error("this kernel build cannot simplify meshes");
      const count = Math.min(2, Math.max(1, Number(wanted) || 1));
      const levels = [];
      const transfer = [];
      for (const mesh of geometries ?? []) {
        let indices = mesh.indices;
        for (let level = 1; level <= count; level += 1) {
          const coarse = glue.simplifyMesh(mesh.positions, indices, JSON.stringify({ chordToleranceM, level }));
          if (!coarse) break;
          levels.push({ of: mesh.id, level, indices: coarse });
          transfer.push(coarse.buffer);
          indices = coarse;
        }
      }
      post({ type: "mesh-levels", requestId, modelId, levels, elapsedMs: performance.now() - started }, transfer);
    } catch (error) {
      post({ type: "mesh-levels", requestId, modelId, levels: [], error: readableError(error) });
    }
  }

  /** Evaluate in batches, posting each as an IGP chunk as soon as it exists. */
  function streamGeometry(jobId, modelId, settingsJson) {
    const started = performance.now();
    const summary = JSON.parse(kernel.beginGeometryStream(modelId, settingsJson));
    postPhase(jobId, "Tessellating geometry", `0 of ${summary.products.toLocaleString()} products`, {
      done: 0,
      total: summary.products,
    });
    let budget = FIRST_CHUNK_MS;
    let chunks = 0;
    let firstChunkMs = null;
    let packMs = 0;
    let chunk;
    while ((chunk = kernel.nextGeometryChunk(modelId, budget, 0, CHUNK_TRIANGLES))) {
      const packStarted = performance.now();
      const progress = JSON.parse(kernel.streamProgress(modelId));
      chunks += 1;
      if (firstChunkMs === null) firstChunkMs = performance.now() - started;
      post(
        { type: "chunk", jobId, modelId, buffer: chunk.buffer, progress, elapsedMs: performance.now() - started },
        [chunk.buffer],
      );
      packMs += performance.now() - packStarted;
      budget = Math.min(CHUNK_MS, budget * 2);
    }
    const progress = JSON.parse(kernel.streamProgress(modelId));
    return {
      summary: {
        ...summary,
        products: progress.emitted,
        triangles: progress.triangles,
        diagnostics: progress.diagnostics ?? 0,
        diagnosticInfos: progress.diagnosticInfos ?? 0,
        diagnosticWarnings: progress.diagnosticWarnings ?? 0,
        diagnosticErrors: progress.diagnosticErrors ?? 0,
      },
      streamed: true,
      pack: null,
      chunks,
      firstChunkMs,
      geometryMs: performance.now() - started - packMs,
      packMs,
    };
  }

  /** The one-pack path, for a kernel build without streaming. */
  function wholeGeometry(jobId, modelId, settingsJson) {
    postPhase(jobId, "Tessellating geometry", "Evaluating products and origin-shifting GPU positions.");
    const geometryStarted = performance.now();
    const summary = JSON.parse(kernel.evaluateGeometry(modelId, settingsJson));
    const geometryMs = performance.now() - geometryStarted;

    postPhase(jobId, "Packing geometry", "Deduplicating meshes and attaching IFC class labels.");
    const packStarted = performance.now();
    // takePack moves the geometry into the pack instead of copying it.
    const consumesGeometry = typeof kernel.takePack === "function";
    const pack = consumesGeometry ? kernel.takePack(modelId) : kernel.getPack(modelId);
    const packMs = performance.now() - packStarted;
    if (!pack) throw new Error("The geometry kernel did not return an IGP pack.");
    // An older kernel build without takePack still holds the geometry.
    if (!consumesGeometry) kernel.releaseGeometry(modelId);
    return { summary, streamed: false, pack, chunks: 1, firstChunkMs: geometryMs + packMs, geometryMs, packMs };
  }

  function inspectEntity({ requestId, modelId, expressId }) {
    try {
      ensureActive(modelId);
      const info = kernel.getEntityInfo(modelId, expressId);
      if (!info) throw new Error(`IFC entity #${expressId} is not available.`);
      post({ type: "entity-info", requestId, modelId, expressId, info: JSON.parse(info) });
    } catch (error) {
      post({ type: "entity-error", requestId, expressId, message: readableError(error) });
    }
  }

  /** A read-only session question, answered from the kernel as the session would. */
  function query({ requestId, modelId, method, args }) {
    try {
      ensureActive(modelId);
      const list = Array.isArray(args) ? args : [];
      let result;
      switch (method) {
        case "hierarchy": result = session.hierarchy(); break;
        case "info": result = session.info(); break;
        case "entity": result = session.entity(Number(list[0])); break;
        case "classDefinition": result = session.classDefinition(String(list[0])); break;
        case "idsOfType": result = session.idsOfType(String(list[0])); break;
        case "diagnostics": result = session.diagnostics(); break;
        case "facts": result = modelFacts(); break;
        default: throw new Error(`The worker answers no "${method}" query.`);
      }
      post({ type: "query-result", requestId, modelId, method, result });
    } catch (error) {
      post({ type: "query-error", requestId, modelId, method, message: readableError(error) });
    }
  }

  function editEntity({ requestId, modelId, expressId, changes, patch }) {
    const edits = (changes ?? []).map((change) => ({ ...change, expressId }));
    publishRequest({ requestId, modelId, patch, expressId, changed: edits.length, attributeEdit: true, run: (s) => ({ delta: s.setAttributes(edits) }) });
  }

  /** Attribute edits on any number of entities, each carrying its own express id. */
  function setAttributes({ requestId, modelId, edits, patch }) {
    const list = Array.isArray(edits) ? edits : [];
    publishRequest({ requestId, modelId, patch, changed: list.length, attributeEdit: true, run: (s) => ({ delta: s.setAttributes(list) }) });
  }

  function updateRevision({ requestId, modelId, patch, buffer }) {
    publishRequest({ requestId, modelId, patch, run: (s) => ({ delta: s.applySnapshot(new Uint8Array(buffer)) }) });
  }

  /** Run a browser script; its edits become one snapshot revision. */
  function runScript({ requestId, modelId, source, selection, patch, commit = true }) {
    const started = performance.now();
    publishRequest({ requestId, modelId, patch, run: (s) => {
      const text = String(source ?? "");
      let refused = null;
      if (locking) {
        refused = lockdownGaps.length
          ? `ScriptRefused: scripts are off because this browser kept ${lockdownGaps.join(", ")} in the worker.`
          : scriptRefusal(text);
      }
      if (refused) {
        post({ type: "script-finished", requestId, modelId });
        const operations = { created: 0, modified: 0, deleted: 0 };
        return { delta: null, script: { ok: false, stdout: "", error: refused, traceback: "", changed: false, operations, elapsedMs: performance.now() - started } };
      }
      const engine = edit.createScriptEngine(kernel, modelId);
      const report = edit.runScript(engine, text, selection);
      // The main thread's time limit covers the script; publishing is the kernel's bounded work.
      post({ type: "script-finished", requestId, modelId });
      report.elapsedMs = performance.now() - started;
      if (!report.ok || !report.changed || !commit) {
        if (!commit) report.changed = false;
        return { delta: null, script: report };
      }
      return { delta: s.applySnapshot(engine.snapshotBytes()), script: report };
    } });
  }

  /**
   * Open the committed source again after the worker that held the model was
   * ended; the main thread keeps its scene and the session adopts it.
   */
  function reopenModel({ requestId, buffer, settings: overrides }) {
    let modelId;
    try {
      if (overrides) activeSettings = { ...settings, ...overrides };
      modelId = kernel.openModel(new Uint8Array(buffer));
      const info = JSON.parse(kernel.getModelInfo(modelId));
      if (!info.entities) {
        const diagnostics = JSON.parse(kernel.getDiagnostics(modelId) ?? "[]");
        throw new Error(diagnostics[0]?.message ?? "No IFC entities were found in the file.");
      }
      closeActiveModel();
      activeModelId = modelId;
      session = edit.createEditingSession(kernel, modelId, { settings: activeSettings });
      const hierarchy = JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');
      post({ type: "reopened", requestId, modelId, revision: kernel.getModelRevision?.(modelId) ?? "0", info, facts: modelFacts(), hierarchy });
    } catch (error) {
      if (modelId !== undefined && modelId !== activeModelId) kernel.closeModel(modelId);
      post({ type: "reopen-error", requestId, message: readableError(error) });
    }
  }

  /** Undo or redo the last change by publishing the stored source as a new revision. */
  function scriptHistory({ requestId, modelId, action, patch }) {
    const started = performance.now();
    const available = action === "redo" ? session?.history.redo : session?.history.undo;
    if (!available) {
      post({ type: "script-error", requestId, modelId, message: `Nothing to ${action}.`, history: historyCounts() });
      return;
    }
    publishRequest({ requestId, modelId, patch, run: (s) => {
      const delta = action === "redo" ? s.redo() : s.undo();
      const script = { ok: true, stdout: "", changed: true, operations: { created: 0, modified: 0, deleted: 0 }, label: action, elapsedMs: performance.now() - started };
      return { delta, script };
    } });
  }

  /**
   * Publish one change through the session and report it. `run` returns the
   * delta (null when a script changed nothing) and, for scripts, the report.
   */
  function publishRequest({ requestId, modelId, patch, run, expressId = null, changed = 0, attributeEdit = false }) {
    const started = performance.now();
    let script = null;
    try {
      ensureActive(modelId);
      if (!patch || typeof patch.baseRevision !== "string") throw new Error("The update needs its base revision.");
      if (patch.baseRevision !== session.revision) {
        throw new Error(`The update is based on revision ${patch.baseRevision}, but the model is at ${session.revision}.`);
      }
      // The main thread's assembler allocates geometry ids; the session works in its frame.
      session.adopt({ modelOffset: patch.modelOffset, nextGeometryId: patch.firstGeometryId });
      const outcome = run(session);
      script = outcome.script ?? null;
      if (!outcome.delta) {
        post({ type: "script-result", requestId, modelId, result: script, history: historyCounts() });
        return true;
      }
      const delta = outcome.delta;
      const infoStarted = performance.now();
      const info = JSON.parse(kernel.getModelInfo(modelId));
      // The selected element's attributes ride along so the inspector never shows a loading gap.
      const infoId = expressId ?? patch.selectedExpressId ?? null;
      const entityInfo = infoId == null ? null : JSON.parse(kernel.getEntityInfo(modelId, infoId) ?? "null");
      // The committed source rides along so the main thread can reopen it without this worker.
      const snapshot = kernel.exportModel(modelId);
      const message = {
        type: "revision-result", requestId, modelId, baseRevision: delta.baseRevision, revision: delta.revision, impact: delta.impact, info,
        facts: modelFacts(), hierarchy: delta.hierarchy, expressId, entityInfo, infoId, changed, attributeEdit, script, history: historyCounts(),
        timings: { ...delta.timings, preparedMs: delta.timings.prepareMs, infoMs: performance.now() - infoStarted, workerMs: performance.now() - started },
        buffer: delta.chunk.buffer,
        snapshot: snapshot ? snapshot.buffer : null,
      };
      post(message, snapshot ? [delta.chunk.buffer, snapshot.buffer] : [delta.chunk.buffer]);
      return true;
    } catch (error) {
      post({ type: "revision-error", requestId, modelId, expressId, committed: Boolean(error?.committed), script, history: historyCounts(),
        revision: error?.revision ?? kernel.getModelRevision?.(modelId), message: readableError(error) });
      return false;
    }
  }

  function historyCounts() {
    return session?.history ?? { undo: 0, redo: 0 };
  }

  /** Storeys by class and the length unit, for the assistant's context; the hierarchy is empty until geometry exists. */
  function modelFacts() {
    if (!session) return { storeys: [], lengthUnit: null };
    try {
      return { storeys: edit.storeysOf(session), lengthUnit: edit.lengthUnitOf(session) };
    } catch {
      return { storeys: [], lengthUnit: null };
    }
  }

  function exportModel({ requestId, modelId }) {
    try {
      ensureActive(modelId);
      const bytes = kernel.exportModel(modelId);
      if (!bytes) throw new Error("The editable IFC source is no longer open.");
      post({ type: "export-result", requestId, modelId, buffer: bytes.buffer }, [bytes.buffer]);
    } catch (error) {
      post({ type: "export-error", requestId, message: readableError(error) });
    }
  }

  function ensureActive(modelId) {
    if (modelId !== activeModelId) throw new Error("The requested IFC model is no longer active.");
  }

  function closeActiveModel() {
    session?.close();
    session = null;
    if (activeModelId !== undefined) kernel.closeModel(activeModelId);
    activeModelId = undefined;
  }

  function postPhase(jobId, phase, detail, progress = null) {
    post({ type: "phase", jobId, phase, detail, progress });
  }
}

function readableError(error) {
  return error instanceof Error ? error.message : String(error);
}
