// SPDX-License-Identifier: Apache-2.0
// The package as a host uses it: the example page maps the bare package names
// onto the checkout, opens the embedded fixture and drives the public API.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { pavilionFile } from "../../../viewer/test/fixture.mjs";
import { serveViewer } from "../../../viewer/test/harness.mjs";

const require = createRequire(new URL("../../../viewer/package.json", import.meta.url));
const pkg = fileURLToPath(new URL("../../wasm/pkg/tessifc_wasm_bg.wasm", import.meta.url));
if (!existsSync(pkg)) {
  console.log("skip  bindings/wasm/pkg is missing; run python scripts/build-wasm.py --target web");
  process.exit(0);
}
const { chromium } = require("playwright");

const failures = [];
const check = (condition, message) => {
  console.log(`${condition ? "ok   " : "FAIL "} ${message}`);
  if (!condition) failures.push(message);
};

const server = await serveViewer();
const browser = await chromium.launch({
  channel: process.env.TESSIFC_BROWSER_CHANNEL || undefined,
  args: ["--use-angle=swiftshader", "--use-gl=angle", "--enable-unsafe-swiftshader"],
});
const errors = [];
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 700 } });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
  await page.goto(`${server.origin}/examples/embed-viewer/`);
  await page.waitForFunction(() => Boolean(window.tessifcViewer), null, { timeout: 60_000 });

  await page.evaluate(() => {
    const v = window.tessifcViewer;
    window.__events = { load: 0, progress: 0, close: 0, overlay: [], select: [], visibility: [] };
    v.on("load", () => { window.__events.load += 1; });
    v.on("progress", () => { window.__events.progress += 1; });
    v.on("close", () => { window.__events.close += 1; });
    v.on("overlay", (detail) => { window.__events.overlay.push(detail); });
    v.on("select", (selection) => { window.__events.select.push(selection ? selection.expressIds : null); });
    v.on("visibility", (detail) => { window.__events.visibility.push(detail); });
  });
  await page.setInputFiles("#file", pavilionFile());
  await page.waitForFunction(() => window.__events.load === 1, null, { timeout: 120_000 });

  const loaded = await page.evaluate(() => {
    const v = window.tessifcViewer, r = v.renderer, pack = v.pack();
    r.render(true);
    return {
      instances: pack.instances.count,
      batches: r.batches.length,
      progress: window.__events.progress,
      hierarchy: (v.hierarchy()?.nodes ?? []).length,
      modelId: v.modelId(),
      barShown: !document.getElementById("bar").hidden,
      canvas: [r.canvas.width, r.canvas.height],
    };
  });
  check(loaded.instances > 0 && loaded.batches > 0, `the fixture streams into GPU batches (${loaded.instances} instances, ${loaded.batches} batches)`);
  check(loaded.progress >= 1, `progress events arrive while streaming (${loaded.progress})`);
  check(loaded.hierarchy > 0 && Number.isInteger(loaded.modelId), "the kernel's hierarchy and model id are exposed");
  await page.waitForFunction(() => window.tessifcViewer.overlayState() !== "pending", null, { timeout: 60_000 });
  const overlay = await page.evaluate(() => {
    const v = window.tessifcViewer, r = v.renderer;
    const state = v.overlayState(), triangles = r.displayInfo().depthOverlayTriangles;
    const overlaid = r.contestedOpaqueBatches.length;
    // An analysis that ran out of budget must leave the bounds overlay as it is.
    const empty = { records: new Uint32Array(0), offsets: new Uint32Array([0]), triangles: new Uint32Array(0), exhausted: true };
    const refused = r.applyContestedTriangles(empty) === false;
    const kept = r.displayInfo().depthOverlayTriangles === triangles && r.contestedOpaqueBatches.length === overlaid;
    return { state, triangles, events: window.__events.overlay, refused, kept };
  });
  check(overlay.state === "ready" && Number.isInteger(overlay.triangles), `the overlay is refined to shared planes by the package worker (${overlay.state}, ${overlay.triangles} triangles)`);
  check(overlay.events.length === 1 && overlay.events[0].state === overlay.state && overlay.events[0].triangles === overlay.triangles, "one overlay event reports the settled state");
  check(overlay.refused && overlay.kept, "an exhausted analysis is refused and the overlay stays as it was");
  check(loaded.barShown && loaded.canvas[0] > 0, "the example page reacts to the load event and the canvas has a size");

  const api = await page.evaluate(() => {
    const v = window.tessifcViewer, r = v.renderer, pack = v.pack();
    const ids = [...new Set(Array.from(pack.instances.expressIds))];
    const visibleCount = () => { let n = 0; for (let i = 0; i < pack.instances.count; i++) if (r.visibility[i * 2] === 255) n++; return n; };
    const before = visibleCount();
    const first = ids[0], second = ids[1];
    v.select(first);
    const selected = v.selection();
    const highlighted = selected.records.every((record) => r.visibility[record * 2 + 1] === 255);
    v.hide(first);
    const afterHide = visibleCount();
    const clearedByHide = v.selection() === null;
    v.isolate(second);
    const afterIsolate = visibleCount();
    v.showAll();
    const afterShowAll = visibleCount();
    const distanceBefore = r.camera.distance;
    v.focus(second);
    const focused = r.camera.distance !== distanceBefore;
    v.setView("top");
    const top = r.camera.mode === "top";
    v.setView("perspective");
    v.setStyle("wire");
    const wire = r.style === "wire";
    v.setStyle("shaded");
    v.setSection({ axis: "z", fraction: 0.5 });
    const sectioned = r.section.active && r.section.axis === 2;
    v.setSection(null);
    let threw = null;
    try { v.setView("sideways"); } catch (error) { threw = error.constructor.name; }
    return {
      before, afterHide, afterIsolate, afterShowAll, highlighted, clearedByHide, focused, top, wire, sectioned,
      sectionOff: !r.section.active, threw, secondRecords: v.pack().instances.count,
      selectEvents: window.__events.select, visibilityEvents: window.__events.visibility.length,
    };
  });
  check(api.highlighted, "select lights every part of the product");
  check(api.afterHide < api.before && api.clearedByHide, `hide removes the product and drops its selection (${api.before} to ${api.afterHide})`);
  check(api.afterIsolate < api.afterHide && api.afterIsolate > 0, `isolate keeps only the named product (${api.afterIsolate})`);
  check(api.afterShowAll === api.before, `show all restores every product (${api.afterShowAll})`);
  check(api.focused && api.top && api.wire && api.sectioned && api.sectionOff, "focus, views, styles and sections drive the renderer");
  check(api.threw === "RangeError", "an unknown view is refused with a RangeError");
  check(JSON.stringify(api.selectEvents) === JSON.stringify([[api.selectEvents[0]?.[0]], null]) && api.visibilityEvents === 3, "select and visibility events report each change");

  // A click on the model selects what it hits and pivots there without turning the camera.
  const clicked = await page.evaluate(() => {
    const v = window.tessifcViewer, r = v.renderer;
    v.fit(); r.render(true);
    const rect = r.canvas.getBoundingClientRect();
    let at = null;
    for (let y = 0.3; y < 0.75 && !at; y += 0.05) for (let x = 0.3; x < 0.75 && !at; x += 0.05) {
      const px = rect.left + x * rect.width, py = rect.top + y * rect.height;
      if (v.pick(px, py)) at = { px, py };
    }
    window.__clickAt = at;
    return { at, forward: r.cameraBasis().forward, distance: r.camera.distance };
  });
  check(clicked.at !== null, "the fixture has a pickable surface");
  await page.mouse.click(clicked.at.px, clicked.at.py);
  const afterClick = await page.evaluate((before) => {
    const v = window.tessifcViewer, r = v.renderer;
    const forward = r.cameraBasis().forward;
    const turned = Math.hypot(...forward.map((value, axis) => value - before.forward[axis]));
    return { selection: v.selection(), turned, distance: r.camera.distance, before: before.distance };
  }, clicked);
  check(afterClick.selection !== null, `a click selects the product under it (#${afterClick.selection?.expressIds[0]})`);
  check(afterClick.turned < 1e-9 && afterClick.distance !== afterClick.before, "the click moves the pivot to the surface depth without turning the camera");

  // A second open while the first is still streaming wins; the first releases its kernel model.
  const raced = await page.evaluate(async () => {
    const v = window.tessifcViewer, kernel = window.tessifcKernel;
    const bytes = new Uint8Array(await document.getElementById("file").files[0].arrayBuffer());
    const loads = window.__events.load, closes = window.__events.close;
    const first = v.open(bytes);
    await new Promise((resolve) => { const off = v.on("progress", () => { off(); resolve(); }); });
    const second = v.open(bytes.slice());
    const [a, b] = await Promise.all([first, second]);
    return { first: a, second: b !== null && v.modelId() === b.modelId, models: kernel.modelCount(), loads: window.__events.load - loads, closes: window.__events.close - closes };
  });
  check(raced.first === null && raced.second, "an open superseded mid-stream resolves null and the later one owns the view");
  check(raced.models === 1 && raced.loads === 1 && raced.closes === 2, `a superseded open closes its kernel model and emits no load; the model on screen and the abandoned stream each emit close (${raced.models} model, ${raced.loads} load, ${raced.closes} close)`);

  const closed = await page.evaluate(() => {
    const v = window.tessifcViewer;
    v.close();
    const emptied = v.pack() === null && v.renderer.batches.length === 0 && window.tessifcKernel.modelCount() === 0;
    v.dispose();
    return { emptied, canvasGone: !document.getElementById("host").querySelector("canvas") };
  });
  check(closed.emptied, "close drops the model from the view and the kernel");
  check(closed.canvasGone, "dispose removes the canvas");

  // Without the worker the overlay event still settles, so a host waiting on it is not left hanging.
  const plain = await page.evaluate(async () => {
    const { createViewer } = await import("../../bindings/viewer/src/index.js");
    const v = createViewer(document.getElementById("host"), { kernel: window.tessifcKernel, coincidence: false });
    const events = [];
    v.on("overlay", (detail) => events.push(detail));
    await v.open(document.getElementById("file").files[0]);
    const state = v.overlayState();
    v.dispose();
    return { state, events, models: window.tessifcKernel.modelCount() };
  });
  check(plain.state === "off" && plain.events.length === 1 && plain.events[0].state === "off", `coincidence: false reports the overlay as off (${plain.state})`);
  check(plain.models === 0, "dispose closes the kernel model");

  // An empty model opens as an empty scene; the session and applyDelta fill it in place.
  const built = await page.evaluate(async () => {
    const { createViewer } = await import("../../bindings/viewer/src/index.js");
    const { createModel } = await import("../../bindings/edit/src/create-model.js");
    const v = createViewer(document.getElementById("host"), { kernel: window.tessifcKernel, coincidence: false });
    const events = [];
    v.on("load", (detail) => events.push(["load", detail.empty]));
    v.on("revision", (detail) => events.push(["revision", detail.revision, detail.affectedProducts.length, detail.changed]));
    const opened = await v.open(createModel({ name: "Empty", storeys: [{ name: "Ground floor", elevation: 0 }] }));
    const emptyBatches = v.renderer.batches.length;
    const session = v.session();
    const { report, delta } = session.runScript('const w = ifc.addWall({ from: [0, 0], to: [4, 0], height: 3 }); ifc.addColumn({ at: [1, 1], height: 3 }); print(w.id);');
    const applied = v.applyDelta(delta);
    v.renderer.render(true);
    const wallId = Number(report.stdout);
    v.select(wallId);
    const second = session.runScript(`ifc.get(${wallId}).Representation.Representations[0].Items[0].Depth = 4;`);
    const again = v.applyDelta(second.delta);
    const result = { opened: opened.empty, emptyBatches, applied, again, batches: v.renderer.batches.length, instances: v.pack().instances.active.reduce((a, b) => a + b, 0),
      selected: v.selection()?.expressIds, events, revision: session.revision };
    v.dispose();
    return result;
  });
  check(built.opened === true && built.emptyBatches === 0 && built.events[0]?.[1] === true, "an empty model opens as an empty scene and says so in the load event");
  check(built.applied.revision === "1" && built.applied.affectedProducts.length === 2 && built.batches > 0 && built.instances === 2, `the session's delta adds the wall and the column (${built.batches} batches)`);
  check(built.again.affectedProducts.length === 1 && built.selected?.[0] > 0 && built.events.at(-1)?.[1] === "2", "a second delta keeps the selection and reports revision 2");

  // The page follows a local session server and applies every version it publishes.
  const { createModelHost } = await import("../../mcp/src/session-host.js");
  const { createViewerServer } = await import("../../mcp/src/viewer-server.js");
  const { Kernel } = createRequire(import.meta.url)(fileURLToPath(new URL("../../wasm/pkg-node/tessifc_wasm.js", import.meta.url)));
  const host = createModelHost({ Kernel, save: false, version: "test" });
  await host.newModel({ name: "Followed", storeys: [{ name: "Ground floor", elevation: 0 }] });
  const followed = createViewerServer(host, { root: fileURLToPath(new URL("../../../", import.meta.url)), port: 0 });
  const followedUrl = await followed.listen();
  try {
    await page.goto(`${followedUrl}/examples/embed-viewer/?session=file`);
    await page.waitForFunction(() => Boolean(window.tessifcViewer) && window.tessifcViewer.pack() !== null, null, { timeout: 60_000 });
    await page.evaluate(() => {
      window.__revisions = [];
      window.tessifcViewer.on("revision", (detail) => window.__revisions.push(detail));
    });
    await host.run('ifc.addWall({ from: [0, 0], to: [5, 0], height: 3, name: "Followed wall" });');
    await page.waitForFunction(() => window.__revisions.length === 1, null, { timeout: 60_000 });
    const following = await page.evaluate(() => ({ revisions: window.__revisions, instances: window.tessifcViewer.pack().instances.active.reduce((a, b) => a + b, 0),
      status: document.getElementById("status").textContent }));
    check(following.revisions[0].revision === "1" && following.revisions[0].affectedProducts.length === 1 && following.instances === 1, "a followed session's edit arrives as a revision event");
    check(/Revision 1: 1 updated/.test(following.status), `the example page shows the kernel's counts (${following.status})`);
  } finally {
    await followed.close();
    host.dispose();
  }
  check(errors.length === 0, `no page errors (${errors.join("; ")})`);
} finally {
  await browser.close();
  await server.close();
}

if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
console.log("PASS  the embedded viewer package opens, selects, hides, sections and disposes");
