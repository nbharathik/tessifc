// SPDX-License-Identifier: Apache-2.0

//! The right-hand panel: element properties, model statistics, the conversion
//! report and the attribute editor.

import { classLabelColor, humanizeIfcClass } from "./igp.js";
import { bytes, coordinate, count, duration, errorText, plural } from "./format.js";

const $ = (id) => document.getElementById(id);

const EDITOR_ORDER = ["Name", "Description", "ObjectType", "LongName", "Tag"];

export function createInspector({ onApplyEdits }) {
  const propertyList = $("property-list");
  const propertySearch = $("property-search");
  const editFields = $("edit-fields");

  let fields = [];
  let filterFrame = 0;

  propertySearch.addEventListener("input", () => {
    if (filterFrame) return;
    filterFrame = requestAnimationFrame(() => {
      filterFrame = 0;
      filterProperties();
    });
  });
  $("edit-apply").addEventListener("click", () => {
    const changes = collectChanges();
    if (changes.length) onApplyEdits(changes);
  });
  $("edit-revert").addEventListener("click", revertEdits);

  // ---------------------------------------------------------- selection

  /** Show the selected element and put the properties list into a loading state; `quiet` keeps the current list. */
  function showSelection({ className, expressId, geometryId, triangles, materials, position, size, shared, extent, path, state, quiet = false }) {
    const swatch = classLabelColor(className);
    const label = `${className.toUpperCase()} #${expressId}`;
    $("properties-empty").classList.add("hidden");
    replay($("selection-card"), swatch, quiet);
    $("property-search-box").classList.remove("hidden");
    $("property-heading").classList.remove("hidden");
    $("selection-name").textContent = humanizeIfcClass(className);
    $("selection-class").textContent = label;

    $("element-empty").classList.add("hidden");
    $("element-body").classList.remove("hidden");
    replay($("element-card"), swatch, quiet);
    $("element-name").textContent = humanizeIfcClass(className);
    $("element-class").textContent = label;
    $("element-centre").textContent = `${position.map(coordinate).join(" / ")} m`;
    $("element-size").textContent = size ? `${size.map(coordinate).join(" x ")} m` : "-";
    $("element-min").textContent = extent ? `${extent.min.map(coordinate).join(" / ")} m` : "-";
    $("element-max").textContent = extent ? `${extent.max.map(coordinate).join(" / ")} m` : "-";
    $("element-mesh").textContent = `#${geometryId}`;
    $("element-triangles").textContent = count(triangles);
    $("element-materials").textContent = plural(materials, "material");
    $("element-shared").textContent = shared ? "Shared family, placed by transform" : "Unique";
    renderPath(path);
    setElementState(state);
    for (const tag of [$("inspector-tag"), $("editor-tag")]) {
      tag.textContent = `#${expressId}`;
      tag.classList.remove("hidden");
    }
    if (quiet) return;
    propertySearch.value = "";
    propertySearch.disabled = true;
    $("property-count").textContent = "reading";
    $("property-none").classList.add("hidden");
    const loading = document.createElement("p");
    loading.className = "note";
    loading.textContent = "Reading IFC attributes";
    propertyList.replaceChildren(loading);

    $("edit-empty").classList.add("hidden");
    $("edit-body").classList.remove("hidden");
    $("edit-class").textContent = `${className.toUpperCase()} #${expressId}`;
    editFields.replaceChildren();
    $("edit-status").textContent = "Reading editable fields";
    $("edit-apply").disabled = true;
    $("edit-revert").disabled = true;
    fields = [];
  }

  /** Return the panel to its empty state. */
  function clearSelection() {
    fields = [];
    $("properties-empty").classList.remove("hidden");
    $("selection-card").classList.add("hidden");
    $("property-search-box").classList.add("hidden");
    $("property-heading").classList.add("hidden");
    $("property-none").classList.add("hidden");
    $("inspector-tag").classList.add("hidden");
    $("editor-tag").classList.add("hidden");
    $("element-empty").classList.remove("hidden");
    $("element-body").classList.add("hidden");
    $("element-path").replaceChildren();
    propertyList.replaceChildren();
    propertySearch.value = "";
    propertySearch.disabled = true;
    $("edit-empty").classList.remove("hidden");
    $("edit-body").classList.add("hidden");
    editFields.replaceChildren();
    $("edit-status").textContent = "";
  }

  // The entrance runs through the animation API, so restarting it forces no layout.
  let entrance = null;

  /** Show the card again with its class colour and a short entrance, unless `quiet`. */
  function replay(card, swatch, quiet = false) {
    card.classList.remove("hidden");
    card.style.setProperty("--sel-swatch", swatch);
    if (quiet) return;
    if (!entrance) {
      const tokens = getComputedStyle(document.documentElement);
      entrance = {
        duration: (parseFloat(tokens.getPropertyValue("--base")) || 0.2) * 1000,
        easing: tokens.getPropertyValue("--ease").trim() || "ease",
      };
    }
    for (const animation of card.getAnimations()) animation.cancel();
    card.animate([{ opacity: 0, transform: "translateY(-4px)" }, { opacity: 1, transform: "none" }], entrance);
  }

  /** The spatial containers above the element, top down. */
  function renderPath(path) {
    const items = Array.isArray(path) ? path.slice(0, 64) : [];
    $("element-path").replaceChildren(
      ...items.map((item, depth) => {
        const row = document.createElement("li");
        row.style.setProperty("--depth", String(depth));
        const name = document.createElement("span");
        name.className = "name";
        name.textContent = clip(item.name, 120);
        const kind = document.createElement("span");
        kind.className = "kind";
        kind.textContent = clip(item.class, 40);
        row.append(name, kind);
        return row;
      }),
    );
    $("element-path-none").classList.toggle("hidden", items.length > 0);
  }

  /** Visible, hidden or isolated, as the viewport shows it right now. */
  function setElementState(text) {
    $("element-state").textContent = text;
  }

  /** Put the cursor in the first editable field, if the entity has one. */
  function focusEditor() {
    editFields.querySelector("input, textarea")?.focus();
  }

  // --------------------------------------------------------- properties

  /** Render the attribute list returned by the kernel. */
  function setProperties(info) {
    fields = Array.isArray(info?.fields) ? info.fields : [];
    propertyList.replaceChildren(
      ...fields.map((field) => {
        const row = document.createElement("div");
        row.className = "prop-row";
        const term = document.createElement("dt");
        term.textContent = field.name;
        const type = document.createElement("code");
        type.textContent = field.type || field.kind || "value";
        term.append(type);
        const value = document.createElement("dd");
        const display = propertyValue(field);
        value.textContent = display;
        value.classList.toggle("unset", display === "Not set");
        row.dataset.search = `${field.name} ${field.type ?? ""} ${field.kind ?? ""} ${display}`.toLowerCase();
        row.append(term, value);
        return row;
      }),
    );
    propertySearch.disabled = fields.length === 0;
    $("property-count").textContent = plural(fields.length, "field");
    filterProperties();
    renderEditor(info);
    const name = fields.find((field) => field.name === "Name")?.value;
    if (name) {
      $("selection-name").textContent = name;
      $("element-name").textContent = name;
    }
  }

  /** Report that the attributes could not be read. */
  function setPropertyError(message) {
    const text = clip(message, 300);
    const note = document.createElement("p");
    note.className = "note error";
    note.textContent = text;
    propertyList.replaceChildren(note);
    $("property-count").textContent = "unavailable";
    $("edit-status").textContent = text;
  }

  function filterProperties() {
    const query = propertySearch.value.trim().toLowerCase();
    const rows = [...propertyList.querySelectorAll(".prop-row")];
    let visible = 0;
    for (const row of rows) {
      const match = !query || row.dataset.search.includes(query);
      row.classList.toggle("hidden", !match);
      if (match) visible += 1;
    }
    $("property-none").classList.toggle("hidden", !query || visible > 0);
    if (rows.length) {
      $("property-count").textContent = query ? `${count(visible)} of ${count(rows.length)}` : plural(rows.length, "field");
    }
  }

  // ------------------------------------------------------------- editor

  function renderEditor(info) {
    const editable = (info?.fields ?? [])
      .filter((field) => field.kind === "string" && field.name !== "GlobalId")
      .sort((left, right) => {
        const a = EDITOR_ORDER.indexOf(left.name);
        const b = EDITOR_ORDER.indexOf(right.name);
        return (a < 0 ? 100 : a) - (b < 0 ? 100 : b) || left.index - right.index;
      });

    editFields.replaceChildren(
      ...editable.map((field) => {
        const label = document.createElement("label");
        label.className = "edit-field";
        const heading = document.createElement("span");
        heading.textContent = field.name;
        const type = document.createElement("code");
        type.textContent = field.type || "STRING";
        heading.append(type);
        const input = field.name === "Description" ? document.createElement("textarea") : document.createElement("input");
        if (input instanceof HTMLTextAreaElement) input.rows = 3;
        else input.type = "text";
        input.value = field.value ?? "";
        input.dataset.attribute = field.name;
        input.dataset.original = field.value ?? "";
        input.addEventListener("input", syncEditorButtons);
        label.append(heading, input);
        return label;
      }),
    );
    $("edit-status").textContent = editable.length
      ? "Only changed fields are patched. Every other source byte is preserved."
      : "This entity has no editable text attribute.";
    syncEditorButtons();
  }

  function collectChanges() {
    return [...editFields.querySelectorAll("input, textarea")]
      .filter((input) => input.value !== input.dataset.original)
      .map((input) => ({ attribute: input.dataset.attribute, value: input.value, raw: false }));
  }

  function revertEdits() {
    for (const input of editFields.querySelectorAll("input, textarea")) input.value = input.dataset.original;
    syncEditorButtons();
  }

  function syncEditorButtons() {
    const dirty = collectChanges().length > 0;
    $("edit-apply").disabled = !dirty;
    $("edit-revert").disabled = !dirty;
  }

  /** Report that a patch is on its way to the worker. */
  function setEditPending() {
    $("edit-apply").disabled = true;
    $("edit-status").textContent = "Validating and applying the source patch";
  }

  /** Show the result of an applied patch. */
  function setEditResult(info, changed) {
    setProperties(info);
    $("edit-status").textContent = `${plural(changed, "field")} changed. Export the IFC to keep this revision.`;
  }

  /** Show why a patch was rejected. */
  function setEditError(message) {
    $("edit-status").textContent = errorText(message);
    syncEditorButtons();
  }

  // --------------------------------------------------------- statistics

  /** Fill the model ledger after a conversion. */
  function setStatistics({ info, summary, timings, pack, model, buildMs, totalMs, file }) {
    $("m-entities").textContent = count(info.entities);
    $("m-products").textContent = count(summary.products);
    $("m-triangles").textContent = count(summary.triangles);
    setGeometryFacts({ pack, model });
    $("m-parse").textContent = duration(timings.parseMs);
    $("m-geometry").textContent = duration(timings.geometryMs);
    $("m-pack").textContent = duration(timings.packMs);
    $("m-build").textContent = duration(buildMs);
    $("m-first").textContent = Number.isFinite(timings.firstPaintMs)
      ? `${duration(timings.firstPaintMs)}, ${plural(timings.chunks ?? 1, "chunk")}`
      : "whole pack";
    $("m-total").textContent = duration(totalMs);
    $("m-mem-source").textContent = bytes(info.sourceRetainedBytes ?? file.size);
    $("m-mem-image").textContent = bytes(info.imageBytes);
    $("m-mem-pack").textContent = bytes(pack.bytes);
    $("m-mem-gpu").textContent = bytes(model.gpuBytes);
  }

  /** The mesh and draw facts, which a geometry patch can change on their own. */
  function setGeometryFacts({ pack, model }) {
    $("m-meshes").textContent = count(pack.geometry.length);
    $("m-shared").textContent = pack.sharedRecords
      ? `${count(pack.sharedRecords)} of ${count(pack.instances.count)} records`
      : `none of ${count(pack.instances.count)} records`;
    $("m-draws").textContent = count(model.drawCalls);
    $("m-suppressed").textContent = count(model.suppressedTriangles);
  }

  /** Update the retained GPU estimate after a style or visibility change. */
  function setGpuBytes(value) {
    $("m-mem-gpu").textContent = bytes(value);
  }

  /** Report what the browser actually granted for the drawing buffer. */
  function setDisplayFacts(renderer) {
    if (typeof renderer.displayInfo !== "function") return;
    const info = renderer.displayInfo();
    $("m-depth").textContent = info.depthBits
      ? `${info.depthBits}-bit${info.reversedDepth ? " reversed float" : ""}`
      : "unknown";
    $("m-samples").textContent = info.antialiasing
      ? `${info.samples}x MSAA`
      : info.antialiasRequested
        ? "requested, not granted"
        : "off";
    $("m-resolution").textContent = `${renderer.canvas.width} x ${renderer.canvas.height}`;

    const note = $("m-display-note");
    if (!info.depthOverlayPrecisionSafe) {
      note.textContent =
        "This model reaches beyond the sub-millimetre precision of one render origin. Collision priority is off so a ranked rear layer cannot replace the nearest surface.";
      note.classList.add("error");
    } else if (!info.depthIsAdequate && info.depthBits) {
      note.textContent = `This browser gave a ${info.depthBits}-bit depth buffer and refused a higher-precision target. Very close layers may still lose precision.`;
      note.classList.add("error");
    } else if (info.reversedDepth) {
      note.textContent = info.depthPlanExhausted
        ? "Reversed floating-point depth keeps overlapping surfaces steady. This model has so many overlapping products that every opaque material is given a stable priority, which costs one extra draw per batch."
        : "Reversed floating-point depth and a stable material priority keep overlapping surfaces steady while you orbit.";
      note.classList.remove("error");
    } else if (!info.depthOverlayClamped) {
      note.textContent = "This browser has no bounded polygon offset, so overlapping surfaces are settled with a whole-unit depth step instead. Grazing views of coincident faces may still shimmer.";
      note.classList.add("error");
    } else {
      note.textContent = "A 24-bit target and a stable material priority keep overlapping surfaces steady.";
      note.classList.remove("error");
    }
  }

  // ------------------------------------------------------------ quality

  /** Render the conversion diagnostics and return how many are errors. */
  function setQuality(diagnostics, renderer) {
    const list = Array.isArray(diagnostics) ? diagnostics : [];
    const errors = list.filter((item) => String(item?.sev).toLowerCase() === "error").length;
    $("quality-count").textContent = list.length ? plural(list.length, "diagnostic") : "clean";
    const badge = $("quality-badge");
    badge.textContent = errors ? count(errors) : "";
    badge.dataset.count = String(errors);

    const floored = [];
    if (renderer.dimmedProducts) floored.push(`${count(renderer.dimmedProducts)} styled fully transparent`);
    if (renderer.darkProducts) floored.push(`${count(renderer.darkProducts)} styled darker than the viewport`);
    const flooredNote = floored.length
      ? ` ${floored.join(" and ")}, drawn at the minimum visible setting so they can still be seen and picked. The properties panel shows each file colour unchanged.`
      : "";
    $("quality-summary").textContent =
      (list.length ? `${plural(list.length, "diagnostic")} retained for review.` : "No conversion diagnostic was reported for this model.") +
      flooredNote;

    $("diagnostic-list").replaceChildren(
      ...list.slice(0, 200).map((item) => {
        // File-supplied, so every field is coerced and bounded.
        const severity = String(item?.sev ?? "info").toLowerCase();
        const entry = document.createElement("li");
        entry.className = `diag ${severity === "error" ? "error" : severity === "warning" ? "warning" : ""}`;
        const code = document.createElement("code");
        code.textContent =
          `${severity.toUpperCase()} ${clip(item?.code, 64)}` + (item?.id ? ` #${clip(item.id, 24)}` : "");
        const message = document.createElement("span");
        message.textContent = clip(item?.msg, 400);
        entry.append(code, message);
        return entry;
      }),
    );
    return errors;
  }

  /** Reset every panel for a new session. */
  function clear() {
    clearSelection();
    for (const id of [
      "m-entities", "m-products", "m-triangles", "m-meshes", "m-shared", "m-draws", "m-suppressed",
      "m-parse", "m-geometry", "m-pack", "m-build", "m-first", "m-total",
      "m-mem-source", "m-mem-image", "m-mem-pack", "m-mem-gpu",
    ]) $(id).textContent = "-";
    $("quality-count").textContent = "0";
    $("quality-summary").textContent = "Open an IFC to review its conversion diagnostics.";
    $("diagnostic-list").replaceChildren();
    $("quality-badge").dataset.count = "0";
    $("quality-badge").textContent = "";
  }

  return {
    showSelection,
    clearSelection,
    focusEditor,
    setElementState,
    setProperties,
    setPropertyError,
    setEditPending,
    setEditResult,
    setEditError,
    setStatistics,
    setGeometryFacts,
    setGpuBytes,
    setDisplayFacts,
    setQuality,
    clear,
  };
}

/** Bound a file-supplied string so one long value cannot stall layout. */
function clip(value, limit) {
  const text = String(value ?? "");
  return text.length > limit ? `${text.slice(0, limit - 3)}...` : text;
}

/** The readable value of one attribute, falling back to the raw source token. */
function propertyValue(field) {
  if (field.value !== null && field.value !== undefined && String(field.value).trim()) return String(field.value);
  const raw = String(field.raw ?? "").trim();
  if (!raw || raw === "$" || raw === "*") return "Not set";
  return raw.length > 220 ? `${raw.slice(0, 217)}...` : raw;
}
