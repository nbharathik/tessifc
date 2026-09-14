// SPDX-License-Identifier: Apache-2.0

//! Viewer entry point. Owns the geometry worker, the open model, selection and
//! visibility; the panel modules draw what the user sees and this file decides
//! when.

import {
  DEFAULT_HIDDEN_INSTANCE_FLAGS,
  INSTANCE_OPENING,
  INSTANCE_REFERENCE,
  INSTANCE_SPACE,
  readIgp,
} from "./igp.js";
import { createPackAssembler, isIdentity } from "./stream.js";
import { DEFAULT_LOD_PIXELS as LOD_PIXELS, IfcRenderer } from "./renderer.js";
import { createShell } from "./shell.js";
import { createTree } from "./tree.js";
import { createInspector } from "./inspector.js";
import { createTools } from "./tools.js";
import { createGizmo } from "./gizmo.js";
import { createFrameScheduler, createTaskQueue } from "./scheduler.js";
import { bytes, coordinate, count, duration, errorText, plural } from "./format.js";
import { startFileSession } from "./file-session.js";
import { createSessionPanel } from "./session-panel.js";
import { PROVIDERS, createBrowserAssistant } from "./assistant.js";

const $ = (id) => document.getElementById(id);

const HELPER_FILTERS = [
  { command: "spaces", flag: INSTANCE_SPACE, label: "spaces", title: "space and zone volumes" },
  { command: "openings", flag: INSTANCE_OPENING, label: "openings", title: "opening and void volumes" },
  { command: "references", flag: INSTANCE_REFERENCE, label: "guides", title: "grids, annotations and reference geometry" },
];

const SETTINGS_KEY = "tessifc.settings";
const DEFAULT_SETTINGS = { scale: 1, hideSemantic: true, adaptive: false, lod: true, coincident: true };

/** Rendering settings from the last visit; a blocked or stale store falls back. */
function loadSettings() {
  try {
    const stored = JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "{}");
    if (!stored || typeof stored !== "object") return { ...DEFAULT_SETTINGS };
    const settings = { ...DEFAULT_SETTINGS };
    for (const key of Object.keys(DEFAULT_SETTINGS)) {
      if (typeof stored[key] === typeof DEFAULT_SETTINGS[key]) settings[key] = stored[key];
    }
    // The scale is a fraction of native resolution now; an older stored limit above it means native.
    settings.scale = Math.min(1, Math.max(0.25, settings.scale));
    return settings;
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

const state = {
  worker: null,
  workerReady: false,
  converting: false,
  loadOutcome: "idle",
  dismissLoadError: null,
  jobId: 0,
  requestId: 0,
  loadStarted: 0,
  queuedFile: null,
  replaceApproved: null,
  pendingFile: null,
  model: null,
  // The conversion in flight: its chunk assembler and first-paint time.
  stream: null,
  selection: null,
  dirty: false,
  revisionPending: null,
  fileSession: null,
  scriptHistory: { undo: 0, redo: 0 },
  hiddenClasses: new Set(),
  hiddenRecords: new Set(),
  shownRecords: new Set(),
  hiddenInstanceFlags: DEFAULT_HIDDEN_INSTANCE_FLAGS,
  isolated: null,
  dragDepth: 0,
  settings: loadSettings(),
};

let renderer;
try {
  renderer = new IfcRenderer($("viewport"));
} catch (error) {
  fatal(errorText(error));
  throw error;
}

const shell = createShell();
const tools = createTools({ renderer, shell, scheduleRender });
const inspector = createInspector({ onApplyEdits: applyEdits });
const assistant = createBrowserAssistant({
  inspect: (code, selection) => runBrowserScript(code, selection, { commit: false }),
  execute: (script, selection) => runBrowserScript(script, selection),
  undo: () => browserHistoryAction("undo"),
  context: assistantContext,
});
const sessionPanel = createSessionPanel({
  shell,
  getSession: () => state.fileSession,
  getSelection: selectionSummary,
  hasModel: () => Boolean(state.model && !state.model.stale),
  browser: {
    run: (script, selection) => runBrowserScript(script, selection),
    undo: () => browserHistoryAction("undo"),
    redo: () => browserHistoryAction("redo"),
    history: () => state.scriptHistory,
  },
  assistant,
});
const tree = createTree({
  onVisibility: setNodeVisible,
  onSelect: (expressId) => selectExpressId(expressId),
  onFocus: (records) => {
    renderer.focus(records);
    scheduleRender();
  },
});

let statsFrame = 0;
let syncMemoryNextFrame = false;
let syncGizmoNextFrame = false;
let pointerDown = null;
const uiTasks = createTaskQueue();
const renderScheduler = createFrameScheduler(renderScene, {
  requestFrame: (callback) => requestAnimationFrame(callback),
  cancelFrame: (id) => cancelAnimationFrame(id),
  postTask: uiTasks.post,
  now: () => performance.now(),
  hidden: () => document.visibilityState !== "visible",
});

// ------------------------------------------------------------- rendering

function scheduleRender(updateMemory = false, urgent = !renderer.interacting && !renderer.streaming) {
  syncMemoryNextFrame ||= updateMemory;
  renderScheduler.request(urgent);
}

function renderScene() {
  const memory = syncMemoryNextFrame;
  syncMemoryNextFrame = false;
  renderer.render();
  tools.drawOverlay();
  // A highlight fade keeps asking for frames until it is done.
  if (renderer.animating) renderScheduler.request(false);
  if (syncGizmoNextFrame) {
    syncGizmoNextFrame = false;
    syncGizmo();
  }
  if (memory) {
    syncGpuMemory();
    inspector.setDisplayFacts(renderer);
  }
}

function syncGpuMemory() {
  if (!state.model || typeof renderer.currentGpuBytes !== "function") return;
  const value = renderer.currentGpuBytes();
  if (value === state.model.gpuBytes) return;
  state.model.gpuBytes = value;
  inspector.setGpuBytes(value);
}

const resizeObserver = new ResizeObserver(() => {
  renderer.resize();
  scheduleRender(true);
});
resizeObserver.observe($("viewport"));

const gizmo = createGizmo({
  host: $("gizmo-host"),
  onPick: (direction) => tools.viewAlong(direction),
});
const syncGizmo = () => gizmo.update(renderer.cameraBasis());
let cameraWasInteracting = false;
renderer.onCameraChange = () => {
  syncGizmoNextFrame = true;
  const urgent = !renderer.streaming && (!renderer.interacting || !cameraWasInteracting);
  cameraWasInteracting = renderer.interacting;
  scheduleRender(false, urgent);
};
// A settled resize or a prepared gesture target needs a frame nobody else asked for.
renderer.onDirty = () => scheduleRender(true);
renderer.onFrameReady = () => scheduleRender(false, true);
syncGizmo();

// The frame after an input carries the model; the tree and the attribute list follow in
// the next one, from a task posted inside that frame, so they never delay the first paint.
const panelWork = { selection: false, visibility: false, properties: undefined, frame: 0 };

function schedulePanelWork() {
  if (panelWork.frame) return;
  panelWork.frame = requestAnimationFrame(() => uiTasks.post(runPanelWork));
}

function runPanelWork() {
  panelWork.frame = 0;
  const { selection, visibility, properties } = panelWork;
  panelWork.selection = false;
  panelWork.visibility = false;
  panelWork.properties = undefined;
  if (!state.model) return;
  if (visibility) tree.syncVisibility(isRecordVisible);
  if (selection) {
    const expressId = state.selection?.expressId ?? null;
    tree.select(expressId);
  }
  if (properties && properties.selection === state.selection && properties.info === state.selection.info) {
    inspector.setProperties(properties.info);
  }
}

shell.on("resize", () => requestAnimationFrame(() => {
  renderer.resize();
  scheduleRender(true);
}));

// --------------------------------------------------------------- commands

shell.register([
  { id: "open", label: "Open IFC file", section: "File", hint: "Ctrl O", run: () => $("file-input").click() },
  { id: "export", label: "Export IFC", section: "File", hint: "Ctrl S", disabled: true, run: requestExport },
  { id: "update-ifc", label: "Update from edited IFC", section: "File", disabled: true, run: () => $("revision-input").click() },
  { id: "close", label: "Close model", section: "File", disabled: true, run: closeModel },
  { id: "settings", label: "Settings", section: "Workspace", hint: "Ctrl ,", run: shell.openSettings },
  { id: "help", label: "Keyboard shortcuts", section: "Workspace", hint: "?", run: shell.openHelp },

  { id: "fit", label: "Frame the model", section: "View", hint: "F", disabled: true, run: () => tools.fitView() },
  { id: "zoom-in", label: "Zoom in", section: "View", hint: "+", disabled: true, run: () => renderer.zoomAt(0.8) },
  { id: "zoom-out", label: "Zoom out", section: "View", hint: "-", disabled: true, run: () => renderer.zoomAt(1.25) },
  { id: "focus", label: "Frame the selection", section: "View", hint: "Shift F", disabled: true, run: focusSelection },
  { id: "plan", label: "Sectioned plan view, or back to 3D", section: "View", hint: "P", disabled: true, run: () => tools.planView() },
  { id: "style", label: "Cycle the display style", section: "View", hint: "D", disabled: true, run: () => tools.cycleStyle() },
  { id: "spaces", label: "Show spaces", section: "View", disabled: true, run: () => toggleHelperFlag(INSTANCE_SPACE) },
  { id: "openings", label: "Show openings", section: "View", disabled: true, run: () => toggleHelperFlag(INSTANCE_OPENING) },
  { id: "references", label: "Show guides", section: "View", disabled: true, run: () => toggleHelperFlag(INSTANCE_REFERENCE) },
  { id: "view:perspective", label: "Isometric perspective", section: "View", hint: "4", disabled: true, run: () => tools.setView("perspective", true) },
  { id: "view:top", label: "Top view", section: "View", hint: "3", disabled: true, run: () => tools.setView("top", true) },
  { id: "view:front", label: "Front elevation", section: "View", hint: "1", disabled: true, run: () => tools.setView("front", true) },
  { id: "view:right", label: "Right elevation", section: "View", hint: "2", disabled: true, run: () => tools.setView("right", true) },

  { id: "measure", label: "Measure point to point", section: "Tools", hint: "M", disabled: true, run: () => tools.setMeasure(!tools.isMeasuring()) },
  { id: "section", label: "Section plane", section: "Tools", hint: "X", disabled: true, run: () => tools.setSection(!tools.isSectioning()) },

  { id: "isolate", label: "Isolate the selection", section: "Selection", hint: "I", disabled: true, run: toggleIsolation },
  { id: "hide", label: "Hide the selection", section: "Selection", hint: "H", disabled: true, run: hideSelection },
  { id: "show-all", label: "Show everything", section: "Selection", hint: "A", disabled: true, run: restoreVisibility },
  { id: "clear-selection", label: "Clear the selection", section: "Selection", hint: "Esc", disabled: true, run: clearSelection },
  { id: "edit", label: "Edit attributes", section: "Selection", disabled: true, run: () => setEditor(true) },

  { id: "toggle-outliner", label: "Toggle the structure panel", section: "Panels", hint: "Ctrl B", run: () => shell.togglePanel("outliner") },
  { id: "toggle-inspector", label: "Toggle the inspector", section: "Panels", hint: "\\", run: () => shell.togglePanel("inspector") },
  { id: "toggle-editor", label: "Toggle the edit panel", section: "Panels", hint: "E", run: () => setEditor(!shell.panelVisible("editor")) },
  { id: "toggle-session", label: "Toggle the session panel", section: "Panels", run: () => shell.togglePanel("session") },
  { id: "session-script", label: "Python script panel", section: "Session", run: () => sessionPanel.open("script") },
  { id: "session-assistant", label: "Assistant panel", section: "Session", run: () => sessionPanel.open("assistant") },
  { id: "properties", label: "Show properties", section: "Panels", run: () => shell.setInspectorPanel("properties") },
  { id: "element", label: "Show element details", section: "Panels", run: () => shell.setInspectorPanel("element") },
  { id: "model-stats", label: "Show model statistics", section: "Panels", disabled: true, run: () => shell.setInspectorPanel("model") },
  { id: "quality", label: "Show the conversion report", section: "Panels", disabled: true, run: () => shell.setInspectorPanel("quality") },

  { id: "tree-expand", label: "Expand every branch", section: "Structure", disabled: true, run: () => tree.expand(true) },
  { id: "tree-collapse", label: "Collapse the structure", section: "Structure", disabled: true, run: () => tree.expand(false) },

  { id: "theme", label: "Switch the theme: system, light, dark", section: "Workspace", run: () => shell.cycleTheme() },
  { id: "canvas-theme", label: "Switch the viewport background", section: "Workspace", run: () => shell.cycleCanvasTheme() },
]);

shell.setEscapeHandler(() => {
  if (tools.isMeasuring()) tools.escapeMeasure();
  else if (tools.isSectioning()) tools.setSection(false);
  else clearSelection();
});

shell.on("canvasTheme", (theme) => {
  // The clear colour is a stylesheet token, so the canvas matches its panels.
  renderer.setViewportTheme?.(
    theme,
    cssToken(theme === "light" ? "--canvas-light" : "--canvas-dark"),
    cssToken(theme === "light" ? "--section-cap-light" : "--section-cap-dark"),
  );
  scheduleRender();
});

// -------------------------------------------------------------- settings

function saveSettings() {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(state.settings));
  } catch {
    // A blocked storage backend must not break the interface.
  }
}

