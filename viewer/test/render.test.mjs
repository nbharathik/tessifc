// SPDX-License-Identifier: Apache-2.0
// Browser pixel tests using a first-party generated IFC fixture.

import assert from "node:assert/strict";
import { existsSync, statSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";
import { instrumentViewer, serveViewer } from "./harness.mjs";
import { pavilionFile } from "./fixture.mjs";
import { checkInteraction } from "./interaction.mjs";
import { checkRenderWork } from "./render-work.mjs";
import { checkPicking } from "./picking-render.mjs";
import { checkDeltaRendering } from "./delta-render.mjs";
import { checkScheduling } from "./scheduling.mjs";
import { checkGpuPacing } from "./gpu-pacing.mjs";
import { checkSelectionUpdates } from "./selection-updates.mjs";
import { checkTree } from "./tree.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..", "..");

let chromium = null;
try {
  ({ chromium } = await import("playwright"));
} catch {
  chromium = null;
}

// An explicitly supplied model must exist; otherwise use the in-memory fixture.
const requestedModel = process.env.TESSIFC_TEST_MODEL
  ? resolve(repo, process.env.TESSIFC_TEST_MODEL)
  : null;
const model =
  requestedModel && existsSync(requestedModel) && statSync(requestedModel).isFile()
    ? requestedModel
    : requestedModel ? null : pavilionFile();
const wasm = resolve(repo, "bindings", "wasm", "pkg", "tessifc_wasm_bg.wasm");

if (!chromium || !model || !existsSync(wasm)) {
  const missing = !chromium
    ? "playwright is not installed"
    : !model
      ? "TESSIFC_TEST_MODEL does not name an existing IFC file"
      : "the browser WASM package is not built";
  console.error(`rendering tests unavailable: ${missing}`);
  process.exit(1);
}

const server = await serveViewer();
const { origin } = server;

let browser;
try {
  browser = await chromium.launch({
    channel: process.env.TESSIFC_BROWSER_CHANNEL || undefined,
    args: ["--use-angle=swiftshader", "--use-gl=angle", "--enable-unsafe-swiftshader"],
  });
} catch (error) {
  const reason = String(error.message).split(/\r?\n/)[0];
  console.error(`rendering tests: browser would not start (${reason})`);
  await server.close();
  process.exit(1);
}

let failed = false;
const check = (condition, message) => {
  if (condition) {
    console.log(`ok    ${message}`);
  } else {
    failed = true;
    console.log(`FAIL  ${message}`);
  }
};

