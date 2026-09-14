// SPDX-License-Identifier: Apache-2.0

//! A minimal IFC viewer: one container, one kernel, a small API. The reference
//! application adds its panels and ribbon on top of these same pieces.

import { DEFAULT_HIDDEN_INSTANCE_FLAGS, readIgp } from "@tessifc/edit/igp";
import { DEFAULT_LOD_PIXELS, IfcRenderer } from "./renderer.js";
import { createPackAssembler } from "./stream.js";
import { createFrameScheduler, createTaskQueue } from "./scheduler.js";

export { IfcRenderer, DEFAULT_LOD_PIXELS } from "./renderer.js";
export { createPackAssembler } from "./stream.js";
export { createFrameScheduler, createTaskQueue } from "./scheduler.js";
export { frameSphere, wheelZoomFactor, zoomCamera } from "./navigation.js";
export { projectPoint, snapToTriangle, measurementBetween } from "./measure.js";
export { findContestedTriangles, planeKey } from "./depth-planes.js";

/** Helper geometry is kept in the pack so a host can reveal it without converting again. */
export const GEOMETRY_SETTINGS = {
  includeSpaces: true,
  includeOpenings: true,
  includeAnnotations: true,
  includeReferences: true,
};

/** Display styles the renderer understands. */
export const STYLES = ["shaded", "xray", "wire"];
/** Camera modes `setView` accepts. */
export const VIEWS = ["perspective", "top", "front", "right"];

// A small first chunk for an early paint; later chunks grow to amortise overhead.
const FIRST_CHUNK_MS = 45;
const CHUNK_MS = 220;
const CHUNK_TRIANGLES = 600_000;
// A release that travelled further than this is a drag, not a click.
const CLICK_TRAVEL_PX = 4;

/**
 * Create a viewer inside `container`.
 *
 * Options: `kernel` (a `Kernel` from `@tessifc/core/web`, needed by `open`),
 * `hiddenFlags` (instance flags hidden at load; spaces, openings and
 * references by default), `lodPixels` (skip products smaller than this on
 * screen, 0 to draw everything), `theme` (`"light"` or `"dark"`) and
 * `background` (a CSS hex colour for the canvas).
 */
export function createViewer(container, options = {}) {
  if (!(container instanceof Element)) throw new TypeError("createViewer needs a DOM element");
  const renderer = new IfcRenderer(container);
  const tasks = createTaskQueue();
  const listeners = new Map();
  const view = { renderer };

  let model = null;
  let hiddenFlags = options.hiddenFlags ?? DEFAULT_HIDDEN_INSTANCE_FLAGS;
  const hidden = new Set();
  const shown = new Set();
  let isolated = null;
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

  /** Pack records of these express ids, in pack order. */
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
    model = { pack, assembler, byExpressId: indexPack(pack), overlay: "pending", ...extra };
    requestFrame(true);
    emit("load", { modelId: model.modelId ?? null, info: model.info ?? null, summary: model.summary ?? null });
    refineOverlay();
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
   */
  async function open(source, openOptions = {}) {
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
      const settings = JSON.stringify({ ...GEOMETRY_SETTINGS, ...openOptions.settings });
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
      if (!pack.instances.count || !pack.geometry.length) {
        renderer.clear();
        throw new Error("The IFC parsed correctly, but no supported product geometry was produced.");
      }
      renderer.finishStream(pack, visibleIn(pack));
      const hierarchy = JSON.parse(kernel.getSpatialHierarchy?.(modelId) ?? '{"nodes":[]}');
      adopt(pack, assembler, { modelId, kernel, info, summary, hierarchy });
      return { modelId, info, summary, hierarchy };
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

  /** Show an IGP pack directly, for example one written by the `tessifc` CLI. No kernel needed. */
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
    model = null;
    streaming = false;
    resetState();
    renderer.clear();
    // A stale analysis would only delay the next model's; the worker is cheap to restart.
    overlayWorker?.terminate();
    overlayWorker = null;
    if (kernel && modelId !== null && modelId !== undefined) kernel.closeModel(modelId);
    requestFrame(true);
    emit("close", null);
  }

  // -------------------------------------------------------------- selection

  function pick(clientX, clientY) {
    if (!model) return null;
    const hit = renderer.pick(clientX, clientY, true);
    if (!hit) return null;
    return { record: hit.record, expressId: model.pack.instances.expressIds[hit.record], point: hit.point };
  }

  /** Select one express id, several, or `null` to clear; every part of a product is selected together. */
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

  function hide(ids) {
    for (const record of recordsOf(ids)) {
      hidden.add(record);
      shown.delete(record);
    }
    if (selection?.records.some((record) => hidden.has(record))) select(null);
    refreshVisibility();
  }

  function show(ids) {
    for (const record of recordsOf(ids)) {
      hidden.delete(record);
      shown.add(record);
    }
    refreshVisibility();
  }

  /** Show only these products; `null` ends the isolation. */
  function isolate(ids) {
    isolated = ids === null || ids === undefined ? null : new Set(recordsOf(ids));
    refreshVisibility();
  }

  function showAll() {
    hidden.clear();
    isolated = null;
    refreshVisibility();
  }

  /** Change which helper categories stay hidden by default, as instance flag bits. */
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

  /** Frame these products, or the selection when called without ids. */
  function focus(ids) {
    const records = ids === undefined ? selection?.records ?? [] : recordsOf(ids);
    if (!records.length) return;
    renderer.focus(records);
    requestFrame(true);
  }

  function setView(mode, refit = true) {
    if (!VIEWS.includes(mode)) throw new RangeError(`unknown view ${mode}`);
    renderer.setView(mode, refit);
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

  /** Listen for `load`, `progress`, `select`, `visibility`, `camera`, `overlay` or `close`; returns the unsubscribe function. */
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
    setSection,
    setPivot: (point) => renderer.setPivot(point) && requestFrame(true),
    zoom: (factor) => renderer.zoomAt(factor),
    render: () => requestFrame(true),
    resize: () => renderer.resize(),
    on,
    dispose,
    /** The assembled IGP pack of the open model, or `null`. */
    pack: () => model?.pack ?? null,
    /** The kernel's spatial hierarchy for the open model, or `null` for a pack. */
    hierarchy: () => model?.hierarchy ?? null,
    /** `pending`, `ready`, `exhausted`, `failed` or `off`: whether the overlay has been refined to shared planes. */
    overlayState: () => model?.overlay ?? null,
    modelId: () => model?.modelId ?? null,
  });
}

async function toBytes(source) {
  if (source instanceof Uint8Array) return source;
  if (source instanceof ArrayBuffer) return new Uint8Array(source);
  if (typeof Blob !== "undefined" && source instanceof Blob) return new Uint8Array(await source.arrayBuffer());
  throw new TypeError("open expects a File, Blob, ArrayBuffer or Uint8Array");
}
