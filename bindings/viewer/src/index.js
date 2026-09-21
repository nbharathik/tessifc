// SPDX-License-Identifier: Apache-2.0

//! A minimal IFC viewer: one container, one kernel, a small API. The reference
//! application adds its panels and ribbon on top of these same pieces.

import { DEFAULT_HIDDEN_INSTANCE_FLAGS, readIgp } from "@tessifc/edit/igp";
import { createEditingSession } from "@tessifc/edit/session";
import { DEFAULT_LOD_PIXELS, IfcRenderer } from "./renderer.js";
import { createPackAssembler } from "./stream.js";
import { createFrameScheduler, createTaskQueue } from "./scheduler.js";
import { createSessionClient } from "./session-client.js";
import { createKernelClient } from "./kernel-client.js";

export { IfcRenderer, DEFAULT_LOD_PIXELS } from "./renderer.js";
export { createPackAssembler } from "./stream.js";
export { createFrameScheduler, createTaskQueue } from "./scheduler.js";
export { frameSphere, wheelZoomFactor, zoomCamera } from "./navigation.js";
export { projectPoint, snapToTriangle, measurementBetween } from "./measure.js";
export { findContestedTriangles, planeKey } from "./depth-planes.js";
export { createSessionClient } from "./session-client.js";
export { createKernelClient, DEFAULT_SCRIPT_TIMEOUT_MS } from "./kernel-client.js";
export { createTextureCache, resolveTextureUrl, pixelsToRgba, uvMatrix } from "./textures.js";

/** Helper geometry is kept in the pack so a host can reveal it without converting again. */
export const GEOMETRY_SETTINGS = {
  includeSpaces: true,
  includeOpenings: true,
  includeAnnotations: true,
  includeReferences: true,
};

/** Display styles the renderer understands. */
export const STYLES = /** @type {ReadonlyArray<"shaded" | "xray" | "wire">} */ (["shaded", "xray", "wire"]);
/** Camera modes `setView` accepts. */
export const VIEWS = /** @type {ReadonlyArray<"perspective" | "top" | "front" | "right">} */ (["perspective", "top", "front", "right"]);

// A small first chunk for an early paint; later chunks grow to amortise overhead.
const FIRST_CHUNK_MS = 45;
const CHUNK_MS = 220;
const CHUNK_TRIANGLES = 600_000;
// A release that travelled further than this is a drag, not a click.
const CLICK_TRAVEL_PX = 4;

/** @typedef {import("@tessifc/edit/types").Kernel} Kernel */
/** @typedef {import("@tessifc/edit/types").Delta} Delta */
/** @typedef {import("./stream.js").AssembledPack} AssembledPack */

/**
 * The options of `createViewer`.
 * @typedef {object} ViewerOptions
 * @property {Kernel} [kernel] A kernel from `@tessifc/core/web`; `open` needs one.
 * @property {number} [hiddenFlags] Instance flag bits hidden at load; spaces, openings and references by default.
 * @property {number} [lodPixels] Skip products smaller than this on screen; 0 draws everything.
 * @property {"light" | "dark"} [theme]
 * @property {string} [background] A CSS hex colour for the canvas.
 * @property {boolean} [requireGeometry] Refuse a model with no drawable product instead of showing it empty.
 * @property {boolean} [selectOnClick] Off leaves clicks to the host.
 * @property {boolean} [focusOnDoubleClick]
 * @property {boolean} [coincidence] Off skips the shared-plane overlay analysis.
 * @property {boolean | WorkerOptions} [worker] Run the kernel in a Web Worker instead of on the page: `true` for the package's worker next to the checkout's kernel, or where to find things.
 * @property {boolean} [occlusion] Leave product groups hidden behind the model's largest faces out of moving frames; on by default.
 * @property {boolean} [motionLod] Draw the coarse levels a pack carries (the `lodLevels` setting) on moving frames; on by default.
 * @property {boolean} [textures] Ask the kernel for materials and textures and draw them; off by default.
 * @property {boolean} [allowRemoteTextures] Fetch image textures from other origins too; off, only the page's own.
 * @property {string} [textureBaseUrl] Where a texture's relative path resolves; the page's URL by default.
 */

/**
 * Where worker mode finds its pieces; every field is optional.
 * @typedef {object} WorkerOptions
 * @property {URL | string} [url] The worker script; the package's `kernel-worker.js` by default.
 * @property {URL | string} [wasmUrl] The `@tessifc/core/web` module the default worker loads.
 * @property {URL | string} [editUrl] The directory the default worker finds `@tessifc/edit`'s sources in.
 * @property {number} [scriptTimeoutMs] The script limit; 0 runs scripts without one.
 */

/**
 * The options of `viewer.open`.
 * @typedef {object} OpenOptions
 * @property {Kernel} [kernel]
 * @property {Record<string, unknown>} [settings] Geometry settings merged over `GEOMETRY_SETTINGS`.
 * @property {Record<string, unknown>} [modelSettings] Parse settings for `openModel`.
 * @property {boolean} [requireGeometry]
 */