try {
  const page = await browser.newPage({ viewport: { width: 640, height: 480 } });
  await instrumentViewer(page);
  const problems = [];
  page.on("pageerror", (error) => problems.push(String(error.message)));
  page.on("console", (message) => {
    const text = message.text();
    if (/GL_INVALID|WebGL.*error|Uncaught/i.test(text)) problems.push(text);
  });

  await page.goto(`${origin}/viewer/`, { waitUntil: "load" });
  await page.waitForFunction(() => window.__tessifc, null, { timeout: 60_000 });
  const metadataOnly = Buffer.from("ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;#1=IFCWALL('empty',$,$,$,$,$,$,$,$);ENDSEC;END-ISO-10303-21;");
  await page.setInputFiles("#file-input", { name:'metadata.ifc', mimeType:'application/octet-stream', buffer:metadataOnly });
  await page.waitForFunction(() => window.__tessifc.loadStatus?.().state === 'empty', null, { timeout:60_000 });
  check(await page.evaluate(() => window.__tessifc.loadStatus().finished), 'a model without geometry reaches an explicit terminal state');
  await page.setInputFiles("#file-input", model);
  await page.waitForFunction(() => window.__tessifc.ready(), null, { timeout: 300_000 });
  await page.waitForTimeout(800);
  await checkInteraction(page, check);
  await checkRenderWork(page, check);
  await checkPicking(page, check);
  await checkDeltaRendering(page, check);
  await checkScheduling(page, check);
  await checkGpuPacing(page, check);
  await checkSelectionUpdates(page, check);
  await checkTree(page, check);

  const display = await page.evaluate(() => window.__tessifc.renderer.displayInfo());
  check(display.depthIsAdequate, `depth buffer resolves coincident surfaces (${display.depthBits}-bit)`);
  check(display.antialiasing, `the scene is antialiased (${display.samples}x)`);

  const openingControl = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const records = [];
    for (let record = 0; record < r.pack.instances.count; record += 1) {
      if (r.pack.instances.flags[record] & (1 << 1)) records.push(record);
    }
    const button = document.getElementById("cmd-openings");
    return {
      records,
      visibility: records.map((record) => r.visibility[record * 2]),
      disabled: button.disabled,
      pressed: button.getAttribute("aria-pressed"),
    };
  });
  check(
    openingControl.records.length > 0 && openingControl.visibility.every((value) => value === 0),
    `all ${openingControl.records.length} opening records start hidden`,
  );
  check(
    !openingControl.disabled && openingControl.pressed === "false",
    "the opening control is enabled and reflects the hidden state",
  );
  await page.click("#cmd-openings");
  const shownOpenings = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const visible = [];
    for (let record = 0; record < r.pack.instances.count; record += 1) {
      if (r.pack.instances.flags[record] & (1 << 1)) visible.push(r.visibility[record * 2]);
    }
    return visible;
  });
  check(shownOpenings.every((value) => value === 255), "the top control reveals opening geometry");
  await page.click("#cmd-openings");
  const hiddenOpenings = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const hidden = [];
    for (let record = 0; record < r.pack.instances.count; record += 1) {
      if (r.pack.instances.flags[record] & (1 << 1)) hidden.push(r.visibility[record * 2]);
    }
    return hidden;
  });
  check(hiddenOpenings.every((value) => value === 0), "the top control hides opening geometry again");

  // Every product with geometry must paint something when framed alone.
  const isolation = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const gl = r.gl;
    // The whole frame: a product at the ends of a long box paints nothing in its middle.
    const width = r.canvas.width;
    const height = r.canvas.height;
    const pixels = new Uint8Array(width * height * 4);
    const background = Array.from(r.background, (value) => Math.round(value * 255));
    const blank = [];
    for (let record = 0; record < r.pack.instances.count; record += 1) {
      const location = r.recordLocations[record];
      if (!location?.geometry?.indices?.length) continue;
      r.setVisibility((index) => index === record);
      const bounds = location.bounds;
      const centre = bounds.min.map((value, axis) => (value + bounds.max[axis]) / 2);
      const radius = Math.max(
        Math.hypot(...bounds.max.map((value, axis) => value - bounds.min[axis])) / 2,
        1e-3,
      );
      const distance = radius * 3.2;
      r.camera.target = centre;
      r.camera.position = [
        centre[0] + distance * 0.6,
        centre[1] - distance * 0.7,
        centre[2] + distance * 0.5,
      ];
      r.camera.distance = distance;
      r.dirty = true;
      r.render(true);
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
      gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      let lit = 0;
      for (let i = 0; i < width * height; i += 1) {
        const delta =
          Math.abs(pixels[i * 4] - background[0]) +
          Math.abs(pixels[i * 4 + 1] - background[1]) +
          Math.abs(pixels[i * 4 + 2] - background[2]);
        if (delta > 18) lit += 1;
      }
      if (lit === 0) {
        blank.push({ record, express: r.pack.instances.expressIds?.[record] ?? null });
        if (blank.length > 12) break;
      }
    }
    r.setVisibility(() => true);
    r.dirty = true;
    r.render(true);
    return {
      products: r.pack.instances.count,
      blank,
      dimmed: r.dimmedProducts ?? 0,
      dark: r.darkProducts ?? 0,
    };
  });
  check(
    isolation.blank.length === 0,
    `every one of ${isolation.products} products renders when isolated` +
      (isolation.blank.length ? ` (blank: ${JSON.stringify(isolation.blank.slice(0, 5))})` : ""),
  );
  if (isolation.dimmed > 0 || isolation.dark > 0) {
    console.log(
      `      floors applied: ${isolation.dimmed} fully transparent, ${isolation.dark} darker than the viewport`,
    );
  }

  // The coincident-face filter claims to remove nothing visible; off must not change the picture.
  const framePixels = async () => {
    const png = await page.evaluate(() => {
      const r = window.__tessifc.renderer;
      const b = r.renderBounds;
      const distance = b.radius * 2.0;
      r.camera.target = b.center.slice();
      r.camera.position = [
        b.center[0] + distance * 0.62,
        b.center[1] - distance * 0.55,
        b.center[2] + distance * 0.42,
      ];
      r.camera.distance = distance;
      r.dirty = true;
      r.render(true);
      return r.canvas.toDataURL("image/png");
    });
    return Buffer.from(png.split(",")[1], "base64");
  };
  const setSuppression = (on) =>
    page.evaluate((flag) => {
      const r = window.__tessifc.renderer;
      const camera = JSON.parse(JSON.stringify(r.camera));
      r.suppressCoplanar = flag;
      r.load(window.__tessifc.pack());
      r.camera = camera;
      r.dirty = true;
    }, on);

  await setSuppression(true);
  const withFilter = await framePixels();
  await setSuppression(false);
  const withoutFilter = await framePixels();
  if (process.env.TESSIFC_RENDER_ARTIFACTS) {
    const out = resolve(process.env.TESSIFC_RENDER_ARTIFACTS);
    mkdirSync(out, { recursive: true });
    writeFileSync(resolve(out, "filter-on.png"), withFilter);
    writeFileSync(resolve(out, "filter-off.png"), withoutFilter);
    writeFileSync(resolve(out, "filter-state.json"), JSON.stringify(await page.evaluate(() => {
      const r = window.__tessifc.renderer;
      return { camera: r.camera, origin: r.renderOrigin, bounds: r.renderBounds, display: r.displayInfo(), interacting: r.interacting };
    }), null, 2));
  }
  await setSuppression(true);
  const differing = countDifferences(withFilter, withoutFilter);
  check(
    differing.ratio < 0.0005,
    `coincident-face suppression changes no visible pixels (${differing.count} of ${differing.total})`,
  );

  // The tie-break must settle only surfaces two products both claim.
  const setTieBreak = (on) =>
    page.evaluate((flag) => {
      const r = window.__tessifc.renderer;
      r.depthTieBreak = flag;
      r.dirty = true;
    }, on);
  await setTieBreak(false);
  const contested = await framePixels();
  await setTieBreak(true);
  const settled = await framePixels();
  const decided = profileDifferences(contested, settled);
  check(
    decided.ratio < 0.02 &&
      // A one-pixel raster difference is not a statistically meaningful edge ratio.
      (decided.edgeRatio >= 0.98 || decided.offEdge <= 2) &&
      decided.largestComponentRatio < 0.002 &&
      decided.offEdge <= 64,
    `the depth tie-break remains confined to bounded existing edges ` +
      `(${decided.count}/${decided.total}, ${(decided.edgeRatio * 100).toFixed(2)}% edge-local, ` +
      `largest ${decided.largestComponent}, ${decided.offEdge} off-edge)`,
  );

  // Temporal fixture: three colours on one plane, one on the opposite diagonal.
  // Stable material priority makes red win at every angle in both conventions.
  const temporal = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const identity = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
    const positions = Float32Array.from([
      -2, -2, 0,
       2, -2, 0,
       2,  2, 0,
      -2,  2, 0,
    ]);
    const pack = {
      geometry: [
        {
          id: 0,
          bbox: [-2, -2, 0, 2, 2, 0],
          positions,
          indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
        },
        {
          id: 1,
          bbox: [-2, -2, 0, 2, 2, 0],
          positions,
          indices: Uint16Array.from([0, 1, 3, 1, 2, 3]),
        },
      ],
      instances: {
        count: 3,
        geometryIds: Uint32Array.from([0, 0, 1]),
        expressIds: Uint32Array.from([1, 2, 3]),
        classIds: Uint16Array.from([0, 0, 0]),
        transforms: Float32Array.from([...identity, ...identity, ...identity]),
        // First-seen and draw order deliberately disagree with sorted rank.
        colors: Uint8Array.from([
          45, 205, 80, 255,
          230, 55, 45, 255,
          55, 95, 220, 255,
        ]),
        flags: Uint16Array.from([0, 0, 0]),
      },
    };
    const gl = r.gl;
    const sampleOrbit = (scene = pack) => {
      r.load(scene);
      const center = r.recordLocations[0]?.bounds?.center ?? [0, 0, 0];
      const samples = [];
      const sampleSize = 17;
      const pixels = new Uint8Array(sampleSize * sampleSize * 4);
      for (let step = 0; step < 16; step += 1) {
        const angle = step / 16 * Math.PI * 2;
        const offset = [Math.cos(angle) * 4.8, Math.sin(angle) * 4.8, 3.2];
        r.camera.target = center.slice();
        r.camera.position = center.map((value, axis) => value + offset[axis]);
        r.camera.distance = Math.hypot(...offset);
        r.dirty = true;
        r.render(true);
        gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
        gl.readPixels(
          Math.floor(r.canvas.width / 2) - Math.floor(sampleSize / 2),
          Math.floor(r.canvas.height / 2) - Math.floor(sampleSize / 2),
          sampleSize,
          sampleSize,
          gl.RGBA,
          gl.UNSIGNED_BYTE,
          pixels,
        );
        let red = 0;
        for (let at = 0; at < pixels.length; at += 4) {
          if (pixels[at] > pixels[at + 1] && pixels[at] > pixels[at + 2]) red += 1;
        }
        samples.push(red / (sampleSize * sampleSize));
      }
      return samples;
    };

    const reversed = sampleOrbit();
    r.releaseRenderTarget();
    r.offscreen = false;
    r.reversedDepth = false;
    r.applyDepthConvention();
    const fallback = sampleOrbit();
    const rect = r.canvas.getBoundingClientRect();
    const ray = r.pointerRay(rect.left + rect.width / 2, rect.top + rect.height / 2);
    const picked = r.pickRecord(ray)?.record ?? null;
    r.select([0]);
    const pickedWithHiddenSelection = r.pickRecord(ray)?.record ?? null;
    r.select(null);
    const sharing = {
      resources: r.sharedGeometryBuffers.length,
      instanced: r.batches.filter((batch) => !batch.baked).length,
    };

    // Only local collisions need distinct priorities: 0 and 16 overlap, fillers are far apart.
    const aliasTransforms = [];
    const aliasColors = new Uint8Array(18 * 4);
    const aliasGeometryIds = new Uint32Array(18);
    let filler = 0;
    for (let record = 0; record < 18; record += 1) {
      const transform = identity.slice();
      if (record !== 0 && record !== 16) {
        const distance = 6 + Math.floor(filler / 2) * 5;
        transform[12] = filler % 2 ? -distance : distance;
        filler += 1;
      }
      aliasTransforms.push(...transform);
      const red = record === 0 ? 10 : record === 16 ? 230 : record === 17 ? 240 : 20 + record * 10;
      const green = record === 16 ? 50 : 80 + record;
      const blue = record === 0 ? 220 : record === 16 ? 40 : 120 + record;
      aliasColors.set([red, green, blue, 255], record * 4);
      aliasGeometryIds[record] = record === 16 ? 1 : 0;
    }
    const aliasPack = {
      geometry: pack.geometry,
      instances: {
        count: 18,
        geometryIds: aliasGeometryIds,
        expressIds: Uint32Array.from({ length: 18 }, (_, record) => record + 1),
        classIds: new Uint16Array(18),
        transforms: Float32Array.from(aliasTransforms),
        colors: aliasColors,
        flags: new Uint16Array(18),
      },
    };
    r.releaseRenderTarget();
    r.offscreen = true;
    r.reversedDepth = true;
    r.applyDepthConvention();
    const aliasReversed = sampleOrbit(aliasPack);
    const aliasPriorities = [r.depthRanks[0], r.depthRanks[16]];
    const aliasConflicts = {
      pairs: r.depthConflictPairs,
      contested: r.contestedMaterials,
    };
    r.releaseRenderTarget();
    r.offscreen = false;
    r.reversedDepth = false;
    r.applyDepthConvention();
    const aliasFallback = sampleOrbit(aliasPack);

    // The overlay step is rank-independent: the lowest and highest u32 ranks get
    // the same offset. Forced into the contested cache, then pixel and pick checked.
    const rawHighRank = 0xffff_ffff;
    const sampleOverlayGap = (gap, camera, up) => {
      const behind = identity.slice();
      behind[14] = -gap;
      r.load({
        geometry: [
          {
            id: 0,
            bbox: [-2, -2, 0, 2, 2, 0],
            positions,
            indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
          },
          {
            id: 1,
            bbox: [-2, -2, 0, 2, 2, 0],
            positions,
            indices: Uint16Array.from([0, 1, 3, 1, 2, 3]),
          },
        ],
        instances: {
          count: 2,
          geometryIds: Uint32Array.from([0, 1]),
          expressIds: Uint32Array.from([1, 2]),
          classIds: Uint16Array.from([0, 0]),
          transforms: Float32Array.from([...identity, ...behind]),
          colors: Uint8Array.from([
            55, 95, 220, 255,
            230, 55, 45, 255,
          ]),
          flags: Uint16Array.from([0, 0]),
        },
      });
      r.depthRanks[0] = 0;
      r.depthRanks[1] = rawHighRank;
      r.depthContested[0] = 1;
      r.depthContested[1] = 1;
      for (const batch of r.opaqueBatches) {
        batch.depthRank = batch.sourceRecord === 1 ? rawHighRank : 0;
        batch.depthContested = true;
      }
      r.contestedOpaqueBatches = r.opaqueBatches.slice().sort(
        (left, right) => left.depthRank - right.depthRank || left.sourceRecord - right.sourceRecord,
      );
      const center = r.recordLocations[0].bounds.center;
      r.camera.target = center.slice();
      r.camera.position = center.map((value, axis) => value + camera[axis]);
      r.camera.up = up;
      r.camera.distance = Math.hypot(...camera);
      r.dirty = true;
      r.render(true);
      const pixel = new Uint8Array(4);
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
      gl.readPixels(
        Math.floor(r.canvas.width / 2),
        Math.floor(r.canvas.height / 2),
        1,
        1,
        gl.RGBA,
        gl.UNSIGNED_BYTE,
        pixel,
      );
      const visual = pixel[0] > pixel[2] ? 1 : 0;
      const rect = r.canvas.getBoundingClientRect();
      const ray = r.pointerRay(rect.left + rect.width / 2, rect.top + rect.height / 2);
      return {
        gap,
        visual,
        picked: r.pickRecord(ray)?.record ?? null,
        ranks: [r.depthRanks[0], r.depthRanks[1]],
      };
    };
    const sampleOverlayGaps = () => {
      const gaps = [0, 0.000005, 0.001, 0.002, 0.005, 0.01];
      return {
        grazing: gaps.map((gap) => sampleOverlayGap(gap, [0, -5, 0.2], [0, 0, 1])),
        face: gaps.map((gap) => sampleOverlayGap(gap, [0, 0, 5], [0, 1, 0])),
      };
    };

    r.releaseRenderTarget();
    r.offscreen = true;
    r.reversedDepth = true;
    r.applyDepthConvention();
    const reversedGaps = sampleOverlayGaps();
    r.releaseRenderTarget();
    r.offscreen = false;
    r.reversedDepth = false;
    r.applyDepthConvention();
    const fallbackGaps = sampleOverlayGaps();

    // At infrastructure scale one f32 ULP exceeds the tie envelope: strict depth wins.
    const largePositions = Float32Array.from([
      -200, -200, 0,
       200, -200, 0,
       200,  200, 0,
      -200,  200, 0,
    ]);
    const largeBehind = identity.slice();
    largeBehind[14] = -0.001;
    const largeAuxiliary = identity.slice();
    largeAuxiliary[12] = 20_000;
    const largeAuxiliaryBehind = largeAuxiliary.slice();
    largeAuxiliaryBehind[14] = -0.001;
    r.releaseRenderTarget();
    r.offscreen = true;
    r.reversedDepth = true;
    r.applyDepthConvention();
    r.depthTieBreak = true;
    const largeLoad = r.load({
      geometry: [
        {
          id: 0,
          bbox: [-200, -200, -0.001, 200, 200, 0.001],
          positions: largePositions,
          indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
        },
        {
          id: 1,
          bbox: [-200, -200, -0.001, 200, 200, 0.001],
          positions: largePositions,
          indices: Uint16Array.from([0, 1, 3, 1, 2, 3]),
        },
      ],
      instances: {
        count: 4,
        geometryIds: Uint32Array.from([0, 1, 0, 1]),
        expressIds: Uint32Array.from([1, 2, 3, 4]),
        classIds: new Uint16Array(4),
        transforms: Float32Array.from([
          ...identity,
          ...largeBehind,
          ...largeAuxiliary,
          ...largeAuxiliaryBehind,
        ]),
        colors: Uint8Array.from([
          10, 80, 220, 255,
          230, 50, 40, 255,
          10, 80, 220, 255,
          230, 50, 40, 255,
        ]),
        flags: new Uint16Array(4),
      },
    });
    const largeCenter = r.recordLocations[0].bounds.center;
    const largeCamera = [0, -500, 20];
    r.camera.target = largeCenter.slice();
    r.camera.position = largeCenter.map((value, axis) => value + largeCamera[axis]);
    r.camera.up = [0, 0, 1];
    r.camera.distance = Math.hypot(...largeCamera);
    r.dirty = true;
    r.render(true);
    const largePixel = new Uint8Array(4);
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
    gl.readPixels(
      Math.floor(r.canvas.width / 2),
      Math.floor(r.canvas.height / 2),
      1,
      1,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      largePixel,
    );
    const largeRect = r.canvas.getBoundingClientRect();
    const largeRay = r.pointerRay(
      largeRect.left + largeRect.width / 2,
      largeRect.top + largeRect.height / 2,
    );
    const largePrecision = {
      visual: largePixel[2] > largePixel[0] ? 0 : 1,
      picked: r.pickRecord(largeRay)?.record ?? null,
      safe: r.depthOverlayPrecisionSafe,
      active: r.displayInfo().depthOverlayActive,
      overlayDrawCalls: largeLoad.overlayDrawCalls,
      contestedBatches: r.contestedOpaqueBatches.length,
      contestedMaterials: largeLoad.contestedMaterials,
    };

    // Transparent products stay sortable; a shared upload is worth it only for repeats.
    const below = identity.slice();
    below[14] = -1;
    const above = identity.slice();
    above[14] = 1;
    r.load({
      geometry: [
        {
          id: 0,
          bbox: [-2, -2, 0, 2, 2, 0],
          positions,
          indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
        },
        {
          id: 1,
          bbox: [-2, -2, 0, 2, 2, 0],
          positions,
          indices: Uint16Array.from([0, 1, 3, 1, 2, 3]),
        },
      ],
      instances: {
        count: 3,
        geometryIds: Uint32Array.from([0, 0, 1]),
        expressIds: Uint32Array.from([1, 2, 3]),
        classIds: Uint16Array.from([0, 0, 0]),
        transforms: Float32Array.from([...below, ...above, ...identity]),
        colors: Uint8Array.from([
          80, 150, 190, 128,
          80, 150, 190, 128,
          80, 150, 190, 128,
        ]),
        flags: Uint16Array.from([0, 0, 0]),
      },
    });
    r.style = "solid";
    const transparent = r.transparentBatches;
    const repeated = transparent.filter((batch) => !batch.baked);
    const unique = transparent.filter((batch) => batch.baked);
    r.camera.position = [0, 0, 10];
    r.camera.target = [0, 0, 0];
    const frontOrder = r.orderedBatches(false).map((batch) => batch.sourceRecord);
    r.camera.position = [0, 0, -10];
    const backOrder = r.orderedBatches(false).map((batch) => batch.sourceRecord);
    const transparentSharing = {
      resources: r.sharedGeometryBuffers.length,
      instanced: repeated.length,
      baked: unique.length,
      sourceOrder: transparent.map((batch) => batch.sourceRecord),
      frontOrder,
      backOrder,
      shared:
        repeated.length === 2 &&
        repeated[0].sharedGeometry === repeated[1].sharedGeometry &&
        repeated[0].indexBuffer === repeated[1].indexBuffer,
      independent: repeated.every((batch) => batch.instanceCount === 1),
      leanUnique:
        unique.length === 1 &&
        unique[0].sourceRecord === 2 &&
        unique[0].buffers.length === 3 &&
        !unique[0].sharedGeometry,
    };
    return {
      reversed,
      fallback,
      picked,
      pickedWithHiddenSelection,
      sharing,
      aliasReversed,
      aliasFallback,
      aliasPriorities,
      aliasConflicts,
      reversedGaps,
      fallbackGaps,
      largePrecision,
      transparentSharing,
    };
  });
  check(
    temporal.reversed.every((redShare) => redShare > 0.9),
    "coplanar mixed-colour instances keep one deterministic winner through a reversed-depth orbit" +
      ` (${JSON.stringify(temporal.reversed)})`,
  );
  check(
    temporal.fallback.every((redShare) => redShare > 0.9),
    "the same coplanar orbit remains stable after fixed-depth fallback" +
      ` (${JSON.stringify(temporal.fallback)})`,
  );
  check(
    temporal.aliasReversed.every((redShare) => redShare > 0.9) &&
      temporal.aliasFallback.every((redShare) => redShare > 0.9),
    "sorted materials 0 and 16 receive distinct local winners in both depth conventions" +
      ` (${JSON.stringify(temporal.aliasReversed)}, ` +
      `${JSON.stringify(temporal.aliasFallback)})`,
  );
  check(
    temporal.aliasPriorities[0] !== temporal.aliasPriorities[1] &&
      temporal.aliasConflicts.pairs === 1 &&
      temporal.aliasConflicts.contested === 2,
    "eighteen global materials keep raw ranks around one contested material pair" +
      ` (${JSON.stringify({ priorities: temporal.aliasPriorities, ...temporal.aliasConflicts })})`,
  );
  check(temporal.picked === 1, `clicking the visible coplanar winner selects record 1 (${temporal.picked})`);
  check(
    temporal.pickedWithHiddenSelection === 1,
    `an already selected hidden coplanar record cannot override the visible winner ` +
      `(${temporal.pickedWithHiddenSelection})`,
  );
  // Tiny reused meshes bake into colour batches; instancing starts at INSTANCE_MIN_VERTICES.
  check(
    temporal.sharing.resources === 0 && temporal.sharing.instanced === 0,
    `tiny reused meshes bake into colour batches instead of instancing (${JSON.stringify(temporal.sharing)})`,
  );
  const overlayGapsPass = (profiles) => Object.values(profiles).every((samples) =>
    samples.every((sample) => {
      const expected = sample.gap === 0 ? 1 : sample.gap >= 0.001 ? 0 : null;
      return (
        (expected === null || sample.visual === expected && sample.picked === expected) &&
        sample.ranks[0] === 0 &&
        sample.ranks[1] === 0xffff_ffff
      );
    }));
  check(
    overlayGapsPass(temporal.reversedGaps),
    `the reversed overlay resolves exact raw-rank ties and preserves >=1 mm gaps ` +
      `(${JSON.stringify(temporal.reversedGaps)})`,
  );
  check(
    overlayGapsPass(temporal.fallbackGaps),
    `the fixed-depth overlay resolves exact raw-rank ties and preserves >=1 mm gaps ` +
      `(${JSON.stringify(temporal.fallbackGaps)})`,
  );
  check(
    temporal.largePrecision.visual === 0 &&
      temporal.largePrecision.picked === 0 &&
      temporal.largePrecision.safe === false &&
      temporal.largePrecision.active === false &&
      temporal.largePrecision.overlayDrawCalls === 0 &&
      temporal.largePrecision.contestedBatches > 0 &&
      temporal.largePrecision.contestedMaterials === 2,
    `the 100x precision gate keeps the canonical front for rendering and picking ` +
      `(${JSON.stringify(temporal.largePrecision)})`,
  );
  check(
    temporal.transparentSharing.resources === 1 &&
      temporal.transparentSharing.instanced === 2 &&
      temporal.transparentSharing.baked === 1 &&
      temporal.transparentSharing.shared &&
      temporal.transparentSharing.independent &&
      temporal.transparentSharing.leanUnique,
    `transparent reuse shares one immutable upload while the singleton stays baked ` +
      `(${JSON.stringify(temporal.transparentSharing)})`,
  );
  check(
    JSON.stringify(temporal.transparentSharing.sourceOrder) === JSON.stringify([0, 1, 2]) &&
      JSON.stringify(temporal.transparentSharing.frontOrder) === JSON.stringify([0, 2, 1]) &&
      JSON.stringify(temporal.transparentSharing.backOrder) === JSON.stringify([1, 2, 0]),
    `transparent submissions retain deterministic ties and reorder independently with the camera ` +
      `(${JSON.stringify(temporal.transparentSharing)})`,
  );

  // A section through a closed solid is filled in; an open shell is not, because
  // it has no inside to fill.
  const capping = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    // Earlier blocks leave the scene isolated and reloaded; start from the
    // whole model again so there is something to cap.
    const camera = JSON.parse(JSON.stringify(r.camera));
    r.setVisibility(() => true);
    r.load(window.__tessifc.pack());
    r.camera = camera;
    r.dirty = true;
    r.render(true);
    const info = r.displayInfo();
    const closedBatches = r.opaqueBatches.filter((batch) => batch.closed).length;
    const axis = "z";
    const cut = r.sectionValue(axis, 0.5);
    r.setSection(true, axis, cut, false, false);
    r.dirty = true;
    r.render(true);
    r.setSection(true, axis, cut, false, true);
    r.dirty = true;
    r.render(true);
    const capped = r.displayInfo().cappingActive;
    r.setSection(false, axis, cut);
    r.camera = camera;
    r.dirty = true;
    r.render(true);
    return {
      stencilBits: info.stencilBits,
      closedBatches,
      totalBatches: r.opaqueBatches.length,
      capped,
    };
  });
  check(
    capping.stencilBits >= 8,
    `the render target carries a stencil (${capping.stencilBits} bits)`,
  );
  check(
    capping.closedBatches > 0 && capping.closedBatches <= capping.totalBatches,
    `closed solids are identified for capping (${capping.closedBatches} of ${capping.totalBatches} batches)`,
  );
  check(capping.capped === true, "the cap pass runs when a section is on and closed solids exist");

  check(problems.length === 0, `no WebGL or page errors${problems.length ? `: ${problems[0]}` : ""}`);
} finally {
  await browser.close();
  server.close();
}

