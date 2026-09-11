// SPDX-License-Identifier: Apache-2.0

//! The editing session: one open model, its committed revision, and every
//! way to change it. Each change returns a scene delta that any renderer
//! adapter applies; the kernel decides which products the delta touches.

import { readIgp } from "./igp.js";
import { createScriptEngine, runScript as runEngineScript } from "./script-engine.js";

const HISTORY_LIMIT = 10;
const HISTORY_BYTES = 256 << 20;

/**
 * Open an editing session over a model the kernel already holds.
 *
 * `settings` are the geometry settings of the initial evaluation or stream;
 * the kernel refuses a patch evaluated with different ones. Either call
 * `evaluate()` here to produce the initial scene, or `adopt()` the model
 * offset and next geometry id of a scene the host built itself.
 */
export function createEditingSession(kernel, modelId, options = {}) {
  const settings = { ...(options.settings ?? {}) };
  let modelOffset = options.modelOffset ?? null;
  let nextGeometryId = options.firstGeometryId ?? null;
  const limits = { count: options.historyLimit ?? HISTORY_LIMIT, bytes: options.historyBytes ?? HISTORY_BYTES };
  const history = { undo: [], redo: [] };
  let closed = false;

  function open() {
    if (closed) throw new Error("The editing session is closed.");
  }

  function ready() {
    open();
    if (!modelOffset || nextGeometryId === null) {
      throw new Error("Establish the scene first: call evaluate() or adopt({ modelOffset, nextGeometryId }).");
    }
  }

  function patchSettings() {
    return JSON.stringify({ ...settings, modelOffset, firstGeometryId: nextGeometryId });
  }

  /** Note the geometry ids a chunk used, so the next patch allocates above them. */
  function consume(pack) {
    let highest = nextGeometryId - 1;
    for (const geometry of pack.geometry) if (geometry.id > highest) highest = geometry.id;
    nextGeometryId = highest + 1;
  }

  /** Evaluate the whole model and return its parsed pack; the scene basis comes from it. */
  function evaluate() {
    open();
    const summary = JSON.parse(kernel.evaluateGeometry(modelId, JSON.stringify(settings)));
    const outcomes = JSON.parse(kernel.getProductOutcomes?.(modelId) ?? "null");
    const bytes = kernel.takePack(modelId);
    if (!bytes) throw new Error("The kernel returned no geometry pack.");
    const pack = readIgp(bytes);
    modelOffset = pack.index.model_offset ?? [0, 0, 0];
    nextGeometryId = 0;
    consume(pack);
    return { pack, summary, outcomes, hierarchy: hierarchy() };
  }

  /** Take over a scene the host built from its own evaluation or stream. */
  function adopt({ modelOffset: offset, nextGeometryId: next }) {
    open();
    if (!Array.isArray(offset) || offset.length !== 3) throw new Error("adopt() needs the pack's model offset.");
    if (!Number.isSafeInteger(next) || next < 0) throw new Error("adopt() needs the next free geometry id.");
    modelOffset = offset.slice();
    nextGeometryId = next;
  }

  function hierarchy() {
    return JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');
  }

  /** Stage, evaluate, check and commit one candidate; the delta describes what changed. */
  function publish(prepare, extra = {}) {
    ready();
    const started = now();
    const baseRevision = kernel.getModelRevision(modelId);
    const candidate = JSON.parse(prepare(baseRevision));
    let committed = false;
    try {
      const geometryStarted = now();
      const chunk = kernel.evaluatePreparedRevision(modelId, candidate.candidateToken, patchSettings());
      const impact = JSON.parse(kernel.getPreparedRevisionInfo(modelId));
      if (!impact.evaluationAccepted) {
        const error = new Error(rejectionMessage(impact));
        error.impact = impact;
        throw error;
      }
      const revision = kernel.commitRevision(modelId, baseRevision, candidate.candidateToken);
      committed = true;
      const pack = readIgp(chunk);
      consume(pack);
      if (extra.before) {
        pushHistory(history.undo, extra.before);
        history.redo.length = 0;
      }
      return {
        kind: impact.fullRebuild ? "full" : "selective",
        revision,
        baseRevision,
        chunk,
        pack,
        impact,
        affectedProducts: impact.affectedProducts ?? [],
        removedProducts: impact.removedProducts ?? [],
        metadataProducts: impact.metadataProducts ?? [],
        fullRebuild: Boolean(impact.fullRebuild),
        hierarchy: hierarchy(),
        timings: { prepareMs: geometryStarted - started, geometryMs: now() - geometryStarted, totalMs: now() - started },
        ...extra.result,
      };
    } finally {
      if (!committed) kernel.discardRevision(modelId, candidate.candidateToken);
    }
  }

  /** Publish an externally edited copy of the whole file. */
  function applySnapshot(bytes) {
    const before = kernel.exportModel(modelId);
    return publish((base) => kernel.prepareRevision(modelId, asBytes(bytes), base), { before });
  }

  /** Publish attribute edits, each `{ expressId, attribute, value, raw }`, preserving unrelated bytes. */
  function setAttributes(edits) {
    const before = kernel.exportModel(modelId);
    return publish((base) => kernel.prepareAttributeEdits(modelId, JSON.stringify(edits), base), { before });
  }

  /**
   * Run a script; `report` is the script's own result and `delta` the published
   * revision, or null when nothing changed. `commit: false` runs it read-only.
   */
  function runScript(source, selection = null, { commit = true } = {}) {
    ready();
    const engine = createScriptEngine(kernel, modelId);
    const report = runEngineScript(engine, String(source ?? ""), selection);
    if (!report.ok || !report.changed || !commit) {
      if (!commit) report.changed = false;
      return { report, delta: null };
    }
    const before = kernel.exportModel(modelId);
    const bytes = engine.snapshotBytes();
    const delta = publish((base) => kernel.prepareRevision(modelId, bytes, base), { before });
    return { report, delta };
  }

  /**
   * Re-tessellate the named products without changing the model: the host
   * decides the set, for example after a settings change it made elsewhere.
   */
  function refreshProducts(expressIds) {
    ready();
    const ids = Uint32Array.from(expressIds, (value) => Number(value));
    const started = now();
    const chunk = kernel.evaluateProducts(modelId, ids, patchSettings());
    if (!chunk) throw new Error("The kernel returned no geometry for these products.");
    const pack = readIgp(chunk);
    consume(pack);
    const present = new Set(pack.instances.expressIds);
    return {
      kind: "direct",
      revision: kernel.getModelRevision(modelId),
      baseRevision: kernel.getModelRevision(modelId),
      chunk,
      pack,
      impact: null,
      affectedProducts: Array.from(ids),
      removedProducts: [],
      metadataProducts: [],
      emptyProducts: Array.from(ids).filter((id) => !present.has(id)),
      fullRebuild: false,
      hierarchy: null,
      timings: { prepareMs: 0, geometryMs: now() - started, totalMs: now() - started },
    };
  }

  function restore(source, target, label) {
    ready();
    if (!source.length) throw new Error(`Nothing to ${label}.`);
    const bytes = source.pop();
    const current = kernel.exportModel(modelId);
    try {
      const delta = publish((base) => kernel.prepareRevision(modelId, bytes, base), { result: { label } });
      pushHistory(target, current);
      return delta;
    } catch (error) {
      source.push(bytes);
      throw error;
    }
  }

  function pushHistory(stack, bytes) {
    stack.push(bytes);
    let total = stack.reduce((sum, item) => sum + item.byteLength, 0);
    while (stack.length > 1 && (stack.length > limits.count || total > limits.bytes)) total -= stack.shift().byteLength;
  }

  return {
    get modelId() {
      return modelId;
    },
    get revision() {
      return kernel.getModelRevision(modelId);
    },
    get modelOffset() {
      return modelOffset ? modelOffset.slice() : null;
    },
    get nextGeometryId() {
      return nextGeometryId;
    },
    get history() {
      return { undo: history.undo.length, redo: history.redo.length };
    },
    settings: () => ({ ...settings }),
    evaluate,
    adopt,
    applySnapshot,
    setAttributes,
    runScript,
    refreshProducts,
    undo: () => restore(history.undo, history.redo, "undo"),
    redo: () => restore(history.redo, history.undo, "redo"),
    export: () => {
      open();
      return kernel.exportModel(modelId);
    },
    entity: (expressId) => JSON.parse(kernel.getEntityInfo(modelId, expressId) ?? "null"),
    classDefinition: (className) => JSON.parse(kernel.getClassAttributes(modelId, className) ?? "null"),
    hierarchy,
    close() {
      closed = true;
      history.undo.length = 0;
      history.redo.length = 0;
    },
  };
}

function rejectionMessage(impact) {
  const diagnostics = Array.isArray(impact.diagnostics) ? impact.diagnostics : [];
  const first = diagnostics.find((item) => item?.severity === "error") ?? diagnostics[0];
  return first ? `The candidate revision was rejected: ${first.code ?? ""} ${first.message ?? ""}`.trim()
    : "The candidate revision was rejected by the kernel.";
}

function asBytes(input) {
  if (input instanceof Uint8Array) return input;
  if (input instanceof ArrayBuffer) return new Uint8Array(input);
  if (typeof input === "string") return new TextEncoder().encode(input);
  throw new TypeError("Expected IFC bytes.");
}

function now() {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}
