// SPDX-License-Identifier: Apache-2.0

//! A promise API over the kernel worker: open a file and receive its chunks,
//! ask the session questions, publish revisions, and stop a script that runs
//! past its limit by ending the worker and reopening the model without it.

import { readIgp } from "@tessifc/edit/igp";

/** The script limit a client applies unless told otherwise. */
export const DEFAULT_SCRIPT_TIMEOUT_MS = 30_000;

/**
 * @typedef {object} KernelClientOptions
 * @property {URL | string} [url] The worker script; the package's `kernel-worker.js` by default.
 * @property {URL | string} [wasmUrl] The `@tessifc/core/web` module the default worker loads; the checkout's or the installed package's by default.
 * @property {URL | string} [editUrl] The directory the default worker finds `@tessifc/edit`'s sources in.
 * @property {Record<string, unknown>} [settings] Geometry settings every open merges its own over.
 * @property {number} [scriptTimeoutMs] The script limit in milliseconds; 0 runs scripts without one.
 * @property {(detail: ReopenResult) => void} [onReopen] Called after a stopped script's model was reopened in a fresh worker.
 */

/**
 * @typedef {object} OpenResult
 * @property {number} modelId
 * @property {string} revision
 * @property {Record<string, any>} info
 * @property {Record<string, any>} summary
 * @property {{ nodes: Array<Record<string, any>> }} hierarchy
 * @property {Record<string, any>} facts
 * @property {boolean} streamed
 * @property {Record<string, number>} timings
 */

/**
 * @typedef {object} ReopenResult
 * @property {number} modelId
 * @property {string} revision
 * @property {Record<string, any>} info
 * @property {{ nodes: Array<Record<string, any>> }} hierarchy
 * @property {Record<string, any>} facts
 */

/**
 * What a revision needs from the scene it lands in: the revision it builds
 * on, the pack's model offset and the next free geometry id.
 * @typedef {{ baseRevision: string, modelOffset: number[], firstGeometryId: number, selectedExpressId?: number | null }} Patch
 */

/** @typedef {ReturnType<typeof createKernelClient>} KernelClient */

/**
 * Create a client; the worker starts on the first call that needs it.
 * @param {KernelClientOptions} [options]
 */