console.log(failed ? "FAIL" : "PASS");
process.exit(failed ? 1 : 0);

function countDifferences(first, second) {
  const a = decodePng(first);
  const b = decodePng(second);
  assert.equal(a.width, b.width);
  const total = a.width * a.height;
  let count = 0;
  for (let i = 0; i < total; i += 1) {
    const at = i * a.channels;
    const to = i * b.channels;
    const delta =
      Math.abs(a.data[at] - b.data[to]) +
      Math.abs(a.data[at + 1] - b.data[to + 1]) +
      Math.abs(a.data[at + 2] - b.data[to + 2]);
    if (delta > 24) count += 1;
  }
  return { count, total, ratio: count / total };
}

// A correct tie-break only changes pixels along an existing material edge.
function profileDifferences(unbiased, settled) {
  const a = decodePng(unbiased);
  const b = decodePng(settled);
  assert.equal(a.width, b.width);
  assert.equal(a.height, b.height);
  const { width, height } = a;
  const total = width * height;
  const changed = new Uint8Array(total);
  const edges = new Uint8Array(total);
  let count = 0;
  const colorDelta = (left, right) => {
    const at = left * a.channels;
    const to = right * a.channels;
    return (
      Math.abs(a.data[at] - a.data[to]) +
      Math.abs(a.data[at + 1] - a.data[to + 1]) +
      Math.abs(a.data[at + 2] - a.data[to + 2])
    );
  };
  for (let pixel = 0; pixel < total; pixel += 1) {
    const at = pixel * a.channels;
    const to = pixel * b.channels;
    const delta =
      Math.abs(a.data[at] - b.data[to]) +
      Math.abs(a.data[at + 1] - b.data[to + 1]) +
      Math.abs(a.data[at + 2] - b.data[to + 2]);
    if (delta > 24) {
      changed[pixel] = 1;
      count += 1;
    }
    const x = pixel % width;
    const y = Math.floor(pixel / width);
    if (
      (x > 0 && colorDelta(pixel, pixel - 1) > 24) ||
      (x + 1 < width && colorDelta(pixel, pixel + 1) > 24) ||
      (y > 0 && colorDelta(pixel, pixel - width) > 24) ||
      (y + 1 < height && colorDelta(pixel, pixel + width) > 24)
    ) edges[pixel] = 1;
  }

  let edgeLocal = 0;
  for (let pixel = 0; pixel < total; pixel += 1) {
    if (!changed[pixel]) continue;
    const x = pixel % width;
    const y = Math.floor(pixel / width);
    let near = false;
    for (let dy = -1; dy <= 1 && !near; dy += 1) {
      for (let dx = -1; dx <= 1 && !near; dx += 1) {
        const nx = x + dx;
        const ny = y + dy;
        if (nx >= 0 && nx < width && ny >= 0 && ny < height && edges[ny * width + nx]) {
          near = true;
        }
      }
    }
    if (near) edgeLocal += 1;
  }

  const seen = new Uint8Array(total);
  const queue = new Int32Array(total);
  let largestComponent = 0;
  for (let start = 0; start < total; start += 1) {
    if (!changed[start] || seen[start]) continue;
    let head = 0;
    let tail = 1;
    let size = 0;
    queue[0] = start;
    seen[start] = 1;
    while (head < tail) {
      const pixel = queue[head];
      head += 1;
      size += 1;
      const x = pixel % width;
      const y = Math.floor(pixel / width);
      const neighbors = [
        x > 0 ? pixel - 1 : -1,
        x + 1 < width ? pixel + 1 : -1,
        y > 0 ? pixel - width : -1,
        y + 1 < height ? pixel + width : -1,
      ];
      for (const next of neighbors) {
        if (next < 0 || seen[next] || !changed[next]) continue;
        seen[next] = 1;
        queue[tail] = next;
        tail += 1;
      }
    }
    largestComponent = Math.max(largestComponent, size);
  }

  return {
    count,
    total,
    ratio: count / total,
    edgeLocal,
    edgeRatio: count ? edgeLocal / count : 1,
    offEdge: count - edgeLocal,
    largestComponent,
    largestComponentRatio: largestComponent / total,
  };
}