$("set-theme").addEventListener("change", (event) => shell.applyTheme(event.target.value));
$("set-canvas").addEventListener("change", (event) => shell.applyCanvasTheme(event.target.value));
$("set-scale").addEventListener("change", (event) => {
  state.settings.scale = Number(event.target.value) || 1;
  renderer.setRenderScale?.(state.settings.scale);
  renderer.resize();
  saveSettings();
  scheduleRender(true);
});
$("set-adaptive").addEventListener("change", (event) => {
  state.settings.adaptive = event.target.checked;
  renderer.setAdaptiveResolution?.(event.target.checked);
  saveSettings();
  scheduleRender(true);
});
$("set-lod").addEventListener("change", (event) => {
  state.settings.lod = event.target.checked;
  renderer.setLodPixels?.(event.target.checked ? LOD_PIXELS : 0);
  saveSettings();
  scheduleRender(true);
});
$("set-coincident").addEventListener("change", (event) => {
  state.settings.coincident = event.target.checked;
  renderer.setDepthTieBreak?.(event.target.checked);
  saveSettings();
  scheduleRender(true);
});
// The assistant fields write straight through; the panel re-reads them on its next refresh.
function syncAssistantFields() {
  const settings = assistant.settings();
  const provider = PROVIDERS[settings.provider] ?? PROVIDERS.off;
  $("set-assistant-provider").value = settings.provider;
  $("set-assistant-model").value = settings.model;
  $("set-assistant-model").placeholder = provider.model ?? "";
  $("set-assistant-url").value = settings.baseUrl;
  $("set-assistant-key").value = settings.key;
  $("set-assistant-model-row").classList.toggle("hidden", settings.provider === "off");
  $("set-assistant-url-row").classList.toggle("hidden", settings.provider !== "compatible");
  $("set-assistant-key-row").classList.toggle("hidden", settings.provider === "off");
}
$("set-assistant-provider").addEventListener("change", (event) => {
  const provider = PROVIDERS[event.target.value] ?? PROVIDERS.off;
  assistant.update({ provider: event.target.value, model: provider.model ?? "", baseUrl: provider.baseUrl ?? "" });
  syncAssistantFields();
  sessionPanel.refresh();
});
for (const [id, key] of [["set-assistant-model", "model"], ["set-assistant-url", "baseUrl"], ["set-assistant-key", "key"]]) {
  $(id).addEventListener("input", (event) => {
    assistant.update({ [key]: event.target.value.trim() });
    sessionPanel.refresh();
  });
}
$("assistant-settings").addEventListener("click", () => shell.openSettings());

$("set-hidden").addEventListener("change", (event) => {
  state.settings.hideSemantic = event.target.checked;
  state.hiddenInstanceFlags = event.target.checked ? DEFAULT_HIDDEN_INSTANCE_FLAGS : 0;
  if (!state.model) return;
  if (event.target.checked) {
    for (let record = 0; record < state.model.pack.instances.count; record += 1) {
      if (state.model.pack.instances.flags[record] & DEFAULT_HIDDEN_INSTANCE_FLAGS) {
        state.shownRecords.delete(record);
      }
    }
  }
  saveSettings();
  refreshVisibility();
});

// ------------------------------------------------------------ file input

$("file-input").addEventListener("change", (event) => {
  const [file] = event.target.files;
  if (file) openFile(file, { detachSession: true });
  event.target.value = "";
});

$("revision-input").addEventListener("change", (event) => {
  const [file] = event.target.files;
  event.target.value = "";
  if (file) updateFromFile(file).catch((error) => shell.toast(errorText(error), "error"));
});

