// SPDX-License-Identifier: Apache-2.0

import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  DEFAULT_HIDDEN_INSTANCE_FLAGS,
  INSTANCE_OPENING,
  INSTANCE_REFERENCE,
  INSTANCE_SPACE,
  classLabelColor,
  defaultHiddenClassIds,
  humanizeIfcClass,
  readIgp,
} from "../src/igp.js";
import {
  DEPTH_OVERLAY_DISTANCE_TOLERANCE,
  DEPTH_SLOPE_FACTOR_STEP,
  PICK_DEPTH_TIE_TOLERANCE,
  cameraDepthRange,
  DEPTH_CLAMP_STEPS_PER_OCTAVE,
  FIXED_DEPTH_MINIMUM_UNITS,
  MAX_DEPTH_PAIR_TESTS,
  fixedDepthClampFloor,
  depthOverlayClamp,
  depthOverlayFallbackUnits,
  quantiseDepthClamp,
  translucentDepthOffset,
  depthOverlayOffset,
  depthOverlayPrecisionSupported,
  filterCoplanarPatchTriangles,
  filterCoincidentTriangles,
  pickDepthTieTolerance,
  planDepthMaterials,
  planDepthRanks,
  planRenderBatches,
  preferDepthHit,
  renderPixelRatio,
  renderTargetPlan,
  restrictDepthPlan,
} from "../src/renderer.js";

import {
  SNAP_PIXELS,
  measurementBetween,
  measurementDetail,
  measurementLabel,
  measurementText,
  projectPoint,
  snapToTriangle,
} from "../src/measure.js";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..", "..");
const wasmGlue = resolve(repo, "bindings", "wasm", "pkg-node", "tessifc_wasm.js");
// A wall and a slab, the smallest file that still produces two solids.
const FRAGMENT = [
  "ISO-10303-21;",
  "HEADER;",
  "FILE_SCHEMA(('IFC4'));",
  "ENDSEC;",
  "DATA;",
  "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,0.3);",
  "#2=IFCDIRECTION((0.,0.,1.));",
  "#3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.5);",
  "#4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));",
  "#5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));",
  "#6=IFCWALL('1wall',$,'Wall A',$,$,$,#5,$,$);",
  "#11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,4.);",
  "#12=IFCEXTRUDEDAREASOLID(#11,$,#2,0.2);",
  "#13=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#12));",
  "#14=IFCPRODUCTDEFINITIONSHAPE($,$,(#13));",
  "#15=IFCSLAB('1slab',$,'Slab A',$,$,$,#14,$,$);",
  "ENDSEC;",
  "END-ISO-10303-21;",
].join(String.fromCharCode(10));
// Set TESSIFC_TEST_MODEL to run the integration check over a whole file instead.
const requestedModel = process.env.TESSIFC_TEST_MODEL
  ? resolve(repo, process.env.TESSIFC_TEST_MODEL)
  : null;
const haveModel =
  Boolean(requestedModel) && existsSync(requestedModel) && statSync(requestedModel).isFile();

// No published test may name a directory a public checkout does not have.
const untrackedSegment = String.fromCharCode(34, 100, 101, 118, 34);
for (const suite of [
  "viewer/test/igp.test.mjs",
  "viewer/test/render.test.mjs",
  "bindings/wasm/test/smoke.mjs",
  "adapters/three/test/build.test.mjs",
]) {
  const text = readFileSync(resolve(repo, suite), "utf8");
  assert.ok(!text.includes(untrackedSegment), `${suite} must not build a path into an untracked tree`);
  assert.ok(text.includes("TESSIFC_TEST_MODEL"), `${suite} must take its model from TESSIFC_TEST_MODEL`);
}
console.log("ok    every published test suite takes its model from the environment");
const html = readFileSync(resolve(repo, "viewer", "index.html"), "utf8");
// Source guards read the app modules as one blob, wherever a rule's code lives.
const SEPARATOR = String.fromCharCode(10);
const appModules = ["main.js", "shell.js", "tree.js", "inspector.js", "tools.js", "format.js", "boot.js", "stream.js", "measure.js"];
const app = appModules.map((name) => readFileSync(resolve(repo, "viewer", "src", name), "utf8")).join(SEPARATOR);
const renderer = readFileSync(resolve(repo, "viewer", "src", "renderer.js"), "utf8");
const worker = readFileSync(resolve(repo, "viewer", "src", "worker.js"), "utf8");
const packageJson = JSON.parse(readFileSync(resolve(repo, "viewer", "package.json"), "utf8"));

const htmlIds = [...html.matchAll(/\sid="([^"]+)"/g)].map((match) => match[1]);
assert.equal(new Set(htmlIds).size, htmlIds.length, "HTML ids must be unique");
for (const [, id] of app.matchAll(/\$\("([^"]+)"\)/g)) {
  assert.ok(htmlIds.includes(id), `viewer/src expects #${id} in index.html`);
}
console.log("ok    viewer controls resolve to unique HTML elements");