/**
 * What `open` resolves with: the kernel's model id and reports.
 * @typedef {object} OpenResult
 * @property {number} modelId
 * @property {Record<string, any>} info
 * @property {Record<string, any>} summary
 * @property {{ nodes: Array<Record<string, any>> }} hierarchy
 * @property {boolean} empty
 */

/**
 * The current selection: express ids and their pack records.
 * @typedef {{ expressIds: number[], records: number[] }} ViewerSelection
 */

/**
 * A section cut; see `setSection`.
 * @typedef {{ axis?: "x" | "y" | "z", value?: number, fraction?: number, flipped?: boolean, cap?: boolean }} SectionOptions
 */

/**
 * What `applyDelta` returns and the `revision` event carries.
 * @typedef {{ revision: string | null, kind: string, affectedProducts: number[], removedProducts: number[], metadataProducts: number[], fullRebuild: boolean, changed: boolean }} RevisionReport
 */

/** @typedef {ReturnType<typeof createViewer>} Viewer */

/**
 * Create a viewer inside `container`.
 *
 * Options: `kernel` (a `Kernel` from `@tessifc/core/web`, needed by `open`),
 * `hiddenFlags` (instance flags hidden at load; spaces, openings and
 * references by default), `lodPixels` (skip products smaller than this on
 * screen, 0 to draw everything), `theme` (`"light"` or `"dark"`),
 * `background` (a CSS hex colour for the canvas) and `requireGeometry`
 * (refuse a model with no drawable product instead of showing it empty).
 * @param {Element} container
 * @param {ViewerOptions} [options]
 */