window.addEventListener("dragenter", (event) => {
  if (!hasFiles(event)) return;
  event.preventDefault();
  state.dragDepth += 1;
  $("dropzone").classList.remove("hidden");
  $("dropzone").classList.add("dragging");
});
window.addEventListener("dragover", (event) => {
  if (!hasFiles(event)) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = "copy";
});
window.addEventListener("dragleave", (event) => {
  if (!hasFiles(event)) return;
  state.dragDepth = Math.max(0, state.dragDepth - 1);
  if (!state.dragDepth) endDrag();
});
window.addEventListener("drop", (event) => {
  if (!hasFiles(event)) return;
  event.preventDefault();
  state.dragDepth = 0;
  endDrag();
  const [file] = event.dataTransfer.files;
  if (file) openFile(file, { detachSession: true });
});
window.addEventListener("beforeunload", (event) => {
  if (!state.dirty) return;
  event.preventDefault();
  event.returnValue = "";
});

function endDrag() {
  $("dropzone").classList.remove("dragging");
  if (state.model) $("dropzone").classList.add("hidden");
}

function hasFiles(event) {
  return Array.from(event.dataTransfer?.types ?? []).includes("Files");
}

// ------------------------------------------------------------- selection

$("viewport").addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || renderer.pointers.size > 1) {
    pointerDown = null;
    return;
  }
  pointerDown = { x: event.clientX, y: event.clientY, pointerId: event.pointerId };
});
$("viewport").addEventListener("pointerup", pickAtRelease);
$("viewport").addEventListener("pointercancel", () => {
  pointerDown = null;
});
$("viewport").addEventListener("dblclick", (event) => {
  if (!state.model || tools.isMeasuring() || event.button !== 0) return;
  const hit = renderer.pick(event.clientX, event.clientY, false);
  if (hit) {
    renderer.focus([hit.record]);
    scheduleRender();
  }
});
$("viewport").addEventListener("pointermove", (event) => {
  if (pointerDown?.pointerId === event.pointerId &&
      Math.hypot(event.clientX - pointerDown.x, event.clientY - pointerDown.y) > 4) {
    pointerDown = null;
  }
  if (tools.isMeasuring() && !renderer.interacting) tools.hoverMeasure(event.clientX, event.clientY);
});
window.addEventListener("keydown", (event) => {
  if (!tools.isMeasuring() || event.key !== "Backspace" && event.key !== "Delete") return;
  const target = event.target;
  if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target?.isContentEditable) return;
  event.preventDefault();
  tools.removeLastMeasurement();
});

/** A release that did not drag selects what is under it, in the same task as the release frame so one frame shows both. */
function pickAtRelease(event) {
  if (!pointerDown || pointerDown.pointerId !== event.pointerId) return;
  const travelled = Math.hypot(event.clientX - pointerDown.x, event.clientY - pointerDown.y);
  pointerDown = null;
  if (travelled > 4 || !state.model) return;
  const at = { x: event.clientX, y: event.clientY };
  if (tools.isMeasuring()) {
    const surface = renderer.pickSurface(at.x, at.y);
    if (surface) tools.addMeasurePoint(surface, at);
    return;
  }
  const hit = renderer.pick(at.x, at.y, true);
  if (!hit) clearSelection();
  else {
    // The clicked surface becomes the orbit and zoom centre.
    if (hit.point) renderer.setPivot(hit.point);
    selectRecord(hit.record);
  }
}

/** Bring the inspector forward for a selection, unless an edit or session panel is in its place. */
function revealSelectionPanel(panel) {
  if (shell.panelVisible("editor") || shell.panelVisible("session")) return;
  if (panel === "element") shell.setPanel("inspector", true);
  else shell.setInspectorPanel("properties");
}

function selectExpressId(expressId, refresh = false) {
  const records = state.model?.index.recordsByExpressId.get(expressId);
  if (records?.length) selectRecord(records[0], refresh);
}

function selectRecord(record, refresh = false) {
  const selected = state.selection;
  if (!refresh && selected?.record === record && selected.modelId === state.model.modelId &&
      (selected.infoState === "pending" || selected.infoState === "ready")) {
    const panel = shell.inspectorPanel();
    if (!shell.panelVisible("inspector") || panel !== "element" && panel !== "properties") {
      revealSelectionPanel(panel === "element" ? "element" : "properties");
    }
    return;
  }
  const { pack, index } = state.model;
  const className = String(pack.index.classes[pack.instances.classIds[record]] ?? "IfcUnknown");
  const expressId = pack.instances.expressIds[record];
  const geometryId = pack.instances.geometryIds[record];

  // One record per material, so a window's glass comes along with its frame.
  const records = index.recordsByExpressId.get(expressId) ?? [record];
  let triangles = 0;
  for (const item of records) triangles += index.triangles[item];

  // A refresh of the element already shown keeps its attributes on screen until the new ones arrive.
  const quiet = refresh && selected?.expressId === expressId && selected.modelId === state.model.modelId;
  state.selection = { record, records, expressId, className, modelId: state.model.modelId,
    requestId: ++state.requestId, editRequestId: 0, infoState: state.worker ? "pending" : "idle",
    info: quiet ? selected.info : undefined };
  renderer.select(records);
  scheduleRender();

  // Back to IFC coordinates: the pack removed the model offset.
  const bounds = typeof renderer.recordBounds === "function" ? renderer.recordBounds(records) : null;
  const offset = pack.index.model_offset ?? [0, 0, 0];
  const toIfc = (point) => point.map((value, axis) => value + offset[axis]);
  // A user reading the element tab keeps it; anyone else lands on the properties.
  revealSelectionPanel(shell.inspectorPanel() === "element" ? "element" : "properties");
  inspector.showSelection({
    className,
    expressId,
    geometryId,
    triangles,
    materials: records.length,
    shared: records.some((item) => !isIdentity(pack.instances.transforms, item * 16)),
    position: bounds
      ? toIfc(bounds.min.map((value, axis) => (value + bounds.max[axis]) / 2))
      : renderer.recordPosition(record),
    size: bounds ? bounds.max.map((value, axis) => value - bounds.min[axis]) : null,
    extent: bounds ? { min: toIfc(bounds.min), max: toIfc(bounds.max) } : null,
    path: tree.pathOf(expressId),
    state: describeVisibility(records),
    quiet,
  });
  syncIsolation(isIsolated(records));
  sessionPanel.setSelection(true);
  panelWork.selection = true;
  schedulePanelWork();
  for (const id of ["isolate", "hide", "clear-selection", "focus", "edit"]) shell.setEnabled(id, true);
  $("dock-selection").classList.remove("hidden");

  state.worker?.postMessage({
    type: "entity-info",
    requestId: state.selection.requestId,
    modelId: state.model.modelId,
    expressId,
  });
}

/** The selection as a script target: express id, class, and the GlobalId once its attributes arrived. */
function selectionSummary() {
  const selection = state.selection;
  if (!selection || selection.modelId !== state.model?.modelId) return null;
  const fields = Array.isArray(selection.info?.fields) ? selection.info.fields : [];
  const field = (name) => fields.find((item) => item.name === name)?.value ?? null;
  const node = state.model.hierarchy?.nodes?.find((item) => item.expressId === selection.expressId);
  return {
    expressId: selection.expressId,
    className: selection.className,
    globalId: field("GlobalId") ?? node?.globalId ?? null,
    name: field("Name") ?? node?.name ?? null,
  };
}

function clearSelection() {
  renderer.select(null);
  scheduleRender();
  state.selection = null;
  sessionPanel.setSelection(false);
  inspector.clearSelection();
  panelWork.selection = true;
  schedulePanelWork();
  syncIsolation(false);
  for (const id of ["isolate", "hide", "clear-selection", "focus", "edit"]) shell.setEnabled(id, false);
  $("dock-selection").classList.add("hidden");
}

/** The isolate buttons read "Show all" while the selection is the only thing visible. */
function syncIsolation(isolated) {
  shell.setPressed("isolate", isolated);
  shell.setLabel("isolate", isolated ? "Show all" : "Isolate", isolated ? "Show everything again (I)" : "Isolate the selection (I)");
}

/** Open or close the edit panel. Opening on a selection puts the cursor in the first field. */
function setEditor(visible) {
  shell.setPanel("editor", visible);
  if (visible && state.selection) inspector.focusEditor();
}

function focusSelection() {
  if (state.selection) {
    renderer.focus(state.selection.records);
    scheduleRender();
  }
}

// ------------------------------------------------------------ visibility

function isRecordVisible(record) {
  return state.model ? isRecordVisibleInPack(state.model.pack, record) : false;
}

