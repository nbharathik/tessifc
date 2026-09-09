// SPDX-License-Identifier: Apache-2.0

//! The geometry worker: parses, tessellates and packs off the main thread.
//! Streams IGP chunks for progressive display; older kernels get one pack.

import init, { Kernel, version } from "../../bindings/wasm/pkg/tessifc_wasm.js";

let kernel;
let activeModelId;

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
  else if (data.type === "export") exportModel(data);
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
    const settings = JSON.stringify({
      includeSpaces: true,
      includeOpenings: true,
      includeAnnotations: true,
      includeReferences: true,
    });
    const outcome =
      typeof kernel.beginGeometryStream === "function"
        ? streamGeometry(jobId, modelId, settings)
        : wholeGeometry(jobId, modelId, settings);
    const hierarchy = JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');

    // The model stays open for inspection and edits.
    if (previousModelId !== undefined) kernel.closeModel(previousModelId);
    activeModelId = modelId;
    const message = {
      type: "result",
      jobId,
      modelId,
      info,
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
  try {
    ensureActive(modelId);
    const info = JSON.parse(kernel.setAttributes(modelId, expressId, JSON.stringify(changes)));
    self.postMessage({ type: "edit-result", requestId, modelId, expressId, info, changed: changes.length });
    // Only the edited product is re-evaluated and redrawn.
    if (patch && typeof kernel.evaluateProducts === "function") {
      const started = performance.now();
      const chunk = kernel.evaluateProducts(
        modelId,
        Uint32Array.from([expressId]),
        JSON.stringify({
          includeSpaces: true,
          includeOpenings: true,
          includeAnnotations: true,
          includeReferences: true,
          modelOffset: patch.modelOffset,
          firstGeometryId: patch.firstGeometryId,
        }),
      );
      if (chunk) {
        self.postMessage(
          { type: "product-geometry", requestId, modelId, expressId, buffer: chunk.buffer, elapsedMs: performance.now() - started },
          [chunk.buffer],
        );
      }
    }
  } catch (error) {
    self.postMessage({ type: "edit-error", requestId, expressId, message: readableError(error) });
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
  if (activeModelId !== undefined) kernel.closeModel(activeModelId);
  activeModelId = undefined;
}

function postPhase(jobId, phase, detail, progress = null) {
  self.postMessage({ type: "phase", jobId, phase, detail, progress });
}

function readableError(error) {
  return error instanceof Error ? error.message : String(error);
}