export function createViewer(container, options = {}) {
  if (!(container instanceof Element)) throw new TypeError("createViewer needs a DOM element");
  const renderer = new IfcRenderer(container);
  const tasks = createTaskQueue();
  const listeners = new Map();
  const view = { renderer };

  /** @type {Record<string, any> | null} */
  let model = null;
  let hiddenFlags = options.hiddenFlags ?? DEFAULT_HIDDEN_INSTANCE_FLAGS;
  // Worker mode: the kernel lives in a worker the client owns for the viewer's life.
  const workerOptions = options.worker === true ? {} : options.worker && typeof options.worker === "object" ? options.worker : null;
  /** @type {import("./kernel-client.js").KernelClient | null} */
  let kernelClient = null;
  /** @type {Set<number>} */
  const hidden = new Set();
  /** @type {Set<number>} */
  const shown = new Set();
  /** @type {Set<number> | null} */
  let isolated = null;
  /** @type {ViewerSelection | null} */
  let selection = null;
  let disposed = false;

  const scheduler = createFrameScheduler(
    () => {
      renderer.render();
      if (renderer.animating) scheduler.request(false);
    },
    {
      requestFrame: (callback) => requestAnimationFrame(callback),
      cancelFrame: (id) => cancelAnimationFrame(id),
      postTask: tasks.post,
      now: () => performance.now(),
      hidden: () => document.visibilityState !== "visible",
    },
  );
  const requestFrame = (urgent = !renderer.interacting && !renderer.streaming) => scheduler.request(urgent);
  renderer.onCameraChange = () => {
    requestFrame();
    emit("camera", { camera: renderer.camera });
  };
  renderer.onDirty = () => requestFrame(true);
  renderer.onFrameReady = () => requestFrame(true);
  const resizeObserver = new ResizeObserver(() => {
    renderer.resize();
    requestFrame(true);
  });
  resizeObserver.observe(container);

  renderer.setLodPixels?.(options.lodPixels ?? DEFAULT_LOD_PIXELS);
  if (options.theme || options.background) renderer.setViewportTheme(options.theme ?? "dark", options.background);
  renderer.setOcclusionCulling?.(options.occlusion !== false);
  renderer.setMotionLod?.(options.motionLod !== false);
  let textures = Boolean(options.textures);
  renderer.setTextures?.(textures, { allowRemote: Boolean(options.allowRemoteTextures), baseUrl: options.textureBaseUrl ?? null });

  /** The geometry settings of an open: the defaults, textures when wanted, then the caller's. */
  function geometrySettings(overrides) {
    return { ...GEOMETRY_SETTINGS, ...(textures ? { textures: true } : {}), ...overrides };
  }

  function emit(event, detail) {
    for (const listener of listeners.get(event) ?? []) listener(detail);
  }

  /** The visibility rule for `pack`; the renderer asks it per record. */
  function visibleIn(pack) {
    return (record) => {
      if (pack.instances.active && !pack.instances.active[record]) return false;
      if (isolated && !isolated.has(record)) return false;
      if (hidden.has(record)) return false;
      if (shown.has(record)) return true;
      return !(pack.instances.flags[record] & hiddenFlags);
    };
  }

  function refreshVisibility() {
    if (!model) return;
    renderer.setVisibility(visibleIn(model.pack));
    requestFrame(true);
    emit("visibility", { hidden: hidden.size, isolated: isolated ? isolated.size : null });
  }

  /**
   * Pack records of these express ids, in pack order.
   * @param {number | number[]} ids
   * @returns {number[]}
   */
  function recordsOf(ids) {
    if (!model) return [];
    const wanted = Array.isArray(ids) ? ids : [ids];
    const records = [];
    for (const id of wanted) for (const record of model.byExpressId.get(Number(id)) ?? []) records.push(record);
    return records;
  }

  function indexPack(pack) {
    const byExpressId = new Map();
    for (let record = 0; record < pack.instances.count; record += 1) {
      if (pack.instances.active && !pack.instances.active[record]) continue;
      const id = pack.instances.expressIds[record];
      const list = byExpressId.get(id);
      if (list) list.push(record);
      else byExpressId.set(id, [record]);
    }
    return byExpressId;
  }

  function resetState() {
    hidden.clear();
    shown.clear();
    isolated = null;
    selection = null;
  }

  function adopt(pack, assembler, extra) {
    model = { pack, assembler, byExpressId: indexPack(pack), overlay: "pending", session: null, ...extra };
    requestFrame(true);
    emit("load", { modelId: model.modelId ?? null, info: model.info ?? null, summary: model.summary ?? null, empty: Boolean(model.empty) });
    refineOverlay();
  }

  // ------------------------------------------------------------ revisions

  /**
   * The editing session over the open model, created on first use and kept in the scene's frame.
   * @returns {import("@tessifc/edit/session").Session}
   */
  function session() {
    if (model?.remote) {
      model.session ??= createRemoteSession(model.remote, () => model);
      return model.session;
    }
    if (!model?.kernel || model.modelId === null || model.modelId === undefined) throw new Error("Open a model with the kernel first.");
    model.session ??= createEditingSession(model.kernel, model.modelId, { settings: model.settings ?? GEOMETRY_SETTINGS });
    // The assembler allocates geometry ids; the session works in the same pack space.
    model.session.adopt({ modelOffset: model.pack.index.model_offset ?? [0, 0, 0], nextGeometryId: model.assembler.nextGeometryId() });
    return model.session;
  }

  function hierarchyGuids(hierarchy) {
    const guids = new Map();
    for (const node of hierarchy?.nodes ?? []) if (node.globalId) guids.set(node.expressId, node.globalId);
    return guids;
  }

  function hierarchyIds(hierarchy) {
    const ids = new Map();
    for (const node of hierarchy?.nodes ?? []) {
      if (!node.globalId) continue;
      ids.set(node.globalId, ids.has(node.globalId) ? null : node.expressId);
    }
    return ids;
  }

  function identitiesOf(records, guids) {
    const ids = new Set();
    for (const record of records) ids.add(model.pack.instances.expressIds[record]);
    return [...ids].map((id) => ({ id, guid: guids.get(id) ?? null }));
  }

  function restoreIdentities(identities, ids, fullRebuild) {
    return identities.map(({ id, guid }) => {
      if (!guid) return fullRebuild ? null : id;
      const found = ids.get(guid);
      if (Number.isInteger(found)) return found;
      return found === null && !fullRebuild ? id : null;
    }).filter((id) => Number.isInteger(id));
  }

  /**
   * Apply a scene delta from `@tessifc/edit` (a session's `runScript`,
   * `applySnapshot`, `undo`, or a delta received from elsewhere): affected and
   * removed products are retired, the delta's instances added, unrelated GPU
   * batches kept, and selection and visibility restored by product identity.
   * @param {Delta | (Partial<Delta> & { pack: import("@tessifc/edit/types").Pack })} delta
   * @returns {RevisionReport}
   */
  function applyDelta(delta) {
    if (!model) throw new Error("Open a model first.");
    if (!delta?.pack) throw new Error("applyDelta needs a delta with its parsed pack.");
    const full = delta.kind === "full" || Boolean(delta.fullRebuild);
    const affected = delta.affectedProducts ?? [];
    const removed = delta.removedProducts ?? [];
    const metadata = delta.metadataProducts ?? [];
    const guids = hierarchyGuids(model.hierarchy);
    const kept = { hidden: identitiesOf(hidden, guids), shown: identitiesOf(shown, guids), isolated: isolated ? identitiesOf(isolated, guids) : null,
      selected: selection ? selection.expressIds.map((id) => ({ id, guid: guids.get(id) ?? null })) : [] };
    let assembler = model.assembler;
    let result;
    if (full) {
      assembler = createPackAssembler();
      assembler.append(delta.pack);
      result = { changed: true };
    } else {
      result = assembler.replaceProducts([...new Set([...affected, ...removed])], delta.pack);
    }
    const pack = assembler.pack();
    const hierarchy = delta.hierarchy ?? model.hierarchy;
    const ids = hierarchyIds(hierarchy);
    model = { ...model, pack, assembler, byExpressId: indexPack(pack), hierarchy, empty: !pack.instances.count };
    const restore = (items) => new Set(recordsOf(restoreIdentities(items, ids, full)));
    const nextHidden = restore(kept.hidden);
    const nextShown = restore(kept.shown);
    hidden.clear();
    shown.clear();
    for (const record of nextHidden) hidden.add(record);
    for (const record of nextShown) shown.add(record);
    isolated = kept.isolated ? restore(kept.isolated) : null;
    if (result.changed) {
      if (full) renderer.reload(pack, visibleIn(pack));
      else renderer.applyDelta(pack, result, visibleIn(pack));
    }
    const selectedIds = restoreIdentities(kept.selected, ids, full).filter((id) => model.byExpressId.has(id));
    if (selectedIds.length) {
      selection = { expressIds: selectedIds, records: recordsOf(selectedIds) };
      renderer.select(selection.records);
    } else if (selection) {
      selection = null;
      renderer.select([]);
      emit("select", null);
    }
    if (result.changed && !full) renderer.flash?.(recordsOf([...affected, ...metadata]));
    if (result.changed) refineOverlay();
    requestFrame(true);
    const report = { revision: delta.revision ?? null, kind: delta.kind ?? (full ? "full" : "selective"), affectedProducts: affected,
      removedProducts: removed, metadataProducts: metadata, fullRebuild: full, changed: Boolean(result.changed) };
    emit("revision", report);
    return report;
  }

  let following = null;

  /**
   * Follow a local editing session (tessifc-mcp or the Python session server)
   * at `target`, a base URL or `{ baseUrl }`; "" means the page's own origin.
   * Every published version is opened or applied as a delta and reported as a
   * `revision` event; `session` events carry the host's status. Returns the client.
   * @param {string | { baseUrl?: string }} [target]
   */
  function follow(target = "") {
    unfollow();
    const baseUrl = typeof target === "string" ? target : target?.baseUrl ?? "";
    const client = createSessionClient({
      baseUrl: baseUrl.replace(/\/$/, ""),
      ready: () => !disposed && !streaming,
      loaded: () => Boolean(model?.kernel || model?.remote),
      open: (file) => open(file),
      update: async (file) => {
        const delta = await session().applySnapshot(new Uint8Array(await file.arrayBuffer()));
        return applyDelta(delta);
      },
      report: (message) => emit("session", { error: message }),
      status: (status) => emit("session", { status }),
    });
    following = client;
    return client;
  }

  function unfollow() {
    following?.stop();
    following = null;
  }

  // The coincident-surface overlay starts from bounding boxes; a worker then finds the
  // triangles that really share a plane with another product and the overlay shrinks to those.
  let overlayWorker = null;
  let overlayRequest = 0;
  function settleOverlay(current, state, triangles = 0) {
    current.overlay = state;
    emit("overlay", { state, triangles });
  }
  function refineOverlay() {
    const current = model;
    if (!current) return;
    if (options.coincidence === false || typeof Worker === "undefined") {
      settleOverlay(current, "off");
      return;
    }
    try {
      overlayWorker ??= new Worker(new URL("./contested-worker.js", import.meta.url), { type: "module" });
    } catch {
      settleOverlay(current, "off");
      return;
    }
    const { pack } = current;
    const requestId = ++overlayRequest;
    // Copies, not views: a view would clone the whole chunk buffer behind it.
    const geometries = pack.geometry.map((geometry) => ({
      id: geometry.id,
      positions: Float32Array.from(geometry.positions),
      indices: geometry.indices.slice(),
    }));
    const instances = {
      count: pack.instances.count,
      transforms: pack.instances.transforms.slice(),
      colors: pack.instances.colors.slice(),
      geometryIds: pack.instances.geometryIds.slice(),
      active: pack.instances.active ? pack.instances.active.slice() : null,
    };
    const transfer = [instances.transforms.buffer, instances.colors.buffer, instances.geometryIds.buffer];
    if (instances.active) transfer.push(instances.active.buffer);
    for (const geometry of geometries) transfer.push(geometry.positions.buffer, geometry.indices.buffer);
    const worker = overlayWorker;
    worker.onmessage = ({ data }) => {
      if (disposed || model !== current || data.requestId !== requestId) return;
      if (data.error) {
        settleOverlay(current, "failed");
      } else if (data.exhausted) {
        // Past its budget the analysis has no answer; whole products stay in the overlay.
        settleOverlay(current, "exhausted");
      } else {
        renderer.applyContestedTriangles(data);
        requestFrame(true);
        settleOverlay(current, "ready", data.triangles.length);
      }
    };
    worker.onerror = () => {
      worker.terminate();
      if (overlayWorker === worker) overlayWorker = null;
      if (!disposed && model === current && current.overlay === "pending") settleOverlay(current, "failed");
    };
    worker.postMessage({ requestId, instances, geometries }, transfer);
  }

  // --------------------------------------------------------------- loading

  // Each open, loadPack, close and dispose starts a generation; an open still
  // streaming from an older one stops at its next chunk and releases its kernel model.
  let loadGeneration = 0;
  let streaming = false;

  /**
   * Parse an IFC file (a `File`, `Blob`, `ArrayBuffer` or `Uint8Array`) with
   * the kernel and stream its geometry into the view. Resolves with the model
   * id, the kernel's model info, the geometry summary and the spatial
   * hierarchy, or with `null` when another `open`, `loadPack`, `close` or
   * `dispose` superseded it before it finished. The kernel keeps the model
   * open for inspection until `close`.
   * @param {Blob | ArrayBuffer | Uint8Array} source
   * @param {OpenOptions} [openOptions]
   * @returns {Promise<OpenResult | null>}
   */
  async function open(source, openOptions = {}) {
    if (workerOptions && !openOptions.kernel) return openInWorker(source, openOptions);
    const kernel = openOptions.kernel ?? options.kernel;
    if (!kernel) throw new Error("open needs a kernel: pass one to createViewer or to open");
    let generation = ++loadGeneration;
    const superseded = () => disposed || generation !== loadGeneration;
    const bytes = await toBytes(source);
    if (superseded()) return null;
    close();
    generation = loadGeneration;
    streaming = true;
    let modelId;
    try {
      modelId = kernel.openModel(bytes, openOptions.modelSettings ? JSON.stringify(openOptions.modelSettings) : undefined);
      const info = JSON.parse(kernel.getModelInfo(modelId));
      if (!info.entities) {
        const diagnostics = JSON.parse(kernel.getDiagnostics(modelId) ?? "[]");
        throw new Error(diagnostics[0]?.message ?? "No IFC entities were found in this file.");
      }
      const settings = JSON.stringify(geometrySettings(openOptions.settings));
      const assembler = createPackAssembler();
      resetState();
      renderer.beginStream();
      let summary;
      if (typeof kernel.beginGeometryStream === "function") {
        summary = JSON.parse(kernel.beginGeometryStream(modelId, settings));
        let budget = FIRST_CHUNK_MS;
        let chunk;
        while (!superseded() && (chunk = kernel.nextGeometryChunk(modelId, budget, 0, CHUNK_TRIANGLES))) {
          const { from, to } = assembler.append(readIgp(chunk));
          const pack = assembler.pack();
          renderer.appendStream(pack, from, to, visibleIn(pack));
          const progress = JSON.parse(kernel.streamProgress(modelId));
          emit("progress", { done: progress.done ?? to, total: progress.total ?? summary.products, triangles: progress.triangles ?? 0 });
          requestFrame();
          budget = Math.min(CHUNK_MS, budget * 2);
          // A macrotask between chunks lets the page paint what arrived.
          await new Promise((resolve) => setTimeout(resolve, 0));
        }
        const progress = JSON.parse(kernel.streamProgress(modelId));
        summary = { ...summary, products: progress.emitted, triangles: progress.triangles, diagnostics: progress.diagnostics ?? 0 };
      } else {
        summary = JSON.parse(kernel.evaluateGeometry(modelId, settings));
        const whole = typeof kernel.takePack === "function" ? kernel.takePack(modelId) : kernel.getPack(modelId);
        if (!whole) throw new Error("The geometry kernel did not return an IGP pack.");
        const { from, to } = assembler.append(readIgp(whole));
        const pack = assembler.pack();
        renderer.appendStream(pack, from, to, visibleIn(pack));
      }
      if (superseded()) {
        // The view already belongs to the newer load; only the kernel model is ours.
        kernel.closeModel(modelId);
        return null;
      }
      streaming = false;
      const pack = assembler.pack();
      const empty = !pack.instances.count || !pack.geometry.length;
      if (empty && (openOptions.requireGeometry ?? options.requireGeometry)) {
        renderer.clear();
        throw new Error("The IFC parsed correctly, but no supported product geometry was produced.");
      }
      // An empty scene stays open: a session or a delta adds the products.
      renderer.finishStream(pack, visibleIn(pack));
      const hierarchy = JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');
      adopt(pack, assembler, { modelId, kernel, info, summary, hierarchy, settings: geometrySettings(openOptions.settings), empty });
      return { modelId, info, summary, hierarchy, empty };
    } catch (error) {
      if (modelId !== undefined) kernel.closeModel(modelId);
      if (!superseded()) {
        streaming = false;
        renderer.clear();
        model = null;
      }
      throw error;
    }
  }

  /** The client of worker mode, started on first use. */
  function client() {
    if (!kernelClient) {
      kernelClient = createKernelClient({
        url: workerOptions.url,
        wasmUrl: workerOptions.wasmUrl,
        editUrl: workerOptions.editUrl,
        scriptTimeoutMs: workerOptions.scriptTimeoutMs,
        onReopen: (detail) => {
          if (!model?.remote) return;
          model = { ...model, modelId: detail.modelId, hierarchy: detail.hierarchy ?? model.hierarchy, info: detail.info ?? model.info };
          emit("reopen", { modelId: detail.modelId, revision: detail.revision });
        },
      });
    }
    return kernelClient;
  }

  /** `open` in worker mode: the worker parses and tessellates, the page only assembles and draws. */
  async function openInWorker(source, openOptions) {
    let generation = ++loadGeneration;
    const superseded = () => disposed || generation !== loadGeneration;
    const kernel = client();
    await kernel.ready();
    if (superseded()) return null;
    close();
    generation = loadGeneration;
    streaming = true;
    const settings = geometrySettings(openOptions.settings);
    const assembler = createPackAssembler();
    resetState();
    renderer.beginStream();
    let total = 0;
    try {
      const result = await kernel.open(source, {
        settings,
        onPhase: ({ progress }) => {
          if (progress?.total) total = progress.total;
        },
        onChunk: ({ buffer, progress }) => {
          if (superseded()) return;
          const { from, to } = assembler.append(readIgp(buffer));
          const pack = assembler.pack();
          renderer.appendStream(pack, from, to, visibleIn(pack));
          emit("progress", { done: progress?.done ?? to, total: progress?.total ?? total, triangles: progress?.triangles ?? 0 });
          requestFrame();
        },
      });
      if (superseded()) return null;
      streaming = false;
      const pack = assembler.pack();
      const empty = !pack.instances.count || !pack.geometry.length;
      if (empty && (openOptions.requireGeometry ?? options.requireGeometry)) {
        renderer.clear();
        kernel.close();
        throw new Error("The IFC parsed correctly, but no supported product geometry was produced.");
      }
      renderer.finishStream(pack, visibleIn(pack));
      adopt(pack, assembler, { modelId: result.modelId, kernel: null, remote: kernel, info: result.info, summary: result.summary,
        hierarchy: result.hierarchy, settings, empty });
      return { modelId: result.modelId, info: result.info, summary: result.summary, hierarchy: result.hierarchy, empty };
    } catch (error) {
      // A later open, a close or a dispose ended this one; the newer owner has the view.
      if (superseded()) return null;
      streaming = false;
      renderer.clear();
      model = null;
      throw error;
    }
  }

  /**
   * Show an IGP pack directly, for example one written by the `tessifc` CLI. No kernel needed.
   * @param {Uint8Array | ArrayBuffer} source
   * @returns {{ pack: AssembledPack }}
   */
  function loadPack(source) {
    close();
    const pack = readIgp(source instanceof Uint8Array ? source : new Uint8Array(source));
    if (!pack.instances.count || !pack.geometry.length) throw new Error("The pack holds no geometry.");
    const assembler = createPackAssembler();
    assembler.append(pack);
    const assembled = assembler.pack();
    resetState();
    renderer.load(assembled, visibleIn(assembled));
    adopt(assembled, assembler, { modelId: null, kernel: null, info: null, summary: null, hierarchy: null });
    return { pack: assembled };
  }

  /** Drop the model from the view and, when the kernel opened it, from the kernel. */
  function close() {
    loadGeneration += 1;
    if (!model && !streaming) return;
    const kernel = model?.kernel;
    const modelId = model?.modelId;
    model?.session?.close();
    model = null;
    const wasStreaming = streaming;
    streaming = false;
    resetState();
    renderer.clear();
    // A stale analysis would only delay the next model's; the worker is cheap to restart.
    overlayWorker?.terminate();
    overlayWorker = null;
    if (kernel && modelId !== null && modelId !== undefined) kernel.closeModel(modelId);
    // The worker drops its model too; one still streaming is abandoned with its worker.
    if (kernelClient && (modelId !== undefined || wasStreaming)) kernelClient.close();
    requestFrame(true);
    emit("close", null);
  }

  // -------------------------------------------------------------- selection

  /**
   * The product under a client point, or `null`.
   * @param {number} clientX
   * @param {number} clientY
   * @returns {{ record: number, expressId: number, point: number[] | null } | null}
   */
  function pick(clientX, clientY) {
    if (!model) return null;
    const hit = renderer.pick(clientX, clientY, true);
    if (!hit) return null;
    return { record: hit.record, expressId: model.pack.instances.expressIds[hit.record], point: hit.point };
  }

  /**
   * Select one express id, several, or `null` to clear; every part of a product is selected together.
   * @param {number | number[] | null | undefined} ids
   */
  function select(ids) {
    if (!model) return;
    const list = ids === null || ids === undefined ? [] : Array.isArray(ids) ? ids : [ids];
    const records = recordsOf(list);
    selection = records.length ? { expressIds: [...new Set(list.map(Number))], records } : null;
    renderer.select(records);
    requestFrame(true);
    emit("select", selection ? { ...selection } : null);
  }

  function pickAndSelect(clientX, clientY) {
    const hit = pick(clientX, clientY);
    if (!hit) {
      select(null);
      return;
    }
    // The clicked surface becomes the orbit and zoom centre.
    if (hit.point) renderer.setPivot(hit.point);
    select(hit.expressId);
  }

  let pointerDown = null;
  const canvas = renderer.canvas;
  const onPointerDown = (event) => {
    if (event.button !== 0) return;
    pointerDown = { x: event.clientX, y: event.clientY, id: event.pointerId };
  };
  const onPointerMove = (event) => {
    if (pointerDown?.id === event.pointerId &&
        Math.hypot(event.clientX - pointerDown.x, event.clientY - pointerDown.y) > CLICK_TRAVEL_PX) pointerDown = null;
  };
  const onPointerUp = (event) => {
    if (!pointerDown || pointerDown.id !== event.pointerId) return;
    const travelled = Math.hypot(event.clientX - pointerDown.x, event.clientY - pointerDown.y);
    pointerDown = null;
    if (travelled <= CLICK_TRAVEL_PX && options.selectOnClick !== false) pickAndSelect(event.clientX, event.clientY);
  };
  const onPointerCancel = () => {
    pointerDown = null;
  };
  const onDoubleClick = (event) => {
    if (event.button !== 0 || options.focusOnDoubleClick === false) return;
    const hit = pick(event.clientX, event.clientY);
    if (hit) focus(hit.expressId);
  };
  canvas.addEventListener("pointerdown", onPointerDown);
  canvas.addEventListener("pointermove", onPointerMove);
  canvas.addEventListener("pointerup", onPointerUp);
  canvas.addEventListener("pointercancel", onPointerCancel);
  canvas.addEventListener("dblclick", onDoubleClick);

  // ------------------------------------------------------------- visibility

  /** @param {number | number[]} ids */
  function hide(ids) {
    for (const record of recordsOf(ids)) {
      hidden.add(record);
      shown.delete(record);
    }
    if (selection?.records.some((record) => hidden.has(record))) select(null);
    refreshVisibility();
  }

  /** @param {number | number[]} ids */
  function show(ids) {
    for (const record of recordsOf(ids)) {
      hidden.delete(record);
      shown.add(record);
    }
    refreshVisibility();
  }

  /**
   * Show only these products; `null` ends the isolation.
   * @param {number | number[] | null | undefined} ids
   */
  function isolate(ids) {
    isolated = ids === null || ids === undefined ? null : new Set(recordsOf(ids));
    refreshVisibility();
  }

  function showAll() {
    hidden.clear();
    isolated = null;
    refreshVisibility();
  }

  /**
   * Change which helper categories stay hidden by default, as instance flag bits.
   * @param {number} flags
   */
  function setHiddenFlags(flags) {
    hiddenFlags = Number(flags) || 0;
    shown.clear();
    refreshVisibility();
  }

  // ----------------------------------------------------------------- camera

  function fit() {
    renderer.fit();
    requestFrame(true);
  }

  /**
   * Frame these products, or the selection when called without ids.
   * @param {number | number[]} [ids]
   */
  function focus(ids) {
    const records = ids === undefined ? selection?.records ?? [] : recordsOf(ids);
    if (!records.length) return;
    renderer.focus(records);
    requestFrame(true);
  }

  /**
   * @param {"perspective" | "top" | "front" | "right"} mode
   * @param {boolean} [refit]
   */
  function setView(mode, refit = true) {
    if (!VIEWS.includes(mode)) throw new RangeError(`unknown view ${mode}`);
    renderer.setView(mode, refit);
    requestFrame(true);
  }

  /** @param {"shaded" | "xray" | "wire"} style */
  /**
   * Draw the pack's textures, or every product in its flat colour. Takes
   * effect on the model in view at once; a model opened while this was off
   * carries no textures until it is opened again.
   * @param {boolean} active
   */
  function setTextures(active) {
    textures = Boolean(active);
    renderer.setTextures?.(textures);
    requestFrame(true);
  }

  /**
   * Leave product groups hidden behind the model's largest faces out of moving frames, or draw everything.
   * @param {boolean} active
   */
  function setOcclusion(active) {
    renderer.setOcclusionCulling?.(active !== false);
    requestFrame(true);
  }

  /**
   * Draw the pack's coarse mesh levels on moving frames, or the full meshes always.
   * @param {boolean} active
   */
  function setMotionLod(active) {
    renderer.setMotionLod?.(active !== false);
    requestFrame(true);
  }

  function setStyle(style) {
    if (!STYLES.includes(style)) throw new RangeError(`unknown style ${style}`);
    renderer.setStyle(style);
    requestFrame(true);
  }

  /**
   * Cut the model at `value` along `axis` (`"x"`, `"y"` or `"z"`), in IFC
   * coordinates. `fraction` between 0 and 1 places the cut across the model
   * bounds instead. `flipped` keeps the other side. `null` removes the cut.
   * @param {SectionOptions | null} section
   */
  function setSection(section) {
    if (!model) return;
    if (!section) {
      renderer.setSection(false, "z", 0);
    } else {
      const axis = section.axis ?? "z";
      const value = section.fraction !== undefined
        ? renderer.sectionValue(axis, section.fraction)
        : Number(section.value) - (model.pack.index.model_offset ?? [0, 0, 0])[{ x: 0, y: 1, z: 2 }[axis] ?? 2];
      renderer.setSection(true, axis, value, Boolean(section.flipped), section.cap !== false);
    }
    requestFrame(true);
  }

  // ------------------------------------------------------------------ events

  /**
   * Listen for `load`, `progress`, `select`, `visibility`, `camera`, `overlay`, `close`, `revision`, `session` or `reopen` (worker mode brought a model back after a stopped script); returns the unsubscribe function.
   * @param {"load" | "progress" | "select" | "visibility" | "camera" | "overlay" | "close" | "revision" | "session" | "reopen"} event
   * @param {(detail: any) => void} listener
   */
  function on(event, listener) {
    const list = listeners.get(event) ?? [];
    list.push(listener);
    listeners.set(event, list);
    return () => {
      const index = list.indexOf(listener);
      if (index >= 0) list.splice(index, 1);
    };
  }

  function dispose() {
    if (disposed) return;
    disposed = true;
    unfollow();
    close();
    scheduler.cancel();
    tasks.dispose();
    resizeObserver.disconnect();
    canvas.removeEventListener("pointerdown", onPointerDown);
    canvas.removeEventListener("pointermove", onPointerMove);
    canvas.removeEventListener("pointerup", onPointerUp);
    canvas.removeEventListener("pointercancel", onPointerCancel);
    canvas.removeEventListener("dblclick", onDoubleClick);
    overlayWorker?.terminate();
    overlayWorker = null;
    kernelClient?.dispose();
    kernelClient = null;
    renderer.dispose();
    listeners.clear();
  }

  return Object.assign(view, {
    open,
    loadPack,
    close,
    pick,
    select,
    selection: () => (selection ? { ...selection } : null),
    hide,
    show,
    isolate,
    showAll,
    setHiddenFlags,
    fit,
    focus,
    setView,
    setStyle,
    setTextures,
    setOcclusion,
    setMotionLod,
    setSection,
    /** @param {number[]} point */
    setPivot: (point) => renderer.setPivot(point) && requestFrame(true),
    /** @param {number} factor */
    zoom: (factor) => renderer.zoomAt(factor),
    render: () => requestFrame(true),
    resize: () => renderer.resize(),
    on,
    dispose,
    applyDelta,
    session,
    follow,
    unfollow,
    /** The assembled IGP pack of the open model, or `null`. */
    pack: () => /** @type {AssembledPack | null} */ (model?.pack ?? null),
    /** The kernel's spatial hierarchy for the open model, or `null` for a pack. */
    hierarchy: () => /** @type {{ nodes: Array<Record<string, any>> } | null} */ (model?.hierarchy ?? null),
    /** `pending`, `ready`, `exhausted`, `failed` or `off`: whether the overlay has been refined to shared planes. */
    overlayState: () => /** @type {"pending" | "ready" | "exhausted" | "failed" | "off" | null} */ (model?.overlay ?? null),
    modelId: () => /** @type {number | null} */ (model?.modelId ?? null),
    /** The coarse levels in the pack and on the GPU, and whether the last frame drew them. */
    lodState: () => renderer.meshLevelState?.() ?? null,
    /** Worker mode's kernel: whether its worker runs, the kernel version once it answered, and the script limit; `null` on the page-thread path. */
    worker: () => (kernelClient ? { running: kernelClient.running, version: kernelClient.version, scriptTimeoutMs: kernelClient.scriptTimeoutMs } : workerOptions ? { running: false, version: null, scriptTimeoutMs: null } : null),
  });
}