function isRecordVisibleInPack(pack, record) {
  if (pack.instances.active && !pack.instances.active[record]) return false;
  if (state.isolated && !state.isolated.has(record)) return false;
  if (state.hiddenRecords.has(record)) return false;
  if (state.shownRecords.has(record)) return true;
  if (pack.instances.flags[record] & state.hiddenInstanceFlags) return false;
  return !state.hiddenClasses.has(pack.instances.classIds[record]);
}

function setNodeVisible(node, visible) {
  if (node.classId !== null && node.classId !== undefined) setClassVisible(node.classId, visible);
  else setRecordsVisible(node.records, visible);
}

function setClassVisible(classId, visible) {
  if (visible) state.hiddenClasses.delete(classId);
  else state.hiddenClasses.add(classId);
  const item = state.model?.index.classes.get(classId);
  for (const record of item?.records ?? []) {
    state.hiddenRecords.delete(record);
    if (visible && (state.model.pack.instances.flags[record] & state.hiddenInstanceFlags)) {
      state.shownRecords.add(record);
    } else {
      state.shownRecords.delete(record);
    }
  }
  if (!visible && state.selection && state.model.pack.instances.classIds[state.selection.record] === classId) {
    clearSelection();
  }
  refreshVisibility();
}

function setRecordsVisible(records, visible) {
  for (const record of records) {
    if (visible) {
      state.hiddenRecords.delete(record);
      state.shownRecords.add(record);
    } else {
      state.shownRecords.delete(record);
      state.hiddenRecords.add(record);
    }
  }
  if (!visible && state.selection?.records.some((record) => records.includes(record))) clearSelection();
  refreshVisibility();
}

function hideSelection() {
  if (!state.selection) return;
  const records = state.selection.records;
  if (isIsolated(records)) state.isolated = null;
  for (const record of records) {
    state.shownRecords.delete(record);
    state.hiddenRecords.add(record);
  }
  clearSelection();
  refreshVisibility();
}

function toggleIsolation() {
  if (!state.selection) return;
  const records = state.selection.records;
  state.isolated = isIsolated(records) ? null : new Set(records);
  syncIsolation(Boolean(state.isolated));
  refreshVisibility();
}

function isIsolated(records) {
  return Boolean(state.isolated) && records.every((record) => state.isolated.has(record));
}

/** Undo every hide and isolation; the helper categories keep their own toggles. */
function restoreVisibility() {
  state.hiddenClasses.clear();
  state.hiddenRecords.clear();
  state.shownRecords.clear();
  state.isolated = null;
  tree.markAllVisible();
  syncIsolation(false);
  refreshVisibility();
}

function refreshVisibility() {
  if (!state.model) return;
  renderer.setVisibility(isRecordVisible);
  scheduleRender();
  panelWork.visibility = true;
  schedulePanelWork();
  scheduleVisibilityStats();
  if (state.selection) inspector.setElementState(describeVisibility(state.selection.records));
  syncHelperControls(state.model.pack);
}

function toggleHelperFlag(flag) {
  if (!state.model) return;
  if (state.hiddenInstanceFlags & flag) {
    state.hiddenInstanceFlags &= ~flag;
  } else {
    state.hiddenInstanceFlags |= flag;
    for (let record = 0; record < state.model.pack.instances.count; record += 1) {
      if (state.model.pack.instances.flags[record] & flag) state.shownRecords.delete(record);
    }
    if (state.selection?.records.some((record) => (state.model.pack.instances.flags[record] & flag))) {
      clearSelection();
    }
  }
  refreshVisibility();
}

function syncHelperControls(pack) {
  const availableFlags = pack && state.model?.pack === pack ? state.model.index.instanceFlags : 0;
  for (const item of HELPER_FILTERS) {
    const available = Boolean(availableFlags & item.flag);
    const visible = available && !(state.hiddenInstanceFlags & item.flag);
    shell.setEnabled(item.command, available);
    shell.setPressed(item.command, visible);
    shell.setLabel(
      item.command,
      `${visible ? "Hide" : "Show"} ${item.label}`,
      `${visible ? "Hide" : "Show"} ${item.title}`,
    );
  }
}

/** How the selection is shown right now, for the element panel. */
function describeVisibility(records) {
  if (isIsolated(records)) return "Isolated";
  const visible = records.filter(isRecordVisible).length;
  if (visible === records.length) return "Visible";
  return visible ? "Partly hidden" : "Hidden";
}

function scheduleVisibilityStats() {
  if (statsFrame) return;
  statsFrame = requestAnimationFrame(() => {
    statsFrame = 0;
    const instances = state.model?.pack.instances;
    const total = instances?.activeCount ?? instances?.count ?? 0;
    let visible = 0;
    for (let record = 0; record < (instances?.count ?? 0); record += 1) if (isRecordVisible(record)) visible += 1;
    $("stat-visible").textContent = count(visible);
    $("stat-hidden").textContent = count(total - visible);
    // The dock's show-all button lights up while a hide or an isolation is in force.
    const restorable = state.hiddenRecords.size > 0 || state.hiddenClasses.size > 0 || Boolean(state.isolated);
    $("dock-show-all").classList.toggle("attention", restorable);
  });
}

// ---------------------------------------------------------------- worker

function startWorker() {
  if (state.worker) state.worker.terminate();
  abandonStream();
  state.workerReady = false;
  state.converting = false;

  const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  state.worker = worker;

  worker.addEventListener("message", ({ data }) => {
    if (worker !== state.worker) return;
    switch (data.type) {
      case "ready":
        return workerReady(data);
      case "boot-error":
        return workerFailed(data);
      case "entity-info":
        return receiveEntityInfo(data);
      case "entity-error":
        return receiveEntityError(data);
      case "edit-result":
        return receiveEditResult(data);
      case "edit-error":
        return receiveEditError(data);
      case "export-result":
        return receiveExport(data);
      case "export-error":
        return shell.toast(data.message, "error");
      case "product-geometry":
        return receiveProductGeometry(data);
      case "script-result":
        receiveScriptResult(data);
        break;
      case "script-error":
        receiveScriptError(data);
        break;
      case "revision-result":
        return receiveRevision(data);
      case "revision-error":
        return receiveRevisionError(data);
      case "contested-triangles":
        return receiveContestedTriangles(data);
      default:
        break;
    }
    if (data.jobId !== state.jobId) return;
    if (data.type === "phase") {
      $("load-name").textContent = data.phase;
      $("load-detail").textContent = data.detail;
      setProgress(data.progress);
      setStatus(data.phase, "busy");
    } else if (data.type === "chunk") {
      receiveChunk(data);
    } else if (data.type === "conversion-error") {
      state.converting = false;
      abandonStream();
      finishLoading();
      restoreModelLabel();
      state.dismissLoadError = shell.toast(data.message, "error", 0);
      state.loadOutcome = "failed";
      setStatus(state.loadOutcome === "empty" ? "No drawable geometry" : "Conversion stopped", "err");
    } else if (data.type === "result") {
      showResult(data);
    }
  });

  worker.addEventListener("error", (event) => {
    if (worker !== state.worker) return;
    // Let go of the dead worker, or the next open waits on a kernel that never reports ready.
    worker.terminate();
    state.worker = null;
    state.workerReady = false;
    if (state.revisionPending) finishRevision(new Error("The geometry worker stopped."));
    state.converting = false;
    state.loadOutcome = "failed";
    abandonStream();
    // The model's attributes and source lived in that worker.
    if (state.model) disposeModel();
    setStatus("The geometry worker stopped", "err");
    state.dismissLoadError = shell.toast(event.message || "The geometry worker stopped unexpectedly.", "error", 0);
    finishLoading();
  });
}

function workerReady(data) {
  state.workerReady = true;
  setStatus("Kernel ready", "on");
  $("status-engine").textContent = `TessIFC ${data.version}`;
  if (!state.model) $("model-name").textContent = "No model open";
  if (state.queuedFile) {
    const queued = state.queuedFile;
    state.queuedFile = null;
    openFile(queued);
  }
}

function workerFailed(data) {
  state.worker?.terminate();
  state.worker = null;
  state.workerReady = false;
  state.converting = false;
  state.loadOutcome = "failed";
  finishLoading();
  setStatus("Kernel failed to start", "err");
  $("model-name").textContent = "Kernel unavailable";
  shell.toast(
    `The geometry kernel failed to start: ${data.message} Reload the page. From a checkout, build the browser WASM package first.`,
    "error",
    0,
  );
}

// ------------------------------------------------------------ model load