export function createKernelClient(options = {}) {
  const scriptTimeoutMs = Math.max(0, Number(options.scriptTimeoutMs ?? DEFAULT_SCRIPT_TIMEOUT_MS) || 0);
  const workerName = JSON.stringify({
    wasmUrl: options.wasmUrl ? String(options.wasmUrl) : undefined,
    editUrl: options.editUrl ? String(options.editUrl) : undefined,
    settings: options.settings,
  });
  /** @type {Worker | null} */
  let worker = null;
  /** @type {Promise<{ version: string, streaming: boolean }> | null} */
  let booted = null;
  let nextId = 0;
  /** @type {Map<number, { resolve: (value: any) => void, reject: (error: Error) => void, timer?: any, type: string }>} */
  const pending = new Map();
  /** @type {{ jobId: number, resolve: (value: any) => void, reject: (error: Error) => void, onChunk?: Function, onPhase?: Function, source: Blob | Uint8Array | null } | null} */
  let job = null;
  /**
   * The model the worker holds: its ids and the bytes a reopen starts from.
   * @type {{ modelId: number, revision: string, source: Blob | Uint8Array | null, snapshot: ArrayBuffer | null, history: { undo: number, redo: number }, hierarchy: any, info: any } | null}
   */
  let model = null;
  let version = null;
  let streaming = false;
  let disposed = false;

  function start() {
    if (worker) return booted;
    const created = new Worker(options.url ?? new URL("./kernel-worker.js", import.meta.url), { type: "module", name: workerName });
    worker = created;
    booted = new Promise((resolve, reject) => {
      pending.set(0, { resolve, reject, type: "ready" });
    });
    // Whoever asked awaits the same promise; an unasked failure must not surface as unhandled.
    booted.catch(() => {});
    created.addEventListener("message", ({ data }) => {
      if (created === worker) receive(data);
    });
    created.addEventListener("error", (event) => {
      if (created !== worker) return;
      fail(new Error(event.message || "The geometry worker stopped unexpectedly."));
    });
    return booted;
  }

  /** Drop the worker and fail everything it owed. */
  function fail(error) {
    const stopped = worker;
    worker = null;
    booted = null;
    stopped?.terminate();
    for (const [id, request] of pending) {
      pending.delete(id);
      if (request.timer) clearTimeout(request.timer);
      request.reject(error);
    }
    if (job) {
      const failed = job;
      job = null;
      failed.reject(error);
    }
  }

  function settle(id, error, value) {
    const request = pending.get(id);
    if (!request) return;
    pending.delete(id);
    if (request.timer) clearTimeout(request.timer);
    if (error) request.reject(error);
    else request.resolve(value);
  }

  function receive(data) {
    if (!data || typeof data !== "object") return;
    switch (data.type) {
      case "ready":
        version = data.version;
        streaming = Boolean(data.streaming);
        return settle(0, null, { version, streaming });
      case "boot-error":
        return fail(new Error(`The geometry kernel failed to start: ${data.message}`));
      case "phase":
        if (job && data.jobId === job.jobId) job.onPhase?.({ phase: data.phase, detail: data.detail, progress: data.progress });
        return;
      case "chunk":
        if (job && data.jobId === job.jobId) job.onChunk?.({ buffer: data.buffer, progress: data.progress, modelId: data.modelId });
        return;
      case "result": {
        if (!job || data.jobId !== job.jobId) return;
        const finished = job;
        job = null;
        if (data.pack) finished.onChunk?.({ buffer: data.pack, progress: null, modelId: data.modelId });
        model = { modelId: data.modelId, revision: data.revision ?? "0", source: finished.source, snapshot: null,
          history: { undo: 0, redo: 0 }, hierarchy: data.hierarchy, info: data.info };
        return finished.resolve({ modelId: data.modelId, revision: model.revision, info: data.info, summary: data.summary,
          hierarchy: data.hierarchy, facts: data.facts, streamed: Boolean(data.streamed), timings: data.timings });
      }
      case "conversion-error": {
        if (!job || data.jobId !== job.jobId) return;
        const failed = job;
        job = null;
        return failed.reject(new Error(data.message));
      }
      case "entity-info":
        return settle(data.requestId, null, data.info);
      case "entity-error":
        return settle(data.requestId, new Error(data.message));
      case "query-result":
        return settle(data.requestId, null, data.result);
      case "query-error":
        return settle(data.requestId, new Error(data.message));
      case "script-finished": {
        // The script returned; the kernel's bounded work follows without the limit.
        const request = pending.get(data.requestId);
        if (request?.timer) clearTimeout(request.timer);
        if (request) request.timer = null;
        return;
      }
      case "script-result":
        if (model && data.history) model.history = data.history;
        return settle(data.requestId, null, { report: data.result, delta: null });
      case "script-error":
        if (model && data.history) model.history = data.history;
        return settle(data.requestId, new Error(data.message));
      case "revision-result":
        return settle(data.requestId, null, revisionOf(data));
      case "revision-error": {
        const error = /** @type {Error & { committed?: boolean, revision?: string | null }} */ (new Error(data.message));
        error.committed = Boolean(data.committed);
        error.revision = data.revision ?? null;
        if (model && data.committed && data.revision) model.revision = data.revision;
        if (model && data.history) model.history = data.history;
        return settle(data.requestId, error);
      }
      case "reopened":
        return settle(data.requestId, null, data);
      case "reopen-error":
        return settle(data.requestId, new Error(data.message));
      case "export-result":
        return settle(data.requestId, null, new Uint8Array(data.buffer));
      case "export-error":
        return settle(data.requestId, new Error(data.message));
      case "contested-triangles":
        return data.error ? settle(data.requestId, new Error(data.error)) : settle(data.requestId, null, data);
      default:
        return;
    }
  }

  /** A revision message as the delta `applyDelta` and the retained scenes take. */
  function revisionOf(data) {
    const chunk = new Uint8Array(data.buffer);
    const pack = readIgp(chunk);
    const impact = data.impact ?? {};
    if (model) {
      model.revision = data.revision;
      model.snapshot = data.snapshot ?? model.snapshot;
      model.history = data.history ?? model.history;
      model.hierarchy = data.hierarchy ?? model.hierarchy;
      model.info = data.info ?? model.info;
    }
    const delta = {
      kind: impact.fullRebuild ? "full" : "selective",
      revision: data.revision,
      baseRevision: data.baseRevision,
      chunk,
      pack,
      impact,
      affectedProducts: impact.affectedProducts ?? [],
      removedProducts: impact.removedProducts ?? [],
      metadataProducts: impact.metadataProducts ?? [],
      fullRebuild: Boolean(impact.fullRebuild),
      hierarchy: data.hierarchy ?? null,
      timings: data.timings ?? {},
    };
    return { delta, report: data.script ?? null, info: data.info, facts: data.facts, entityInfo: data.entityInfo ?? null, history: data.history };
  }

  /** Post one request and wait for its answer. */
  function request(type, fields, transfer = [], timeoutMs = 0) {
    return new Promise((resolve, reject) => {
      if (disposed) return reject(new Error("The kernel client is disposed."));
      if (!worker) return reject(new Error("The geometry worker is not running."));
      const requestId = ++nextId;
      const entry = { resolve, reject, type, timer: null };
      if (timeoutMs > 0) entry.timer = setTimeout(() => scriptTimedOut(requestId, timeoutMs), timeoutMs);
      pending.set(requestId, entry);
      worker.postMessage({ type, requestId, ...fields }, transfer);
    });
  }

  /** The worker is single-threaded: a script past its limit ends with the worker, then the model comes back. */
  function scriptTimedOut(requestId, timeoutMs) {
    const entry = pending.get(requestId);
    if (!entry) return;
    pending.delete(requestId);
    const limit = timeoutMs >= 1000 ? `${timeoutMs / 1000} s` : `${timeoutMs} ms`;
    const message = `ScriptTimeout: the script ran longer than ${limit} and was stopped. The model reopens at its last revision; the undo history is cleared.`;
    const others = new Error("The geometry worker was stopped to end a script that ran past its limit.");
    fail(others);
    reopen().then(
      () => entry.resolve({ report: { ok: false, timedOut: true, error: message, changed: false, stdout: "", elapsedMs: timeoutMs }, delta: null }),
      (error) => entry.reject(Object.assign(new Error(`${message} ${error.message}`), { timedOut: true })),
    );
  }

  /** Bring the model back in a fresh worker from its last committed source. */
  async function reopen() {
    const current = model;
    if (!current) throw new Error("No model to reopen.");
    if (worker) fail(new Error("The geometry worker was restarted."));
    await start();
    const buffer = current.snapshot ? current.snapshot.slice(0) : await sourceBuffer(current.source);
    if (!buffer) throw new Error("The model's source is no longer available; open the file again.");
    const result = await request("reopen", { buffer, settings: options.settings }, [buffer]);
    model = { ...current, modelId: result.modelId, revision: result.revision ?? "0", history: { undo: 0, redo: 0 },
      hierarchy: result.hierarchy ?? current.hierarchy, info: result.info ?? current.info };
    const detail = { modelId: result.modelId, revision: model.revision, info: result.info, hierarchy: result.hierarchy, facts: result.facts };
    options.onReopen?.(detail);
    return detail;
  }

  async function sourceBuffer(source) {
    if (!source) return null;
    if (typeof Blob !== "undefined" && source instanceof Blob) return source.arrayBuffer();
    if (source instanceof Uint8Array) return source.slice().buffer;
    return null;
  }

  /** The patch a revision needs, checked before it goes to the worker. */
  function patchOf(patch) {
    if (!patch || typeof patch.baseRevision !== "string" || !Array.isArray(patch.modelOffset) || !Number.isSafeInteger(patch.firstGeometryId)) {
      throw new Error("A revision needs its base revision, the model offset and the next geometry id.");
    }
    return { baseRevision: patch.baseRevision, modelOffset: patch.modelOffset, firstGeometryId: patch.firstGeometryId,
      selectedExpressId: patch.selectedExpressId ?? null };
  }

  return {
    /** Start the worker if needed and wait for the kernel; resolves with its version. */
    ready: () => {
      if (disposed) return Promise.reject(new Error("The kernel client is disposed."));
      return start();
    },
    /**
     * Parse and tessellate a file in the worker. `onChunk` receives each IGP
     * chunk's buffer as it arrives (transferred, so it is yours). A `Blob` is
     * read here and kept for a reopen; bytes are copied to the worker unless
     * `transfer` is set, in which case a stopped script cannot reopen a model
     * that has no committed revision yet.
     * @param {Blob | Uint8Array} source
     * @param {{ settings?: Record<string, unknown>, onChunk?: (chunk: { buffer: ArrayBuffer, progress: any, modelId: number }) => void, onPhase?: (phase: { phase: string, detail: string, progress: any }) => void, transfer?: boolean }} [openOptions]
     * @returns {Promise<OpenResult>}
     */
    async open(source, openOptions = {}) {
      if (disposed) throw new Error("The kernel client is disposed.");
      // The worker runs one job at a time: a job still running is abandoned with its worker.
      if (job) fail(new Error("The open was superseded by a later one."));
      await start();
      const isBlob = typeof Blob !== "undefined" && source instanceof Blob;
      const bytes = isBlob ? new Uint8Array(await source.arrayBuffer()) : source;
      if (!(bytes instanceof Uint8Array)) throw new TypeError("open expects a Blob or a Uint8Array");
      if (disposed || !worker) throw new Error("The geometry worker is not running.");
      const transfer = isBlob || openOptions.transfer === true;
      const buffer = transfer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength ? bytes.buffer : bytes.slice().buffer;
      // The previous model stays the worker's until the new one replaces it there.
      return new Promise((resolve, reject) => {
        const jobId = ++nextId;
        job = { jobId, resolve, reject, onChunk: openOptions.onChunk, onPhase: openOptions.onPhase,
          source: isBlob ? source : transfer ? null : bytes };
        worker.postMessage({ type: "convert", jobId, buffer, settings: openOptions.settings }, [buffer]);
      });
    },
    /**
     * A read-only session question: `hierarchy`, `info`, `entity`, `classDefinition`, `idsOfType`, `diagnostics` or `facts`.
     * @param {number} modelId
     * @param {string} method
     * @param {...unknown} args
     */
    query: (modelId, method, ...args) => request("query", { modelId, method, args }),
    /** @param {number} modelId @param {number} expressId */
    entityInfo: (modelId, expressId) => request("entity-info", { modelId, expressId }),
    /**
     * Run a script under the client's time limit; a stopped script resolves
     * with `report.timedOut` after the model has been reopened.
     * @param {number} modelId
     * @param {string} source
     * @param {unknown} selection
     * @param {{ commit?: boolean, patch: Patch }} runOptions
     */
    runScript: (modelId, source, selection, runOptions) =>
      request("run-script", { modelId, source: String(source ?? ""), selection: selection ?? null, commit: runOptions.commit !== false,
        patch: patchOf(runOptions.patch) }, [], scriptTimeoutMs),
    /** @param {number} modelId @param {Array<Record<string, unknown>>} edits @param {Patch} patch */
    setAttributes: (modelId, edits, patch) => request("set-attributes", { modelId, edits, patch: patchOf(patch) }),
    /** @param {number} modelId @param {Uint8Array | ArrayBuffer} bytes @param {Patch} patch */
    applySnapshot: (modelId, bytes, patch) => {
      const copy = bytes instanceof Uint8Array ? bytes.slice().buffer : bytes.slice(0);
      return request("update-revision", { modelId, buffer: copy, patch: patchOf(patch) }, [copy]);
    },
    /** @param {number} modelId @param {Patch} patch */
    undo: (modelId, patch) => request("script-history", { modelId, action: "undo", patch: patchOf(patch) }),
    /** @param {number} modelId @param {Patch} patch */
    redo: (modelId, patch) => request("script-history", { modelId, action: "redo", patch: patchOf(patch) }),
    /** @param {number} modelId */
    exportModel: (modelId) => request("export", { modelId }),
    /** The coincident-plane analysis, in the kernel's worker instead of a second one. */
    contestedTriangles: (modelId, instances, geometries, transfer = []) => request("contested-triangles", { modelId, instances, geometries }, transfer),
    /** Drop the model from the worker; the worker stays for the next open. */
    close: () => {
      if (job) fail(new Error("The open was closed before it finished."));
      if (model && worker) worker.postMessage({ type: "close" });
      model = null;
    },
    /** End the worker; a later call starts a new one. */
    terminate: () => {
      fail(new Error("The geometry worker was terminated."));
      model = null;
    },
    /** End the worker for good. */
    dispose: () => {
      disposed = true;
      fail(new Error("The kernel client is disposed."));
      model = null;
    },
    get version() {
      return version;
    },
    get streaming() {
      return streaming;
    },
    get running() {
      return Boolean(worker);
    },
    /** The model in the worker: `modelId`, `revision` and `history` counts, or null. */
    get model() {
      return model ? { modelId: model.modelId, revision: model.revision, history: { ...model.history } } : null;
    },
    get scriptTimeoutMs() {
      return scriptTimeoutMs;
    },
  };
}
