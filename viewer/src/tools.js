// SPDX-License-Identifier: Apache-2.0

//! Viewport tools: standard views, display style, the measure tape and the
//! section plane.

import { coordinate, point as formatPoint } from "./format.js";
import {
  measurementBetween,
  measurementDetail,
  measurementLabel,
  measurementText,
  snapToTriangle,
} from "./measure.js";

const $ = (id) => document.getElementById(id);

const STYLES = ["shaded", "xray", "wire"];
const STYLE_LABELS = { shaded: "Shaded", xray: "X-ray", wire: "Wireframe" };
// The view each orientation gizmo axis looks from.
const AXIS_VIEWS = {
  "0,0,1": "top",
  "0,0,-1": "bottom",
  "0,-1,0": "front",
  "0,1,0": "back",
  "1,0,0": "right",
  "-1,0,0": "left",
};

function readCss(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function scaleUi() {
  return Number(readCss("--ui")) || 1;
}

// Read once per overlay frame; every marker and label shares it.
let uiScale = 1;

function drawLine(ctx, a, b, color, dashed = false) {
  ctx.save();
  ctx.strokeStyle = "rgba(0, 0, 0, 0.55)";
  ctx.lineWidth = 4;
  if (dashed) ctx.setLineDash([6, 5]);
  ctx.beginPath();
  ctx.moveTo(a.x, a.y);
  ctx.lineTo(b.x, b.y);
  ctx.stroke();
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.stroke();
  ctx.restore();
}

/** A square for a corner, a diamond for an edge, a circle for a surface point. */
function drawMarker(ctx, at, kind, color, hover = false) {
  const size = (hover ? 6 : 4.5) * uiScale;
  ctx.save();
  ctx.translate(at.x, at.y);
  ctx.beginPath();
  if (kind === "vertex") {
    ctx.rect(-size, -size, size * 2, size * 2);
  } else if (kind === "edge") {
    ctx.moveTo(0, -size * 1.3);
    ctx.lineTo(size * 1.3, 0);
    ctx.lineTo(0, size * 1.3);
    ctx.lineTo(-size * 1.3, 0);
    ctx.closePath();
  } else {
    ctx.arc(0, 0, size, 0, Math.PI * 2);
  }
  ctx.fillStyle = hover ? "rgba(0, 0, 0, 0.35)" : color;
  ctx.fill();
  ctx.lineWidth = hover ? 2 : 1.5;
  ctx.strokeStyle = hover ? color : "rgba(0, 0, 0, 0.6)";
  ctx.stroke();
  ctx.restore();
}

function drawLabel(ctx, at, text, background, ink) {
  const padding = 5 * uiScale;
  const width = ctx.measureText(text).width + padding * 2;
  const height = 18 * uiScale;
  ctx.save();
  ctx.fillStyle = background;
  ctx.beginPath();
  ctx.roundRect(at.x - width / 2, at.y - height / 2, width, height, height / 2);
  ctx.fill();
  ctx.fillStyle = "#0b0c0e";
  ctx.fillText(text, at.x, at.y + 0.5);
  ctx.restore();
}

export function createTools({ renderer, shell, scheduleRender }) {
  const state = {
    view: "perspective",
    style: "shaded",
    measure: { active: false, points: [], hover: null, measurements: [] },
    section: { active: false, axis: "z", flipped: false, cap: true, fraction: 0.72, value: 0 },
    plan: null,
  };

  let hasModel = false;

  for (const button of $("section-card").querySelectorAll("[data-axis]")) {
    button.addEventListener("click", () => setAxis(button.dataset.axis));
  }
  $("section-range").addEventListener("input", (event) => {
    state.section.fraction = Number(event.target.value) / 1000;
    updateSection();
  });
  $("section-flip").addEventListener("click", () => {
    state.section.flipped = !state.section.flipped;
    updateSection();
  });
  $("section-cap").addEventListener("click", () => {
    state.section.cap = !state.section.cap;
    updateSection();
  });
  $("section-off").addEventListener("click", () => setSection(false));
  $("measure-clear").addEventListener("click", clearMeasurement);

  // -------------------------------------------------------------- views

  function setView(view, fit = true) {
    if (!hasModel) return;
    // Any view but the top leaves the plan and brings back the cut it replaced.
    if (state.plan && view !== "top") restoreSectionBeforePlan();
    state.view = view;
    renderer.setView(view, fit);
    scheduleRender();
    syncViewButtons();
  }

  function fitView() {
    if (!hasModel) return;
    renderer.fit(state.view);
    scheduleRender();
  }

  /** Cut horizontally and look straight down; pressed again, return to 3D. */
  function planView() {
    if (!hasModel) return;
    if (state.plan) {
      setView("perspective", true);
      return;
    }
    state.plan = { section: state.section.active ? { ...state.section } : null };
    shell.setPressed("plan", true);
    setSection(true, { axis: "z", fraction: state.section.active ? state.section.fraction : 0.68 });
    setView("top", true);
  }

  /** Look along one world axis from the orientation gizmo, keeping the zoom. */
  function viewAlong(axis) {
    if (!hasModel) return;
    const view = AXIS_VIEWS[axis.join(",")];
    if (!view) return;
    // Any view but the top leaves the plan and brings back the cut it replaced.
    if (state.plan && view !== "top") restoreSectionBeforePlan();
    state.view = view;
    renderer.setView(view, false);
    renderer.viewAlong(axis);
    scheduleRender();
    syncViewButtons();
  }

  function leavePlan() {
    state.plan = null;
    shell.setPressed("plan", false);
  }

  function restoreSectionBeforePlan() {
    const before = state.plan?.section;
    leavePlan();
    if (!before) {
      setSection(false);
      return;
    }
    state.section.flipped = before.flipped;
    setSection(true, { axis: before.axis, fraction: before.fraction });
  }

  function syncViewButtons() {
    for (const button of document.querySelectorAll("[data-view]")) {
      const active = button.dataset.view === state.view;
      button.classList.toggle("active", active);
      button.setAttribute("aria-pressed", String(active));
    }
  }

  // -------------------------------------------------------------- style

  function cycleStyle() {
    if (!hasModel) return;
    state.style = STYLES[(STYLES.indexOf(state.style) + 1) % STYLES.length];
    renderer.setStyle(state.style);
    scheduleRender(state.style === "wire");
    shell.setLabel("style", STYLE_LABELS[state.style], `${STYLE_LABELS[state.style]} display, press D to change`);
  }

  // ------------------------------------------------------------ measure

  const overlay = document.createElement("canvas");
  overlay.className = "measure-overlay";
  overlay.setAttribute("aria-hidden", "true");
  renderer.container.append(overlay);
  const overlayContext = overlay.getContext("2d");
  let hoverFrame = 0;
  let hoverPending = null;
  let lastPointer = null;

  const project = (point) => renderer.project(point);

  // Snapping compares against projected corners, so the pointer is made canvas-local.
  const canvasLocal = (pointer) => {
    const rect = renderer.canvas.getBoundingClientRect();
    return { x: pointer.x - rect.left, y: pointer.y - rect.top };
  };

  function setMeasure(active) {
    if (!hasModel) return;
    state.measure.active = active;
    state.measure.points = [];
    state.measure.hover = null;
    document.body.classList.toggle("measure-mode", active);
    $("measure-card").classList.toggle("hidden", !active);
    shell.setPressed("measure", active);
    updateMeasureCard();
    drawOverlay();
  }

  /** Take the snapped point under a click as the next end of a measurement. */
  function addMeasurePoint(hit, pointer) {
    const snapped = snapToTriangle(hit, canvasLocal(pointer), project);
    if (!snapped) return;
    state.measure.hover = null;
    state.measure.points.push({ point: snapped.point, kind: snapped.kind });
    if (state.measure.points.length === 2) {
      const [start, end] = state.measure.points;
      state.measure.measurements.push(measurementBetween(start.point, end.point));
      state.measure.points = [];
    }
    updateMeasureCard();
    drawOverlay();
  }

  /** Preview the snap under the pointer, at most once a frame. */
  function hoverMeasure(clientX, clientY) {
    if (!state.measure.active || !hasModel) return;
    lastPointer = { x: clientX, y: clientY };
    hoverPending = lastPointer;
    if (hoverFrame) return;
    hoverFrame = requestAnimationFrame(() => {
      hoverFrame = 0;
      const at = hoverPending;
      hoverPending = null;
      if (!at || !state.measure.active || renderer.interacting) return;
      const hit = renderer.pickSurface(at.x, at.y);
      const snapped = hit ? snapToTriangle(hit, canvasLocal(at), project) : null;
      state.measure.hover = snapped ? { ...snapped, pointer: at } : { point: null, kind: "none", pointer: at };
      drawOverlay();
    });
  }

  /** Drop the point being placed, or the last finished measurement. */
  function removeLastMeasurement() {
    if (state.measure.points.length) state.measure.points = [];
    else state.measure.measurements.pop();
    updateMeasureCard();
    drawOverlay();
  }

  function removeMeasurement(index) {
    state.measure.measurements.splice(index, 1);
    updateMeasureCard();
    drawOverlay();
  }

  function clearMeasurement() {
    state.measure.points = [];
    state.measure.measurements = [];
    state.measure.hover = null;
    updateMeasureCard();
    drawOverlay();
  }

  /** Esc while measuring: first the point in progress, then the tool. */
  function escapeMeasure() {
    if (state.measure.points.length) {
      state.measure.points = [];
      updateMeasureCard();
      drawOverlay();
      return;
    }
    setMeasure(false);
  }

  async function copyMeasurements() {
    const { measurements } = state.measure;
    if (!measurements.length) return;
    const offset = renderer.pack?.index?.model_offset ?? [0, 0, 0];
    try {
      await navigator.clipboard.writeText(measurementText(measurements, offset));
      shell.toast(`Copied ${measurements.length === 1 ? "one measurement" : `${measurements.length} measurements`} as tab-separated text`, "success");
    } catch {
      shell.toast("The clipboard is not available in this context", "error");
    }
  }

  function updateMeasureCard() {
    const { points, measurements } = state.measure;
    const last = measurements[measurements.length - 1];
    if (points.length === 1) {
      $("measure-value").textContent = "Pick the second point";
      $("measure-detail").textContent = `From ${formatPoint(points[0].point)}`;
    } else if (last) {
      $("measure-value").textContent = measurementLabel(last);
      $("measure-detail").textContent = measurementDetail(last);
    } else {
      resetMeasureReadout();
    }
    const list = $("measure-list");
    list.replaceChildren(
      ...measurements.map((measurement, index) => {
        const row = document.createElement("li");
        const number = document.createElement("span");
        number.className = "n";
        number.textContent = String(index + 1);
        const length = document.createElement("span");
        length.className = "len";
        length.textContent = measurementLabel(measurement);
        const detail = document.createElement("span");
        detail.className = "d";
        detail.textContent = measurementDetail(measurement);
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "icon-btn sm";
        remove.title = "Remove this measurement";
        remove.setAttribute("aria-label", `Remove measurement ${index + 1}`);
        remove.textContent = "×";
        remove.addEventListener("click", () => removeMeasurement(index));
        row.append(number, length, detail, remove);
        return row;
      }),
    );
    list.classList.toggle("hidden", !measurements.length);
    $("measure-copy").disabled = !measurements.length;
  }

  function resetMeasureReadout() {
    $("measure-value").textContent = "Pick the first point";
    $("measure-detail").textContent = "Snaps to corners and edges";
  }

  let overlayEmpty = false;
  /** Draw every measurement, the rubber band and the snap marker on the 2D overlay canvas. */
  function drawOverlay() {
    const { points, measurements, hover, active } = state.measure;
    const empty = !hasModel || (!measurements.length && !points.length && !active);
    if (empty && overlayEmpty) return;
    overlayEmpty = empty;
    const width = renderer.container.clientWidth;
    const height = renderer.container.clientHeight;
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    if (overlay.width !== Math.round(width * ratio) || overlay.height !== Math.round(height * ratio)) {
      overlay.width = Math.round(width * ratio);
      overlay.height = Math.round(height * ratio);
    }
    const ctx = overlayContext;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    if (empty) return;

    uiScale = scaleUi();
    const accent = readCss("--measure") || "#ff8c1a";
    const ink = readCss("--fg") || "#fff";
    const rect = renderer.canvas.getBoundingClientRect();
    const toLocal = (screen) => ({ x: screen.x, y: screen.y });
    const pointerLocal = (pointer) => ({ x: pointer.x - rect.left, y: pointer.y - rect.top });

    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.font = `600 ${11 * uiScale}px ${readCss("--font") || "sans-serif"}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    measurements.forEach((measurement, index) => {
      const a = project(measurement.a);
      const b = project(measurement.b);
      if (!a.visible || !b.visible) return;
      drawLine(ctx, toLocal(a), toLocal(b), accent);
      drawMarker(ctx, toLocal(a), "vertex", accent);
      drawMarker(ctx, toLocal(b), "vertex", accent);
      drawLabel(ctx, { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }, `${index + 1}  ${measurementLabel(measurement)}`, accent, ink);
    });

    if (points.length === 1) {
      const a = project(points[0].point);
      if (a.visible) {
        const target = hover?.point ? project(hover.point) : hover?.pointer ? { ...pointerLocal(hover.pointer), visible: true } : null;
        if (target?.visible) {
          const local = hover?.point ? toLocal(target) : target;
          drawLine(ctx, toLocal(a), local, accent, true);
          if (hover?.point) {
            drawLabel(ctx, { x: (a.x + local.x) / 2, y: (a.y + local.y) / 2 }, measurementLabel(measurementBetween(points[0].point, hover.point)), accent, ink);
          }
        }
        drawMarker(ctx, toLocal(a), points[0].kind, accent);
      }
    }

    if (active && hover?.point) {
      const screen = project(hover.point);
      if (screen.visible) drawMarker(ctx, toLocal(screen), hover.kind, accent, true);
    }
  }

  $("measure-copy").addEventListener("click", copyMeasurements);
  // ------------------------------------------------------------ section

  function setSection(active, options = {}) {
    if (!hasModel) return;
    if (!active && state.plan) leavePlan();
    state.section.active = active;
    if (options.axis) state.section.axis = options.axis;
    if (Number.isFinite(options.fraction)) state.section.fraction = options.fraction;
    $("section-card").classList.toggle("hidden", !active);
    shell.setPressed("section", active);
    document.body.classList.toggle("section-mode", active);
    updateSection();
  }

  function setAxis(axis) {
    if (!["x", "y", "z"].includes(axis)) return;
    if (axis !== "z" && state.plan) leavePlan();
    state.section.axis = axis;
    updateSection();
  }

  function updateSection() {
    if (!hasModel) return;
    const value = renderer.sectionValue(state.section.axis, state.section.fraction);
    state.section.value = value;
    renderer.setSection(
      state.section.active,
      state.section.axis,
      value,
      state.section.flipped,
      state.section.cap,
    );
    $("section-cap").classList.toggle("active", state.section.cap);
    $("section-cap").setAttribute("aria-pressed", String(state.section.cap));
    scheduleRender();
    $("section-range").value = String(Math.round(state.section.fraction * 1000));
    $("section-value").textContent = `${state.section.axis.toUpperCase()} ${coordinate(value)} m`;
    for (const button of $("section-card").querySelectorAll("[data-axis]")) {
      const on = button.dataset.axis === state.section.axis;
      button.classList.toggle("active", on);
      button.setAttribute("aria-pressed", String(on));
    }
  }

  // ------------------------------------------------------------- shared

  /** Put every tool back to its default for a newly opened model. */
  function reset(modelOpen) {
    hasModel = modelOpen;
    state.view = "perspective";
    state.style = "shaded";
    state.measure = { active: false, points: [], hover: null, measurements: [] };
    state.section = { active: false, axis: "z", flipped: false, cap: true, fraction: 0.72, value: 0 };
    state.plan = null;
    shell.setPressed("plan", false);
    document.body.classList.remove("measure-mode", "section-mode");
    $("measure-card").classList.add("hidden");
    $("section-card").classList.add("hidden");
    $("section-range").value = "720";
    shell.setPressed("measure", false);
    shell.setPressed("section", false);
    shell.setLabel("style", STYLE_LABELS.shaded);
    updateMeasureCard();
    drawOverlay();
    renderer.setStyle("shaded");
    renderer.setSection(false, "z", 0, false);
    syncViewButtons();
    scheduleRender();
  }

  return {
    state,
    setView,
    viewAlong,
    fitView,
    planView,
    cycleStyle,
    setMeasure,
    addMeasurePoint,
    hoverMeasure,
    removeLastMeasurement,
    clearMeasurement,
    escapeMeasure,
    copyMeasurements,
    drawOverlay,
    setSection,
    setAxis,
    reset,
    isMeasuring: () => state.measure.active,
    isSectioning: () => state.section.active,
    isPlan: () => Boolean(state.plan),
    style: () => state.style,
  };
}