async function openFile(file, { detachSession = false } = {}) {
  if (state.revisionPending) {
    shell.toast("Wait for the current IFC update to finish.", "info");
    return;
  }
  if (!file.name.toLowerCase().endsWith(".ifc")) {
    shell.toast("Choose an IFC-SPF file with the .ifc extension.", "error");
    return;
  }
  if (!file.size) {
    shell.toast("This IFC file is empty.", "error");
    return;
  }
  if (state.model && state.dirty && state.replaceApproved !== file) {
    if (!window.confirm("This model has unsaved IFC edits. Discard them and open another file?")) return;
    state.replaceApproved = file;
  }
  if (detachSession) detachFileSession();
  if (state.converting) {
    state.queuedFile = file;
    state.jobId += 1;
    // The worker holding the open model goes with the job it is busy on.
    if (state.model) disposeModel();
    startWorker();
    showLoading(file.name, "Restarting", "Releasing the previous job first.");
    return;
  }
  if (!state.workerReady) {
    state.queuedFile = file;
    if (!state.worker) startWorker();
    showLoading(file.name, "Preparing the kernel", "Loading the WebAssembly geometry engine once for this session.");
    return;
  }

  const jobId = ++state.jobId;
  abandonStream();
  state.replaceApproved = null;
  state.loadStarted = performance.now();
  state.loadOutcome = "loading";
  showLoading(file.name, "Reading IFC", "Moving the source bytes into the geometry worker.");
  setStatus("Reading IFC", "busy");
  try {
    const buffer = await file.arrayBuffer();
    if (jobId !== state.jobId) return;
    state.pendingFile = { name: file.name, size: file.size };
    state.converting = true;
    state.loadOutcome = "loading";
    state.worker.postMessage({ type: "convert", jobId, buffer }, [buffer]);
  } catch (error) {
    state.loadOutcome = "failed";
    finishLoading();
    shell.toast(errorText(error), "error");
  }
}

function showLoading(name, phase, detail) {
  state.dismissLoadError?.();
  state.dismissLoadError = null;
  $("dropzone").classList.add("hidden");
  $("loading").classList.remove("hidden");
  $("load-name").textContent = phase;
  $("load-detail").textContent = detail;
  $("model-name").textContent = name;
  $("model-chip").classList.remove("blank");
  setProgress(null);
}

function finishLoading() {
  $("loading").classList.add("hidden");
  document.body.classList.remove("streaming");
  setProgress(null);
  if (!state.model) $("dropzone").classList.remove("hidden");
}

/** A determinate bar while the kernel reports products done, a sweep otherwise. */
function setProgress(progress) {
  const track = $("load-track");
  const bar = $("load-bar");
  if (progress && progress.total > 0) {
    track.classList.add("determinate");
    bar.style.width = `${Math.max(1, Math.min(100, (100 * progress.done) / progress.total)).toFixed(1)}%`;
  } else {
    track.classList.remove("determinate");
    bar.style.width = "";
  }
}

/**
 * Ask the worker which triangles share a plane with another product. The renderer keeps
 * the bounds-based overlay until the answer arrives, then redraws only those triangles.
 */
function requestContestedTriangles() {
  const model = state.model;
  if (!model || !state.worker || typeof renderer.applyContestedTriangles !== "function") return;
  const { pack } = model;
  const requestId = ++state.requestId;
  model.overlayAnalysis = { requestId, state: "pending" };
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
  state.worker.postMessage({ type: "contested-triangles", requestId, modelId: model.modelId, instances, geometries }, transfer);
}

function receiveContestedTriangles(data) {
  const model = state.model;
  if (!model || model.overlayAnalysis?.requestId !== data.requestId) return;
  if (data.error) {
    model.overlayAnalysis = { requestId: data.requestId, state: "failed", error: data.error };
    return;
  }
  // Past its budget the analysis has no answer; the bounds overlay stays.
  if (data.exhausted) {
    model.overlayAnalysis = { requestId: data.requestId, state: "exhausted", elapsedMs: data.elapsedMs };
    return;
  }
  renderer.applyContestedTriangles(data);
  model.overlayAnalysis = { requestId: data.requestId, state: "ready", triangles: data.triangles.length, pairs: data.pairs, elapsedMs: data.elapsedMs };
  inspector.setDisplayFacts(renderer);
  scheduleRender(true);
}

/** Merge one streamed IGP chunk and draw it; the first chunk clears the previous model. */
function receiveChunk(data) {
  // A damaged chunk ends this job, so a later one must never start a fresh assembler.
  if (state.stream?.failed) return;
  if (!state.stream) {
    disposeModel();
    state.stream = { assembler: createPackAssembler(), firstPaintMs: null };
    renderer.beginStream();
    $("dropzone").classList.add("hidden");
    document.body.classList.add("streaming");
  }
  try {
    const chunk = readIgp(data.buffer);
    const { from, to } = state.stream.assembler.append(chunk);
    const pack = state.stream.assembler.pack();
    renderer.appendStream(pack, from, to, (record) => isRecordVisibleInPack(pack, record));
    if (state.stream.firstPaintMs === null) state.stream.firstPaintMs = performance.now() - state.loadStarted;
    const progress = data.progress ?? {};
    $("load-name").textContent = "Tessellating geometry";
    $("load-detail").textContent =
      `${count(progress.done ?? to)} of ${count(progress.total ?? to)} products, ${count(progress.triangles ?? 0)} triangles`;
    setProgress(progress);
    setStatus(`Streaming ${count(progress.done ?? 0)} of ${count(progress.total ?? 0)} products`, "busy");
    scheduleRender();
  } catch (error) {
    state.stream = { failed: true, firstPaintMs: state.stream?.firstPaintMs ?? null };
    renderer.clear();
    shell.toast(errorText(error), "error", 0);
  }
}

function showResult(data) {
  const buildStarted = performance.now();
  state.converting = false;
  try {
    let pack;
    let model;
    let assembler = null;
    const firstPaintMs = state.stream?.firstPaintMs ?? null;
    if (data.streamed) {
      if (state.stream?.failed) {
        disposeModel();
        throw new Error("Geometry streaming stopped after a damaged chunk; the model is incomplete.");
      }
      // A stream that produced no chunk is a model with nothing to draw.
      if (!state.stream) {
        state.loadOutcome = "empty";
        throw new Error("The IFC parsed correctly, but no supported product geometry was produced.");
      }
      assembler = state.stream.assembler;
      state.stream = null;
      pack = assembler.pack();
      if (!pack.instances.count || !pack.geometry.length) {
        renderer.clear();
        state.loadOutcome = "empty";
        throw new Error("The IFC parsed correctly, but no supported product geometry was produced.");
      }
      model = renderer.finishStream(pack, (record) => isRecordVisibleInPack(pack, record));
    } else {
      state.stream = null;
      pack = readIgp(data.pack);
      if (!pack.instances.count || !pack.geometry.length) {
        state.loadOutcome = "empty";
        throw new Error("The IFC parsed correctly, but no supported product geometry was produced.");
      }
      disposeModel();
      // Through the assembler too, so an edit can patch it the same way.
      assembler = createPackAssembler();
      assembler.append(pack);
      pack = assembler.pack();
      model = renderer.load(pack, (record) => isRecordVisibleInPack(pack, record));
    }
    state.model = {
      ...model,
      pack,
      assembler,
      info: data.info,
      summary: data.summary,
      hierarchy: data.hierarchy,
      modelId: data.modelId,
      revision: data.revision ?? "0",
      index: buildIndex(pack),
      file: state.pendingFile,
    };
    state.loadOutcome = "ready";
    state.dirty = false;
    setDirty(false);

    tools.reset(true);
    // A camera the user moved while the model was streaming in is theirs.
    tools.setView("perspective", !model.cameraKept);

    tree.build(pack, data.hierarchy, state.model.index);
    tree.syncVisibility(isRecordVisible);
    scheduleVisibilityStats();

    const buildMs = performance.now() - buildStarted;
    const totalMs = performance.now() - state.loadStarted;
    inspector.setStatistics({
      info: data.info,
      summary: data.summary,
      timings: { ...data.timings, firstPaintMs },
      pack,
      model,
      buildMs,
      totalMs,
      file: state.pendingFile,
    });
    inspector.setDisplayFacts(renderer);
    inspector.setQuality(pack.index.diagnostics ?? [], renderer);
    enableModelCommands(true);
    syncHelperControls(pack);
    describeModel(data, pack, totalMs);
    scheduleRender(true);
    finishLoading();
    requestContestedTriangles();
  } catch (error) {
    finishLoading();
    restoreModelLabel();
    state.dismissLoadError = shell.toast(errorText(error), "error", 0);
    if (state.loadOutcome !== "empty") state.loadOutcome = "failed";
    setStatus(state.loadOutcome === "empty" ? "No drawable geometry" : "Conversion stopped", "err");
  }
}