/**
 * The session of worker mode: the same calls as `createEditingSession`, each
 * returning a promise, since the model lives in the worker. The scene basis a
 * revision needs is read from the viewer's pack at each call.
 * @param {import("./kernel-client.js").KernelClient} client
 * @param {() => Record<string, any> | null} current the viewer's model
 */
function createRemoteSession(client, current) {
  const id = () => {
    const model = current();
    if (!model?.remote) throw new Error("The model is no longer open.");
    return model.modelId;
  };
  const patch = () => {
    const model = current();
    return {
      baseRevision: client.model?.revision ?? "0",
      modelOffset: model.pack.index.model_offset ?? [0, 0, 0],
      firstGeometryId: model.assembler.nextGeometryId(),
      selectedExpressId: null,
    };
  };
  const deltaOf = (promise) => promise.then((result) => result.delta);
  return {
    /** True: every call below returns a promise. */
    remote: true,
    get modelId() {
      return id();
    },
    get revision() {
      return client.model?.revision ?? null;
    },
    get history() {
      return client.model?.history ?? { undo: 0, redo: 0 };
    },
    settings: () => ({ ...(current()?.settings ?? {}) }),
    /** The scene basis is read at each call; nothing to adopt. */
    adopt() {},
    /**
     * @param {string} source
     * @param {unknown} [selection]
     * @param {{ commit?: boolean }} [runOptions]
     * @returns {Promise<{ report: any, delta: any }>}
     */
    runScript: (source, selection = null, runOptions = {}) =>
      client.runScript(id(), source, selection, { commit: runOptions.commit !== false, patch: patch() }),
    /** @param {Array<Record<string, unknown>>} edits */
    setAttributes: (edits) => deltaOf(client.setAttributes(id(), edits, patch())),
    /** @param {Uint8Array | ArrayBuffer | string} bytes */
    applySnapshot: (bytes) => deltaOf(client.applySnapshot(id(), typeof bytes === "string" ? new TextEncoder().encode(bytes) : bytes, patch())),
    undo: () => deltaOf(client.undo(id(), patch())),
    redo: () => deltaOf(client.redo(id(), patch())),
    export: () => client.exportModel(id()),
    /** @param {number} expressId */
    entity: (expressId) => client.query(id(), "entity", expressId),
    /** @param {string} className */
    classDefinition: (className) => client.query(id(), "classDefinition", className),
    info: () => client.query(id(), "info"),
    /** @param {string} className */
    idsOfType: (className) => client.query(id(), "idsOfType", className),
    diagnostics: () => client.query(id(), "diagnostics"),
    hierarchy: () => client.query(id(), "hierarchy"),
    close() {},
  };
}

/** @param {Blob | ArrayBuffer | Uint8Array} source */
async function toBytes(source) {
  if (source instanceof Uint8Array) return source;
  if (source instanceof ArrayBuffer) return new Uint8Array(source);
  if (typeof Blob !== "undefined" && source instanceof Blob) return new Uint8Array(await source.arrayBuffer());
  throw new TypeError("open expects a File, Blob, ArrayBuffer or Uint8Array");
}