for (const tab of ["file", "home", "view", "analyze", "review"]) {
  assert.match(html, new RegExp(`data-tab="${tab}"`), `ribbon must expose the ${tab} task`);
}
for (const view of ["spatial", "types"]) {
  assert.match(html, new RegExp(`data-outliner="${view}"`), `outliner must expose the ${view} view`);
}
for (const panel of ["properties", "element", "model", "quality"]) {
  assert.match(html, new RegExp(`data-panel="${panel}"`), `inspector must expose the ${panel} view`);
}
assert.match(html, /<aside id="editor"/, "attribute editing must have its own side panel");
assert.match(html, /id="rail-editor"[^>]*aria-pressed=/, "the rail must toggle the edit panel");
assert.match(html, /id="dock-selection"/, "the viewport must carry a selection dock");
for (const command of ["spaces", "openings", "references"]) {
  assert.match(html, new RegExp(`id="cmd-${command}"[^>]*aria-pressed=`), `the top bar must toggle ${command}`);
}
assert.doesNotMatch(html, /id="selection-(?:focus|isolate|hide|position|size|geometry)"/, "the properties card carries identity only; facts live in the element tab");
assert.match(app, /renderer\.setViewportTheme\?\./, "canvas theme control must use the first-party renderer");
assert.match(app, /function filterProperties\(/, "property filtering must be implemented locally");
console.log("ok    task ribbon, model navigator, and inspector views are wired");

// A URL to a real host is a network dependency; the XML namespace in an inline
// SVG is an identifier, not a fetch.
const remoteUrl = /https?:\/\/(?!www\.w3\.org\/)/;
for (const [name, source] of Object.entries({ html, app, renderer, worker })) {
  assert.doesNotMatch(source, remoteUrl, `${name} must not fetch runtime code`);
  assert.doesNotMatch(source, /(?:from\s+["']three|THREE\.)/, `${name} must use the first-party renderer`);
}
assert.deepEqual(packageJson.dependencies ?? {}, {}, "viewer must have no npm runtime dependencies");
assert.doesNotMatch(html, /<script[^>]+src=["']https?:/, "viewer scripts must be local");
console.log("ok    viewer is self-contained and has no runtime package or CDN dependency");

assert.match(renderer, /gl\.depthFunc\(gl\.LESS\)/, "equal-depth faces must not overwrite each other");
assert.match(renderer, /uCameraPosition - vWorld/, "two-sided face normals must follow geometry, not IFC winding");
assert.doesNotMatch(renderer, /gl_FrontFacing/, "mixed IFC winding must not alternate the shaded normal");
assert.match(renderer, /centerDepth - padding/, "camera clipping must stay tight around the model");
assert.match(renderer, /matrix\[12\] - this\.renderOrigin\[0\]/, "GPU instance transforms must be origin rebased");
assert.match(renderer, /polar[\s\S]* - dy \* 0\.006/, "vertical orbit must match OrbitControls direction");
assert.match(renderer, /this\.interactionScale = 1;/, "orbiting must keep the stable full-resolution sample grid");
assert.doesNotMatch(app, /setInteractionScale\(/, "the viewer must not silently override full-resolution orbiting");
assert.match(renderer, /pickRecord\(ray\)/, "selection must use the CPU broad phase");
assert.match(renderer, /rayBounds\(/, "selection must reject records outside the pointer ray");
assert.doesNotMatch(renderer, /readPixels/, "selection must not synchronously stall on a GPU readback");
assert.match(
  renderer,
  /this\.visibility\[record \* 2\] < 128/,
  "CPU picking must reject hidden records",
);
assert.match(
  renderer,
  /vSelected > 0\.5/,
  "the highlight must come from per-record state, so every part of one product lights up",
);
assert.match(renderer, /const clipped =\s*this\.section\.active/, "CPU picking must apply the live section plane");
assert.match(renderer, /this\.section\.world = value;/, "the section plane is stored in world coordinates, not against one render origin");
assert.match(renderer, /uSectionValue, this\.sectionCut\(\)/, "the GPU cut must follow the render origin of the current model");
assert.match(
  renderer,
  /addEventListener\("webglcontextlost"[\s\S]{0,200}?event\.preventDefault\(\)/,
  "a lost WebGL context must be recoverable, so the default must be prevented",
);
assert.match(
  renderer,
  /addEventListener\("webglcontextrestored"[\s\S]{0,120}?handleContextRestored\(\)/,
  "a restored WebGL context must be taken back and the model rebuilt",
);
assert.match(renderer, /render\(force = false\) \{\s*if \(this\.contextLost\) return;/, "no frame may be issued into a dead context");
assert.match(
  renderer,
  /const location = this\.recordLocations\[item\.record\];\s*if \(location\) includeBounds/,
  "a streamed record with no usable bounds must not break the batch it landed in",
);
assert.match(renderer, /const wire = new WireArray\(capacity\);/, "wire extraction must fill one bounded typed array, not grow a boxed one");
assert.match(
  renderer,
  /return this\.gpuBufferBytes \+ this\.renderTargetBytes\(\) \+ this\.canvasStorageBytes\(\)/,
  "GPU accounting must include buffers, the active target, and canvas storage",
);
assert.match(app, /syncGpuMemory\(\)/, "the model ledger must track lazy wire allocation");
assert.match(
  renderer,
  /for \(const source of batch\.wireSources\) \{[\s\S]*?const edges = [\s\S]*?createEdgeSet\([\s\S]*?const indices = source\.indices;/,
  "each baked source mesh must keep an independent wire-edge namespace",
);
console.log("ok    shaded rendering guards against coplanar and mixed-winding flicker");

const insideDepthRange = cameraDepthRange(
  { center: [0, 0, 0], radius: 50 },
  { mode: "perspective", position: [0, -20, 0], target: [0, 0, 0] },
);
assert.ok(
  insideDepthRange.far / insideDepthRange.near < 900,
  "a camera inside the model must retain useful fixed-depth precision",
);
const outsideDepthRange = cameraDepthRange(
  { center: [0, 0, 0], radius: 50 },
  { mode: "perspective", position: [0, -150, 0], target: [0, 0, 0] },
);
assert.equal(outsideDepthRange.near, 90, "an outside camera must keep the tight model bound");
assert.equal(outsideDepthRange.far, 210, "the far plane must still contain the model bound");
const closeDepthRange = cameraDepthRange(
  { center: [0, 0, 0], radius: 50 },
  { mode: "perspective", position: [0, -0.1, 0], target: [0, 0, 0] },
);
assert.equal(closeDepthRange.near, 0.001, "very-close inspection must not clip beyond one percent of camera distance");
const reversedInsideDepthRange = cameraDepthRange(
  { center: [0, 0, 0], radius: 50 },
  { mode: "perspective", position: [0, -20, 0], target: [0, 0, 0] },
  true,
);
assert.equal(
  reversedInsideDepthRange.near,
  0.001,
  "reversed float depth must not use the fixed-depth interior near-plane floor",
);
console.log("ok    perspective clipping stays stable inside and outside model bounds");

const desktopRatio = renderPixelRatio(1920, 1080, 2, 1.5, 4_000_000, [16_384, 16_384]);
assert.ok(desktopRatio < 1.5, "large high-DPI viewports must respect the render-pixel budget");
assert.ok(
  1920 * 1080 * desktopRatio ** 2 <= 4_000_000 + 1e-6,
  "drawing-buffer pixels must stay within the fill-rate budget",
);
assert.equal(renderPixelRatio(800, 600, 2, 1.5), 1.5, "normal viewports retain crisp 1.5x output");
assert.deepEqual(
  renderTargetPlan(2000, 2000, 4),
  { samples: 2, estimatedBytes: 64_000_000 },
  "MSAA must step down before a large target exceeds its sample budget",
);
assert.deepEqual(
  renderTargetPlan(640, 480, 4),
  { samples: 4, estimatedBytes: 9_830_400 },
  "compact viewports retain 4x MSAA",
);
console.log("ok    viewport pixels, samples, and target memory stay within bounded budgets");
assert.match(
  renderer,
  /new Set\(\[targetPlan\.samples, 2, 1\]\)/,
  "render targets must retry 2x and 1x samples before default-buffer fallback",
);
assert.match(
  renderer,
  /this\.targetFailures\.add\(this\.target\.key\);[\s\S]*?return this\.render\(true\);/,
  "a failed blit must retry the remaining target ladder in the same frame",
);
assert.match(
  renderer,
  /this\.resizeDirty \|\| this\.resizeSettleTimer && force \|\| this\.devicePixelRatio !==/,
  "DPR and explicit quality changes must invalidate the cached canvas size, and a forced frame must settle a pending resize",
);
assert.match(
  renderer,
  /if \(!settleNow && now - this\.backingChangedAt < RESIZE_BURST_MS\) \{[\s\S]*?setTimeout\([\s\S]*?this\.resize\(true\);[\s\S]*?this\.onDirty\?\.\(\);/,
  "a run of canvas size changes must reallocate the backing store once it settles, not once per frame",
);
assert.match(
  renderer,
  /!this\.interactionPrepared && !this\.interactionPrepareFrame && !this\.interactionPrepareTimer\) \{[\s\S]*?requestAnimationFrame\([\s\S]*?setTimeout\([\s\S]*?this\.prepareInteractionTarget\(false\);/,
  "the gesture target is prepared after the frame that follows a resize has been committed, not inside it",
);
assert.doesNotMatch(
  renderer,
  /this\.offscreen = false/,
  "a large-target failure must remain recoverable after the viewport shrinks",
);
console.log("ok    target and resize fallbacks recover without steady-frame DOM work");

const rankedColors = Uint8Array.from([
  0, 0, 255, 255,
  255, 0, 0, 255,
  0, 0, 255, 255,
  0, 255, 0, 255,
]);
const depthPlan = planDepthRanks(rankedColors, 4);
assert.deepEqual(
  [...depthPlan.ranks],
  [0, 2, 0, 1],
  "colour ranks must follow sorted RGBA keys rather than first-seen batch order",
);
assert.equal(depthPlan.colors, 3);
assert.ok(depthPlan.ranks instanceof Uint32Array, "material ranks must not wrap in large palettes");
const manyColors = new Uint8Array(300 * 4);
for (let record = 0; record < 300; record += 1) {
  manyColors[record * 4] = record >>> 8;
  manyColors[record * 4 + 1] = record & 255;
  manyColors[record * 4 + 3] = 255;
}
const manyRanks = planDepthRanks(manyColors, 300).ranks;
assert.equal(new Set(manyRanks).size, 300, "more than 255 display colours retain distinct source ranks");

const rgbaKey = (colors, record) => Array.from(colors.subarray(record * 4, record * 4 + 4)).join(",");
const materialState = (colors, plan) => new Map(
  Array.from(
    { length: plan.ranks.length },
    (_, record) => [
      rgbaKey(colors, record),
      { rank: plan.ranks[record], contested: plan.contested[record] },
    ],
  ),
);
const makeDepthFixture = (count, boundsForRecord) => {
  const colors = new Uint8Array(count * 4);
  const bounds = [];
  for (let record = 0; record < count; record += 1) {
    colors.set([record, record * 37 & 255, record * 73 & 255, 255], record * 4);
    bounds.push(boundsForRecord(record));
  }
  return { colors, bounds };
};
const pairFixture = makeDepthFixture(34, (record) => {
  const x = Math.floor(record / 2) * 3;
  return { min: [x, 0, 0], max: [x + 1, 1, 0] };
});
const pairPlan = planDepthMaterials(pairFixture.colors, pairFixture.bounds);
assert.equal(pairPlan.conflictPairs, 17, "separated overlap pairs form only local material conflicts");
assert.equal(pairPlan.contestedColors, 34, "both materials in every overlap pair are contested");
assert.equal(new Set(pairPlan.ranks).size, 34, "raw material ranks never wrap through a slot count");
assert.ok(pairPlan.contested.every(Boolean), "every material participating in a pair gets overlaid");

const reversedRecords = Array.from({ length: 34 }, (_, record) => 33 - record);
const reorderedColors = new Uint8Array(pairFixture.colors.length);
const reorderedBounds = [];
for (let record = 0; record < reversedRecords.length; record += 1) {
  const source = reversedRecords[record];
  reorderedColors.set(pairFixture.colors.subarray(source * 4, source * 4 + 4), record * 4);
  reorderedBounds.push(pairFixture.bounds[source]);
}
const reorderedPlan = planDepthMaterials(reorderedColors, reorderedBounds);
assert.deepEqual(
  [...materialState(pairFixture.colors, pairPlan)].sort(),
  [...materialState(reorderedColors, reorderedPlan)].sort(),
  "raw ranks and contested flags must be independent of record and draw order",
);

const clique = makeDepthFixture(
  40,
  () => ({ min: [0, 0, 0], max: [1, 1, 0] }),
);
const cliquePlan = planDepthMaterials(clique.colors, clique.bounds);
assert.equal(cliquePlan.conflictPairs, 40 * 39 / 2, "a dense overlap reports every material pair");
assert.equal(new Set(cliquePlan.ranks).size, 40, "N-way ties retain a full deterministic ordering");
assert.ok(cliquePlan.contested.every(Boolean), "N-way ties have no graph-colouring capacity limit");

const sameColorPlan = planDepthMaterials(
  Uint8Array.from([10, 20, 30, 255, 10, 20, 30, 255]),
  [
    { min: [0, 0, 0], max: [1, 1, 0] },
    { min: [0, 0, 0], max: [1, 1, 0] },
  ],
);
assert.equal(sameColorPlan.conflictPairs, 0, "same-colour overlaps need no visible winner");
assert.deepEqual([...sameColorPlan.contested], [0, 0]);

const isolatedColors = Uint8Array.from([10, 20, 30, 255, 40, 50, 60, 128]);
const isolatedPlan = planDepthMaterials(
  isolatedColors,
  [
    { min: [0, 0, 0], max: [1, 1, 0] },
    { min: [0, 0, 0], max: [1, 1, 0] },
  ],
);
assert.deepEqual([...isolatedPlan.contested], [0, 0], "isolated opaque and transparent materials are not overlaid");
assert.equal(isolatedPlan.exhausted, false, "a small plan never reports an exhausted budget");

const helperCollisionPlan = planDepthMaterials(
  Uint8Array.from([10, 20, 30, 255, 40, 50, 60, 255]),
  [
    { min: [0, 0, 0], max: [1, 1, 1] },
    { min: [0, 0, 0], max: [1, 1, 1] },
  ],
  2,
  (record) => record === 0,
);
assert.equal(helperCollisionPlan.conflictPairs, 0, "hidden helpers do not enter collision planning");
assert.deepEqual([...helperCollisionPlan.contested], [0, 0]);

// Past the pair-test budget every opaque material is contested: draws, not flicker.
const stackCount = Math.ceil(Math.sqrt(MAX_DEPTH_PAIR_TESTS * 2)) + 8;
const stack = makeDepthFixture(stackCount, () => ({ min: [0, 0, 0], max: [1, 1, 0] }));
stack.colors.set([10, 20, 30, 128], 0);
const stackPlan = planDepthMaterials(stack.colors, stack.bounds);
assert.equal(stackPlan.exhausted, true, "a stack of every product exceeds the pair-test budget");
assert.equal(stackPlan.conflictPairs, 0, "an exhausted plan reports no pair it did not finish counting");
assert.equal(stackPlan.contested[0], 0, "an exhausted plan still leaves translucent materials alone");
assert.ok(
  stackPlan.contested.subarray(1).every(Boolean),
  "an exhausted plan contests every opaque material so coincident surfaces keep a stable winner",
);
assert.equal(stackPlan.contestedColors, stackPlan.opaqueColors, "every opaque colour is contested past the budget");
console.log("ok    contested materials keep full raw ranks without a slot or clique limit");

// A smaller visible set is planned from the load-time pairs without another sweep.
const pairedPlan = planDepthMaterials(pairFixture.colors, pairFixture.bounds, 34, () => true, { collectPairs: true });
assert.equal(pairedPlan.pairs.length, 34, "seventeen overlapping record pairs are kept when asked for");
const evenOnly = (record) => record % 2 === 0;
const restrictedEven = restrictDepthPlan(pairedPlan, pairFixture.colors, 34, evenOnly);
const replannedEven = planDepthMaterials(pairFixture.colors, pairFixture.bounds, 34, evenOnly);
assert.deepEqual([...restrictedEven.contested], [...replannedEven.contested], "a subset read off the pairs matches a full plan of that subset");
assert.equal(restrictedEven.conflictPairs, replannedEven.conflictPairs, "every pair lost a record, so nothing is contested");
assert.equal(restrictedEven.contestedColors, 0);
assert.deepEqual([...restrictedEven.ranks], [...pairedPlan.ranks], "ranks depend on the colours alone and are reused");
const onePairKept = (record) => record < 2 || record % 2 === 0;
const restrictedPair = restrictDepthPlan(pairedPlan, pairFixture.colors, 34, onePairKept);
const replannedPair = planDepthMaterials(pairFixture.colors, pairFixture.bounds, 34, onePairKept);
assert.deepEqual([...restrictedPair.contested], [...replannedPair.contested], "the one pair still visible keeps both its materials contested");
assert.equal(restrictedPair.conflictPairs, 1);
assert.equal(restrictedPair.contestedColors, 2);
assert.equal(restrictDepthPlan(pairPlan, pairFixture.colors, 34, () => true), null, "a plan without pairs cannot be restricted");
const exhaustedPairs = planDepthMaterials(stack.colors, stack.bounds, undefined, undefined, { collectPairs: true });
assert.equal(exhaustedPairs.pairs, null, "an exhausted plan keeps no pairs");
assert.equal(restrictDepthPlan(exhaustedPairs, stack.colors, stackCount, () => true), null, "an exhausted plan cannot be restricted");
assert.match(
  renderer,
  /depthPlanFor\(isVisible\) \{[\s\S]*?if \(!base\.visible\[record\] && isVisible\(record\)\) subset = false;[\s\S]*?restrictDepthPlan\(base\.plan, this\.renderColors, count, isVisible\)/,
  "a visibility change is planned from the pairs only while it shows a subset of the planned records",
);
console.log("ok    a smaller visible set is planned from the load-time pairs");

assert.equal(DEPTH_SLOPE_FACTOR_STEP, 1 / 32, "the overlay uses one measured slope-eligibility step");
assert.equal(DEPTH_OVERLAY_DISTANCE_TOLERANCE, 7.5e-4, "overlay eligibility stays below a real millimetre gap");
assert.deepEqual(
  depthOverlayOffset(true),
  { factor: 1 / 32, units: 0 },
  "reversed float depth gets one slope step without an implementation-sized unit shift",
);
assert.deepEqual(
  depthOverlayOffset(false),
  { factor: -1 / 32, units: -1 },
  "fixed depth gets one toward-camera slope and depth-unit step",
);
const perspectiveClamp = depthOverlayClamp(1, 101, true, DEPTH_OVERLAY_DISTANCE_TOLERANCE, 10);
assert.equal(
  perspectiveClamp,
  (1 * 101 / 100) * DEPTH_OVERLAY_DISTANCE_TOLERANCE /
    (10 * (10 - DEPTH_OVERLAY_DISTANCE_TOLERANCE)),
  "the perspective clamp projects the physical envelope at a batch's far depth",
);
assert.equal(
  depthOverlayClamp(1, 101, false, DEPTH_OVERLAY_DISTANCE_TOLERANCE, 10),
  DEPTH_OVERLAY_DISTANCE_TOLERANCE / 100,
  "the orthographic clamp maps the physical envelope linearly",
);
assert.equal(depthOverlayPrecisionSupported(1_000), true);
assert.equal(depthOverlayPrecisionSupported(7_000), false, "the overlay yields once f32 model space exceeds its envelope");
// One rasterizer state per distinct clamp; rounding to a grid bounds the count.
for (const value of [1e-9, 3.7e-7, 0.001, 0.42, 1]) {
  const quantised = quantiseDepthClamp(value);
  assert.ok(quantised <= value, `a quantised clamp never exceeds its envelope (${value})`);
  assert.ok(quantised >= value * 2 ** (-1 / DEPTH_CLAMP_STEPS_PER_OCTAVE), `a quantised clamp keeps most of its envelope (${value})`);
}
assert.equal(quantiseDepthClamp(0), 0, "no envelope quantises to no clamp");
assert.equal(quantiseDepthClamp(-0.5), quantiseDepthClamp(0.5), "the sign belongs to the depth convention, not the grid");
const octave = new Set();
for (let step = 0; step < 1000; step += 1) octave.add(quantiseDepthClamp(1 + step / 1000));
assert.equal(octave.size, DEPTH_CLAMP_STEPS_PER_OCTAVE, "one octave of clamps collapses to the grid's step count");
assert.match(
  renderer,
  /polygonOffsetClampEXT\([\s\S]*?direction \* fixedDepthClampFloor\(quantiseDepthClamp\(depthOverlayClamp\(/,
  "every clamp the overlay sets goes through the grid",
);
assert.deepEqual(translucentDepthOffset(true), { factor: 1 / 32, units: 2 }, "the opt-in translucent step adds two units on reversed depth");
assert.deepEqual(translucentDepthOffset(false), { factor: -1 / 32, units: -3 }, "the opt-in translucent step adds two units on fixed depth");
assert.match(renderer, /this\.translucentTieBreak = false;/, "the translucent step stays opt-in until it measures better than strict depth");
// At thirty metres a 24-bit unit is about the envelope, so a one-unit clamp
// cannot beat the buffer's rounding; two units are the fixed-depth floor.
assert.equal(FIXED_DEPTH_MINIMUM_UNITS, 2);
assert.equal(fixedDepthClampFloor(1e-9, 24), 2 * 2 ** -24, "a sub-unit envelope on 24-bit depth is floored at two units");
assert.equal(fixedDepthClampFloor(1e-3, 24), 1e-3, "an envelope above the floor is kept");
assert.equal(fixedDepthClampFloor(1e-9, 0), 1e-9, "float depth passes zero bits and keeps the exact envelope");
assert.equal(fixedDepthClampFloor(1e-9, 16), 2 * 2 ** -16, "a 16-bit canvas floors at its own coarser unit");
assert.match(
  renderer,
  /fixedDepthClampFloor\(quantiseDepthClamp\(depthOverlayClamp\([\s\S]*?this\.reversedDepth \? 0 : this\.overlayDepthBits\(\)/,
  "the floor applies only on the fixed-depth convention",
);
assert.equal(depthOverlayFallbackUnits(0, 24), FIXED_DEPTH_MINIMUM_UNITS, "without a clamp the fallback still takes the floor in whole depth units");
assert.equal(
  depthOverlayFallbackUnits(perspectiveClamp, 24),
  Math.max(FIXED_DEPTH_MINIMUM_UNITS, Math.floor(perspectiveClamp * 2 ** 24)),
  "the unclamped fallback spends the same envelope as whole units of the depth buffer",
);
assert.equal(depthOverlayFallbackUnits(1e-3, 16), Math.floor(1e-3 * 2 ** 16), "a 16-bit canvas gets fewer units for the same envelope");
assert.equal(depthOverlayFallbackUnits(1e-3, 24), Math.floor(1e-3 * 2 ** 24), "a generous envelope on 24-bit depth is spent whole");
assert.equal(depthOverlayFallbackUnits(1e-3, 32), Math.floor(1e-3 * 2 ** 24), "float depth never inflates the unit count beyond 24-bit scale");
assert.match(
  renderer,
  /applyOverlayOffset\(offset, maximumDepth\) \{[\s\S]*?if \(this\.polygonOffsetClamp\) \{[\s\S]*?polygonOffsetClampEXT\([\s\S]*?direction \* fixedDepthClampFloor\(quantiseDepthClamp\(depthOverlayClamp\([\s\S]*?return;[\s\S]*?gl\.polygonOffset\(0, direction \* depthOverlayFallbackUnits\(envelope, this\.overlayDepthBits\(\)\)\)/,
  "both depth conventions clamp the overlay when they can and fall back to a bounded unit step when they cannot",
);
assert.doesNotMatch(
  renderer,
  /gl\.polygonOffset\(offset\.factor, offset\.units\)/,
  "the fixed-depth path must never apply an unclamped slope offset",
);
assert.match(
  renderer,
  /this\.planStreamDepth\(count, isVisible\);[\s\S]*?planStreamDepth\(count, isVisible = \(\) => true\) \{[\s\S]*?planDepthMaterials\(this\.renderColors, this\.recordLocations, count, isVisible\)[\s\S]*?this\.contestedOpaqueBatches = this\.opaqueBatches/,
  "streamed chunks receive the stable material priority before the final rebuild",
);
assert.match(
  renderer,
  /const bias = this\.translucentTieBreak && this\.depthOverlayPrecisionSafe;[\s\S]*?if \(bias\) this\.applyOverlayOffset\(offset, this\.depthOverlayMaximumDepthForBatch\(batch\)\);/,
  "translucent faces coincident with opaque ones take the same bounded step, gated by the same precision rule",
);
assert.match(
  renderer,
  /for \(const batch of this\.opaqueBatches\) \{[\s\S]*?this\.drawBatch\(batch\);[\s\S]*?gl\.depthMask\(false\);[\s\S]*?gl\.depthFunc\(this\.reversedDepth \? gl\.GEQUAL : gl\.LEQUAL\);[\s\S]*?for \(const batch of this\.contestedOpaqueBatches\)/,
  "the deterministic overlay must read an unchanged canonical opaque depth buffer",
);
assert.match(
  renderer,
  /polygonOffsetClampEXT\([\s\S]*?offset\.factor,[\s\S]*?offset\.units,[\s\S]*?depthOverlayClamp\([\s\S]*?DEPTH_OVERLAY_DISTANCE_TOLERANCE,[\s\S]*?maximumDepth/,
  "reversed depth must clamp the slope offset to the full physical eligibility envelope",
);
assert.doesNotMatch(
  renderer,
  /uDepthOverlayUlps|depthOverlayUlps|floatBitsToUint\(windowDepth\)/,
  "the measured overlay must not spend its post-raster clamp budget on an ineffective vertex residual",
);
assert.match(
  renderer,
  /for \(const batch of this\.contestedOpaqueBatches\)[\s\S]*?const maximumDepth = this\.depthOverlayMaximumDepthForBatch\(batch\);[\s\S]*?depthOverlayClamp\([\s\S]*?DEPTH_OVERLAY_DISTANCE_TOLERANCE,[\s\S]*?maximumDepth/,
  "each reversed contested batch must receive the bounded post-raster clamp at its conservative depth",
);
assert.match(
  renderer,
  /const sortHalfExtents = boundsHalfExtents\(sortBounds\)[\s\S]*?\bsortHalfExtents,[\s\S]*?sortHalfExtents: boundsHalfExtents\(sortBounds\)/,
  "instanced and baked batches retain their conservative AABB half-extents",
);
assert.match(
  renderer,
  /const projectedFarExtent = halfExtents[\s\S]*?Math\.abs\(this\.cameraForward\[0\]\) \* halfExtents\[0\][\s\S]*?Math\.abs\(this\.cameraForward\[1\]\) \* halfExtents\[1\][\s\S]*?Math\.abs\(this\.cameraForward\[2\]\) \* halfExtents\[2\][\s\S]*?centerDepth \+ projectedFarExtent/,
  "a thin batch projects its exact AABB support extent instead of an oversized sphere radius",
);
assert.doesNotMatch(renderer, /gl_FragDepth/, "ordinary and overlay fragments must retain fixed-function sample depth");
assert.match(
  renderer,
  /getExtension\("EXT_clip_control"\)[\s\S]*?getExtension\("EXT_polygon_offset_clamp"\)[\s\S]*?this\.clipControl && this\.polygonOffsetClamp \? \[true, false\] : \[false\]/,
  "reversed D32 requires both zero-to-one clipping and a bounded polygon offset",
);
assert.match(
  renderer,
  /ZERO_TO_ONE_EXT[\s\S]*?NEGATIVE_ONE_TO_ONE_EXT[\s\S]*?const farClip = this\.reversedDepth \? 0 : 1/,
  "clip control and CPU rays must share the true zero-to-one reversed convention",
);
assert.match(
  renderer,
  /depthOverlayPrecisionSupported\(this\.renderBounds\.radius\)[\s\S]*?this\.depthTieBreak && this\.depthOverlayPrecisionSafe && this\.contestedOpaqueBatches\.length/,
  "large scenes must retain canonical strict source order when f32 exceeds the envelope",
);
assert.match(
  renderer,
  /for \(const batch of this\.contestedOpaqueBatches\)[\s\S]*?this\.applyDepthConvention\(\);[\s\S]*?if \(this\.transparentBatches\.length\)/,
  "strict depth and depth writes must be restored before transparent rendering",
);
assert.match(
  renderer,
  /left\.depthRank - right\.depthRank \|\| left\.sourceRecord - right\.sourceRecord/,
  "contested opaque batches must redraw in deterministic raw-rank order",
);
assert.doesNotMatch(
  renderer,
  /depthRank[^\n]*DEPTH_SLOPE_FACTOR_STEP/,
  "rank distance must never become depth displacement",
);
assert.match(
  renderer,
  /const offscreen = this\.ensureRenderTarget\(\);[\s\S]*?this\.updateCameraMatrices\(\);/,
  "projection must follow any depth-convention fallback in the same frame",
);
console.log("ok    opaque depth ties use one bounded eligibility overlay before transparency");

assert.equal(preferDepthHit(5, 2, 5, 1), true, "the visible material wins an exact CPU hit tie");
assert.equal(
  preferDepthHit(5 + PICK_DEPTH_TIE_TOLERANCE / 2, 2, 5, 1),
  true,
  "sub-millimetre depth ties follow the visible material priority",
);
assert.equal(
  preferDepthHit(5 + PICK_DEPTH_TIE_TOLERANCE * 2, 2, 5, 1),
  false,
  "material priority must not replace a genuinely nearer hit",
);
assert.equal(
  preferDepthHit(5 - PICK_DEPTH_TIE_TOLERANCE / 2, 1, 5, 1),
  true,
  "equal-priority CPU ties keep the actually nearer hit",
);
assert.equal(
  preferDepthHit(5 + PICK_DEPTH_TIE_TOLERANCE / 2, 1, 5, 1),
  false,
  "equal-priority CPU ties do not replace a nearer current hit",
);
assert.equal(preferDepthHit(5, 1, 5, 1), false, "an exact equal-priority tie keeps the first hit");
assert.ok(
  pickDepthTieTolerance(1, [0, 0, 0], 1) < PICK_DEPTH_TIE_TOLERANCE,
  "small render-space models use an ULP-scale picking tolerance",
);
assert.equal(
  pickDepthTieTolerance(1, [0, 0, 0], 1),
  2 ** -17,
  "the sub-cap tolerance covers accumulated render-space ray arithmetic",
);
assert.equal(
  pickDepthTieTolerance(10_000, [0, 0, 0], 10_000),
  PICK_DEPTH_TIE_TOLERANCE,
  "large models cannot widen a picking tie beyond the semantic cap",
);
console.log("ok    CPU picking follows the material visible at a coincident surface");

// A flat projection makes screen distance the only thing that matters.
const flatProjection = new Float32Array([
  0.2, 0, 0, 0,
  0, 0.2, 0, 0,
  0, 0, -0.01, 0,
  0, 0, 0, 1,
]);
const projectFlat = (point) => projectPoint(flatProjection, [0, 0, 0], point, 200, 200);
const roundScreen = (screen) => ({ x: Math.round(screen.x * 1000) / 1000, y: Math.round(screen.y * 1000) / 1000, visible: screen.visible });
assert.deepEqual(roundScreen(projectFlat([0, 0, 0])), { x: 100, y: 100, visible: true }, "the origin lands mid-canvas");
assert.deepEqual(roundScreen(projectFlat([2.5, 2.5, 0])), { x: 150, y: 50, visible: true }, "+y goes up the screen");
assert.equal(roundScreen(projectPoint(flatProjection, [5, 5, 0], [5, 5, 0], 200, 200)).x, 100, "the render origin is subtracted first");

const triangle = [[0, 0, 0], [4, 0, 0], [0, 4, 0]];
const hitNearCorner = { point: [0.3, 0.2, 0], triangle };
const cornerSnap = snapToTriangle(hitNearCorner, projectFlat([0.3, 0.2, 0]), projectFlat);
assert.equal(cornerSnap.kind, "vertex", "a pointer within the snap radius of a corner takes the corner");
assert.deepEqual(cornerSnap.point, [0, 0, 0]);
const hitNearEdge = { point: [2, 0.4, 0], triangle };
const edgeSnap = snapToTriangle(hitNearEdge, projectFlat([2, 0.4, 0]), projectFlat);
assert.equal(edgeSnap.kind, "edge", "a pointer near an edge but far from its corners takes the edge");
assert.ok(Math.abs(edgeSnap.point[1]) < 1e-9 && Math.abs(edgeSnap.point[0] - 2) < 1e-9, "the edge point is the perpendicular foot");
const hitMidFace = { point: [1.2, 1.2, 0], triangle };
const faceSnap = snapToTriangle(hitMidFace, projectFlat([1.2, 1.2, 0]), projectFlat);
assert.equal(faceSnap.kind, "face", "a pointer far from every corner and edge keeps the surface point");
assert.deepEqual(faceSnap.point, [1.2, 1.2, 0]);
assert.equal(snapToTriangle({ point: [1, 1, 1], triangle: null }, { x: 0, y: 0 }, projectFlat).kind, "face", "a hit without a triangle is a surface point");
assert.equal(snapToTriangle(null, { x: 0, y: 0 }, projectFlat), null);
assert.ok(SNAP_PIXELS >= 8 && SNAP_PIXELS <= 16, "the snap radius suits a trackpad without grabbing the wrong corner");

// A real perspective divide: clip w is the depth, so screen and edge parameters differ.
const recedingProjection = new Float32Array([
  1, 0, 0, 0,
  0, 1, 0, 0,
  0, 0, 0, 1,
  0, 0, 0, 0,
]);
const projectReceding = (point) => projectPoint(recedingProjection, [0, 0, 0], point, 400, 400);
const recedingTriangle = [[-1, 0, 2], [5, 0, 20], [0, 6, 6]];
// The far corner is ten times the depth of the near one, so a grazing edge is heavily foreshortened.
const grazingPointer = projectReceding([0.5, 0, 6.5]);
const grazingSnap = snapToTriangle({ point: [1, 0, 8], triangle: recedingTriangle }, grazingPointer, projectReceding);
assert.equal(grazingSnap.kind, "edge", "the pointer sits on a receding edge");
const grazingBack = projectReceding(grazingSnap.point);
assert.ok(
  Math.hypot(grazingBack.x - grazingPointer.x, grazingBack.y - grazingPointer.y) < 1,
  "an edge snap must land under the pointer even when the edge runs away from the camera",
);
assert.ok(
  Math.abs(grazingSnap.point[2] - 6.5) < 1e-6,
  "the snapped point is the 3D point the pointer covers, not the screen parameter read as a 3D one",
);

const measure = measurementBetween([1, 2, 3], [4, 6, 3]);
assert.equal(measure.length, 5, "length is the straight distance");
assert.equal(measure.horizontal, 5, "plan length ignores dZ");
assert.deepEqual(measure.delta, [3, 4, 0]);
assert.equal(measurementLabel(measure), "5.000 m");
assert.equal(measurementDetail(measure), "dX 3.000 m  dY 4.000 m  dZ 0.0 mm");
const text = measurementText([measure], [10, 20, 0]);
assert.equal(
  text,
  [
    ["#", "length", "dX", "dY", "dZ", "plan", "from", "to"].join(String.fromCharCode(9)),
    ["1", "5.000 m", "3.000 m", "4.000 m", "0.0 mm", "5.000 m", "11.000 22.000 3.000", "14.000 26.000 3.000"].join(String.fromCharCode(9)),
  ].join(String.fromCharCode(10)),
  "copied text is tab separated in IFC coordinates",
);
assert.match(app, /pointermove[\s\S]*?tools\.hoverMeasure\(/, "moving the pointer while measuring previews the snap");
assert.match(app, /renderer\.pickSurface\(at\.x, at\.y\)[\s\S]*?tools\.addMeasurePoint\(surface, at\)/, "a measure click picks the surface with its triangle");
assert.match(app, /tools\.drawOverlay\(\);/, "the measurement overlay is redrawn after every render");
assert.match(renderer, /pickSurface\(clientX, clientY\)[\s\S]*?triangle: hit\.triangle/, "the renderer exposes the hit triangle for snapping");
assert.match(app, /Backspace[\s\S]*?tools\.removeLastMeasurement\(\)/, "Backspace removes the last measurement");
console.log("ok    measurements snap to corners and edges and copy as text");

assert.match(
  app,
  /addEventListener\("pointerup", pickAtRelease\)[\s\S]*function pickAtRelease\(event\) \{[\s\S]*?const hit = renderer\.pick\(at\.x, at\.y, false\);/,
  "a click selects in the release handler itself, so the release frame and the selection share one render",
);
assert.doesNotMatch(app, /uiTasks\.post\(\(\) => performPick/, "selection must not be queued behind the release frame");
assert.doesNotMatch(renderer, /pick\(clientX, clientY, includePoint = true\) \{[\s\S]{0,400}?this\.dirty = true;/, "picking changes no pixel, so it must not ask for a frame");
assert.match(
  app,
  /if \(open\) buildChildren\(node\);/,
  "tree branches must build their contents when they open, not all at once",
);
assert.match(
  app,
  /const PAGE = \d+;/,
  "a class group must page its elements so one huge class cannot build every row at once",
);
assert.match(
  app,
  /function select\(expressId\)[\s\S]*?scrollIntoView/,
  "selecting an element must reveal and scroll to its row in the tree",
);
assert.match(
  app,
  /tree\.select\(expressId\);/,
  "a viewport selection must be mirrored in the tree",
);
assert.match(
  app,
  /if \(typing\) \{[\s\S]{0,40}?target\.blur\(\);/,
  "Escape inside a field must only leave the field, never clear an edited IFC value",
);
assert.match(
  app,
  /panel\.contains\(document\.activeElement\)/,
  "collapsing a focused panel must hand focus to the viewport",
);
assert.match(
  app,
  /addEventListener\("keydown"[\s\S]{0,200}?if \(settingsDialog\.open \|\| helpDialog\.open\) return;/,
  "an open modal must own the keyboard, so a tool shortcut cannot fire behind it",
);
assert.match(app, /target instanceof HTMLSelectElement/, "a focused select must keep its native type-ahead");
assert.match(
  app,
  /bindRovingTabs\(\s*\[\.\.\.outlinerSwitch\.querySelectorAll\("button"\)\]/,
  "the structure view tabs must move with the arrow keys",
);
assert.match(
  html,
  /id="outliner-tab-types"[^>]*aria-controls="tree-types"/,
  "each structure tab must name the tree it controls",
);
assert.doesNotMatch(html, /role="tab"[^>]*aria-pressed=/, "a tab reports aria-selected, never aria-pressed");
assert.match(
  app,
  /wrapper\.setAttribute\("role", "treeitem"\);[\s\S]{0,160}?wrapper\.tabIndex = -1;/,
  "the focused tree element must be the one carrying the treeitem role",
);
assert.match(
  app,
  /function visibleRows\(host\) \{[\s\S]*?if \(!node\.element \|\| node\.hidden\) return;[\s\S]*?rows\.push\(node\.element\);[\s\S]*?if \(node\.kind !== "branch" \|\| !node\.open\) return;/,
  "the tree keyboard walker must step over mounted, unfiltered tree items under open branches, read from the data rather than forced layout",
);
assert.match(
  app,
  /content-visibility|function continueExpansion\(\) \{[\s\S]*?const deadline = performance\.now\(\) \+ EXPAND_SLICE_MS;[\s\S]*?requestAnimationFrame\(continueExpansion\)/,
  "expanding every branch must build rows in bounded slices across frames",
);
assert.match(app, /const wrapper = templates\[[\s\S]*?\]\.cloneNode\(true\);/, "tree rows are cloned from one parsed template, not built from markup per row");
assert.doesNotMatch(app, /eye\.innerHTML = /, "a visibility change must not rewrite the eye icon markup of every row");
assert.match(
  readFileSync(resolve(repo, "viewer", "src", "styles.css"), "utf8"),
  /\.tnode\.class-group > \.tkids \{ content-visibility: auto;/,
  "off-screen element lists must skip layout and paint",
);
assert.match(
  app,
  /node\.element\?\.setAttribute\("aria-selected", "true"\)/,
  "a selected tree item must expose its state, not only a class",
);
for (const axis of ["x", "y", "z"]) {
  assert.match(
    html,
    new RegExp(`data-axis="${axis}"[^>]*aria-pressed=`),
    `the ${axis} section axis button must report whether it is the active cut`,
  );
}
assert.match(
  app,
  /button\.setAttribute\("aria-pressed", String\(on\)\)/,
  "changing the section axis must update the reported state",
);
{
  const sheet = readFileSync(resolve(repo, "viewer", "src", "styles.css"), "utf8");
  assert.match(
    sheet,
    /:root\[data-theme="light"\][\s\S]*?--measure-ink:/,
    "the measurement index colour must stay readable on a light panel",
  );
  assert.doesNotMatch(sheet, /\.trow:focus-visible/, "the focus ring must follow the focusable tree item");
}
console.log("ok    shell, tree and section controls stay reachable and announced");
assert.match(worker, /typeof kernel\.takePack === "function"/, "worker must prefer owned IGP transfer");
assert.match(worker, /kernel\.getPack\(modelId\)/, "worker must tolerate a cached older WASM build");
assert.match(worker, /kernel\.nextGeometryChunk\(/, "worker must stream chunks when the kernel can");
assert.match(worker, /typeof kernel\.beginGeometryStream === "function"/, "worker must fall back to one pack");
for (const setting of ["includeSpaces", "includeOpenings", "includeAnnotations", "includeReferences"]) {
  assert.match(worker, new RegExp(`${setting}: true`), `worker must retain ${setting} for the top-bar controls`);
}
assert.match(app, /renderer\.appendStream\(/, "chunks must reach the renderer as they arrive");
assert.match(app, /renderer\.finishStream\(/, "the final scene must be rebuilt from the assembled pack");
console.log("ok    interaction cleanup and worker compatibility are regression-gated");

// ------------------------------------------------------ streaming ownership

assert.match(
  app,
  /function startWorker\(\) \{[\s\S]{0,160}?abandonStream\(\);/,
  "restarting the worker must abandon the stream, or a second file merges into the first model",
);
assert.match(
  app,
  /function receiveChunk\(data\) \{[\s\S]{0,240}?state\.stream\?\.failed/,
  "a chunk arriving after a damaged one must not start a fresh assembler",
);
assert.match(
  app,
  /state\.stream = \{ failed: true/,
  "a damaged chunk must mark the job failed instead of clearing the stream",
);
assert.match(
  app,
  /function closeModel\(\)[\s\S]{0,400}?if \(state\.converting\) \{[\s\S]{0,240}?startWorker\(\);/,
  "closing during a conversion must cancel the job, not let it install the model afterwards",
);
assert.match(
  app,
  /function disposeModel\(\)[\s\S]{0,700}?clearModelUi\(\);/,
  "dropping a model must also blank the chrome that described it",
);
assert.match(
  app,
  /function clearModelUi\(\)[\s\S]{0,500}?enableModelCommands\(false\);/,
  "the no-model chrome must disable every model command",
);
assert.match(
  app,
  /function clearModelUi\(\)[\s\S]{0,500}?setDirty\(false\);/,
  "unsaved-edit state must not outlive the model it belongs to",
);
assert.doesNotMatch(
  app,
  /Math\.max\(\.\.\.geometry/,
  "the next geometry id must not spread a file-sized array",
);
console.log("ok    a streaming load has one owner and one failure path");

// ---------------------------------------------------------- stream assembly

{
  const { createPackAssembler, isIdentity } = await import("../src/stream.js");
  const chunkOf = ({ geometry, records, classes, diagnostics = [], stats = {}, final }) => ({
    index: {
      igp: 0,
      schema: "IFC4",
      model_offset: [1, 2, 3],
      classes,
      diagnostics,
      stats,
      stream: { chunk: 0, final, products_done: records.length, products_total: records.length },
    },
    geometry,
    instances: {
      count: records.length,
      geometryIds: Uint32Array.from(records.map((r) => r.geometry)),
      expressIds: Uint32Array.from(records.map((r) => r.expressId)),
      classIds: Uint16Array.from(records.map((r) => r.classId)),
      transforms: Float32Array.from(records.flatMap((r) => r.transform ?? [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1])),
      colors: Uint8Array.from(records.flatMap((r) => r.color ?? [200, 200, 200, 255])),
      flags: Uint16Array.from(records.map(() => 0)),
    },
    flags: final ? 0 : 1,
    bytes: 100,
  });
  const mesh = (id, x = 0) => ({
    id,
    bbox: [x, 0, 0, x + 1, 1, 1],
    positions: Float32Array.from([x, 0, 0, x + 1, 0, 0, x, 1, 0]),
    indices: Uint16Array.from([0, 1, 2]),
  });

  const assembler = createPackAssembler();
  const first = assembler.append(
    chunkOf({
      geometry: [mesh(0)],
      classes: ["IfcWall", "IfcDoor"],
      records: [
        { geometry: 0, expressId: 10, classId: 0 },
        { geometry: 0, expressId: 11, classId: 1 },
      ],
      final: false,
    }),
  );
  assert.deepEqual(first, { from: 0, to: 2 });
  const second = assembler.append(
    chunkOf({
      geometry: [mesh(1, 5)],
      // Class ids are local to a chunk: door is 0 here and slab is new.
      classes: ["IfcDoor", "IfcSlab"],
      records: [
        { geometry: 1, expressId: 12, classId: 0, transform: [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 4, 0, 0, 1] },
        { geometry: 0, expressId: 13, classId: 1 },
      ],
      diagnostics: [{ id: 13, sev: "warn", code: "W_TEST", msg: "hello" }],
      stats: { products: 4 },
      final: true,
    }),
  );
  assert.deepEqual(second, { from: 2, to: 4 });
  const merged = assembler.pack();
  assert.equal(merged.instances.count, 4);
  assert.equal(merged.geometry.length, 2, "geometry ids are global and merged once");
  assert.deepEqual(merged.index.classes, ["IfcWall", "IfcDoor", "IfcSlab"]);
  assert.deepEqual(Array.from(merged.instances.classIds), [0, 1, 1, 2], "chunk-local class ids are remapped");
  assert.deepEqual(Array.from(merged.instances.expressIds), [10, 11, 12, 13]);
  assert.equal(merged.instances.transforms[2 * 16 + 12], 4, "transforms are carried per record");
  assert.equal(merged.sharedRecords, 1, "one record is placed by a non-identity transform");
  assert.equal(merged.index.diagnostics.length, 1);
  assert.equal(merged.index.stats.products, 4);
  assert.deepEqual(merged.index.model_offset, [1, 2, 3]);
  assert.equal(merged.chunks, 2);
  assert.equal(assembler.nextGeometryId(), 2);
  assert.ok(isIdentity(merged.instances.transforms, 0));
  assert.ok(!isIdentity(merged.instances.transforms, 32));

  // A growing pack survives its capacity doubling.
  const big = createPackAssembler();
  for (let chunk = 0; chunk < 5; chunk += 1) {
    const records = Array.from({ length: 700 }, (_, i) => ({ geometry: 0, expressId: chunk * 1000 + i, classId: 0 }));
    big.append(chunkOf({ geometry: chunk === 0 ? [mesh(0)] : [], classes: ["IfcWall"], records, final: chunk === 4 }));
  }
  const grown = big.pack();
  assert.equal(grown.instances.count, 3500);
  assert.equal(grown.instances.expressIds[3499], 4699);
  assert.equal(grown.instances.transforms.length, 3500 * 16);

  // A patch replaces one product's records and reports whether it changed.
  const same = assembler.replaceProducts(
    [12],
    chunkOf({
      geometry: [mesh(9, 5)],
      classes: ["IfcDoor"],
      records: [{ geometry: 9, expressId: 12, classId: 0, transform: [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 4, 0, 0, 1] }],
      final: true,
    }),
  );
  assert.equal(same.removed, 0, "an unchanged product keeps its records where they are");
  assert.equal(same.changed, false, "identical triangles and placement are not a change");
  assert.equal(assembler.geometryCount, 2, "a no-op patch adds no mesh");
  assert.deepEqual(Array.from(assembler.pack().instances.expressIds), [10, 11, 12, 13], "a no-op patch reorders nothing");
  const moved = assembler.replaceProducts(
    [12],
    chunkOf({
      geometry: [{ ...mesh(10, 5), positions: Float32Array.from([5, 0, 0, 6, 0, 0, 5, 2, 0]) }],
      classes: ["IfcDoor"],
      records: [{ geometry: 10, expressId: 12, classId: 0, transform: [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 4, 0, 0, 1] }],
      final: true,
    }),
  );
  assert.equal(moved.changed, true, "a different mesh is a change");
  const patched = assembler.pack();
  assert.equal(patched.instances.count, 4);
  assert.deepEqual(Array.from(patched.instances.expressIds), [10, 11, 13, 12], "the patched product moves to the end");

  // Re-editing the same product must not pile up the meshes it superseded.
  const afterOnePatch = assembler.geometryCount;
  const again = assembler.replaceProducts(
    [12],
    chunkOf({
      geometry: [{ ...mesh(11, 5), positions: Float32Array.from([5, 0, 0, 7, 0, 0, 5, 3, 0]) }],
      classes: ["IfcDoor"],
      records: [{ geometry: 11, expressId: 12, classId: 0 }],
      final: true,
    }),
  );
  assert.equal(again.changed, true);
  assert.equal(assembler.geometryCount, afterOnePatch, "a superseded patch mesh must be pruned");
  assert.equal(assembler.pack().instances.count, 4, "pruning must not disturb the instance columns");
  assert.ok(assembler.nextGeometryId() > 11, "a pruned id must never be handed out again");

  // The next geometry id must be bounded work, whatever the file holds.
  const wide = createPackAssembler();
  const facet = { positions: Float32Array.from([0, 0, 0, 1, 0, 0, 0, 1, 0]), indices: Uint16Array.from([0, 1, 2]) };
  const many = Array.from({ length: 200_000 }, (unused, id) => ({ id, bbox: [0, 0, 0, 1, 1, 1], ...facet }));
  wide.append(chunkOf({ geometry: many, classes: ["IfcWall"], records: [], final: false }));
  assert.equal(wide.nextGeometryId(), 200_000, "a large mesh count must not overflow the call stack");
  wide.append(chunkOf({ geometry: [{ id: undefined, ...facet }], classes: ["IfcWall"], records: [], final: true }));
  assert.equal(wide.nextGeometryId(), 200_000, "a mesh with no id must not poison the next id");
  console.log("ok    streamed chunks assemble into one pack and patch in place");
}

const coincidentPositions = Float32Array.from([
  0, 0, 0, 1, 0, 0, 0, 1, 0,
  0, 1, 0, 1, 0, 0, 0, 0, 0,
  0, 0, 1,
]);
const coincidentIndices = Uint16Array.from([0, 1, 2, 3, 4, 5, 0, 1, 6]);
assert.deepEqual(
  [...filterCoincidentTriangles(coincidentPositions, coincidentIndices)],
  [0, 1, 2, 0, 1, 6],
  "the GPU view must suppress an opposite-wound coincident face without changing source geometry",
);
assert.equal(coincidentIndices.length, 9, "render filtering must leave the IGP index view untouched");
console.log("ok    render-only triangle filtering preserves IFC source geometry");

const oppositeDiagonalPositions = Float32Array.from([
  0, 0, 0,
  1, 0, 0,
  1, 1, 0,
  0, 1, 0,
]);
const oppositeDiagonalIndices = Uint16Array.from([
  0, 1, 2, 0, 2, 3,
  0, 3, 1, 1, 3, 2,
]);
const oppositeDiagonalPositionSource = [...oppositeDiagonalPositions];
const oppositeDiagonalIndexSource = [...oppositeDiagonalIndices];
assert.deepEqual(
  [...filterCoplanarPatchTriangles(oppositeDiagonalPositions, oppositeDiagonalIndices)],
  [0, 1, 2, 0, 2, 3],
  "equal opposite-facing patches must match across different diagonals",
);
assert.deepEqual(
  [...oppositeDiagonalPositions],
  oppositeDiagonalPositionSource,
  "patch filtering must not mutate source positions",
);
assert.deepEqual(
  [...oppositeDiagonalIndices],
  oppositeDiagonalIndexSource,
  "patch filtering must not mutate source indices",
);

const coveredPatchPositions = Float32Array.from([
  0.5, 0.5, 0,
  1.5, 0.5, 0,
  1.5, 1.5, 0,
  0.5, 1.5, 0,
  0, 0, 0,
  2, 0, 0,
  2, 2, 0,
  0, 2, 0,
]);
const coveredPatchIndices = Uint16Array.from([
  0, 1, 2, 0, 2, 3,
  4, 7, 5, 5, 7, 6,
]);
assert.deepEqual(
  [...filterCoplanarPatchTriangles(coveredPatchPositions, coveredPatchIndices)],
  [4, 7, 5, 5, 7, 6],
  "a fully covered interior patch may be suppressed without opening a hole",
);

const partialPatchPositions = Float32Array.from([
  0, 0, 0,
  1, 0, 0,
  1, 1, 0,
  0, 1, 0,
  0.5, 0, 0,
  1.5, 0, 0,
  1.5, 1, 0,
  0.5, 1, 0,
]);
const partialPatchIndices = Uint16Array.from([
  0, 1, 2, 0, 2, 3,
  4, 7, 5, 5, 7, 6,
]);
assert.deepEqual(
  [...filterCoplanarPatchTriangles(partialPatchPositions, partialPatchIndices)],
  [...partialPatchIndices],
  "partial opposite-facing overlap must remain untouched",
);

const oneUlpAboveOne = Math.fround(1 + 2 ** -23);
const offsetPatchPositions = Float32Array.from([
  0, 0, 1,
  1, 0, 1,
  1, 1, 1,
  0, 1, 1,
  0, 0, oneUlpAboveOne,
  1, 0, oneUlpAboveOne,
  1, 1, oneUlpAboveOne,
  0, 1, oneUlpAboveOne,
]);
const offsetPatchIndices = Uint16Array.from([
  0, 1, 2, 0, 2, 3,
  4, 7, 5, 5, 7, 6,
]);
assert.deepEqual(
  [...filterCoplanarPatchTriangles(offsetPatchPositions, offsetPatchIndices)],
  [...offsetPatchIndices],
  "parallel planes separated by one float ULP must never be merged",
);

const slopedPatchPositions = Float32Array.from([
  0, 0, 0,
  1, 0, 1,
  1, 1, 1,
  0, 1, 0,
]);
const slopedPatchIndices = Uint16Array.from([
  0, 1, 2, 0, 2, 3,
  0, 3, 1, 1, 3, 2,
]);
assert.deepEqual(
  [...filterCoplanarPatchTriangles(slopedPatchPositions, slopedPatchIndices)],
  [...slopedPatchIndices],
  "non-axis-aligned patches stay on the conservative fallback path",
);
console.log("ok    exact-plane patch cancellation removes coverage without fuzzy geometry loss");

const planned = planRenderBatches({
  geometry: [
    { id: 0, positions: new Float32Array(9) },
    { id: 1, positions: new Float32Array(9) },
    { id: 2, positions: new Float32Array(9) },
  ],
  instances: {
    count: 4,
    geometryIds: Uint32Array.from([0, 0, 1, 2]),
    colors: Uint8Array.from([
      220, 220, 220, 255,
      220, 220, 220, 255,
      80, 120, 160, 255,
      80, 120, 160, 255,
    ]),
  },
}, undefined, null, null, { instanceMinVertices: 0 });
assert.equal(planned.sourceDrawCalls, 3, "the source plan starts with one draw per geometry");
assert.equal(planned.instanced.length, 1, "repeated geometry remains instanced");
assert.equal(planned.baked.length, 1, "singleton geometry sharing a colour is baked together");
assert.equal(planned.drawCalls, 2, "hybrid batching reduces three source draws to two");
console.log("ok    hybrid batching keeps instances and merges singleton draw calls");

const mixedMaterialPlan = planRenderBatches({
  geometry: [{ id: 0, positions: new Float32Array(9) }],
  instances: {
    count: 2,
    geometryIds: Uint32Array.from([0, 0]),
    colors: Uint8Array.from([
      230, 55, 45, 255,
      55, 95, 220, 255,
    ]),
  },
}, undefined, null, null, { instanceMinVertices: 0 });
assert.equal(
  mixedMaterialPlan.instanced.length,
  2,
  "different materials must get independent submissions over shared geometry",
);
assert.equal(mixedMaterialPlan.baked.length, 0, "reused geometry must not be baked once per material");
console.log("ok    mixed-material reuse separates only where raster priority requires it");

const transparentPlan = planRenderBatches({
  geometry: [
    { id: 0, positions: new Float32Array(9) },
    { id: 1, positions: new Float32Array(9) },
  ],
  instances: {
    count: 3,
    geometryIds: Uint32Array.from([0, 0, 1]),
    colors: Uint8Array.from([
      80, 150, 190, 64,
      80, 150, 190, 64,
      80, 150, 190, 64,
    ]),
  },
});
assert.deepEqual(
  transparentPlan.instanced.map((group) => group.records),
  [[0], [1]],
  "reused transparent geometry keeps one independently sortable submission per product",
);
assert.equal(transparentPlan.baked.length, 1, "unique transparent geometry uses the lean baked path");
assert.deepEqual(
  transparentPlan.baked[0].items.map((item) => item.record),
  [2],
  "the unique transparent product is not merged with another alpha submission",
);
assert.equal(transparentPlan.baked[0].transparent, true);
assert.equal(transparentPlan.drawCalls, 3, "alpha correctness takes precedence over transparent draw merging");
console.log("ok    transparent batching shares only reused geometry and keeps every product sortable");

// Small reused geometry is cheaper copied into a colour batch than drawn alone.
{
  const small = { id: 0, positions: new Float32Array(9) };
  const big = { id: 1, positions: new Float32Array(3 * 4000) };
  const pack = {
    geometry: [small, big],
    instances: {
      count: 4,
      geometryIds: Uint32Array.from([0, 0, 1, 1]),
      colors: Uint8Array.from([
        200, 200, 200, 255,
        200, 200, 200, 255,
        200, 200, 200, 255,
        200, 200, 200, 255,
      ]),
    },
  };
  const plan = planRenderBatches(pack);
  assert.equal(plan.instanced.length, 1, "two copies of 4000 vertices are worth an instanced draw");
  assert.equal(plan.instanced[0].geometry.id, 1);
  assert.equal(plan.baked.length, 1, "two copies of a triangle join the colour batch");
  assert.deepEqual(plan.baked[0].items.map((item) => item.record), [0, 1]);
  assert.equal(plan.drawCalls, 2);
  const eager = planRenderBatches(pack, undefined, null, null, { instanceMinVertices: 0 });
  assert.equal(eager.instanced.length, 2, "a zero threshold instances everything reused");
  console.log("ok    reused geometry is instanced only once its copies would cost more than a draw");
}

assert.equal(humanizeIfcClass("IfcWallStandardCase"), "Wall Standard Case");
assert.equal(humanizeIfcClass("IfcDistributionFlowElement"), "Distribution Flow Element");
assert.equal(classLabelColor("IfcWall"), "#6ecbc4");
assert.equal(classLabelColor("IfcDoor"), "#ff7147");
console.log("ok    class labels are readable and stable");

const hidden = defaultHiddenClassIds({
  instances: {
    count: 6,
    classIds: Uint16Array.from([0, 1, 1, 2, 2, 3]),
    flags: Uint16Array.from([0, INSTANCE_SPACE, INSTANCE_SPACE, INSTANCE_OPENING, 0, INSTANCE_REFERENCE]),
  },
});
assert.deepEqual([...hidden], [1, 3], "helper-only classes start hidden but a mixed class stays visible");
assert.equal(DEFAULT_HIDDEN_INSTANCE_FLAGS, INSTANCE_SPACE | INSTANCE_OPENING | INSTANCE_REFERENCE);
console.log("ok    helper geometry is independently flagged and hidden without class-name guesses");

assert.throws(() => readIgp(new Uint8Array(4)), /shorter than/);
assert.throws(() => readIgp(new Uint8Array(24)), /not IGP/);
console.log("ok    invalid IGP buffers fail with useful errors");

if (!existsSync(wasmGlue)) {
  console.log("skip  build pkg-node for the IFC-to-IGP integration check");
  process.exit(0);
}

const loaded = await import(pathToFileURL(wasmGlue));
const { Kernel } = loaded.Kernel ? loaded : loaded.default;
const kernel = new Kernel();
const source = haveModel
  ? new Uint8Array(readFileSync(requestedModel))
  : new TextEncoder().encode(FRAGMENT);
const modelLabel = haveModel ? basename(requestedModel) : "the embedded fragment";
const id = kernel.openModel(source);
const info = JSON.parse(kernel.getModelInfo(id));
const summary = JSON.parse(kernel.evaluateGeometry(id, JSON.stringify({ includeOpenings: false })));
const raw = kernel.getPack(id);

const pack = readIgp(raw);
assert.equal(pack.stream.final, true, "a whole pack reads as one final chunk");
assert.equal(pack.index.schema, info.schema);
assert.ok(["IFC2X3", "IFC4", "IFC4X3"].includes(pack.index.schema));
// One record per colour, so a two-material product is two records sharing an express id.
assert.ok(pack.instances.count >= summary.products);
assert.equal(
  new Set(pack.instances.expressIds).size,
  summary.products,
  "every product must reach the pack exactly once, however many materials it has",
);
assert.ok(pack.geometry.length > 0);
assert.equal(pack.instances.transforms.length, pack.instances.count * 16);
assert.equal(pack.instances.colors.length, pack.instances.count * 4);
assert.ok(pack.geometry.every((geometry) => geometry.indices.length % 3 === 0));
assert.ok(pack.memory.gpuBytes > pack.memory.geometryBytes);
console.log(
  `ok    ${modelLabel}: ${pack.instances.count} instances, ${pack.geometry.length} unique meshes, zero-copy IGP views`,
);

const [wallId] = kernel.getIdsOfType(id, "IfcWall");
assert.ok(wallId > 0, "sample must contain an editable wall");
const beforeEdit = JSON.parse(kernel.getEntityInfo(id, wallId));
const name = beforeEdit.fields.find((field) => field.name === "Name");
assert.ok(name, "wall must expose its Name field");
const editedInfo = JSON.parse(
  kernel.setAttributes(
    id,
    wallId,
    JSON.stringify([{ attribute: "Name", value: "Viewer edit smoke", raw: false }]),
  ),
);
assert.equal(editedInfo.fields.find((field) => field.name === "Name").value, "Viewer edit smoke");
const editedSource = kernel.exportModel(id);
const verificationKernel = new Kernel();
const editedId = verificationKernel.openModel(editedSource);
const verifiedInfo = JSON.parse(verificationKernel.getEntityInfo(editedId, wallId));
assert.equal(verifiedInfo.fields.find((field) => field.name === "Name").value, "Viewer edit smoke");
assert.equal(JSON.parse(verificationKernel.getModelInfo(editedId)).entities, info.entities);
verificationKernel.closeAll();
kernel.closeAll();
console.log("ok    edit, export, reparse keeps the model structure and changed field");
console.log("PASS");