/** Index the pack once so selection, tree building and statistics never rescan it. */
function buildIndex(pack) {
  const recordsByExpressId = new Map();
  const geometryById = new Map();
  const classes = new Map();
  for (const mesh of pack.geometry) geometryById.set(mesh.id, mesh);

  const total = pack.instances.count;
  const triangles = new Uint32Array(total);
  let instanceFlags = 0;
  for (let record = 0; record < total; record += 1) {
    if (pack.instances.active && !pack.instances.active[record]) continue;
    instanceFlags |= pack.instances.flags[record];
    const expressId = pack.instances.expressIds[record];
    let records = recordsByExpressId.get(expressId);
    if (!records) recordsByExpressId.set(expressId, (records = []));
    records.push(record);

    const classId = pack.instances.classIds[record];
    let item = classes.get(classId);
    if (!item) {
      item = { id: classId, exact: String(pack.index.classes[classId] ?? "IfcUnknown"), records: [] };
      classes.set(classId, item);
    }
    item.records.push(record);

    const mesh = geometryById.get(pack.instances.geometryIds[record]);
    triangles[record] = mesh ? mesh.indices.length / 3 : 0;
  }
  return { recordsByExpressId, geometryById, classes, triangles, instanceFlags };
}

function describeModel(data, pack, totalMs) {
  const file = state.pendingFile;
  if (file) {
    $("model-name").textContent = file.name;
    $("model-name").title = `${file.name}, ${bytes(file.size)}`;
    $("model-chip").classList.remove("blank");
  }
  $("model-schema").textContent = data.info.schema;
  $("model-schema").classList.remove("hidden");
  $("stat-classes").textContent = count(state.model.index.classes.size);
  $("status-counts").textContent =
    `${plural(data.summary.products, "product")}, ${count(data.summary.triangles)} tris, ` +
    `${count(pack.geometry.length)} meshes, ${count(state.model.drawCalls)} draws`;
  $("status-offset").textContent = `offset ${pack.index.model_offset.map(coordinate).join(" / ")}`;
  if (totalMs !== null) setStatus(`Ready in ${duration(totalMs)}`, "on");
  $("gizmo-host").classList.remove("hidden");
  $("hint-bar").classList.remove("hidden");
  setTimeout(() => $("hint-bar").classList.add("hidden"), 6000);
}

function enableModelCommands(enabled) {
  for (const id of [
    "fit", "zoom-in", "zoom-out", "plan", "style", "measure", "section", "show-all", "export", "close",
    "quality", "model-stats", "properties", "element", "tree-expand", "tree-collapse", "update-ifc",
    "spaces", "openings", "references",
    "view:perspective", "view:top", "view:front", "view:right",
  ]) shell.setEnabled(id, enabled);
  $("tree-search").disabled = !enabled;
  sessionPanel.refresh();
}

/** After a failed load, the chip names the model still open, or nothing. */
function restoreModelLabel() {
  const file = state.model?.file;
  state.pendingFile = file ?? null;
  if (file) {
    $("model-name").textContent = file.name;
    $("model-name").title = `${file.name}, ${bytes(file.size)}`;
    $("model-chip").classList.remove("blank");
    return;
  }
  $("model-name").textContent = "No model open";
  $("model-name").title = "";
  $("model-chip").classList.add("blank");
}

/** Drop a stream that will never finish, without touching a loaded model. */
function abandonStream() {
  if (!state.stream) return;
  state.stream = null;
  renderer.clear();
  document.body.classList.remove("streaming");
}

/** Put the chrome back to its no-model state. The model chip and the dropzone belong to the caller. */
function clearModelUi() {
  state.scriptHistory = { undo: 0, redo: 0 };
  sessionPanel.refresh();
  tools.reset(false);
  tree.clear();
  inspector.clear();
  shell.setPanel("editor", false);
  enableModelCommands(false);
  state.dirty = false;
  setDirty(false);
  $("model-schema").classList.add("hidden");
  $("status-counts").textContent = "";
  $("status-offset").textContent = "";
  $("stat-visible").textContent = "0";
  $("stat-hidden").textContent = "0";
  $("dock-show-all").classList.remove("attention");
  $("stat-classes").textContent = "0";
  $("gizmo-host").classList.add("hidden");
  $("hint-bar").classList.add("hidden");
}

function disposeModel() {
  pointerDown = null;
  clearSelection();
  renderer.clear();
  state.model = null;
  state.stream = null;
  state.hiddenClasses.clear();
  state.hiddenRecords.clear();
  state.shownRecords.clear();
  state.hiddenInstanceFlags = state.settings.hideSemantic ? DEFAULT_HIDDEN_INSTANCE_FLAGS : 0;
  state.isolated = null;
  syncHelperControls(null);
  clearModelUi();
}

function closeModel() {
  if (state.revisionPending) {
    shell.toast("Wait for the current IFC update to finish.", "info");
    return;
  }
  if (state.dirty && !window.confirm("This model has unsaved IFC edits. Discard them and close it?")) return;
  const activeId = state.model?.modelId;
  detachFileSession();
  if (state.converting) {
    // Cancel the job the way a replacement open does: a fresh worker holds no model.
    state.jobId += 1;
    state.converting = false;
    startWorker();
    finishLoading();
  } else if (activeId !== undefined) {
    state.worker?.postMessage({ type: "close", modelId: activeId });
  }
  disposeModel();
  state.loadOutcome = "idle";
  state.queuedFile = null;
  state.replaceApproved = null;
  state.pendingFile = null;
  $("dropzone").classList.remove("hidden");
  $("model-chip").classList.add("blank");
  $("model-name").textContent = "No model open";
  if (state.workerReady) setStatus("Kernel ready", "on");
  else setStatus("Restarting the kernel", "busy");
  scheduleRender(true);
}

// ------------------------------------------------------- entity messages

function receiveEntityInfo(data) {
  if (!state.selection || data.requestId !== state.selection.requestId) return;
  if (data.expressId !== state.selection.expressId) return;
  state.selection.info = data.info;
  state.selection.infoState = "ready";
  panelWork.properties = { selection: state.selection, info: data.info };
  schedulePanelWork();
}

function receiveEntityError(data) {
  if (!state.selection || data.requestId !== state.selection.requestId) return;
  state.selection.infoState = "failed";
  inspector.setPropertyError(data.message);
}

/** Run a browser script in the worker; resolves with the script report and, after a commit, the impact. */
function runBrowserScript(source, selection = null, { commit = true } = {}) {
  return new Promise((resolve, reject) => {
    if (!state.model || !state.workerReady) return reject(new Error("Open a model before running a script."));
    if (state.fileSession) return reject(new Error("This model follows a local session; its scripts run there."));
    if (state.revisionPending || state.converting) return reject(new Error("Wait for the current update to finish."));
    if (state.model.stale) return reject(new Error("Reopen the model to restore synchronization."));
    const requestId = ++state.requestId;
    state.revisionPending = { requestId, script: true, resolve, reject };
    state.worker.postMessage({
      type: "run-script", requestId, modelId: state.model.modelId, source: String(source ?? ""), selection, commit,
      patch: revisionOptions(),
    });
  });
}

function browserHistoryAction(action) {
  return new Promise((resolve, reject) => {
    if (!state.model || !state.workerReady) return reject(new Error("Open a model first."));
    if (state.revisionPending || state.converting) return reject(new Error("Wait for the current update to finish."));
    if (state.model.stale) return reject(new Error("Reopen the model to restore synchronization."));
    const requestId = ++state.requestId;
    state.revisionPending = { requestId, script: true, resolve, reject };
    state.worker.postMessage({ type: "script-history", requestId, modelId: state.model.modelId, action, patch: revisionOptions() });
  });
}

function receiveScriptResult(data) {
  if (data.modelId !== state.model?.modelId || data.requestId !== state.revisionPending?.requestId) return;
  if (data.history) state.scriptHistory = data.history;
  finishRevision(null, { ...data.result, impact: null, history: data.history ?? null });
  sessionPanel.refresh();
}

function receiveScriptError(data) {
  if (data.modelId !== state.model?.modelId || data.requestId !== state.revisionPending?.requestId) return;
  if (data.history) state.scriptHistory = data.history;
  finishRevision(new Error(data.message));
  sessionPanel.refresh();
}