// A PNG reader for what Chrome writes: eight bits a channel, no interlace.
function decodePng(buffer) {
  let position = 8;
  let width = 0;
  let height = 0;
  let colour = 0;
  const parts = [];
  while (position < buffer.length) {
    const length = buffer.readUInt32BE(position);
    const type = buffer.toString("ascii", position + 4, position + 8);
    const data = buffer.subarray(position + 8, position + 8 + length);
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      colour = data[9];
    } else if (type === "IDAT") parts.push(data);
    else if (type === "IEND") break;
    position += 12 + length;
  }
  const channels = colour === 6 ? 4 : 3;
  const raw = zlib.inflateSync(Buffer.concat(parts));
  const stride = width * channels;
  const out = Buffer.alloc(height * stride);
  let previous = Buffer.alloc(stride);
  for (let y = 0; y < height; y += 1) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, y * (stride + 1) + 1 + stride);
    const current = Buffer.alloc(stride);
    for (let i = 0; i < stride; i += 1) {
      const a = i >= channels ? current[i - channels] : 0;
      const b = previous[i];
      const c = i >= channels ? previous[i - channels] : 0;
      let value = line[i];
      if (filter === 1) value += a;
      else if (filter === 2) value += b;
      else if (filter === 3) value += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        value += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      current[i] = value & 0xff;
    }
    current.copy(out, y * stride);
    previous = current;
  }
  return { width, height, channels, data: out };
}
