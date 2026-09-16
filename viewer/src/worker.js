// SPDX-License-Identifier: Apache-2.0

//! The geometry worker: parses, tessellates and packs off the main thread.
//! Streams IGP chunks for progressive display; older kernels get one pack.

import init, { Kernel, version } from "../../bindings/wasm/pkg/tessifc_wasm.js";
import { createEditingSession } from "../../bindings/edit/src/session.js";
import { lengthUnitOf, storeysOf } from "../../bindings/edit/src/describe.js";
import { findContestedTriangles } from "../../bindings/viewer/src/depth-planes.js";

let kernel;
let activeModelId;
// The editing session over the active model: every revision, undo and redo goes through it.
let session = null;
const GEOMETRY_SETTINGS = {
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

try {
  await init();
  kernel = new Kernel();
  self.postMessage({ type: "ready", version: version(), streaming: typeof kernel.beginGeometryStream === "function" });
} catch (error) {
  self.postMessage({ type: "boot-error", message: readableError(error) });
}

self.addEventListener("message", ({ data }) => {
  if (!kernel || !data?.type) return;
  if (data.type === "convert") convert(data);
  else if (data.type === "entity-info") inspectEntity(data);
  else if (data.type === "edit") editEntity(data);
  else if (data.type === "update-revision") updateRevision(data);
  else if (data.type === "run-script") runScript(data);
  else if (data.type === "script-history") scriptHistory(data);
  else if (data.type === "export") exportModel(data);
  else if (data.type === "contested-triangles") contestedTriangles(data);
  else if (data.type === "close") closeActiveModel();
});

function convert({ jobId, buffer }) {
  let modelId;
  const previousModelId = activeModelId;
  const started = performance.now();

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
      throw new Error(diagnostics[0]?.message ?? "No IFC entities were found in this file.");
    }

    // Keep helper geometry in the pack so the viewer can reveal each group
    // without converting the IFC again. The main thread hides it initially.
    const settings = JSON.stringify(GEOMETRY_SETTINGS);
    const outcome =
      typeof kernel.beginGeometryStream === "function"
        ? streamGeometry(jobId, modelId, settings)
        : wholeGeometry(jobId, modelId, settings);
    const hierarchy = JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');

    // The model stays open for inspection and edits.
    if (previousModelId !== undefined) kernel.closeModel(previousModelId);
    activeModelId = modelId;
    session?.close();
    session = createEditingSession(kernel, modelId, { settings: GEOMETRY_SETTINGS });
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
      self.postMessage(message, [outcome.pack.buffer]);
    } else {
      self.postMessage(message);
    }
  } catch (error) {
    if (modelId !== undefined) kernel.closeModel(modelId);
    activeModelId = previousModelId;
    self.postMessage({ type: "conversion-error", jobId, message: readableError(error) });
  }
}

/** Which triangles share a plane with another product, for the renderer's tie-break overlay. */
function contestedTriangles({ requestId, modelId, instances, geometries }) {
  const started = performance.now();
  try {
    const byId = new Map(geometries.map((geometry) => [geometry.id, geometry]));
    const result = findContestedTriangles({ instances }, byId);
    self.postMessage(
      { type: "contested-triangles", requestId, modelId, records: result.records, offsets: result.offsets,
        triangles: result.triangles, pairs: result.pairs, exhausted: result.exhausted, elapsedMs: performance.now() - started },
      [result.records.buffer, result.offsets.buffer, result.triangles.buffer],
    );
  } catch (error) {
    self.postMessage({ type: "contested-triangles", requestId, modelId, error: readableError(error) });
  }
}

/** Evaluate in batches, posting each as an IGP chunk as soon as it exists. */
function streamGeometry(jobId, modelId, settings) {
  const started = performance.now();
  const summary = JSON.parse(kernel.beginGeometryStream(modelId, settings));
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
    self.postMessage(
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
function wholeGeometry(jobId, modelId, settings) {
  postPhase(jobId, "Tessellating geometry", "Evaluating products and origin-shifting GPU positions.");
  const geometryStarted = performance.now();
  const summary = JSON.parse(kernel.evaluateGeometry(modelId, settings));
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
    self.postMessage({ type: "entity-info", requestId, modelId, expressId, info: JSON.parse(info) });
  } catch (error) {
    self.postMessage({ type: "entity-error", requestId, expressId, message: readableError(error) });
  }
}

function editEntity({ requestId, modelId, expressId, changes, patch }) {
  const edits = (changes ?? []).map((change) => ({ ...change, expressId }));
  publishRequest({ requestId, modelId, patch, expressId, changed: edits.length, attributeEdit: true, run: (s) => ({ delta: s.setAttributes(edits) }) });
}

function updateRevision({ requestId, modelId, patch, buffer }) {
  publishRequest({ requestId, modelId, patch, run: (s) => ({ delta: s.applySnapshot(new Uint8Array(buffer)) }) });
}

/** Run a browser script; its edits become one snapshot revision. */
function runScript({ requestId, modelId, source, selection, patch, commit = true }) {
  const started = performance.now();
  publishRequest({ requestId, modelId, patch, run: (s) => {
    const { report, delta } = s.runScript(String(source ?? ""), selection, { commit });
    report.elapsedMs = performance.now() - started;
    return { delta, script: report };
  } });
}

/** Undo or redo the last change by publishing the stored source as a new revision. */
function scriptHistory({ requestId, modelId, action, patch }) {
  const started = performance.now();
  const available = action === "redo" ? session?.history.redo : session?.history.undo;
  if (!available) {
    self.postMessage({ type: "script-error", requestId, modelId, message: `Nothing to ${action}.`, history: historyCounts() });
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
      self.postMessage({ type: "script-result", requestId, modelId, result: script, history: historyCounts() });
      return true;
    }
    const delta = outcome.delta;
    const infoStarted = performance.now();
    const info = JSON.parse(kernel.getModelInfo(modelId));
    // The selected element's attributes ride along so the inspector never shows a loading gap.
    const infoId = expressId ?? patch.selectedExpressId ?? null;
    const entityInfo = infoId == null ? null : JSON.parse(kernel.getEntityInfo(modelId, infoId) ?? "null");
    const message = {
      type: "revision-result", requestId, modelId, baseRevision: delta.baseRevision, revision: delta.revision, impact: delta.impact, info,
      facts: modelFacts(), hierarchy: delta.hierarchy, expressId, entityInfo, infoId, changed, attributeEdit, script, history: historyCounts(),
      timings: { ...delta.timings, preparedMs: delta.timings.prepareMs, infoMs: performance.now() - infoStarted, workerMs: performance.now() - started },
      buffer: delta.chunk.buffer,
    };
    self.postMessage(message, [delta.chunk.buffer]);
    return true;
  } catch (error) {
    self.postMessage({ type: "revision-error", requestId, modelId, expressId, committed: Boolean(error?.committed), script, history: historyCounts(),
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
    return { storeys: storeysOf(session), lengthUnit: lengthUnitOf(session) };
  } catch {
    return { storeys: [], lengthUnit: null };
  }
}

function exportModel({ requestId, modelId }) {
  try {
    ensureActive(modelId);
    const bytes = kernel.exportModel(modelId);
    if (!bytes) throw new Error("The editable IFC source is no longer open.");
    self.postMessage({ type: "export-result", requestId, modelId, buffer: bytes.buffer }, [bytes.buffer]);
  } catch (error) {
    self.postMessage({ type: "export-error", requestId, message: readableError(error) });
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
  self.postMessage({ type: "phase", jobId, phase, detail, progress });
}

function readableError(error) {
  return error instanceof Error ? error.message : String(error);
}