/** What the browser assistant learns about the open model and the selection. */
async function assistantContext() {
  const model = state.model;
  if (!model) return "No model is open.";
  const info = model.info ?? {};
  const products = Object.entries(info.products ?? {}).sort((left, right) => right[1] - left[1]).slice(0, 40)
    .map(([name, total]) => `${name} ${total}`).join(", ");
  const storeys = (model.hierarchy?.nodes ?? []).filter((node) => node.class === "IfcBuildingStorey").slice(0, 20)
    .map((node) => `#${node.expressId} ${node.name ?? "unnamed"}`).join("; ");
  const lines = [
    `File: ${model.file?.name ?? "model.ifc"} (${info.schema ?? "unknown schema"}), revision ${model.revision}, lengths in the model's length unit.`,
    `Products by class: ${products || "none"}.`,
  ];
  if (storeys) lines.push(`Storeys: ${storeys}.`);
  const selection = selectionSummary();
  if (selection) {
    const fields = (state.selection?.info?.fields ?? []).filter((field) => field.raw && field.raw !== "$").slice(0, 40)
      .map((field) => `${field.name}=${field.raw}`).join(", ");
    lines.push(`Selected in the viewer (the \`selected\` entity): #${selection.expressId} ${selection.className}${fields ? ` with ${fields}` : ""}.`);
  } else {
    lines.push("Nothing is selected in the viewer.");
  }
  return lines.join("\n");
}

function applyEdits(changes) {
  if (!state.selection || !state.model) return;
  if (state.fileSession) {
    inspector.setEditError("This model follows an external IFC file. Edit it in the connected authoring process.");
    return;
  }
  if (state.revisionPending || state.model.stale) {
    inspector.setEditError("Wait for the current update, or reopen the model if synchronization failed.");
    return;
  }
  state.selection.editRequestId = ++state.requestId;
  state.revisionPending = { requestId: state.selection.editRequestId, attributeEdit: true };
  inspector.setEditPending();
  try {
    const { assembler, pack } = state.model;
    state.worker.postMessage({
      type: "edit",
      requestId: state.selection.editRequestId,
      modelId: state.model.modelId,
      expressId: state.selection.expressId,
      changes,
      // The patch must land in the same pack space, above every geometry id in use.
      patch: assembler
        ? { baseRevision: state.model.revision, modelOffset: pack.index.model_offset ?? [0, 0, 0], firstGeometryId: assembler.nextGeometryId() }
        : null,
    });
  } catch (error) {
    // Never leave the editor stuck on its pending state.
    inspector.setEditError(errorText(error));
    finishRevision(error);
  }
}

/** Swap in the edited product's fresh geometry; the scene is rebuilt only if the triangles changed. */
function receiveProductGeometry(data) {
  const model = state.model;
  if (!model?.assembler || data.modelId !== model.modelId) return;
  try {
    const chunk = readIgp(data.buffer);
    // Read before the columns are compacted, or every hidden record above the edit would shift.
    const hiddenIds = expressIdsOf(state.hiddenRecords);
    const shownIds = expressIdsOf(state.shownRecords);
    const isolatedIds = state.isolated ? expressIdsOf(state.isolated) : null;
    const selected = state.selection?.expressId ?? null;
    const result = model.assembler.replaceProducts([data.expressId], chunk);
    if (!result.changed) {
      setStatus("Edited, geometry unchanged", "busy");
      return;
    }

    const pack = model.assembler.pack();
    const index = buildIndex(pack);
    state.hiddenRecords = recordsOf(hiddenIds, index);
    state.shownRecords = recordsOf(shownIds, index);
    state.isolated = isolatedIds ? recordsOf(isolatedIds, index) : null;
    state.model = { ...model, pack, index };
    const rebuilt = renderer.applyDelta(pack, result, isRecordVisible);
    state.model = { ...state.model, ...rebuilt };
    requestContestedTriangles();
    tree.build(pack, model.hierarchy, index);
    tree.syncVisibility(isRecordVisible);
    syncHelperControls(pack);
    scheduleVisibilityStats();
    if (selected !== null) selectExpressId(selected, true);
    inspector.setGeometryFacts({ pack, model: state.model });
    describeModel({ info: model.info, summary: model.summary }, pack, null);
    setStatus(`Edited, #${data.expressId} redrawn in ${duration(data.elapsedMs ?? 0)}`, "busy");
    scheduleRender(true);
  } catch (error) {
    shell.toast(`The edited element could not be redrawn: ${errorText(error)}`, "error");
  }
}

function expressIdsOf(records) {
  const ids = new Set();
  const expressIds = state.model?.pack.instances.expressIds;
  if (!expressIds) return ids;
  for (const record of records) ids.add(expressIds[record]);
  return ids;
}

function recordsOf(expressIds, index) {
  const records = new Set();
  for (const id of expressIds) for (const record of index.recordsByExpressId.get(id) ?? []) records.add(record);
  return records;
}

function receiveEditResult(data) {
  if (data.modelId === state.model?.modelId) {
    state.dirty = true;
    setDirty(true);
    finishRevision();
  }
  if (!state.selection || data.requestId !== state.selection.editRequestId) return;
  if (data.expressId !== state.selection.expressId) return;
  state.selection.info = data.info;
  state.selection.infoState = "ready";
  state.dirty = true;
  setDirty(true);
  inspector.setEditResult(data.info, data.changed);
  setStatus("Edited, export pending", "busy");
}

function receiveEditError(data) {
  if (data.requestId === state.revisionPending?.requestId) finishRevision(new Error(data.message));
  if (!state.selection || data.requestId !== state.selection.editRequestId) return;
  inspector.setEditError(data.message);
}

function revisionOptions() {
  const { pack, assembler, revision } = state.model;
  return { baseRevision: revision, modelOffset: pack.index.model_offset ?? [0, 0, 0],
    firstGeometryId: assembler.nextGeometryId(), selectedExpressId: state.selection?.expressId ?? null };
}

/** Ingest a saved version while retaining the open scene and its user state. */
async function updateFromFile(file) {
  if (!state.model || !state.workerReady) throw new Error("Open a model before updating it.");
  if (state.revisionPending || state.converting) throw new Error("An IFC update is already running.");
  if (state.model.stale) throw new Error("Reopen the model to restore synchronization.");
  if (!file.size || !/\.ifc$/i.test(file.name)) throw new Error("Choose a nonempty IFC file.");
  if (state.dirty && !window.confirm("Replace the current unsaved edits with this IFC revision?")) {
    throw new Error("Update cancelled.");
  }
  const requestId = ++state.requestId;
  const modelId = state.model.modelId;
  const promise = new Promise((resolve, reject) => {
    state.revisionPending = { requestId, file: { name: file.name, size: file.size }, resolve, reject };
  });
  setStatus("Reading the edited IFC", "busy");
  try {
    const buffer = await file.arrayBuffer();
    if (state.model?.modelId !== modelId) throw new Error("The model changed during the update.");
    state.worker.postMessage({ type: "update-revision", requestId, modelId, buffer, patch: revisionOptions() }, [buffer]);
    setStatus("Evaluating affected objects", "busy");
  } catch (error) {
    finishRevision(error);
    shell.toast(errorText(error), "error");
  }
  return promise;
}

function detachFileSession() {
  if (!state.fileSession) return;
  state.fileSession.stop();
  state.fileSession = null;
  sessionPanel.setStatus(null);
}

function finishRevision(error = null, result = null) {
  const pending = state.revisionPending;
  state.revisionPending = null;
  if (error) pending?.reject?.(error);
  else pending?.resolve?.(result);
}

function productIdentities(model, ids) {
  const nodes = new Map((model.hierarchy?.nodes ?? []).map((node) => [node.expressId, node]));
  return [...ids].map((id) => ({ id, guid: nodes.get(id)?.globalId ?? null }));
}

function restoreIdentities(identities, hierarchy, fullRebuild) {
  const guidIds = new Map();
  for (const node of hierarchy?.nodes ?? []) {
    if (!node.globalId) continue;
    guidIds.set(node.globalId, guidIds.has(node.globalId) ? null : node.expressId);
  }
  return identities.map(({ id, guid }) => guid ? guidIds.get(guid) : fullRebuild ? null : id)
    .filter((id) => Number.isInteger(id));
}

function receiveRevision(data) {
  const model = state.model;
  if (!model || data.modelId !== model.modelId) return;
  if (data.revision === model.revision) return;
  if (data.requestId !== state.revisionPending?.requestId || data.baseRevision !== model.revision) {
    model.stale = true;
    setStatus("IFC revision mismatch; reopen the model", "err");
    finishRevision(new Error("The received IFC revision has an unexpected base."));
    return;
  }
  const pending = state.revisionPending;
  try {
    const impact = data.impact;
    const chunk = data.buffer ? readIgp(data.buffer) : null;
    if (!chunk) throw new Error("The IFC update is missing its geometry payload.");
    const hidden = productIdentities(model, expressIdsOf(state.hiddenRecords));
    const shown = productIdentities(model, expressIdsOf(state.shownRecords));
    const isolated = state.isolated ? productIdentities(model, expressIdsOf(state.isolated)) : null;
    const selected = productIdentities(model, state.selection ? [state.selection.expressId] : []);
    let assembler = model.assembler;
    let result;
    if (impact.fullRebuild) {
      assembler = createPackAssembler();
      assembler.append(chunk);
      result = { changed: true };
    } else {
      result = assembler.replaceProducts([...new Set([...impact.affectedProducts, ...impact.removedProducts])], chunk);
    }
    const pack = assembler.pack();
    pack.index.georef = chunk.index.georef;
    const index = buildIndex(pack);
    const restore = (items) => recordsOf(restoreIdentities(items, data.hierarchy, impact.fullRebuild), index);
    state.hiddenRecords = restore(hidden);
    state.shownRecords = restore(shown);
    state.isolated = isolated ? restore(isolated) : null;
    const summary = { ...model.summary, products: index.recordsByExpressId.size,
      triangles: index.triangles.reduce((sum, value) => sum + value, 0) };
    state.model = { ...model, assembler, pack, index, revision: data.revision, info: data.info,
      hierarchy: data.hierarchy, summary, stale: false, file: pending.file ?? model.file };
    let rendered = {};
    if (result.changed) {
      rendered = impact.fullRebuild ? renderer.reload(pack, isRecordVisible) : renderer.applyDelta(pack, result, isRecordVisible);
      tools.clearMeasurement();
    }
    state.model = { ...state.model, ...rendered,
      lastUpdate: { ...impact, ...data.timings, revision: data.revision, renderer: rendered.patchStats ?? null } };
    if (result.changed) requestContestedTriangles();
    tree.build(pack, data.hierarchy, index);
    tree.syncVisibility(isRecordVisible);
    syncHelperControls(pack);
    scheduleVisibilityStats();
    const selectedId = restoreIdentities(selected, data.hierarchy, impact.fullRebuild)[0];
    if (selectedId != null && index.recordsByExpressId.has(selectedId)) selectExpressId(selectedId, true);
    else clearSelection();
    if (data.entityInfo && state.selection?.expressId === (data.infoId ?? data.expressId)) {
      state.selection.info = data.entityInfo;
      state.selection.infoState = "ready";
      if (data.attributeEdit) inspector.setEditResult(data.entityInfo, data.changed);
      else inspector.setProperties(data.entityInfo);
    }
    if (result.changed && !impact.fullRebuild) {
      const touched = new Set([...impact.affectedProducts, ...(impact.metadataProducts ?? [])]);
      renderer.flash?.([...recordsOf(touched, index)]);
    }
    state.dirty = data.attributeEdit || Boolean(data.script);
    setDirty(state.dirty);
    if (data.history) state.scriptHistory = data.history;
    state.pendingFile = state.model.file;
    inspector.setGeometryFacts({ pack, model: state.model });
    inspector.setQuality(pack.index.diagnostics ?? [], renderer);
    describeModel({ info: data.info, summary }, pack, null);
    const updated = impact.affectedProducts.length;
    const deleted = impact.removedProducts.length;
    const label = impact.fullRebuild ? "Full geometry refresh" : `${updated} geometry updates, ${deleted} removed`;
    setStatus(`Revision ${data.revision}: ${label}`, "on");
    $("status-text").title = (impact.reasons ?? []).slice(0, 30).map((item) => `#${item.expressId}: ${item.reason}`).join("\n");
    scheduleRender(true);
    sessionPanel.refresh();
    finishRevision(null, pending.script
      ? { ...data.script, impact: state.model.lastUpdate, history: data.history ?? null }
      : state.model.lastUpdate);
  } catch (error) {
    state.model.revision = data.revision;
    state.model.stale = true;
    state.dirty = data.attributeEdit || Boolean(data.script);
    setDirty(state.dirty);
    setStatus("IFC updated; the view needs reopening", "err");
    shell.toast(`The IFC revision was committed, but the scene could not be updated: ${errorText(error)}`, "error", 0);
    finishRevision(error);
  }
}

function receiveRevisionError(data) {
  if (data.modelId !== state.model?.modelId || data.requestId !== state.revisionPending?.requestId) return;
  if (data.committed) {
    state.model.revision = data.revision;
    state.model.stale = true;
  }
  if (data.history) state.scriptHistory = data.history;
  inspector.setEditError(data.message);
  setStatus(data.committed ? "IFC updated; reopen to synchronize" : "IFC update rejected; previous revision retained", "err");
  shell.toast(data.message, "error");
  const error = new Error(data.message);
  error.revisionRejected = !data.committed;
  error.script = data.script ?? null;
  finishRevision(error);
  sessionPanel.refresh();
}

function setDirty(dirty) {
  shell.setDirty("export", dirty);
  $("save-revision").classList.toggle("hidden", !dirty);
}

// ---------------------------------------------------------------- export

function requestExport() {
  if (!state.model || !state.workerReady) return;
  state.model.exportRequestId = ++state.requestId;
  setStatus("Preparing the IFC download", "busy");
  state.worker.postMessage({ type: "export", requestId: state.model.exportRequestId, modelId: state.model.modelId });
}

function receiveExport(data) {
  if (!state.model || data.requestId !== state.model.exportRequestId) return;
  const original = state.pendingFile?.name ?? "model.ifc";
  const stem = original.replace(/\.ifc$/i, "");
  const name = state.dirty ? `${stem}.edited.ifc` : `${stem}.copy.ifc`;
  const url = URL.createObjectURL(new Blob([data.buffer], { type: "application/octet-stream" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  state.dirty = false;
  setDirty(false);
  setStatus(`Downloaded ${name}`, "on");
  shell.toast(`Saved ${name}`, "success");
}

// ----------------------------------------------------------------- shell

function setStatus(text, dot) {
  $("status-text").textContent = text;
  $("status-dot").className = `dot ${dot ?? ""}`.trim();
  $("kernel-dot").className = `dot ${dot ?? ""}`.trim();
}

/** A colour token from the stylesheet, so the canvas and the chrome agree. */
const cssTokens = new Map();
function cssToken(name) {
  // The canvas tokens are constants on the root, so one read serves every theme switch
  // and the switch itself forces no style pass of its own.
  let value = cssTokens.get(name);
  if (value === undefined) {
    value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    cssTokens.set(name, value);
  }
  return value;
}

function fatal(message) {
  const zone = document.getElementById("dropzone");
  if (!zone) return;
  const note = document.createElement("p");
  note.className = "note error";
  note.style.marginTop = "16px";
  note.textContent = message;
  zone.querySelector(".dz-card")?.append(note);
  document.getElementById("splash")?.classList.add("done");
}

// ------------------------------------------------------------------ boot

shell.applyTheme(shell.themeMode());
// Both themes' canvas tokens are read while the page is still small, so a later switch reads nothing.
for (const name of ["--canvas-light", "--canvas-dark", "--section-cap-light", "--section-cap-dark"]) cssToken(name);
renderer.setRenderScale?.(state.settings.scale);
$("set-scale").value = String(state.settings.scale);
syncAssistantFields();
renderer.setAdaptiveResolution?.(state.settings.adaptive);
$("set-adaptive").checked = state.settings.adaptive;
renderer.setLodPixels?.(state.settings.lod ? LOD_PIXELS : 0);
$("set-lod").checked = state.settings.lod;
renderer.setDepthTieBreak?.(state.settings.coincident);
$("set-coincident").checked = state.settings.coincident;
tools.reset(false);
tree.clear();
enableModelCommands(false);
clearSelection();
startWorker();
scheduleRender();

if (new URLSearchParams(location.search).get("session") === "file") {
  state.fileSession = startFileSession({
    ready: () => state.workerReady && !state.converting && !state.revisionPending && !state.model?.stale,
    loaded: () => Boolean(state.model),
    open: openFile,
    update: updateFromFile,
    report: (message) => setStatus(message, "err"),
    status: (session) => {
      if (state.fileSession) sessionPanel.setStatus(session);
    },
  });
}

const splash = $("splash");
splash.classList.add("done");
splash.addEventListener("transitionend", () => splash.remove(), { once: true });
setTimeout(() => splash.remove(), 800);
