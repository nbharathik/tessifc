// SPDX-License-Identifier: Apache-2.0

//! A compact WebGL2 IFC renderer with no runtime dependencies: instanced
//! meshes, colour-batched singletons, sortable translucents and CPU picking.
//! A model arrives whole through `load` or as IGP chunks through `appendStream`.
import { projectPoint } from "./measure.js";
import { frameSphere, wheelZoomFactor, zoomCamera } from "./navigation.js";
import { boxInView, viewSidePlanes } from "./culling.js";
import { buildBoundsTree, queryBoundsTree } from "./picking.js";
import { createGpuFrameGate } from "./gpu-frame-gate.js";

export const BATCH_VERTEX_LIMIT = 260_000;
// The drawing buffer follows the display's own pixel ratio up to this cap.
export const DEFAULT_PIXEL_RATIO_LIMIT = 2;
export const DEFAULT_LOD_PIXELS = 2;

// Motion resolution steps, and the frame periods that drop to the next one.
// A scene that already holds 60 Hz never leaves the first step.
export const MOTION_SCALES = [1, 0.6, 0.45];
export const MOTION_SLOW_MS = 20;
export const MOTION_VERY_SLOW_MS = 45;
// Consecutive slow frames before a step down, so one stall cannot soften a view.
export const MOTION_SLOW_FRAMES = 3;
export const RENDER_PIXEL_BUDGET = 4_000_000;
export const SAMPLE_PIXEL_BUDGET = 8_000_000;
export const DEPTH_SLOPE_FACTOR_STEP = 1 / 32;
export const DEPTH_OVERLAY_DISTANCE_TOLERANCE = 7.5e-4;
export const PICK_DEPTH_TIE_TOLERANCE = 7.5e-4;

// A second canvas size change this soon after one is a panel or window animating:
// the backing store and its targets are reallocated once, after the size settles.
const RESIZE_BURST_MS = 250;
const RESIZE_SETTLE_MS = 150;

// Wire indices are prepared in idle slices after a load while they stay this small;
// a larger model builds them on its first wireframe frame instead.
const WIRE_PREPARE_BYTE_LIMIT = 32 * 1024 * 1024;
const WIRE_PREPARE_SLICE_MS = 5;

const RENDER_UNIFORM_NAMES = [
  "uProjection",
  "uView",
  "uBaked",
  "uCameraPosition",
  "uPicking",
  "uDepthOnly",
  "uStyle",
  "uSectionActive",
  "uSectionAxis",
  "uSectionValue",
  "uSectionSign",
  "uFogDensity",
  "uFogColor",
  "uVisibility",
  "uVisibilityWidth",
  "uPlainBatch",
];

/** Choose a stable drawing-buffer scale within device and fill-rate limits. */
export function renderPixelRatio(
  cssWidth,
  cssHeight,
  deviceRatio = 1,
  limit = DEFAULT_PIXEL_RATIO_LIMIT,
  pixelBudget = RENDER_PIXEL_BUDGET,
  maxSize = [Infinity, Infinity],
) {
  const width = Math.max(1, Number(cssWidth) || 1);
  const height = Math.max(1, Number(cssHeight) || 1);
  const requested = Math.max(Number.EPSILON, Number(deviceRatio) || 1);
  const ratioLimit = Math.max(Number.EPSILON, Number(limit) || 1);
  const budget = Math.max(1, Number(pixelBudget) || RENDER_PIXEL_BUDGET);
  return Math.max(
    Number.EPSILON,
    Math.min(
      requested,
      ratioLimit,
      Math.max(1, Number(maxSize[0]) || 1) / width,
      Math.max(1, Number(maxSize[1]) || 1) / height,
      Math.sqrt(budget / (width * height)),
    ),
  );
}

/** Pick a common MSAA count without exceeding the target sample budget. */
export function renderTargetPlan(
  width,
  height,
  maxSamples,
  sampleBudget = SAMPLE_PIXEL_BUDGET,
) {
  const pixels = Math.max(1, Math.floor(width)) * Math.max(1, Math.floor(height));
  const maximum = Math.max(1, Math.floor(Number(maxSamples) || 1));
  const budget = Math.max(1, Number(sampleBudget) || SAMPLE_PIXEL_BUDGET);
  const samples = [4, 2, 1].find((count) => count <= maximum && pixels * count <= budget) ?? 1;
  return { samples, estimatedBytes: pixels * samples * 8 };
}

/** One toward-camera eligibility step for the contested opaque overlay. */
export function depthOverlayOffset(reversedDepth) {
  const direction = reversedDepth ? 1 : -1;
  return {
    factor: direction * DEPTH_SLOPE_FACTOR_STEP,
    units: reversedDepth ? 0 : direction,
  };
}

/** The overlay's shift as whole depth units when there is no clamp extension. */
export function depthOverlayFallbackUnits(clampWindow, depthBits = 24) {
  const bits = clamp(Math.floor(Number(depthBits) || 24), 16, 24);
  const window = Math.max(0, Number(clampWindow) || 0);
  return Math.max(FIXED_DEPTH_MINIMUM_UNITS, Math.floor(window * 2 ** bits));
}

/** Clamp floor in depth-buffer units on fixed depth; float depth passes zero bits. */
export const FIXED_DEPTH_MINIMUM_UNITS = 2;

export function fixedDepthClampFloor(clampWindow, depthBits) {
  const bits = Math.floor(Number(depthBits) || 0);
  if (bits <= 0) return clampWindow;
  return Math.max(Math.abs(Number(clampWindow) || 0), FIXED_DEPTH_MINIMUM_UNITS * 2 ** -clamp(bits, 16, 24));
}

/** Clamps round down to this many steps per octave, so a frame needs few rasterizer states. */
export const DEPTH_CLAMP_STEPS_PER_OCTAVE = 8;

export function quantiseDepthClamp(clampValue, stepsPerOctave = DEPTH_CLAMP_STEPS_PER_OCTAVE) {
  const value = Math.abs(Number(clampValue) || 0);
  if (!(value > 0)) return 0;
  const steps = Math.max(1, Math.floor(stepsPerOctave));
  return 2 ** (Math.floor(Math.log2(value) * steps) / steps);
}

/** The translucent pass: the overlay's slope step plus two whole depth units, still clamped. */
export function translucentDepthOffset(reversedDepth) {
  const base = depthOverlayOffset(reversedDepth);
  return { factor: base.factor, units: base.units + (reversedDepth ? 2 : -2) };
}

/** Cap the overlay's post-raster shift to a world-space eligibility envelope. */
export function depthOverlayClamp(
  near,
  far,
  perspectiveMode = true,
  distanceTolerance = DEPTH_OVERLAY_DISTANCE_TOLERANCE,
  maximumDepth = far,
) {
  const nearDepth = Math.max(Number.EPSILON, Math.abs(Number(near) || 0));
  const farDepth = Math.max(nearDepth + Number.EPSILON, Math.abs(Number(far) || 0));
  const span = farDepth - nearDepth;
  const depth = clamp(Math.abs(Number(maximumDepth) || farDepth), nearDepth, farDepth);
  const distance = Math.min(Math.max(0, Number(distanceTolerance) || 0), depth / 2);
  if (!distance) return 0;
  if (!perspectiveMode) return distance / span;
  const projectionScale = nearDepth * farDepth / span;
  return projectionScale * distance / (depth * (depth - distance));
}

/** Refuse to reinterpret depth ties once render-space f32 loses the envelope. */
export function depthOverlayPrecisionSupported(
  renderRadius,
  distanceTolerance = DEPTH_OVERLAY_DISTANCE_TOLERANCE,
) {
  const radius = Math.max(0, Math.abs(Number(renderRadius) || 0));
  const distance = Math.max(0, Number(distanceTolerance) || 0);
  return radius * 2 ** -23 <= distance;
}

/** Match CPU hit ties to the material priority visible in the depth buffer. */
export function preferDepthHit(
  candidateDistance,
  candidatePriority,
  currentDistance,
  currentPriority,
  tolerance = PICK_DEPTH_TIE_TOLERANCE,
) {
  if (candidateDistance < currentDistance - tolerance) return true;
  if (Math.abs(candidateDistance - currentDistance) > tolerance) return false;
  if (candidatePriority !== currentPriority) return candidatePriority > currentPriority;
  return candidateDistance < currentDistance;
}

/** Bound a CPU hit tie to render-space f32 error, never a semantic gap. */
export function pickDepthTieTolerance(renderRadius = 1, rayOrigin = [0, 0, 0], distance = 0) {
  const originScale = Math.hypot(
    Number(rayOrigin?.[0]) || 0,
    Number(rayOrigin?.[1]) || 0,
    Number(rayOrigin?.[2]) || 0,
  );
  const distanceScale = Number.isFinite(distance) ? Math.abs(distance) : 0;
  const scale = Math.max(1, Math.abs(Number(renderRadius) || 0), originScale, distanceScale);
  const ulp = 2 ** (Math.floor(Math.log2(scale)) - 23);
  return Math.min(PICK_DEPTH_TIE_TOLERANCE, Math.max(1e-7, ulp * 64));
}

// Above this a wire mesh keeps its duplicate edges: strict depth hides them and the hashing stalls.
const MAX_WIRE_DEDUPE_TRIANGLES = 200_000;

// Patch cancellation is a quality pass; pathological soups are left unchanged.
const MAX_PATCH_TRIANGLES = 256;
const MAX_PATCH_PAIR_TESTS = 65_536;
const MIN_PATCH_AREA = 1e-10;
// Rounding slack for the double-precision clipper, never a real coordinate gap.
const PATCH_AREA_RELATIVE_EPSILON = Number.EPSILON * 4096;
const PATCH_AREA_ABSOLUTE_EPSILON = Number.EPSILON * 32;

// Clear colours used when the page passes none; they match the stylesheet tokens.
const CANVAS_COLORS = {
  dark: [0.055, 0.063, 0.078],
  light: [0.875, 0.89, 0.91],
};

/** The fill at a section cut, light enough to read against either ground. */
const SECTION_CAP_COLORS = {
  dark: [0.42, 0.44, 0.48],
  light: [0.62, 0.62, 0.66],
};

/** `#rgb` or `#rrggbb` as 0..1 floats, or null for anything else. */
function parseHexColor(text) {
  const match = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(String(text ?? "").trim());
  if (!match) return null;
  const hex = match[1].length === 3 ? [...match[1]].map((c) => c + c).join("") : match[1];
  return [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
}

export class IfcRenderer {
  constructor(container) {
    this.canvas = document.createElement("canvas");
    this.canvas.className = "ifc-canvas";
    this.canvas.setAttribute("aria-label", "Interactive IFC model");
    container.append(this.canvas);
    this.container = container;
    this.gl = this.canvas.getContext("webgl2", {
      // The offscreen target carries the samples; blitting into an MSAA canvas is invalid.
      antialias: false,
      alpha: false,
      depth: true,
      // The cap at a section plane is a stencil parity count, so the fallback
      // path needs a stencil too; the offscreen target carries its own.
      stencil: true,
      premultipliedAlpha: false,
      powerPreference: "high-performance",
      preserveDrawingBuffer: false,
    });
    if (!this.gl) throw new Error("This viewer needs WebGL 2 support.");
    this.contextLost = false;
    this.onContextLost = null;
    this.onContextRestored = null;
    // Kept so a restored context can rebuild exactly what was on screen.
    this.visibilityPredicate = () => true;
    this.batches = [];
    this.opaqueBatches = [];
    this.contestedOpaqueBatches = [];
    this.transparentBatches = [];
    this.sortedBatches = [];
    this.sharedGeometryBuffers = [];
    this.recordLocations = [];
    // Per-record contested triangles, when a host has run the plane analysis.
    this.contestedTriangles = null;
    this.pickTree = null;
    this.pickCandidates = [];
    // Render views of each mesh, by mesh identity, so the finish reuses the stream's filtering.
    this.preparedByGeometry = new WeakMap();
    // Streaming keeps running bounds; the depth plan is refreshed on a timer.
    this.streamBounds = emptyBounds();
    this.streamDepthPlannedAt = 0;
    this.pack = null;
    this.bounds = emptyBounds();
    // GPU math stays near the origin: a distant f32 instance translation would
    // make edges shimmer. Public coordinates stay in pack space.
    this.renderOrigin = [0, 0, 0];
    this.renderBounds = emptyBounds();
    this.selected = [];
    // Records fading out of a highlight after a revision; null when idle.
    this.flashState = null;
    this.style = "shaded";
    // The cut is kept in world coordinates: the render origin moves with every load.
    this.section = { active: false, axis: 2, world: 0, sign: 1 };
    this.camera = {
      mode: "perspective",
      target: [0, 0, 0],
      position: [8, -9, 7],
      up: [0, 0, 1],
      distance: 12,
      orthoScale: 5,
      fov: Math.PI * 42 / 180,
    };
    this.projection = mat4();
    this.view = mat4();
    this.viewProjection = mat4();
    this.inverseViewProjection = mat4();
    this.cameraDepthRange = { near: 0.001, far: 1 };
    this.cameraForward = [0, 0, -1];
    this.visibility = new Uint8Array(0);
    // What the caller asked for, before a record is dropped for being too small.
    this.baseVisible = new Uint8Array(0);
    this.visibilityVersion = 0;
    this.selectionVersion = 0;
    // Products narrower than this many pixels are not drawn; zero draws them all.
    this.lodPixels = DEFAULT_LOD_PIXELS;
    this.lodHidden = new Uint8Array(0);
    this.lodHiddenCount = 0;
    this.lodState = null;
    this.viewPlanes = new Float64Array(16);
    this.cullBatches = true;
    this.visibilityTexture = null;
    this.visibilityTextureWidth = 1;
    this.visibilityHeight = 1;
    this.drag = null;
    this.interacting = false;
    this.wheelQualityTimer = 0;
    this.pointers = new Map();
    this.pixelRatioLimit = DEFAULT_PIXEL_RATIO_LIMIT;
    // Fraction of the display's native resolution; 1 is one buffer pixel per device pixel.
    this.renderScale = 1;
    this.renderPixelBudget = RENDER_PIXEL_BUDGET;
    this.samplePixelBudget = SAMPLE_PIXEL_BUDGET;
    this.canvasCssWidth = 0;
    this.canvasCssHeight = 0;
    this.devicePixelRatio = 0;
    this.resizeDirty = true;
    this.gpuBufferBytes = 0;
    // Gestures keep the full sample grid unless a caller explicitly requests a lower scale.
    this.interactionScale = 1;
    this.gestureScale = 1;
    // Off by default: measured frames here are bound by geometry, not by fill,
    // so trading pixels for frames costs sharpness and returns nothing.
    this.adaptiveResolution = false;
    this.motionStep = 0;
    this.slowFrames = 0;
    this.slowStep = 0;
    this.motionSteady = true;
    this.lastMotionFrameAt = 0;
    // Render-only coincident-face suppression; the render tests prove it changes no pixel.
    this.suppressCoplanar = true;
    // Display floors from `displayColors`; zero draws the file's own values exactly.
    this.minimumAlpha = MINIMUM_VISIBLE_ALPHA;
    this.minimumLuminance = MINIMUM_VISIBLE_LUMINANCE;
    // False lets coincident surfaces fight, for measuring how much they cover.
    this.depthTieBreak = true;
    this.contestedDepthPrepass = true;
    // Opt-in bounded step for translucent faces; off because it looked worse around glass.
    this.translucentTieBreak = false;
    this.depthPlanExhausted = false;
    this.depthRanks = new Uint32Array(0);
    this.depthContested = new Uint8Array(0);
    this.depthConflictPairs = 0;
    this.contestedMaterials = 0;
    this.depthOverlayPrecisionSafe = true;
    // The load-time plan with its overlapping record pairs; a smaller visible set is planned from it.
    this.depthPlanBase = null;
    // Prefer float reversed depth; failed formats are retried before the default buffer.
    this.offscreen = true;
    this.target = null;
    this.targetCache = new Map();
    this.interactionPrepared = false;
    this.interactionPrepareFrame = 0;
    this.interactionPrepareTimer = 0;
    this.targetFailures = new Set();
    this.targetFailureSize = "";
    this.backingChangedAt = Number.NEGATIVE_INFINITY;
    this.resizeSettleTimer = 0;
    this.wirePrepare = null;
    this.dirty = true;
    this.onCameraChange = null;
    this.onDirty = null;
    this.onFrameReady = null;
    // Streaming: the camera follows the growing model until the user takes it over.
    this.streaming = false;
    this.cameraTouched = false;
    this.renderColors = new Uint8Array(0);
    this.dimmedProducts = 0;
    this.darkProducts = 0;
    this.visibilityCapacity = 0;

    this.viewportTheme = "dark";
    this.background = new Float32Array(CANVAS_COLORS.dark);
    this.sectionCapColor = SECTION_CAP_COLORS.dark;
    this.initGl();
    this.idleGpuLimit = 1;
    this.gpuPacingWidth = 0;
    this.gpuPacingHeight = 0;
    this.gpuPacing = createGpuFrameGate(this.gl, () => {
      if (!this.dirty || this.contextLost) return;
      if (this.onFrameReady) this.onFrameReady();
      else this.render();
    });
    this.bindControls();
    this.resize();
  }

  /** Everything that belongs to the GL context, so a restored context can take it all again. */
  initGl() {
    const gl = this.gl;
    this.clipControl = gl.getExtension("EXT_clip_control");
    this.polygonOffsetClamp = gl.getExtension("EXT_polygon_offset_clamp");
    this.reversedDepth = Boolean(this.clipControl && this.polygonOffsetClamp);
    this.surfaceProgram = createProgram(gl, VERTEX_SHADER, FRAGMENT_SHADER);
    this.surfaceUniforms = uniforms(gl, this.surfaceProgram, RENDER_UNIFORM_NAMES);
    this.sectionProgram = createProgram(gl, VERTEX_SHADER,
      FRAGMENT_SHADER.replace("#version 300 es", "#version 300 es\n#define SECTION"));
    this.sectionUniforms = uniforms(gl, this.sectionProgram, RENDER_UNIFORM_NAMES);
    this.program = this.surfaceProgram;
    this.uniforms = this.surfaceUniforms;
    const viewport = gl.getParameter(gl.MAX_VIEWPORT_DIMS);
    const renderbuffer = gl.getParameter(gl.MAX_RENDERBUFFER_SIZE);
    this.maxCanvasSize = [
      Math.min(viewport[0], renderbuffer),
      Math.min(viewport[1], renderbuffer),
    ];
    // What the browser actually granted, reported so the depth precision is visible.
    const attributes = gl.getContextAttributes?.() ?? {};
    this.display = {
      depthBits: gl.getParameter(gl.DEPTH_BITS) || 0,
      samples: gl.getParameter(gl.SAMPLES) || 0,
      antialiasRequested: attributes.antialias !== false,
      maxSamples: gl.getParameter(gl.MAX_SAMPLES) || 0,
    };
    // The cap at a section plane: one quad on the plane, filled where a stencil
    // parity count says the pixel is inside a solid.
    this.capProgram = createProgram(gl, CAP_VERTEX_SHADER, CAP_FRAGMENT_SHADER);
    this.capUniforms = uniforms(gl, this.capProgram, ["uProjection", "uColor"]);
    this.capVao = gl.createVertexArray();
    this.capBuffer = gl.createBuffer();
    gl.bindVertexArray(this.capVao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.capBuffer);
    const capPosition = gl.getAttribLocation(this.capProgram, "aPosition");
    gl.enableVertexAttribArray(capPosition);
    gl.vertexAttribPointer(capPosition, 3, gl.FLOAT, false, 0, 0);
    gl.bindVertexArray(null);

    gl.clearColor(...this.background, 1);
    gl.clearStencil(0);
    gl.enable(gl.DEPTH_TEST);
    this.applyDepthConvention();
    // IFC shells may be open or inconsistently wound: draw both sides.
    gl.disable(gl.CULL_FACE);
  }

  /** Stencil bits available on whatever is being rendered into. */
  get stencilBits() {
    const gl = this.gl;
    // The offscreen target always carries a packed depth-stencil; the canvas
    // only carries one if the browser granted the attribute.
    if (this.target) return 8;
    return gl.getParameter(gl.STENCIL_BITS) || 0;
  }

  /** Drop every GPU handle without touching the dead context. */
  handleContextLost() {
    if (this.contextLost) return;
    this.contextLost = true;
    this.gpuPacing.reset(true);
    this.gpuPacingWidth = this.gpuPacingHeight = 0;
    if (this.wheelQualityTimer) clearTimeout(this.wheelQualityTimer);
    this.wheelQualityTimer = 0;
    this.clearTimers();
    this.cancelWirePreparation();
    this.drag = null;
    this.interacting = false;
    this.gestureScale = 1;
    this.motionStep = 0;
    this.slowFrames = 0;
    this.slowStep = 0;
    this.motionSteady = true;
    this.lastMotionFrameAt = 0;
    this.batches = [];
    this.opaqueBatches = [];
    this.contestedOpaqueBatches = [];
    this.transparentBatches = [];
    this.sortedBatches = [];
    this.sharedGeometryBuffers = [];
    this.contestedTriangles = null;
    this.gpuBufferBytes = 0;
    this.target = null;
    this.targetCache.clear();
    this.visibilityTexture = null;
    this.visibilityCapacity = 0;
    this.onContextLost?.();
  }

  /** Take the new context and rebuild the model the page was showing. */
  handleContextRestored() {
    if (!this.contextLost || this.gl.isContextLost()) return;
    this.contextLost = false;
    this.initGl();
    this.gpuPacing.reset();
    this.targetFailures.clear();
    this.targetFailureSize = "";
    this.resizeDirty = true;
    this.dirty = true;
    if (this.pack) this.reload(this.pack, this.visibilityPredicate);
    // The page may have no frame scheduled, so the restored view is drawn here.
    this.render(true);
    this.onContextRestored?.();
  }

  /** The render view of a mesh, filtered once per mesh and opacity; the source mesh is untouched. */
  prepareGeometry(geometry, suppressCoplanar) {
    let entry = this.preparedByGeometry.get(geometry);
    if (!entry) {
      entry = {};
      this.preparedByGeometry.set(geometry, entry);
    }
    const key = suppressCoplanar ? "coplanar" : "plain";
    if (!entry[key]) entry[key] = prepareGeometryForGpu(geometry, suppressCoplanar);
    return entry[key];
  }

  /**
   * Bind the target for `scaleValue`, allocating it when it is missing or too small. A target
   * keeps the largest frame it has drawn, so a panel folding back needs no new allocation.
   * False when no offscreen target can be built.
   */
  ensureRenderTarget(scaleValue = this.interacting ? Math.min(this.interactionScale, this.gestureScale) : 1) {
    const gl = this.gl;
    if (!this.offscreen) { this.target = null; return false; }
    const frameWidth = Math.max(1, Math.floor(this.canvas.width * scaleValue));
    const frameHeight = Math.max(1, Math.floor(this.canvas.height * scaleValue));
    const slot = scaleValue >= 1 ? "full" : "gesture";
    const cached = this.targetCache.get(slot);
    if (cached && cached.width >= frameWidth && cached.height >= frameHeight) {
      cached.frameWidth = frameWidth;
      cached.frameHeight = frameHeight;
      this.target = cached;
      this.reversedDepth = cached.reversed;
      this.applyDepthConvention();
      return true;
    }
    const width = Math.max(frameWidth, cached?.width ?? 0);
    const height = Math.max(frameHeight, cached?.height ?? 0);
    if (cached) this.deleteRenderTarget(cached);
    this.target = null;
    const failureSize = `${width}x${height}`;
    const canvasSize = `${this.canvas.width}x${this.canvas.height}`;
    if (this.targetFailureSize !== canvasSize) {
      this.targetFailureSize = canvasSize;
      this.targetFailures.clear();
    }

    const targetPlan = renderTargetPlan(
      width,
      height,
      this.display.maxSamples,
      this.samplePixelBudget,
    );
    // A multisampled blit needs matching colour formats, so the canvas format is read back.
    const colourFormat = gl.getParameter(gl.ALPHA_BITS) > 0 ? gl.RGBA8 : gl.RGB8;
    const sampleCounts = [...new Set([targetPlan.samples, 2, 1])]
      .filter((samples) => samples <= targetPlan.samples);
    const attempts = [];
    for (const reversed of this.clipControl && this.polygonOffsetClamp ? [true, false] : [false]) {
      for (const samples of sampleCounts) {
        const key = `${failureSize}:${reversed ? 1 : 0}:${samples}`;
        if (!this.targetFailures.has(key)) attempts.push({ reversed, samples, key });
      }
    }

    for (const attempt of attempts) {
      while (gl.getError() !== gl.NO_ERROR) { /* drain allocation errors */ }
      const framebuffer = gl.createFramebuffer();
      const colour = gl.createRenderbuffer();
      const depth = gl.createRenderbuffer();
      if (!framebuffer || !colour || !depth) {
        if (framebuffer) gl.deleteFramebuffer(framebuffer);
        if (colour) gl.deleteRenderbuffer(colour);
        if (depth) gl.deleteRenderbuffer(depth);
        this.targetFailures.add(attempt.key);
        continue;
      }
      gl.bindRenderbuffer(gl.RENDERBUFFER, colour);
      if (attempt.samples > 1) {
        gl.renderbufferStorageMultisample(
          gl.RENDERBUFFER,
          attempt.samples,
          colourFormat,
          width,
          height,
        );
      } else {
        gl.renderbufferStorage(gl.RENDERBUFFER, colourFormat, width, height);
      }
      gl.bindRenderbuffer(gl.RENDERBUFFER, depth);
      const depthFormat = attempt.reversed ? gl.DEPTH32F_STENCIL8 : gl.DEPTH24_STENCIL8;
      if (attempt.samples > 1) {
        gl.renderbufferStorageMultisample(
          gl.RENDERBUFFER,
          attempt.samples,
          depthFormat,
          width,
          height,
        );
      } else {
        gl.renderbufferStorage(gl.RENDERBUFFER, depthFormat, width, height);
      }
      gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
      gl.framebufferRenderbuffer(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.RENDERBUFFER, colour);
      gl.framebufferRenderbuffer(
        gl.FRAMEBUFFER,
        gl.DEPTH_STENCIL_ATTACHMENT,
        gl.RENDERBUFFER,
        depth,
      );
      let complete = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
      let resolveFramebuffer = null;
      let resolveColour = null;
      // A gesture frame is smaller than the canvas, so its samples resolve before the scaled blit.
      if (complete && slot === "gesture" && attempt.samples > 1) {
        resolveFramebuffer = gl.createFramebuffer();
        resolveColour = gl.createRenderbuffer();
        if (resolveFramebuffer && resolveColour) {
          gl.bindRenderbuffer(gl.RENDERBUFFER, resolveColour);
          gl.renderbufferStorage(gl.RENDERBUFFER, colourFormat, width, height);
          gl.bindFramebuffer(gl.FRAMEBUFFER, resolveFramebuffer);
          gl.framebufferRenderbuffer(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.RENDERBUFFER, resolveColour);
          complete = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
        } else complete = false;
      }
      const failed = gl.getError() !== gl.NO_ERROR;
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.bindRenderbuffer(gl.RENDERBUFFER, null);
      if (!complete || failed) {
        gl.deleteFramebuffer(framebuffer);
        gl.deleteRenderbuffer(colour);
        gl.deleteRenderbuffer(depth);
        if (resolveFramebuffer) gl.deleteFramebuffer(resolveFramebuffer);
        if (resolveColour) gl.deleteRenderbuffer(resolveColour);
        this.targetFailures.add(attempt.key);
        continue;
      }

      this.reversedDepth = attempt.reversed;
      this.applyDepthConvention();
      this.target = {
        framebuffer,
        colour,
        depth,
        resolveFramebuffer,
        resolveColour,
        reversed: attempt.reversed,
        width,
        height,
        frameWidth,
        frameHeight,
        samples: attempt.samples,
        depthBits: attempt.reversed ? 32 : 24,
        estimatedBytes: width * height * (attempt.samples * 8 + (resolveColour ? 4 : 0)),
        blitValidated: false,
        key: attempt.key,
        slot,
      };
      this.targetCache.set(slot, this.target);
      return true;
    }

    this.reversedDepth = false;
    this.applyDepthConvention();
    return false;
  }

  deleteRenderTarget(target) {
    if (!target) return;
    const gl = this.gl;
    gl.deleteFramebuffer(target.framebuffer);
    gl.deleteRenderbuffer(target.colour);
    gl.deleteRenderbuffer(target.depth);
    if (target.resolveFramebuffer) gl.deleteFramebuffer(target.resolveFramebuffer);
    if (target.resolveColour) gl.deleteRenderbuffer(target.resolveColour);
    this.targetCache.delete(target.slot);
    if (this.target === target) this.target = null;
  }

  releaseRenderTarget() {
    for (const target of this.targetCache.values()) this.deleteRenderTarget(target);
    this.target = null;
    this.interactionPrepared = false;
  }

  /** Resolve the frame's samples at their native size before scaling into the fixed canvas. */
  presentTarget() {
    const gl = this.gl, target = this.target;
    const width = target.frameWidth, height = target.frameHeight;
    if (!target.blitValidated) {
      while (gl.getError() !== gl.NO_ERROR) { /* isolate presentation errors */ }
    }
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER, target.framebuffer);
    if (target.resolveFramebuffer) {
      gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, target.resolveFramebuffer);
      gl.blitFramebuffer(0, 0, width, height, 0, 0, width, height, gl.COLOR_BUFFER_BIT, gl.NEAREST);
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, target.resolveFramebuffer);
    }
    gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
    const scaled = width !== this.canvas.width || height !== this.canvas.height;
    gl.blitFramebuffer(0, 0, width, height, 0, 0, this.canvas.width, this.canvas.height,
      gl.COLOR_BUFFER_BIT, scaled ? gl.LINEAR : gl.NEAREST);
    const failed = !target.blitValidated && gl.getError() !== gl.NO_ERROR;
    if (!failed) target.blitValidated = true;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    return !failed;
  }

  /** Allocate gesture buffers before interaction; `present` also validates their blit into the canvas. */
  prepareInteractionTarget(present = true) {
    this.interactionPrepared = true;
    const scaleValue = Math.min(this.interactionScale, this.chooseGestureScale());
    if (scaleValue >= 1 || !this.offscreen || this.contextLost) return;
    const saved = this.target;
    while (this.ensureRenderTarget(scaleValue) && present && !this.target.blitValidated) {
      const gl = this.gl;
      gl.bindFramebuffer(gl.FRAMEBUFFER, this.target.framebuffer);
      gl.clear(gl.COLOR_BUFFER_BIT);
      if (this.presentTarget()) break;
      this.targetFailures.add(this.target.key);
      this.deleteRenderTarget(this.target);
    }
    this.target = saved;
    this.reversedDepth = saved?.reversed ?? false;
    this.applyDepthConvention();
    if (present) this.dirty = true;
  }

  /** Strict depth test for the current convention, so equal-priority duplicates keep the first fragment. */
  applyDepthConvention() {
    const gl = this.gl;
    if (this.reversedDepth && (!this.clipControl || !this.polygonOffsetClamp)) this.reversedDepth = false;
    this.clipControl?.clipControlEXT(
      this.clipControl.LOWER_LEFT_EXT,
      this.reversedDepth
        ? this.clipControl.ZERO_TO_ONE_EXT
        : this.clipControl.NEGATIVE_ONE_TO_ONE_EXT,
    );
    if (this.reversedDepth) {
      gl.clearDepth(0);
      gl.depthFunc(gl.GREATER);
    } else {
      gl.clearDepth(1);
      gl.depthFunc(gl.LESS);
    }
  }

  load(pack, isVisible = () => true) {
    isVisible = activePredicate(pack, isVisible);
    this.visibilityPredicate = isVisible;
    if (this.contextLost) {
      // Nothing can reach a dead context; the restore rebuilds from this pack.
      this.pack = pack;
      this.bounds = computeModelBounds(pack, isVisible);
      return { bounds: this.bounds, drawCalls: 0, baseDrawCalls: 0, overlayDrawCalls: 0, gpuBytes: 0 };
    }
    this.clear();
    this.pack = pack;
    // The depth convention is resolved first: a driver may reject multisampled
    // float depth, and the fallback projection must match from the first frame.
    this.resize();
    this.ensureRenderTarget();
    this.bounds = computeModelBounds(pack, isVisible);
    this.renderOrigin = this.bounds.center.slice();
    this.renderBounds = offsetBounds(this.bounds, scale(this.renderOrigin, -1));
    this.depthOverlayPrecisionSafe = depthOverlayPrecisionSupported(this.renderBounds.radius);
    this.createVisibilityTexture(pack.instances.count);
    const geometryById = geometryIndex(pack);
    this.recordLocations = Array(pack.instances.count).fill(null);
    for (let record = 0; record < pack.instances.count; record += 1) {
      if (!isActiveRecord(pack, record)) continue;
      const geometryId = pack.instances.geometryIds[record];
      const geometry = geometryById.get(geometryId);
      if (!geometry) continue;
      const worldBounds = transformedBounds(geometry.bbox, pack.instances.transforms, record * 16);
      this.recordLocations[record] = {
        geometry,
        bounds: offsetBounds(worldBounds, scale(this.renderOrigin, -1)),
      };
    }
    this.pickTree = buildBoundsTree(this.recordLocations.length, (record) => this.recordLocations[record]?.bounds);

    this.renderColors = displayColors(pack, this.minimumAlpha, this.minimumLuminance);
    this.dimmedProducts = this.renderColors.raisedCount ?? 0;
    this.darkProducts = this.renderColors.brightenedCount ?? 0;
    // Planned once over every record; each visible set, this one included, is read off its pairs.
    const everyRecord = (record) => isActiveRecord(pack, record);
    const basePlan = planDepthMaterials(
      this.renderColors,
      this.recordLocations,
      pack.instances.count,
      everyRecord,
      { collectPairs: true },
    );
    this.rememberDepthPlan(basePlan, pack.instances.count, everyRecord);
    const depthPlan = restrictDepthPlan(basePlan, this.renderColors, pack.instances.count, isVisible) ??
      planDepthMaterials(this.renderColors, this.recordLocations, pack.instances.count, isVisible);
    this.depthRanks = depthPlan.ranks;
    this.depthContested = depthPlan.contested;
    this.depthConflictPairs = depthPlan.conflictPairs;
    this.contestedMaterials = depthPlan.contestedColors;
    this.depthPlanExhausted = Boolean(depthPlan.exhausted);
    const plan = planRenderBatches(pack, undefined, this.renderColors, null, {
      geometryById,
      contested: this.depthContested,
      recordLocations: this.recordLocations,
    });
    const preparedByKey = new Map();
    const sharedByKey = new Map();
    const suppressedByGeometry = new Map();
    let gpuBytes = 0;
    let eagerWireBytesAvoided = 0;
    for (const group of plan.instanced) {
      const geometry = group.geometry;
      const preparedKey = `${geometry.id}:${group.transparent ? 1 : 0}`;
      let prepared = preparedByKey.get(preparedKey);
      if (!prepared) {
        prepared = this.prepareGeometry(geometry, this.suppressCoplanar && !group.transparent);
        preparedByKey.set(preparedKey, prepared);
        suppressedByGeometry.set(
          geometry.id,
          Math.max(suppressedByGeometry.get(geometry.id) ?? 0, prepared.suppressedTriangles),
        );
      }
      let shared = sharedByKey.get(preparedKey);
      if (!shared) {
        shared = this.createSharedGeometryBuffers(geometry, prepared);
        sharedByKey.set(preparedKey, shared);
        this.sharedGeometryBuffers.push(shared);
        gpuBytes += shared.gpuBytes;
        eagerWireBytesAvoided += shared.indexCount * indexByteWidth(shared.indexType, this.gl) * 2;
      }
      const batch = this.createInstancedBatch(
        geometry,
        prepared,
        group.records,
        group.transparent,
        shared,
      );
      this.batches.push(batch);
      gpuBytes += batch.gpuBytes;
    }
    for (const group of plan.baked) {
      const items = group.items.map((item) => {
        const preparedKey = `${item.geometry.id}:${group.transparent ? 1 : 0}`;
        let prepared = preparedByKey.get(preparedKey);
        if (!prepared) {
          prepared = this.prepareGeometry(item.geometry, this.suppressCoplanar && !group.transparent);
          preparedByKey.set(preparedKey, prepared);
          suppressedByGeometry.set(
            item.geometry.id,
            Math.max(suppressedByGeometry.get(item.geometry.id) ?? 0, prepared.suppressedTriangles),
          );
        }
        return { ...item, prepared };
      });
      const batch = this.createBakedBatch(items, group.color, group.vertexCount);
      this.batches.push(batch);
      gpuBytes += batch.gpuBytes;
      eagerWireBytesAvoided += batch.indexCount * indexByteWidth(batch.indexType, this.gl) * 2;
    }
    const suppressedTriangles = [...suppressedByGeometry.values()].reduce(
      (total, count) => total + count,
      0,
    );
    this.batches.sort((left, right) => {
      const transparencyOrder = Number(left.transparent) - Number(right.transparent);
      if (transparencyOrder !== 0) return transparencyOrder;
      // Restore source order so batch construction cannot change the depth fallbacks.
      return left.sourceRecord - right.sourceRecord;
    });
    for (let index = 0; index < this.batches.length; index += 1) {
      this.batches[index].sortOrder = index;
    }
    this.opaqueBatches = this.batches.filter((batch) => !batch.transparent);
    this.contestedOpaqueBatches = this.opaqueBatches
      .filter((batch) => this.batchNeedsOverlay(batch))
      .sort((left, right) => left.depthRank - right.depthRank || left.sourceRecord - right.sourceRecord);
    this.transparentBatches = this.batches.filter((batch) => batch.transparent);
    for (let record = 0; record < pack.instances.count; record += 1) {
      this.baseVisible[record] = isVisible(record) ? 255 : 0;
      this.visibility[record * 2] = this.baseVisible[record];
    }
    this.uploadVisibility();
    gpuBytes += this.visibility.byteLength;
    this.gpuBufferBytes = gpuBytes;
    this.prepareInteractionTarget();
    this.scheduleWirePreparation();
    this.fit("perspective");
    this.dirty = true;
    const baseDrawCalls = this.batches.length;
    const overlayDrawCalls = this.depthTieBreak && this.depthOverlayPrecisionSafe
      ? this.contestedOpaqueBatches.length
      : 0;
    return {
      bounds: this.bounds,
      // Actual solid-frame submissions: contested materials are redrawn, not reallocated.
      drawCalls: baseDrawCalls + overlayDrawCalls,
      baseDrawCalls,
      overlayDrawCalls,
      gpuBytes: this.currentGpuBytes(),
      eagerWireBytesAvoided,
      sourceDrawCalls: plan.sourceDrawCalls,
      instancedDrawCalls: plan.instanced.length,
      bakedDrawCalls: plan.baked.length,
      suppressedTriangles,
      depthConflictPairs: this.depthConflictPairs,
      contestedMaterials: this.contestedMaterials,
      depthOverlayPrecisionSafe: this.depthOverlayPrecisionSafe,
    };
  }

  /** Drop everything and get ready for chunks. */
  beginStream() {
    this.clear();
    this.resize();
    this.ensureRenderTarget();
    this.streaming = true;
    this.cameraTouched = false;
    // Pack space is already near the origin; the finish picks the real centre.
    this.renderOrigin = [0, 0, 0];
    this.bounds = emptyBounds();
    this.renderBounds = emptyBounds();
    this.renderColors = new Uint8Array(4096 * 4);
    this.depthOverlayPrecisionSafe = true;
    this.streamBounds = emptyBounds();
    this.streamDepthPlannedAt = 0;
    this.ensureVisibilityCapacity(4096);
    this.dirty = true;
  }

  /** Draw records `from..to` of the assembled pack as lean batches; the finish does the full analysis. */
  appendStream(pack, from, to, isVisible = () => true) {
    isVisible = activePredicate(pack, isVisible);
    this.visibilityPredicate = isVisible;
    if (this.contextLost) {
      this.pack = pack;
      return;
    }
    if (!this.streaming) this.beginStream();
    this.pack = pack;
    const count = pack.instances.count;
    this.ensureVisibilityCapacity(count);
    if (this.renderColors.length < count * 4) {
      let capacity = Math.max(4096, this.renderColors.length / 4);
      while (capacity < count) capacity *= 2;
      const grown = new Uint8Array(capacity * 4);
      grown.set(this.renderColors);
      this.renderColors = grown;
    }
    this.renderColors.set(pack.instances.colors.subarray(from * 4, to * 4), from * 4);
    const floors = applyDisplayFloors(this.renderColors, from, to, this.minimumAlpha, this.minimumLuminance);
    this.dimmedProducts += floors.raised;
    this.darkProducts += floors.brightened;

    const geometryById = geometryIndex(pack);
    for (let record = from; record < to; record += 1) {
      // Written first: a record without usable bounds must not keep the default visible byte.
      this.baseVisible[record] = isVisible(record) ? 255 : 0;
      this.visibility[record * 2] = this.baseVisible[record];
      if (!isActiveRecord(pack, record)) continue;
      const geometry = geometryById.get(pack.instances.geometryIds[record]);
      if (!geometry) continue;
      const bounds = transformedBounds(geometry.bbox, pack.instances.transforms, record * 16);
      if (!validDepthBounds(bounds)) continue;
      this.recordLocations[record] = { geometry, bounds };
      if (this.visibility[record * 2]) includeBounds(this.streamBounds, bounds);
    }
    // The bounds only grow while streaming; the finish measures the whole pack again.
    const grown = { min: this.streamBounds.min.slice(), max: this.streamBounds.max.slice(), center: [0, 0, 0], radius: 1 };
    finishBounds(grown);
    this.renderBounds = grown;
    this.bounds = offsetBounds(grown, this.renderOrigin);
    this.depthOverlayPrecisionSafe = depthOverlayPrecisionSupported(grown.radius);
    this.uploadVisibility();

    const plan = planRenderBatches(pack, undefined, this.renderColors, { from, to }, {
      geometryById,
      recordLocations: this.recordLocations,
    });
    const sharedByKey = new Map();
    let gpuBytes = 0;
    const prepare = (geometry, transparent) => this.prepareGeometry(geometry, this.suppressCoplanar && !transparent);
    for (const group of plan.instanced) {
      const prepared = prepare(group.geometry, group.transparent);
      const key = `${group.geometry.id}:${group.transparent ? 1 : 0}`;
      let shared = sharedByKey.get(key);
      if (!shared) {
        shared = this.createSharedGeometryBuffers(group.geometry, prepared);
        sharedByKey.set(key, shared);
        this.sharedGeometryBuffers.push(shared);
        gpuBytes += shared.gpuBytes;
      }
      const batch = this.createInstancedBatch(group.geometry, prepared, group.records, group.transparent, shared);
      this.pushStreamBatch(batch);
      gpuBytes += batch.gpuBytes;
    }
    for (const group of plan.baked) {
      const items = group.items.map((item) => ({ ...item, prepared: prepare(item.geometry, group.transparent) }));
      const batch = this.createBakedBatch(items, group.color, group.vertexCount);
      this.pushStreamBatch(batch);
      gpuBytes += batch.gpuBytes;
    }
    this.gpuBufferBytes += gpuBytes;
    // The plan is a sweep over every record, so a stream refreshes it on a timer;
    // batches that arrive between plans wait for the next one, or the finish.
    const now = performance.now();
    if (count <= 2048 || now - this.streamDepthPlannedAt > 400) {
      this.planStreamDepth(count, isVisible);
      this.streamDepthPlannedAt = now;
    }
    if (!this.cameraTouched) this.fit(this.camera.mode);
    this.dirty = true;
  }

  /** Give streamed batches the stable material priority the finish will, so early orbiting stays steady. */
  planStreamDepth(count, isVisible = () => true) {
    const depthPlan = planDepthMaterials(this.renderColors, this.recordLocations, count, isVisible);
    this.applyDepthPlan(depthPlan);
  }

  applyDepthPlan(depthPlan) {
    this.depthRanks = depthPlan.ranks;
    this.depthContested = depthPlan.contested;
    this.depthConflictPairs = depthPlan.conflictPairs;
    this.contestedMaterials = depthPlan.contestedColors;
    this.depthPlanExhausted = Boolean(depthPlan.exhausted);
    this.depthOverlayPrecisionSafe = depthOverlayPrecisionSupported(this.renderBounds.radius);
    for (const batch of this.opaqueBatches) {
      batch.depthRank = this.depthRanks[batch.sourceRecord] ?? 0;
      batch.depthContested = batch.records.some((record) => this.depthContested[record] === 1);
    }
    this.contestedOpaqueBatches = this.opaqueBatches
      .filter((batch) => this.batchNeedsOverlay(batch))
      .sort((left, right) => left.depthRank - right.depthRank || left.sourceRecord - right.sourceRecord);
  }

  /** A batch joins the overlay when its bounds rule says so, or, once the plane analysis is in, when it holds contested triangles. */
  batchNeedsOverlay(batch) {
    if (batch.overlayResolved) return batch.overlay !== null;
    return batch.depthContested;
  }

  /**
   * Take the plane analysis (`findContestedTriangles`): every opaque batch gets an index
   * buffer holding only its triangles on a plane another product shares, and the overlay
   * redraws those instead of whole records. Batches built later fall back to the whole
   * record until the next analysis. An analysis that ran out of budget is refused, since
   * its empty table would switch the overlay off. Returns whether the table was taken.
   */
  applyContestedTriangles(result) {
    if (!this.pack || !result?.records || !result.offsets || !result.triangles || result.exhausted) return false;
    const byRecord = new Map();
    for (let i = 0; i < result.records.length; i += 1) byRecord.set(result.records[i], [result.offsets[i], result.offsets[i + 1]]);
    this.contestedTriangles = { byRecord, triangles: result.triangles };
    const mappings = new Map();
    for (const batch of this.opaqueBatches) this.buildOverlaySubset(batch, mappings);
    this.contestedOpaqueBatches = this.opaqueBatches
      .filter((batch) => this.batchNeedsOverlay(batch))
      .sort((left, right) => left.depthRank - right.depthRank || left.sourceRecord - right.sourceRecord);
    this.dirty = true;
    return true;
  }

  /** The overlay index buffer of one batch from the contested triangle table, or none. */
  buildOverlaySubset(batch, mappings = new Map()) {
    const gl = this.gl;
    if (batch.overlay) {
      gl.deleteBuffer(batch.overlay.buffer);
      batch.buffers = batch.buffers.filter((buffer) => buffer !== batch.overlay.buffer);
      batch.gpuBytes -= batch.overlay.bytes;
      this.gpuBufferBytes -= batch.overlay.bytes;
    }
    batch.overlay = null;
    batch.overlayResolved = true;
    const table = this.contestedTriangles;
    if (!table || batch.transparent) return;
    const out = [];
    for (const source of batch.wireSources ?? []) {
      const records = batch.baked ? [source.record] : batch.records;
      let mapping = mappings.get(source.indices);
      if (mapping === undefined) {
        mapping = preparedTriangleMap(source.geometry.indices, source.indices);
        mappings.set(source.indices, mapping);
      }
      const seen = batch.baked ? null : new Set();
      for (const record of records) {
        const range = table.byRecord.get(record);
        if (!range) continue;
        for (let at = range[0]; at < range[1]; at += 1) {
          const triangle = table.triangles[at];
          const prepared = mapping ? mapping[triangle] : triangle;
          if (prepared < 0 || prepared === undefined || prepared * 3 + 2 >= source.indices.length) continue;
          if (seen) {
            if (seen.has(prepared)) continue;
            seen.add(prepared);
          }
          const base = source.base;
          out.push(source.indices[prepared * 3] + base, source.indices[prepared * 3 + 1] + base, source.indices[prepared * 3 + 2] + base);
        }
      }
    }
    if (!out.length) return;
    const IndexArray = batch.indexType === gl.UNSIGNED_INT ? Uint32Array : Uint16Array;
    const indices = IndexArray.from(out);
    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, indices, gl.STATIC_DRAW);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, null);
    batch.overlay = { buffer, count: indices.length, bytes: indices.byteLength };
    batch.buffers.push(buffer);
    // On the batch as well, so a delta's recount of batch bytes keeps the overlay.
    batch.gpuBytes += indices.byteLength;
    this.gpuBufferBytes += indices.byteLength;
  }

  updateVisibleBounds(isVisible, count = this.pack?.instances.count ?? 0) {
    const bounds = emptyBounds();
    for (let record = 0; record < count; record += 1) {
      const location = this.recordLocations[record];
      if (location && isActiveRecord(this.pack, record) && isVisible(record)) includeBounds(bounds, location.bounds);
    }
    finishBounds(bounds);
    this.renderBounds = bounds;
    this.bounds = offsetBounds(bounds, this.renderOrigin);
    this.depthOverlayPrecisionSafe = depthOverlayPrecisionSupported(bounds.radius);
  }

  pushStreamBatch(batch) {
    batch.sortOrder = this.batches.length;
    batch.depthContested = false;
    this.batches.push(batch);
    if (batch.transparent) this.transparentBatches.push(batch);
    else this.opaqueBatches.push(batch);
  }

  /** Rebuild from the assembled pack, keeping a camera the user moved during the stream. */
  finishStream(pack, isVisible = () => true) {
    const touched = this.cameraTouched;
    const origin = this.renderOrigin.slice();
    const saved = {
      mode: this.camera.mode,
      up: this.camera.up.slice(),
      position: add(this.camera.position, origin),
      target: add(this.camera.target, origin),
      distance: this.camera.distance,
      orthoScale: this.camera.orthoScale,
    };
    this.streaming = false;
    const result = this.load(pack, isVisible);
    if (touched) {
      this.camera.mode = saved.mode;
      this.camera.up = saved.up;
      this.camera.position = sub(saved.position, this.renderOrigin);
      this.camera.target = sub(saved.target, this.renderOrigin);
      this.camera.distance = saved.distance;
      this.camera.orthoScale = saved.orthoScale;
      this.dirty = true;
    }
    return { ...result, cameraKept: touched };
  }

  /** Rebuild from a patched pack without moving the camera. */
  reload(pack, isVisible = () => true) {
    this.cameraTouched = true;
    return this.finishStream(pack, isVisible);
  }

  /** Replace affected bounded batches while keeping every other GPU allocation. */
  applyDelta(pack, delta, isVisible = () => true) {
    if (!this.pack) throw new Error("Load a model before applying a scene delta.");
    const previousOffset = this.pack.index.model_offset ?? [0, 0, 0];
    const nextOffset = pack.index.model_offset ?? [0, 0, 0];
    if (previousOffset.length !== nextOffset.length || previousOffset.some((value, axis) => value !== nextOffset[axis])) {
      throw new Error("A selective scene delta cannot change the model offset.");
    }
    isVisible = activePredicate(pack, isVisible);
    if (!delta.changed) return { ...this.deltaSummary(), patchStats: { reusedBatches: this.batches.length, rebuiltBatches: 0, createdBatches: 0, uploadedBytes: 0 } };
    const selectedIds = new Set(this.selected.map((record) => this.pack.instances.expressIds[record]));
    const count = pack.instances.count;
    if (!this.contextLost && count > Math.min(2 ** 24, this.gl.getParameter(this.gl.MAX_TEXTURE_SIZE) ** 2)) {
      throw new Error("This GPU cannot index more stable IFC slots; reopen a saved revision to reclaim retired slots.");
    }
    if (this.contextLost) {
      // Context restoration consumes this latest logical scene, including tombstones.
      this.pack = pack;
      this.visibilityPredicate = isVisible;
      this.recordLocations = Array(count).fill(null);
      this.pickTree = null;
      this.bounds = computeModelBounds(pack, isVisible);
      this.renderBounds = offsetBounds(this.bounds, scale(this.renderOrigin, -1));
      return { ...this.deltaSummary(), patchStats: { deferred: true, reusedBatches: 0, rebuiltBatches: 0, createdBatches: 0, uploadedBytes: 0 } };
    }

    const previous = {
      pack: this.pack, recordLocations: this.recordLocations, renderColors: this.renderColors,
      depthRanks: this.depthRanks, depthContested: this.depthContested,
    };
    const geometryById = geometryIndex(pack);
    const locations = Array(count).fill(null);
    for (let record = 0; record < count; record++) {
      if (!isActiveRecord(pack, record)) continue;
      const geometry = geometryById.get(pack.instances.geometryIds[record]);
      if (!geometry) continue;
      const bounds = transformedBounds(geometry.bbox, pack.instances.transforms, record * 16);
      if (validDepthBounds(bounds)) locations[record] = { geometry, bounds: offsetBounds(bounds, scale(this.renderOrigin, -1)) };
    }
    const colors = displayColors(pack, this.minimumAlpha, this.minimumLuminance);
    const allActive = (record) => isActiveRecord(pack, record);
    const basePlan = planDepthMaterials(colors, locations, count, allActive, { collectPairs: true });
    const depthPlan = restrictDepthPlan(basePlan, colors, count, isVisible) ??
      planDepthMaterials(colors, locations, count, isVisible);
    const removed = new Set(delta.removedRecords ?? []);
    const replacing = new Set();
    const records = new Set();
    for (const batch of this.batches) {
      if (!batch.records.some((record) => removed.has(record) || !isActiveRecord(pack, record) ||
          Boolean(previous.depthContested[record]) !== Boolean(depthPlan.contested[record]))) continue;
      replacing.add(batch);
      for (const record of batch.records) if (isActiveRecord(pack, record)) records.add(record);
    }
    const range = delta.appended ?? delta;
    for (let record = range.from; record < range.to; record++) if (isActiveRecord(pack, record)) records.add(record);
    const plan = planRenderBatches(pack, undefined, colors, null, {
      geometryById, contested: depthPlan.contested, recordLocations: locations, records,
    });
    const retained = this.batches.filter((batch) => !replacing.has(batch));
    const created = [], createdShared = [];
    const sharedByKey = new Map();
    for (const batch of this.batches) if (batch.sharedGeometry) {
      sharedByKey.set(`${batch.sharedGeometry.geometry.id}:${Number(batch.transparent)}`, batch.sharedGeometry);
    }
    this.pack = pack;
    this.recordLocations = locations;
    this.renderColors = colors;
    this.depthRanks = depthPlan.ranks;
    this.depthContested = depthPlan.contested;
    this.cancelWirePreparation();
    try {
      const prepare = (geometry, transparent) => this.prepareGeometry(geometry, this.suppressCoplanar && !transparent);
      for (const group of plan.instanced) {
        const prepared = prepare(group.geometry, group.transparent);
        const key = `${group.geometry.id}:${Number(group.transparent)}`;
        let shared = sharedByKey.get(key);
        if (!shared) {
          shared = this.createSharedGeometryBuffers(group.geometry, prepared);
          sharedByKey.set(key, shared);
          createdShared.push(shared);
        }
        created.push(this.createInstancedBatch(group.geometry, prepared, group.records, group.transparent, shared));
      }
      for (const group of plan.baked) {
        const items = group.items.map((item) => ({ ...item, prepared: prepare(item.geometry, group.transparent) }));
        created.push(this.createBakedBatch(items, group.color, group.vertexCount));
      }
    } catch (error) {
      for (const batch of created) this.releaseBatch(batch);
      for (const shared of createdShared) for (const buffer of shared.buffers) this.gl.deleteBuffer(buffer);
      Object.assign(this, previous);
      this.scheduleWirePreparation();
      throw error;
    }
    // Publish only once all new batches exist. Retired batches are bounded
    // by the existing batching limits; no inactive triangles remain allocated.
    const uploadedBytes = created.reduce((bytes, batch) => bytes + batch.gpuBytes, 0) +
      createdShared.reduce((bytes, shared) => bytes + shared.gpuBytes, 0);
    for (const batch of replacing) this.releaseBatch(batch);
    this.batches = [...retained, ...created].sort((left, right) => Number(left.transparent) - Number(right.transparent) || left.sourceRecord - right.sourceRecord);
    for (let i = 0; i < this.batches.length; i++) this.batches[i].sortOrder = i;
    const liveShared = new Set(this.batches.map((batch) => batch.sharedGeometry).filter(Boolean));
    for (const shared of this.sharedGeometryBuffers) if (!liveShared.has(shared)) {
      for (const buffer of shared.buffers) this.gl.deleteBuffer(buffer);
    }
    this.sharedGeometryBuffers = [...liveShared];
    this.opaqueBatches = this.batches.filter((batch) => !batch.transparent);
    this.transparentBatches = this.batches.filter((batch) => batch.transparent);
    this.sortedBatches = [];
    this.pickTree = buildBoundsTree(count, (record) => locations[record]?.bounds);
    this.pickCandidates.length = 0;
    this.visibilityPredicate = isVisible;
    this.rememberDepthPlan(basePlan, count, allActive);
    this.updateVisibleBounds(isVisible);
    this.applyDepthPlan(depthPlan);
    this.dimmedProducts = colors.raisedCount ?? 0;
    this.darkProducts = colors.brightenedCount ?? 0;
    this.ensureVisibilityCapacity(count);
    this.visibility.fill(0);
    this.baseVisible.fill(0);
    this.selected = [];
    this.flashState = null;
    for (let record = 0; record < count; record++) {
      this.baseVisible[record] = isVisible(record) ? 255 : 0;
      this.visibility[record * 2] = this.baseVisible[record];
      if (isActiveRecord(pack, record) && selectedIds.has(pack.instances.expressIds[record])) {
        this.selected.push(record);
        this.visibility[record * 2 + 1] = 255;
      }
    }
    this.selectionVersion += 1;
    this.lodState = null;
    this.lodHidden.fill(0);
    this.lodHiddenCount = 0;
    this.uploadVisibility();
    this.gpuBufferBytes = this.visibility.byteLength +
      this.batches.reduce((bytes, batch) => bytes + batch.gpuBytes, 0) +
      this.sharedGeometryBuffers.reduce((bytes, shared) => bytes + shared.gpuBytes, 0);
    this.scheduleWirePreparation();
    this.dirty = true;
    return { ...this.deltaSummary(), patchStats: { reusedBatches: retained.length, rebuiltBatches: replacing.size, createdBatches: created.length, uploadedBytes } };
  }

  releaseBatch(batch) {
    this.gl.deleteVertexArray(batch.vao);
    for (const buffer of batch.buffers) this.gl.deleteBuffer(buffer);
  }

  deltaSummary() {
    const baseDrawCalls = this.batches.length;
    const overlayDrawCalls = this.depthTieBreak && this.depthOverlayPrecisionSafe ? this.contestedOpaqueBatches.length : 0;
    const sources = new Set(), suppressed = new Map();
    let eagerWireBytesAvoided = 0;
    for (const batch of this.batches) for (const source of batch.wireSources ?? []) {
      sources.add(`${source.geometry.id}:${Number(batch.transparent)}`);
      const prepared = this.prepareGeometry(source.geometry, this.suppressCoplanar && !batch.transparent);
      suppressed.set(source.geometry.id, Math.max(suppressed.get(source.geometry.id) ?? 0, prepared.suppressedTriangles));
    }
    for (const owner of [...this.sharedGeometryBuffers, ...this.batches.filter((batch) => batch.baked)]) {
      if (!owner.wireBuffer) eagerWireBytesAvoided += owner.indexCount * indexByteWidth(owner.indexType, this.gl) * 2;
    }
    return {
      bounds: this.bounds, drawCalls: baseDrawCalls + overlayDrawCalls, baseDrawCalls, overlayDrawCalls,
      gpuBytes: this.currentGpuBytes(), instancedDrawCalls: this.batches.filter((batch) => !batch.baked).length,
      bakedDrawCalls: this.batches.filter((batch) => batch.baked).length,
      sourceDrawCalls: sources.size, suppressedTriangles: [...suppressed.values()].reduce((sum, count) => sum + count, 0),
      eagerWireBytesAvoided,
      depthConflictPairs: this.depthConflictPairs, contestedMaterials: this.contestedMaterials,
      depthOverlayPrecisionSafe: this.depthOverlayPrecisionSafe,
    };
  }

  /** Room in the visibility texture for at least `count` records. */
  ensureVisibilityCapacity(count) {
    if (count <= this.visibilityCapacity && this.visibilityTexture) return;
    const previous = this.visibility;
    const previousCount = this.visibilityCapacity;
    const previousBase = this.baseVisible;
    if (this.visibilityTexture) this.gl.deleteTexture(this.visibilityTexture);
    let capacity = Math.max(1, this.visibilityCapacity);
    while (capacity < count) capacity *= 2;
    this.createVisibilityTexture(capacity);
    if (previous && previousCount) {
      this.visibility.set(previous.subarray(0, previousCount * 2));
      this.baseVisible.set(previousBase.subarray(0, previousCount));
    }
    this.uploadVisibility();
  }

  createSharedGeometryBuffers(geometry, prepared) {
    const gl = this.gl;
    const { positions, indices } = prepared;
    const positionBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, positions, gl.STATIC_DRAW);
    const indexBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, indexBuffer);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, indices, gl.STATIC_DRAW);
    return {
      geometry,
      indices,
      positionBuffer,
      indexBuffer,
      indexCount: indices.length,
      indexType: indices instanceof Uint32Array ? gl.UNSIGNED_INT : gl.UNSIGNED_SHORT,
      wireBuffer: null,
      wireCount: 0,
      gpuBytes: positions.byteLength + indices.byteLength,
      buffers: [positionBuffer, indexBuffer],
    };
  }

  createInstancedBatch(geometry, prepared, records, transparent, shared) {
    const gl = this.gl;
    const { indices } = prepared;
    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);

    gl.bindBuffer(gl.ARRAY_BUFFER, shared.positionBuffer);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 0, 0);

    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, shared.indexBuffer);

    const matrices = new Float32Array(records.length * 16);
    const colors = new Uint8Array(records.length * 4);
    const ids = new Float32Array(records.length);
    for (let instance = 0; instance < records.length; instance += 1) {
      const record = records[instance];
      const matrix = this.pack.instances.transforms.subarray(record * 16, record * 16 + 16);
      matrices.set(matrix, instance * 16);
      matrices[instance * 16 + 12] = matrix[12] - this.renderOrigin[0];
      matrices[instance * 16 + 13] = matrix[13] - this.renderOrigin[1];
      matrices[instance * 16 + 14] = matrix[14] - this.renderOrigin[2];
      colors.set(this.renderColors.subarray(record * 4, record * 4 + 4), instance * 4);
      ids[instance] = record;
    }
    const sortBounds = emptyBounds();
    for (let instance = 0; instance < records.length; instance += 1) {
      includeBounds(sortBounds, transformedBounds(geometry.bbox, matrices, instance * 16));
    }
    const sortCenter = boundsCenter(sortBounds);
    const sortHalfExtents = boundsHalfExtents(sortBounds);

    const matrixBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, matrixBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, matrices, gl.STATIC_DRAW);
    for (let column = 0; column < 4; column += 1) {
      const location = 1 + column;
      gl.enableVertexAttribArray(location);
      gl.vertexAttribPointer(location, 4, gl.FLOAT, false, 64, column * 16);
      gl.vertexAttribDivisor(location, 1);
    }

    const colorBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, colorBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, colors, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(5);
    gl.vertexAttribPointer(5, 4, gl.UNSIGNED_BYTE, true, 0, 0);
    gl.vertexAttribDivisor(5, 1);

    const idBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, idBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, ids, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(6);
    gl.vertexAttribPointer(6, 1, gl.FLOAT, false, 0, 0);
    gl.vertexAttribDivisor(6, 1);

    gl.bindVertexArray(null);

    return {
      vao,
      baked: false,
      records: Uint32Array.from(records),
      color: null,
      depthRank: this.depthRanks[records[0]] ?? 0,
      depthContested: records.some((record) => this.depthContested[record] === 1),
      sourceRecord: records[0],
      sortCenter,
      sortHalfExtents,
      instanceCount: records.length,
      transparent,
      // Only a closed solid may be capped at a section plane; the kernel says
      // so per geometry, and an open shell has no inside to fill.
      closed: geometry?.closed === true,
      indexBuffer: shared.indexBuffer,
      indexCount: shared.indexCount,
      wireBuffer: null,
      wireCount: 0,
      wireSources: [{ geometry, indices, base: 0 }],
      sharedGeometry: shared,
      indexType: shared.indexType,
      gpuBytes: matrices.byteLength + colors.byteLength + ids.byteLength,
      buffers: [matrixBuffer, colorBuffer, idBuffer],
    };
  }

  createBakedBatch(items, color, vertexCount) {
    const gl = this.gl;
    const positions = new Float32Array(vertexCount * 3);
    const ids = new Float32Array(vertexCount);
    const indexCount = items.reduce((total, item) => total + item.prepared.indices.length, 0);
    const IndexArray = vertexCount > 65_535 ? Uint32Array : Uint16Array;
    const indices = new IndexArray(indexCount);
    const wireSources = [];
    const sortBounds = emptyBounds();
    let vertexOffset = 0;
    let indexOffset = 0;
    for (const item of items) {
      const source = item.prepared.positions;
      const matrix = this.pack.instances.transforms.subarray(item.record * 16, item.record * 16 + 16);
      transformPositions(positions, vertexOffset * 3, source, matrix, this.renderOrigin);
      ids.fill(item.record, vertexOffset, vertexOffset + source.length / 3);
      for (let index = 0; index < item.prepared.indices.length; index += 1) {
        indices[indexOffset + index] = item.prepared.indices[index] + vertexOffset;
      }
      wireSources.push({
        geometry: item.geometry,
        indices: item.prepared.indices,
        base: vertexOffset,
        record: item.record,
      });
      // Streaming skips a record whose transformed bounds are not finite.
      const location = this.recordLocations[item.record];
      if (location) includeBounds(sortBounds, location.bounds);
      vertexOffset += source.length / 3;
      indexOffset += item.prepared.indices.length;
    }
    // A group of only skipped records would otherwise sort on the empty sentinel.
    if (!Number.isFinite(sortBounds.min[0])) finishBounds(sortBounds);

    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    const positionBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, positionBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, positions, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 0, 0);
    const idBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, idBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, ids, gl.STATIC_DRAW);
    gl.enableVertexAttribArray(6);
    gl.vertexAttribPointer(6, 1, gl.FLOAT, false, 0, 0);
    const indexBuffer = gl.createBuffer();
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, indexBuffer);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, indices, gl.STATIC_DRAW);
    gl.bindVertexArray(null);

    return {
      vao,
      baked: true,
      records: Uint32Array.from(items, (item) => item.record),
      color: Float32Array.from(color, (value) => value / 255),
      depthRank: this.depthRanks[items[0]?.record] ?? 0,
      depthContested: items.some((item) => this.depthContested[item.record] === 1),
      sourceRecord: items[0]?.record ?? Number.MAX_SAFE_INTEGER,
      sortCenter: boundsCenter(sortBounds),
      sortHalfExtents: boundsHalfExtents(sortBounds),
      instanceCount: 1,
      transparent: color[3] < 255,
      // A baked batch is capped only when every mesh baked into it is closed.
      closed: items.every((item) => item.geometry?.closed === true),
      indexBuffer,
      indexCount,
      wireBuffer: null,
      wireCount: 0,
      wireSources,
      indexType: indices instanceof Uint32Array ? gl.UNSIGNED_INT : gl.UNSIGNED_SHORT,
      gpuBytes: positions.byteLength + indices.byteLength + ids.byteLength,
      buffers: [positionBuffer, indexBuffer, idBuffer],
    };
  }

  clear() {
    this.gpuPacing?.cancelRetry();
    const gl = this.gl;
    if (this.wheelQualityTimer) clearTimeout(this.wheelQualityTimer);
    this.wheelQualityTimer = 0;
    this.cancelWirePreparation();
    this.interacting = false;
    this.gestureScale = 1;
    this.drag = null;
    this.pointers.clear();
    for (const batch of this.batches) {
      gl.deleteVertexArray(batch.vao);
      for (const buffer of batch.buffers) gl.deleteBuffer(buffer);
    }
    for (const shared of this.sharedGeometryBuffers) {
      for (const buffer of shared.buffers) gl.deleteBuffer(buffer);
    }
    this.batches = [];
    this.opaqueBatches = [];
    this.contestedOpaqueBatches = [];
    this.transparentBatches = [];
    this.sortedBatches = [];
    this.sharedGeometryBuffers = [];
    this.contestedTriangles = null;
    this.gpuBufferBytes = 0;
    this.recordLocations = [];
    this.pickTree = null;
    this.pickCandidates.length = 0;
    this.pack = null;
    this.bounds = emptyBounds();
    this.renderOrigin = [0, 0, 0];
    this.renderBounds = emptyBounds();
    this.selected = [];
    if (this.visibilityTexture) gl.deleteTexture(this.visibilityTexture);
    this.visibilityTexture = null;
    this.visibility = new Uint8Array(0);
    this.depthRanks = new Uint32Array(0);
    this.depthContested = new Uint8Array(0);
    this.depthConflictPairs = 0;
    this.contestedMaterials = 0;
    this.depthPlanExhausted = false;
    this.depthPlanBase = null;
    this.depthOverlayPrecisionSafe = true;
    this.visibilityTextureWidth = 1;
    this.visibilityHeight = 1;
    this.visibilityCapacity = 0;
    this.streaming = false;
    this.renderColors = new Uint8Array(0);
    this.dimmedProducts = 0;
    this.darkProducts = 0;
    this.dirty = true;
  }

  dispose() {
    this.gpuPacing.dispose();
    if (this.wheelQualityTimer) clearTimeout(this.wheelQualityTimer);
    this.clearTimers();
    this.releaseRenderTarget();
    this.clear();
    this.gl.deleteProgram(this.surfaceProgram);
    this.gl.deleteProgram(this.sectionProgram);
    this.gl.deleteProgram(this.capProgram);
    this.gl.deleteVertexArray(this.capVao);
    this.gl.deleteBuffer(this.capBuffer);
    this.canvas.remove();
  }

  clearTimers() {
    if (this.resizeSettleTimer) clearTimeout(this.resizeSettleTimer);
    this.resizeSettleTimer = 0;
    if (this.interactionPrepareFrame) cancelAnimationFrame(this.interactionPrepareFrame);
    this.interactionPrepareFrame = 0;
    if (this.interactionPrepareTimer) clearTimeout(this.interactionPrepareTimer);
    this.interactionPrepareTimer = 0;
  }

  /** Follow the container. A run of size changes reallocates once, when it settles, unless `settleNow`. */
  resize(settleNow = false) {
    const cssWidth = Math.max(1, this.container.clientWidth);
    const cssHeight = Math.max(1, this.container.clientHeight);
    const devicePixelRatio = window.devicePixelRatio || 1;
    const nativeRatio = renderPixelRatio(
      cssWidth,
      cssHeight,
      devicePixelRatio * this.renderScale,
      this.pixelRatioLimit,
      this.renderPixelBudget,
      this.maxCanvasSize,
    );
    const ratio = nativeRatio;
    const width = Math.max(1, Math.min(this.maxCanvasSize[0], Math.floor(cssWidth * ratio)));
    const height = Math.max(1, Math.min(this.maxCanvasSize[1], Math.floor(cssHeight * ratio)));
    if (this.canvasCssWidth !== cssWidth || this.canvasCssHeight !== cssHeight) {
      this.canvas.style.width = `${cssWidth}px`;
      this.canvas.style.height = `${cssHeight}px`;
      this.canvasCssWidth = cssWidth;
      this.canvasCssHeight = cssHeight;
    }
    if (this.canvas.width !== width || this.canvas.height !== height) {
      const now = performance.now();
      if (!settleNow && now - this.backingChangedAt < RESIZE_BURST_MS) {
        // The browser scales the last frame into the new box until the size settles.
        if (this.resizeSettleTimer) clearTimeout(this.resizeSettleTimer);
        this.resizeSettleTimer = setTimeout(() => {
          this.resizeSettleTimer = 0;
          this.resize(true);
          this.onDirty?.();
        }, RESIZE_SETTLE_MS);
      } else {
        if (this.resizeSettleTimer) clearTimeout(this.resizeSettleTimer);
        this.resizeSettleTimer = 0;
        this.backingChangedAt = now;
        // The targets stay: a frame that fits draws into a corner of them, a larger one grows them.
        this.interactionPrepared = false;
        this.canvas.width = width;
        this.canvas.height = height;
        this.dirty = true;
      }
    }
    this.devicePixelRatio = devicePixelRatio;
    this.resizeDirty = false;
  }

  render(force = false) {
    if (this.contextLost) return;
    if (this.resizeDirty || this.resizeSettleTimer && force || this.devicePixelRatio !== (window.devicePixelRatio || 1)) {
      this.resize(force);
    }
    if (!force && !this.dirty && !this.flashState) return;
    const replacedBacking = this.canvas.width !== this.gpuPacingWidth || this.canvas.height !== this.gpuPacingHeight;
    const moving = this.interacting && (!this.drag || this.drag.moved || this.pointers.size > 1 || this.wheelQualityTimer);
    if (!this.gpuPacing.allow(force || replacedBacking, moving || this.streaming ? 2 : this.idleGpuLimit)) return;
    this.adaptMotionScale(performance.now());
    const gl = this.gl;
    const offscreen = this.ensureRenderTarget();
    if (this.chooseGestureScale() < 1 && !this.interacting && this.pack && !this.interactionPrepared && !this.interactionPrepareFrame && !this.interactionPrepareTimer) {
      // A task posted from inside the next frame runs after that frame is committed, so a
      // fold or resize pays for one target now and the gesture target after it has painted.
      this.interactionPrepareFrame = requestAnimationFrame(() => {
        this.interactionPrepareFrame = 0;
        this.interactionPrepareTimer = setTimeout(() => {
          this.interactionPrepareTimer = 0;
          if (!this.interacting && this.pack && !this.interactionPrepared && !this.contextLost) this.prepareInteractionTarget(false);
        }, 0);
      });
    }
    const width = offscreen ? this.target.frameWidth : this.canvas.width;
    const height = offscreen ? this.target.frameHeight : this.canvas.height;
    // The projection is built only after target creation settles the depth convention.
    this.updateCameraMatrices();
    this.applyLod();
    if (this.flashState) this.advanceFlash(performance.now());
    gl.bindFramebuffer(gl.FRAMEBUFFER, offscreen ? this.target.framebuffer : null);
    gl.viewport(0, 0, width, height);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT | gl.STENCIL_BUFFER_BIT);
    this.draw(false);
    if (offscreen) {
      if (!this.presentTarget()) {
        // Try the next target; the failed key stays skipped until the canvas resizes.
        this.targetFailures.add(this.target.key);
        this.deleteRenderTarget(this.target);
        this.dirty = true;
        return this.render(true);
      }
    }
    this.dirty = false;
    this.gpuPacingWidth = this.canvas.width;
    this.gpuPacingHeight = this.canvas.height;
    this.gpuPacing.committed();
  }

  bindProgram(program, locations, picking, projection) {
    const gl = this.gl;
    gl.useProgram(program);
    gl.uniformMatrix4fv(locations.uProjection, false, projection);
    gl.uniformMatrix4fv(locations.uView, false, this.view);
    gl.uniform3fv(locations.uCameraPosition, this.camera.position);
    gl.uniform1i(locations.uPicking, picking ? 1 : 0);
    gl.uniform1i(locations.uDepthOnly, 0);
    gl.uniform1i(locations.uStyle, this.style === "xray" ? 1 : this.style === "wire" ? 2 : 0);
    gl.uniform1i(locations.uSectionActive, this.section.active ? 1 : 0);
    gl.uniform1i(locations.uSectionAxis, this.section.axis);
    gl.uniform1f(locations.uSectionValue, this.sectionCut());
    gl.uniform1f(locations.uSectionSign, this.section.sign);
    gl.uniform1f(locations.uFogDensity, 0.085 / Math.max(this.bounds.radius * 2, 1));
    gl.uniform3fv(locations.uFogColor, this.background);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.visibilityTexture);
    gl.uniform1i(locations.uVisibility, 0);
    gl.uniform1i(locations.uVisibilityWidth, this.visibilityTextureWidth);
  }

  draw(picking, projection = this.projection) {
    const gl = this.gl;
    // Only sectioned views need fragment discard, which inhibits early depth rejection.
    this.program = this.section.active ? this.sectionProgram : this.surfaceProgram;
    this.uniforms = this.section.active ? this.sectionUniforms : this.surfaceUniforms;
    this.updateBatchVisibility();
    this.bindProgram(this.program, this.uniforms, picking, projection);
    const wire = !picking && this.style === "wire";
    const normalFill = !picking && !wire && this.style !== "xray";
    if (normalFill) {
      gl.disable(gl.BLEND);
      gl.disable(gl.POLYGON_OFFSET_FILL);
      gl.depthMask(true);
      this.applyDepthConvention();
      // The overlay supplies every visible sample of contested batches; their base pass only needs depth.
      const prepass = this.contestedDepthPrepass && this.depthTieBreak &&
        this.depthOverlayPrecisionSafe && this.contestedOpaqueBatches.length > 0;
      let depthOnly = false;
      for (const batch of this.opaqueBatches) {
        if (batch.culled) continue;
        const next = prepass && batch.depthContested && !batch.overlayResolved;
        if (next !== depthOnly) {
          gl.colorMask(!next, !next, !next, !next);
          gl.uniform1i(this.uniforms.uDepthOnly, next ? 1 : 0);
          depthOnly = next;
        }
        this.drawBatch(batch);
      }
      if (depthOnly) {
        gl.colorMask(true, true, true, true);
        gl.uniform1i(this.uniforms.uDepthOnly, 0);
      }

      if (this.depthTieBreak && this.depthOverlayPrecisionSafe && this.contestedOpaqueBatches.length) {
        const offset = depthOverlayOffset(this.reversedDepth);
        gl.enable(gl.POLYGON_OFFSET_FILL);
        gl.depthMask(false);
        gl.depthFunc(this.reversedDepth ? gl.GEQUAL : gl.LEQUAL);
        for (const batch of this.contestedOpaqueBatches) {
          if (batch.culled) continue;
          const maximumDepth = this.depthOverlayMaximumDepthForBatch(batch);
          this.applyOverlayOffset(offset, maximumDepth);
          this.drawBatch(batch, false, this.uniforms, Boolean(batch.overlay));
        }
        gl.disable(gl.POLYGON_OFFSET_FILL);
        gl.depthMask(true);
        this.applyDepthConvention();
      }

      this.drawSectionCap();

      if (this.transparentBatches.length) {
        gl.enable(gl.BLEND);
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        gl.depthMask(false);
        // Coincident glass passes strict depth only where rasterisation favours it.
        // The overlay's bounded step lets it blend over the opaque face everywhere.
        const bias = this.translucentTieBreak && this.depthOverlayPrecisionSafe;
        if (bias) gl.enable(gl.POLYGON_OFFSET_FILL);
        const offset = translucentDepthOffset(this.reversedDepth);
        for (const batch of this.orderedBatches(false)) {
          if (!batch.transparent) continue;
          if (bias) this.applyOverlayOffset(offset, this.depthOverlayMaximumDepthForBatch(batch));
          this.drawBatch(batch);
        }
        if (bias) {
          gl.disable(gl.POLYGON_OFFSET_FILL);
          gl.polygonOffset(0, 0);
        }
        gl.depthMask(true);
        gl.disable(gl.BLEND);
      }
      gl.bindVertexArray(null);
      return;
    }

    let blending = false;
    let depthWriting = true;
    gl.disable(gl.BLEND);
    gl.disable(gl.POLYGON_OFFSET_FILL);
    gl.depthMask(true);
    this.applyDepthConvention();
    for (const batch of this.orderedBatches(picking)) {
      if (batch.culled) continue;
      if (wire) this.ensureWireBuffer(batch);
      const translucent = !picking && (batch.transparent || this.style === "xray" || wire);
      if (translucent !== blending) {
        blending = translucent;
        if (blending) {
          gl.enable(gl.BLEND);
          gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
        } else {
          gl.disable(gl.BLEND);
        }
      }
      // Wire mode writes depth so shared edges do not accumulate alpha.
      const wantsDepthWrite = wire || !translucent;
      if (wantsDepthWrite !== depthWriting) {
        depthWriting = wantsDepthWrite;
        gl.depthMask(depthWriting);
      }
      this.drawBatch(batch, wire);
    }
    gl.bindVertexArray(null);
    if (!depthWriting) gl.depthMask(true);
    if (blending) gl.disable(gl.BLEND);
  }

  drawBatch(batch, wire = false, locations = this.uniforms, overlay = false) {
    if (batch.culled) return;
    const gl = this.gl;
    gl.bindVertexArray(batch.vao);
    gl.uniform1i(locations.uBaked, batch.baked ? 1 : 0);
    gl.uniform1i(locations.uPlainBatch, batch.allRecordsVisible && !batch.anyRecordSelected ? 1 : 0);
    if (batch.baked) gl.vertexAttrib4fv(5, batch.color);
    const subset = overlay && batch.overlay ? batch.overlay : null;
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, wire ? batch.wireBuffer : subset ? subset.buffer : batch.indexBuffer);
    gl.drawElementsInstanced(
      wire ? gl.LINES : gl.TRIANGLES,
      wire ? batch.wireCount : subset ? subset.count : batch.indexCount,
      batch.indexType,
      0,
      batch.instanceCount,
    );
  }

  /** One toward-camera step: clamped with the extension, whole depth units without it. */
  applyOverlayOffset(offset, maximumDepth) {
    const gl = this.gl;
    const direction = this.reversedDepth ? 1 : -1;
    if (this.polygonOffsetClamp) {
      this.polygonOffsetClamp.polygonOffsetClampEXT(
        offset.factor,
        offset.units,
        direction * fixedDepthClampFloor(quantiseDepthClamp(depthOverlayClamp(
          this.cameraDepthRange.near,
          this.cameraDepthRange.far,
          this.camera.mode === "perspective",
          DEPTH_OVERLAY_DISTANCE_TOLERANCE,
          maximumDepth,
        )), this.reversedDepth ? 0 : this.overlayDepthBits()),
      );
      return;
    }
    const envelope = quantiseDepthClamp(depthOverlayClamp(
      this.cameraDepthRange.near,
      this.cameraDepthRange.far,
      this.camera.mode === "perspective",
      DEPTH_OVERLAY_DISTANCE_TOLERANCE,
      maximumDepth,
    ));
    gl.polygonOffset(0, direction * depthOverlayFallbackUnits(envelope, this.overlayDepthBits()));
  }

  /** The depth precision the overlay is working against. */
  overlayDepthBits() {
    return this.target ? this.target.depthBits : this.display.depthBits;
  }

  depthOverlayMaximumDepthForBatch(batch) {
    const dx = batch.sortCenter[0] - this.camera.position[0];
    const dy = batch.sortCenter[1] - this.camera.position[1];
    const dz = batch.sortCenter[2] - this.camera.position[2];
    const centerDepth =
      dx * this.cameraForward[0] +
      dy * this.cameraForward[1] +
      dz * this.cameraForward[2];
    const halfExtents = batch.sortHalfExtents;
    // The AABB support point is conservative without inventing depth along thin axes.
    const projectedFarExtent = halfExtents
      ? Math.abs(this.cameraForward[0]) * halfExtents[0] +
        Math.abs(this.cameraForward[1]) * halfExtents[1] +
        Math.abs(this.cameraForward[2]) * halfExtents[2]
      : 0;
    return clamp(
      centerDepth + projectedFarExtent,
      this.cameraDepthRange.near,
      this.cameraDepthRange.far,
    );
  }

  /** Cache visibility between edits; camera changes only test one box per batch. */
  updateBatchVisibility() {
    viewSidePlanes(this.viewProjection, this.target?.frameWidth ?? this.canvas.width,
      this.target?.frameHeight ?? this.canvas.height, this.viewPlanes);
    for (const batch of this.batches) {
      if (batch.visibilityVersion !== this.visibilityVersion) {
        let visible = 0;
        for (const record of batch.records) if (isActiveRecord(this.pack, record) && this.visibility[record * 2] >= 128) visible += 1;
        batch.hasVisibleRecords = visible > 0;
        batch.allRecordsVisible = visible === batch.records.length;
        batch.visibilityVersion = this.visibilityVersion;
      }
      if (batch.selectionVersion !== this.selectionVersion) {
        batch.anyRecordSelected = (this.selected.length > 0 || this.flashState !== null) &&
          batch.records.some((record) => this.visibility[record * 2 + 1] !== 0);
        batch.selectionVersion = this.selectionVersion;
      }
      batch.culled = this.cullBatches && (
        !batch.hasVisibleRecords || !boxInView(batch.sortCenter, batch.sortHalfExtents, this.viewPlanes)
      );
    }
  }

  orderedBatches(picking) {
    if (picking || this.style === "wire") return this.batches;
    const sortAll = this.style === "xray";
    const source = sortAll ? this.batches : this.transparentBatches;
    if (source.length < 2) return source;
    const dx = this.camera.target[0] - this.camera.position[0];
    const dy = this.camera.target[1] - this.camera.position[1];
    const dz = this.camera.target[2] - this.camera.position[2];
    const inverseLength = 1 / Math.max(Math.hypot(dx, dy, dz), Number.EPSILON);
    const vx = dx * inverseLength;
    const vy = dy * inverseLength;
    const vz = dz * inverseLength;
    this.sortedBatches.length = 0;
    for (const batch of source) {
      if (batch.culled) continue;
      batch.sortDepth =
        (batch.sortCenter[0] - this.camera.position[0]) * vx +
        (batch.sortCenter[1] - this.camera.position[1]) * vy +
        (batch.sortCenter[2] - this.camera.position[2]) * vz;
      this.sortedBatches.push(batch);
    }
    this.sortedBatches.sort((left, right) => {
      const depthOrder = right.sortDepth - left.sortDepth;
      return Math.abs(depthOrder) > 1e-9 ? depthOrder : left.sortOrder - right.sortOrder;
    });
    return this.sortedBatches;
  }

  ensureWireBuffer(batch) {
    if (batch.wireBuffer) return;
    const shared = batch.sharedGeometry;
    if (shared?.wireBuffer) {
      batch.wireBuffer = shared.wireBuffer;
      batch.wireCount = shared.wireCount;
      return;
    }
    const gl = this.gl;
    const WireArray = batch.indexType === gl.UNSIGNED_INT ? Uint32Array : Uint16Array;
    // Three edges of two indices per triangle is the exact upper bound.
    let capacity = 0;
    for (const source of batch.wireSources) capacity += source.indices.length * 2;
    const wire = new WireArray(capacity);
    let cursor = 0;
    for (const source of batch.wireSources) {
      // Edge keys stay local to each source mesh; vertices are rebased by `source.base`.
      const edges = source.indices.length <= MAX_WIRE_DEDUPE_TRIANGLES * 3 ? createEdgeSet(source.indices.length) : null;
      // The shaded render view, so wire mode cannot resurrect a cancelled face.
      const indices = source.indices;
      for (let offset = 0; offset < indices.length; offset += 3) {
        const a = indices[offset];
        const b = indices[offset + 1];
        const c = indices[offset + 2];
        for (let edge = 0; edge < 3; edge += 1) {
          const first = edge === 0 ? a : edge === 1 ? b : c;
          const second = edge === 0 ? b : edge === 1 ? c : a;
          if (edges && !edges.add(Math.min(first, second), Math.max(first, second))) continue;
          wire[cursor] = first + source.base;
          wire[cursor + 1] = second + source.base;
          cursor += 2;
        }
      }
    }
    const wireIndices = wire.subarray(0, cursor);
    batch.wireBuffer = gl.createBuffer();
    batch.wireCount = wireIndices.length;
    if (shared) {
      shared.wireBuffer = batch.wireBuffer;
      shared.wireCount = batch.wireCount;
      shared.buffers.push(batch.wireBuffer);
      shared.gpuBytes += wireIndices.byteLength;
    } else {
      batch.buffers.push(batch.wireBuffer);
      batch.gpuBytes += wireIndices.byteLength;
    }
    this.gpuBufferBytes += wireIndices.byteLength;
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, batch.wireBuffer);
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, wireIndices, gl.STATIC_DRAW);
  }

  /** Build every batch's wire indices in idle slices while they stay small, so the first wireframe frame has them. */
  scheduleWirePreparation() {
    this.cancelWirePreparation();
    let indexBytes = 0;
    for (const batch of this.batches) indexBytes += batch.indexCount * indexByteWidth(batch.indexType, this.gl);
    if (indexBytes * 2 > WIRE_PREPARE_BYTE_LIMIT) return;
    const work = { pending: this.batches.filter((batch) => !batch.wireBuffer), index: 0, handle: null };
    const slice = () => {
      work.handle = null;
      if (this.wirePrepare !== work || this.contextLost) return;
      const until = performance.now() + WIRE_PREPARE_SLICE_MS;
      while (work.index < work.pending.length && performance.now() < until) {
        this.ensureWireBuffer(work.pending[work.index]);
        work.index += 1;
      }
      if (work.index < work.pending.length) work.handle = whenIdle(slice);
      else this.wirePrepare = null;
    };
    this.wirePrepare = work;
    work.handle = whenIdle(slice);
  }

  cancelWirePreparation() {
    const work = this.wirePrepare;
    if (!work) return;
    if (work.handle) cancelWhenIdle(work.handle);
    this.wirePrepare = null;
  }

  /** What the drawing buffer actually provides, for the telemetry panel. */
  displayInfo() {
    const target = this.target;
    // The offscreen target when one exists, the canvas otherwise.
    const depthBits = target ? target.depthBits : this.display.depthBits;
    const samples = target ? target.samples : this.display.samples;
    return {
      depthBits,
      samples,
      stencilBits: this.stencilBits,
      cappingActive: Boolean(this.cappingActive),
      maxSamples: this.display.maxSamples,
      offscreen: Boolean(target),
      targetSize: target ? [target.width, target.height] : null,
      frameSize: target ? [target.frameWidth, target.frameHeight] : [this.canvas.width, this.canvas.height],
      reversedDepth: Boolean(target) && this.reversedDepth,
      canvasDepthBits: this.display.depthBits,
      // 24 bits resolves a tenth of a millimetre at room distance; 16 does not.
      depthIsAdequate: depthBits >= 24,
      antialiasing: samples > 1,
      depthConflictPairs: this.depthConflictPairs,
      contestedMaterials: this.contestedMaterials,
      depthOverlayActive:
        this.depthTieBreak &&
        this.depthOverlayPrecisionSafe &&
        this.contestedOpaqueBatches.length > 0,
      depthOverlayPrecisionSafe: this.depthOverlayPrecisionSafe,
      depthOverlayTriangles: this.contestedTriangles ? this.contestedTriangles.triangles.length : null,
      depthOverlayClamped: Boolean(this.polygonOffsetClamp),
      depthPlanExhausted: this.depthPlanExhausted,
      translucentTieBreak: this.translucentTieBreak && this.depthOverlayPrecisionSafe,
      targetBytes: this.renderTargetBytes(),
      canvasBytes: this.canvasStorageBytes(),
      pixelRatio: this.canvas.width / Math.max(this.canvasCssWidth, 1),
    };
  }

  canvasStorageBytes() {
    const depthBytes = Math.max(2, Math.ceil(this.display.depthBits / 8));
    return this.canvas.width * this.canvas.height * (4 + depthBytes);
  }

  currentGpuBytes() {
    return this.gpuBufferBytes + this.renderTargetBytes() + this.canvasStorageBytes();
  }

  renderTargetBytes() {
    let bytes = 0;
    for (const target of this.targetCache.values()) bytes += target.estimatedBytes;
    return bytes;
  }

  createVisibilityTexture(recordCount) {
    const gl = this.gl;
    const maximum = gl.getParameter(gl.MAX_TEXTURE_SIZE);
    this.visibilityTextureWidth = Math.min(maximum, Math.max(1, recordCount));
    const height = Math.max(1, Math.ceil(recordCount / this.visibilityTextureWidth));
    if (height > maximum) {
      throw new Error(`This GPU cannot index ${recordCount} visible IFC records.`);
    }
    // Two bytes a record: drawn and selected; a selection spans a product's records.
    this.visibility = new Uint8Array(this.visibilityTextureWidth * height * 2);
    for (let at = 0; at < this.visibility.length; at += 2) this.visibility[at] = 255;
    this.baseVisible = new Uint8Array(this.visibilityTextureWidth * height);
    this.baseVisible.fill(255);
    this.lodHidden = new Uint8Array(this.baseVisible.length);
    this.lodHiddenCount = 0;
    this.lodState = null;
    this.visibilityHeight = height;
    this.visibilityCapacity = recordCount;
    this.visibilityTexture = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, this.visibilityTexture);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      gl.RG8,
      this.visibilityTextureWidth,
      height,
      0,
      gl.RG,
      gl.UNSIGNED_BYTE,
      this.visibility,
    );
    gl.bindTexture(gl.TEXTURE_2D, null);
  }

  uploadVisibility() {
    this.visibilityVersion += 1;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.visibilityTexture);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texSubImage2D(
      gl.TEXTURE_2D,
      0,
      0,
      0,
      this.visibilityTextureWidth,
      this.visibilityHeight,
      gl.RG,
      gl.UNSIGNED_BYTE,
      this.visibility,
    );
    gl.bindTexture(gl.TEXTURE_2D, null);
    this.dirty = true;
  }

  /** Dark or light canvas. A CSS hex colour, when given, replaces the built-in clear colour. */
  setViewportTheme(theme, color, capColor) {
    this.viewportTheme = theme === "light" ? "light" : "dark";
    this.background.set(parseHexColor(color) ?? CANVAS_COLORS[this.viewportTheme]);
    this.sectionCapColor =
      parseHexColor(capColor) ?? SECTION_CAP_COLORS[this.viewportTheme];
    this.gl.clearColor(...this.background, 1);
    this.dirty = true;
  }

  setPixelRatioLimit(limit) {
    this.pixelRatioLimit = clamp(Number(limit) || 1, 0.5, 3);
    this.resizeDirty = true;
    this.dirty = true;
  }

  /** Draw at this fraction of the display's native resolution. */
  setRenderScale(scaleValue) {
    this.renderScale = clamp(Number(scaleValue) || 1, 0.25, 2);
    this.resizeDirty = true;
    this.dirty = true;
  }

  setInteractionScale(scaleValue) {
    this.interactionScale = clamp(Number(scaleValue) || 1, 0.5, 1);
    this.interactionPrepared = false;
    this.dirty = true;
  }

  setStyle(style) {
    this.style = style;
    this.dirty = true;
  }

  setSection(active, axis, value, flipped = false, cap = this.section.cap !== false) {
    this.section.active = active;
    this.section.axis = { x: 0, y: 1, z: 2 }[axis] ?? 2;
    this.section.world = value;
    this.section.sign = flipped ? -1 : 1;
    this.section.cap = cap !== false;
    this.dirty = true;
  }

  /** Fill the cut through every closed solid, so a section reads as a solid. */
  drawSectionCap() {
    const gl = this.gl;
    this.cappingActive = false;
    if (!this.section.active || this.section.cap === false) return;
    if (!this.stencilBits || !this.capProgram) return;
    const cappable = this.opaqueBatches.filter((batch) => batch.closed && !batch.culled);
    if (!cappable.length) return;

    // A pixel with an unbalanced face count on the kept side is inside a cut solid.
    // Two-sided stencil: the renderer never culls, and a welded shell is wound consistently.
    gl.enable(gl.STENCIL_TEST);
    gl.colorMask(false, false, false, false);
    gl.depthMask(false);
    gl.disable(gl.DEPTH_TEST);
    gl.stencilFunc(gl.ALWAYS, 0, 0xff);
    gl.stencilOpSeparate(gl.BACK, gl.KEEP, gl.KEEP, gl.INCR_WRAP);
    gl.stencilOpSeparate(gl.FRONT, gl.KEEP, gl.KEEP, gl.DECR_WRAP);
    for (const batch of cappable) this.drawBatch(batch);

    // Then fill, once, wherever the count did not come back to zero.
    gl.enable(gl.DEPTH_TEST);
    gl.colorMask(true, true, true, true);
    gl.depthMask(true);
    gl.stencilFunc(gl.NOTEQUAL, 0, 0xff);
    gl.stencilOp(gl.KEEP, gl.KEEP, gl.KEEP);
    this.drawCapQuad();
    gl.disable(gl.STENCIL_TEST);
    gl.clearStencil(0);
    gl.clear(gl.STENCIL_BUFFER_BIT);
    gl.bindVertexArray(null);
    this.bindProgram(this.program, this.uniforms, false, this.projection);
    this.cappingActive = true;
  }

  /** A quad lying on the section plane, big enough to cover the model. */
  drawCapQuad() {
    const gl = this.gl;
    const axis = this.section.axis;
    const cut = this.sectionCut();
    const low = [0, 1, 2].map((i) => this.bounds.min[i] - this.renderOrigin[i]);
    const high = [0, 1, 2].map((i) => this.bounds.max[i] - this.renderOrigin[i]);
    // A margin, so a solid touching the model's own bounding box is still covered.
    const margin = Math.max(1, high[0] - low[0], high[1] - low[1], high[2] - low[2]);
    const other = [0, 1, 2].filter((i) => i !== axis);
    const corner = (u, v) => {
      const point = [0, 0, 0];
      point[axis] = cut;
      point[other[0]] = u ? high[other[0]] + margin : low[other[0]] - margin;
      point[other[1]] = v ? high[other[1]] + margin : low[other[1]] - margin;
      return point;
    };
    const quad = new Float32Array([
      ...corner(0, 0), ...corner(1, 0), ...corner(1, 1),
      ...corner(0, 0), ...corner(1, 1), ...corner(0, 1),
    ]);
    gl.useProgram(this.capProgram);
    gl.bindVertexArray(this.capVao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.capBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, quad, gl.DYNAMIC_DRAW);
    gl.uniformMatrix4fv(this.capUniforms.uProjection, false, this.projection);
    const colour = this.sectionCapColor ?? SECTION_CAP_COLORS[this.viewportTheme];
    gl.uniform3f(this.capUniforms.uColor, colour[0], colour[1], colour[2]);
    gl.drawArrays(gl.TRIANGLES, 0, 6);
  }

  /** The cut in render space, so the GPU and the pick ray cannot drift apart. */
  sectionCut() {
    return this.section.world - this.renderOrigin[this.section.axis];
  }

  sectionValue(axis, fraction) {
    const index = { x: 0, y: 1, z: 2 }[axis] ?? 2;
    return mix(this.bounds.min[index], this.bounds.max[index], fraction);
  }

  setVisibility(predicate) {
    predicate = activePredicate(this.pack, predicate);
    this.visibilityPredicate = predicate;
    if (!this.pack || !this.visibilityTexture) return;
    for (let record = 0; record < this.pack.instances.count; record += 1) {
      this.baseVisible[record] = predicate(record) ? 255 : 0;
      this.visibility[record * 2] = this.baseVisible[record];
    }
    this.lodState = null;
    this.updateVisibleBounds(predicate);
    this.applyDepthPlan(this.depthPlanFor(predicate));
    this.uploadVisibility();
  }

  /** Pixels per metre at one metre for the current camera, or null when there is no scale. */
  pixelsPerMetre(viewportHeight) {
    if (this.camera.mode === "perspective") {
      return viewportHeight / (2 * Math.tan(this.camera.fov / 2));
    }
    return viewportHeight / Math.max(2 * this.camera.orthoScale, 1e-6);
  }

  /**
   * Stop drawing products narrower than `lodPixels` on screen. The answer only
   * changes once the camera has moved a meaningful fraction of the way toward
   * them, so the sweep is skipped on most frames of an orbit.
   */
  applyLod() {
    if (!this.pack || !this.visibilityTexture) return false;
    const count = this.pack.instances.count;
    const height = this.target?.frameHeight ?? this.canvas.height;
    const threshold = this.lodPixels;
    if (!threshold && !this.lodHiddenCount) return false;
    const state = this.lodState;
    // Projected size changes with distance, so far from the model a long step
    // still cannot change any answer.
    const away = Math.hypot(
      this.camera.position[0] - this.renderBounds.center[0],
      this.camera.position[1] - this.renderBounds.center[1],
      this.camera.position[2] - this.renderBounds.center[2],
    );
    const epsilon = Math.max(0.25, this.renderBounds.radius * 0.01, away * 0.02);
    if (
      state && state.threshold === threshold && state.height === height &&
      state.mode === this.camera.mode && state.orthoScale === this.camera.orthoScale &&
      state.version === this.visibilityVersion &&
      Math.hypot(
        state.position[0] - this.camera.position[0],
        state.position[1] - this.camera.position[1],
        state.position[2] - this.camera.position[2],
      ) < epsilon
    ) return false;

    const scale = this.pixelsPerMetre(height);
    const [px, py, pz] = this.camera.position;
    const perspective = this.camera.mode === "perspective";
    let changed = 0;
    for (let record = 0; record < count; record += 1) {
      const location = this.recordLocations[record];
      let hide = 0;
      // A selected product stays drawn, so a tree click never highlights nothing.
      if (threshold && location && this.baseVisible[record] && !this.visibility[record * 2 + 1]) {
        const { min, max } = location.bounds;
        const radius = 0.5 * Math.hypot(max[0] - min[0], max[1] - min[1], max[2] - min[2]);
        let pixels;
        if (perspective) {
          const away = Math.hypot((min[0] + max[0]) / 2 - px, (min[1] + max[1]) / 2 - py, (min[2] + max[2]) / 2 - pz);
          // Inside the product's own sphere it fills the view; the frustum owns that case.
          pixels = away <= radius ? Infinity : (radius * 2 * scale) / away;
        } else {
          pixels = radius * 2 * scale;
        }
        hide = pixels < threshold ? 1 : 0;
      }
      if (hide === this.lodHidden[record]) continue;
      this.lodHidden[record] = hide;
      this.lodHiddenCount += hide ? 1 : -1;
      this.visibility[record * 2] = hide ? 0 : this.baseVisible[record];
      changed += 1;
    }
    this.lodState = {
      threshold, height, mode: this.camera.mode, orthoScale: this.camera.orthoScale,
      position: this.camera.position.slice(), version: this.visibilityVersion,
    };
    if (!changed) return false;
    this.uploadVisibility();
    this.lodState.version = this.visibilityVersion;
    return true;
  }

  /** Off draws every coincident face once, which is faster and can flicker. */
  setDepthTieBreak(active) {
    const wanted = active !== false;
    if (wanted === this.depthTieBreak) return;
    this.depthTieBreak = wanted;
    this.dirty = true;
  }

  /** Below this projected width a product is left out; zero draws them all. */
  setLodPixels(pixels) {
    const value = Math.max(0, Number(pixels) || 0);
    if (value === this.lodPixels) return;
    this.lodPixels = value;
    this.lodState = null;
    this.dirty = true;
  }

  /** The plan for a visible set: read off the load-time pairs when it is a subset, planned in full otherwise. */
  depthPlanFor(isVisible) {
    const count = this.pack.instances.count;
    const base = this.depthPlanBase;
    if (base && base.visible.length === count) {
      let subset = true;
      for (let record = 0; record < count && subset; record += 1) {
        if (!base.visible[record] && isVisible(record)) subset = false;
      }
      const plan = subset ? restrictDepthPlan(base.plan, this.renderColors, count, isVisible) : null;
      if (plan) return plan;
    }
    const plan = planDepthMaterials(this.renderColors, this.recordLocations, count, isVisible, { collectPairs: true });
    this.rememberDepthPlan(plan, count, isVisible);
    return plan;
  }

  rememberDepthPlan(plan, count, isVisible) {
    if (!plan.pairs) {
      this.depthPlanBase = null;
      return;
    }
    const visible = new Uint8Array(count);
    for (let record = 0; record < count; record += 1) visible[record] = isVisible(record) ? 1 : 0;
    this.depthPlanBase = { plan, visible };
  }

  /** Highlight these records only; a product is one record per material, so a list is taken. */
  select(records) {
    if (!this.visibilityTexture) return;
    for (const record of this.selected) this.visibility[record * 2 + 1] = 0;
    this.selected = recordList(records).filter((record) => isActiveRecord(this.pack, record) && record * 2 + 1 < this.visibility.length);
    for (const record of this.selected) this.visibility[record * 2 + 1] = 255;
    this.selectionVersion += 1;
    if (this.flashState) this.advanceFlash(performance.now());
    else this.uploadVisibility();
  }

  /** Light these records up and fade them back over `duration` ms; frames follow while it runs. */
  flash(records, duration = 900) {
    if (!this.visibilityTexture) return;
    const list = recordList(records).filter((record) => isActiveRecord(this.pack, record) && record * 2 + 1 < this.visibility.length);
    if (!list.length) return;
    if (this.flashState) for (const record of this.flashState.records) if (this.visibility[record * 2 + 1] !== 255) this.visibility[record * 2 + 1] = 0;
    this.flashState = { records: list, started: performance.now(), duration: Math.max(1, duration) };
    this.advanceFlash(this.flashState.started);
  }

  /** Whether a highlight fade still needs frames. */
  get animating() {
    return this.flashState !== null;
  }

  advanceFlash(now) {
    const flash = this.flashState;
    if (!flash) return;
    const progress = Math.min(1, (now - flash.started) / flash.duration);
    // Selection uses the full byte; the fade lives in the lower half so the shader can tell them apart.
    const level = progress >= 1 ? 0 : Math.max(1, Math.round(127 * (1 - progress) ** 2));
    for (const record of flash.records) if (this.visibility[record * 2 + 1] !== 255) this.visibility[record * 2 + 1] = level;
    if (progress >= 1) this.flashState = null;
    this.selectionVersion += 1;
    this.uploadVisibility();
  }

  setView(mode, fit = true) {
    this.camera.mode = mode;
    if (mode === "top") this.camera.up = [0, 1, 0];
    else this.camera.up = [0, 0, 1];
    if (fit) this.fit(mode);
    this.dirty = true;
  }

  fit(mode = this.camera.mode) {
    if (!this.pack) return;
    const center = this.renderBounds.center.slice();
    this.resize();
    const radius = Math.max(this.renderBounds.radius, 0.002);
    const framing = frameSphere(radius, this.camera.fov, this.canvasCssWidth / Math.max(this.canvasCssHeight, 1));
    this.camera.mode = mode;
    this.camera.target = center;
    this.camera.orthoScale = framing.orthoScale;
    const distance = mode === "perspective" ? framing.distance : radius * 3;
    this.camera.distance = distance;
    if (mode === "top") {
      this.camera.position = [center[0], center[1], center[2] + radius * 3];
      this.camera.up = [0, 1, 0];
    } else if (mode === "front") {
      this.camera.position = [center[0], center[1] - radius * 3, center[2]];
      this.camera.up = [0, 0, 1];
    } else if (mode === "right") {
      this.camera.position = [center[0] + radius * 3, center[1], center[2]];
      this.camera.up = [0, 0, 1];
    } else {
      this.camera.position = add(center, scale(normalize([0.82, -0.92, 0.68]), distance));
      this.camera.up = [0, 0, 1];
    }
    this.dirty = true;
    this.onCameraChange?.();
  }

  focus(records) {
    const bbox = emptyBounds();
    for (const record of recordList(records)) {
      if (!isActiveRecord(this.pack, record)) continue;
      const location = this.recordLocations[record];
      if (location) includeBounds(bbox, location.bounds);
    }
    if (!Number.isFinite(bbox.min[0])) return;
    const center = boundsCenter(bbox);
    this.resize();
    const radius = Math.max(boundsRadius(bbox), 0.002);
    const framing = frameSphere(radius, this.camera.fov, this.canvasCssWidth / Math.max(this.canvasCssHeight, 1), 1.35);
    const direction = normalize(sub(this.camera.position, this.camera.target));
    this.camera.target = center;
    if (this.camera.mode === "perspective") {
      this.camera.distance = framing.distance;
      this.camera.position = add(center, scale(direction, this.camera.distance));
    } else {
      this.camera.orthoScale = framing.orthoScale;
      this.camera.distance = radius * 3;
      this.camera.position = add(center, scale(direction, radius * 3));
    }
    this.dirty = true;
    this.onCameraChange?.();
  }

  recordPosition(record) {
    const at = record * 16;
    const matrix = this.pack?.instances.transforms;
    return matrix ? [matrix[at + 12], matrix[at + 13], matrix[at + 14]] : [0, 0, 0];
  }

  /** Pack-space bounds of these records, or `null`; add `index.model_offset` for IFC coordinates. */
  recordBounds(records) {
    const bbox = emptyBounds();
    for (const record of recordList(records)) {
      if (!isActiveRecord(this.pack, record)) continue;
      const location = this.recordLocations[record];
      if (location) includeBounds(bbox, location.bounds);
    }
    if (!Number.isFinite(bbox.min[0])) return null;
    return {
      min: add(bbox.min, this.renderOrigin),
      max: add(bbox.max, this.renderOrigin),
    };
  }

  pick(clientX, clientY, includePoint = true) {
    if (!this.pack) return null;
    const ray = this.pointerRay(clientX, clientY);
    const hit = this.pickRecord(ray);
    if (!hit) return null;
    const record = hit.record;
    const point = includePoint ? this.intersectRecord(record, clientX, clientY, hit.point) : null;
    return { record, point };
  }

  /** The surface under a pointer: record, hit point and its triangle, in pack space. */
  pickSurface(clientX, clientY) {
    if (!this.pack) return null;
    const hit = this.pickRecord(this.pointerRay(clientX, clientY));
    if (!hit?.point) return null;
    return { record: hit.record, point: hit.point, triangle: hit.triangle ?? null };
  }

  /** Where a pack-space point lands on the canvas in CSS pixels, using the last frame's matrices. */
  project(point) {
    return projectPoint(
      this.viewProjection,
      this.renderOrigin,
      point,
      this.canvasCssWidth,
      this.canvasCssHeight,
    );
  }

  pointerRay(clientX, clientY) {
    this.updateCameraMatrices();
    const rect = this.canvas.getBoundingClientRect();
    const nx = clamp(((clientX - rect.left) / Math.max(rect.width, 1)) * 2 - 1, -1, 1);
    const ny = clamp(1 - ((clientY - rect.top) / Math.max(rect.height, 1)) * 2, -1, 1);
    // Reversed depth puts near at +1 and far at 0; the fallback keeps -1 to +1.
    const nearClip = this.reversedDepth ? 1 : -1;
    const farClip = this.reversedDepth ? 0 : 1;
    const near = unproject(this.inverseViewProjection, nx, ny, nearClip);
    const far = unproject(this.inverseViewProjection, nx, ny, farClip);
    return { origin: near, direction: normalize(sub(far, near)) };
  }

  pickRecord(ray) {
    // Large declared bounds over sparse meshes defeat the early exit; this bounds one click.
    const MAX_NARROW_RECORDS = 512;
    let tested = 0;
    const candidates = [];
    const records = this.pickTree
      ? queryBoundsTree(this.pickTree, ray.origin, ray.direction, this.pickCandidates)
      : null;
    const count = records ? records.length : this.recordLocations.length;
    for (let at = 0; at < count; at += 1) {
      const record = records ? records[at] : at;
      const location = this.recordLocations[record];
      if (!isActiveRecord(this.pack, record) || !location || this.visibility[record * 2] < 128) continue;
      const distance = rayBounds(ray.origin, ray.direction, location.bounds);
      if (distance !== null) candidates.push({ record, distance });
    }
    candidates.sort((left, right) => left.distance - right.distance);
    let closest = Infinity;
    let closestPriority = Number.NEGATIVE_INFINITY;
    let result = null;
    for (const candidate of candidates) {
      const broadTolerance = pickDepthTieTolerance(
        this.renderBounds.radius,
        ray.origin,
        closest,
      );
      if (candidate.distance > closest + broadTolerance) break;
      if (tested >= MAX_NARROW_RECORDS) break;
      tested += 1;
      const hit = this.intersectRecordRay(candidate.record, ray);
      const priority = this.depthTieBreak && this.depthOverlayPrecisionSafe && this.depthContested[candidate.record]
        ? (this.depthRanks[candidate.record] ?? 0)
        : 0;
      const hitTolerance = hit
        ? pickDepthTieTolerance(
            this.renderBounds.radius,
            ray.origin,
            Math.max(Number.isFinite(closest) ? closest : 0, hit.distance),
          )
        : broadTolerance;
      if (hit && preferDepthHit(hit.distance, priority, closest, closestPriority, hitTolerance)) {
        closest = hit.distance;
        closestPriority = priority;
        result = {
          record: candidate.record,
          point: add(hit.point, this.renderOrigin),
          triangle: hit.triangle?.map((corner) => add(corner, this.renderOrigin)) ?? null,
        };
      }
    }
    return result;
  }

  intersectRecord(record, clientX, clientY, knownPoint = null) {
    if (!isActiveRecord(this.pack, record)) return null;
    if (knownPoint) return knownPoint;
    const hit = this.intersectRecordRay(record, this.pointerRay(clientX, clientY));
    return hit ? add(hit.point, this.renderOrigin) : null;
  }

  intersectRecordRay(record, ray) {
    if (!isActiveRecord(this.pack, record)) return null;
    const location = this.recordLocations[record];
    if (!location) return null;
    const geometry = location.geometry;
    const transform = renderTransform(this.pack.instances.transforms, record, this.renderOrigin);
    const inverse = mat4();
    if (!invert(inverse, transform)) return null;
    const localNear = transformPoint(inverse, ray.origin);
    const localFar = transformPoint(inverse, add(ray.origin, ray.direction));
    const direction = normalize(sub(localFar, localNear));
    let closest = Infinity;
    let hit = null;
    let hitIndex = -1;
    const sectionCut = this.sectionCut();
    for (let index = 0; index < geometry.indices.length; index += 3) {
      const distance = rayTriangleIndexed(
        localNear,
        direction,
        geometry.positions,
        geometry.indices[index],
        geometry.indices[index + 1],
        geometry.indices[index + 2],
      );
      if (distance !== null) {
        const localPoint = add(localNear, scale(direction, distance));
        const point = transformPoint(transform, localPoint);
        const coordinate = point[this.section.axis];
        const clipped =
          this.section.active && (coordinate - sectionCut) * this.section.sign > 0;
        const worldDistance = dot(sub(point, ray.origin), ray.direction);
        if (!clipped && worldDistance > 0 && worldDistance < closest) {
          closest = worldDistance;
          hit = point;
          hitIndex = index;
        }
      }
    }
    if (!hit) return null;
    // The winning triangle's corners in render space, for snapping.
    const triangle = [0, 1, 2].map((corner) => {
      const at = geometry.indices[hitIndex + corner] * 3;
      return transformPoint(transform, [
        geometry.positions[at],
        geometry.positions[at + 1],
        geometry.positions[at + 2],
      ]);
    });
    return { point: hit, distance: closest, triangle };
  }

  /**
   * Move the orbit centre to the depth of a pack-space point without turning
   * the camera: the target slides along the view axis, so zoom and orbit work
   * around what was clicked even when the model centre lies in empty space.
   */
  setPivot(point) {
    if (!this.pack || !Array.isArray(point) || point.length < 3 || !point.every(Number.isFinite)) return false;
    const local = sub(point, this.renderOrigin);
    const forward = normalize(sub(this.camera.target, this.camera.position));
    const depth = dot(sub(local, this.camera.position), forward);
    if (!(depth > 0.001) || !Number.isFinite(depth)) return false;
    this.camera.target = add(this.camera.position, scale(forward, depth));
    this.camera.distance = depth;
    this.cameraTouched = true;
    this.onCameraChange?.();
    return true;
  }

  /** Zoom along the view direction; the orbit target stays put, so rotation stays centred. */
  zoomAt(factor) {
    if (!this.pack || factor === 1) return;
    if (zoomCamera(this.camera, factor)) {
      this.cameraTouched = true;
      this.dirty = true;
      this.onCameraChange?.();
    }
  }

  chooseGestureScale() {
    if (!this.adaptiveResolution) return this.interactionScale;
    return Math.min(this.interactionScale, MOTION_SCALES[this.motionStep] ?? 1);
  }

  /** Only a gesture that never went slow earns a step back, so a heavy model
   *  does not stutter at the start of every drag. */
  beginInteraction() {
    if (this.interacting) return;
    if (this.motionSteady) this.motionStep = Math.max(0, this.motionStep - 1);
    this.motionSteady = true;
    this.slowFrames = 0;
    this.lastMotionFrameAt = 0;
    this.gestureScale = this.chooseGestureScale();
    this.interacting = true;
  }

  /**
   * Drop a step while a gesture is not holding its frame rate. Steps only ever
   * go down inside one gesture, so the scale cannot oscillate mid-drag.
   */
  adaptMotionScale(now) {
    if (!this.adaptiveResolution || !this.interacting) {
      this.lastMotionFrameAt = 0;
      this.slowFrames = 0;
      return;
    }
    const previous = this.lastMotionFrameAt;
    this.lastMotionFrameAt = now;
    if (!previous) return;
    const period = now - previous;
    const wanted = period > MOTION_VERY_SLOW_MS ? 2 : period > MOTION_SLOW_MS ? 1 : 0;
    if (wanted > 0) this.motionSteady = false;
    if (wanted <= this.motionStep) {
      this.slowFrames = 0;
      return;
    }
    // The gentlest verdict in the run wins, so one spike inside it cannot overshoot.
    this.slowStep = this.slowFrames ? Math.min(this.slowStep, wanted) : wanted;
    this.slowFrames += 1;
    if (this.slowFrames < MOTION_SLOW_FRAMES) return;
    this.slowFrames = 0;
    this.motionStep = this.slowStep;
    this.gestureScale = this.chooseGestureScale();
  }

  /** Off keeps every gesture at full size; on trades pixels for frames. */
  setAdaptiveResolution(active) {
    const wanted = active !== false;
    if (wanted === this.adaptiveResolution) return;
    this.adaptiveResolution = wanted;
    if (!wanted) this.motionStep = 0;
    this.gestureScale = this.chooseGestureScale();
    this.interactionPrepared = false;
    this.dirty = true;
  }

  bindControls() {
    this.canvas.addEventListener("webglcontextlost", (event) => {
      // Without preventDefault the browser never offers a restore.
      event.preventDefault();
      this.handleContextLost();
    });
    this.canvas.addEventListener("webglcontextrestored", () => this.handleContextRestored());
    this.canvas.addEventListener("contextmenu", (event) => event.preventDefault());
    this.canvas.addEventListener("pointerdown", (event) => {
      if (event.button > 2) return;
      this.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      this.canvas.setPointerCapture(event.pointerId);
      this.beginInteraction();
      this.drag = {
        x: event.clientX,
        y: event.clientY,
        startX: event.clientX,
        startY: event.clientY,
        moved: false,
        pan: event.button !== 0 || event.shiftKey || this.camera.mode !== "perspective",
      };
    });
    this.canvas.addEventListener("pointermove", (event) => {
      if (!this.drag) return;
      if (!this.pointers.has(event.pointerId)) return;
      const before = [...this.pointers.values()];
      this.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      if (this.pointers.size > 1) {
        const after = [...this.pointers.values()];
        const span = (points) => Math.hypot(points[0].x - points[1].x, points[0].y - points[1].y);
        const cx = (after[0].x + after[1].x) / 2;
        const cy = (after[0].y + after[1].y) / 2;
        const nextSpan = span(after);
        if (nextSpan > 2 && span(before) > 2) this.zoomAt(span(before) / nextSpan);
        this.pan(cx - (before[0].x + before[1].x) / 2, cy - (before[0].y + before[1].y) / 2);
        this.cameraTouched = true;
        return;
      }
      const dx = event.clientX - this.drag.x;
      const dy = event.clientY - this.drag.y;
      this.drag.x = event.clientX;
      this.drag.y = event.clientY;
      this.drag.moved ||= Math.hypot(event.clientX - this.drag.startX, event.clientY - this.drag.startY) > 1;
      if (this.drag.moved) this.cameraTouched = true;
      if (this.drag.pan) this.pan(dx, dy);
      else this.orbit(dx, dy);
    });
    const releasePointer = (event) => {
      if (!this.pointers.has(event.pointerId)) return;
      this.pointers.delete(event.pointerId);
      const remaining = this.pointers.values().next().value;
      this.drag = remaining ? { ...remaining, startX: remaining.x, startY: remaining.y, moved: true, pan: this.camera.mode !== "perspective" } : null;
      this.interacting = this.pointers.size > 0 || Boolean(this.wheelQualityTimer);
      if (!this.interacting && this.target?.slot === "gesture") this.dirty = true;
      // Notify the scheduler of the boundary without redrawing an unchanged full-quality frame.
      this.onCameraChange?.();
    };
    this.canvas.addEventListener("pointerup", releasePointer);
    this.canvas.addEventListener("pointercancel", releasePointer);
    this.canvas.addEventListener("lostpointercapture", releasePointer);
    this.canvas.addEventListener(
      "wheel",
      (event) => {
        event.preventDefault();
        const factor = wheelZoomFactor(event.deltaY, event.deltaMode, this.canvasCssHeight);
        if (factor === 1 || !this.pack) return;
        this.beginInteraction();
        this.zoomAt(factor);
        this.cameraTouched = true;
        if (this.wheelQualityTimer) clearTimeout(this.wheelQualityTimer);
        this.wheelQualityTimer = setTimeout(() => {
          this.wheelQualityTimer = 0;
          this.interacting = this.pointers.size > 0;
          if (!this.interacting && this.target?.slot === "gesture") this.dirty = true;
          this.onCameraChange?.();
        }, 120);
      },
      { passive: false },
    );
  }

  orbit(dx, dy) {
    if (dx === 0 && dy === 0) return;
    const offset = sub(this.camera.position, this.camera.target);
    let radius = Math.max(length(offset), 0.001);
    let azimuth = Math.atan2(offset[1], offset[0]) - dx * 0.006;
    // Vertical drag direction follows the common orbit-camera convention.
    let polar = Math.acos(clamp(offset[2] / radius, -1, 1)) - dy * 0.006;
    polar = clamp(polar, 0.03, Math.PI - 0.03);
    this.camera.position = [
      this.camera.target[0] + radius * Math.sin(polar) * Math.cos(azimuth),
      this.camera.target[1] + radius * Math.sin(polar) * Math.sin(azimuth),
      this.camera.target[2] + radius * Math.cos(polar),
    ];
    this.camera.distance = radius;
    this.dirty = true;
    this.onCameraChange?.();
  }

  /** The camera axes in world space, for the orientation gizmo. */
  cameraBasis() {
    const forward = normalize(sub(this.camera.target, this.camera.position));
    const right = normalize(cross(forward, this.camera.up));
    return { forward, right, up: normalize(cross(right, forward)) };
  }

  /** Look at the target from `direction`, keeping the distance and the mode. */
  viewAlong(direction) {
    const away = normalize(direction);
    if (!Number.isFinite(away[0])) return;
    // Looking straight down needs a different up vector, or the basis degenerates.
    this.camera.up = Math.abs(away[2]) > 0.999 ? [0, 1, 0] : [0, 0, 1];
    this.camera.position = add(this.camera.target, scale(away, this.camera.distance));
    this.dirty = true;
    this.onCameraChange?.();
  }

  pan(dx, dy) {
    if (dx === 0 && dy === 0) return;
    const forward = normalize(sub(this.camera.target, this.camera.position));
    const right = normalize(cross(forward, this.camera.up));
    const up = normalize(cross(right, forward));
    const worldPerPixel =
      this.camera.mode === "perspective"
        ? 2 * this.camera.distance * Math.tan(this.camera.fov / 2) / Math.max(this.canvasCssHeight, 1)
        : 2 * this.camera.orthoScale / Math.max(this.canvasCssHeight, 1);
    const movement = add(scale(right, -dx * worldPerPixel), scale(up, dy * worldPerPixel));
    this.camera.target = add(this.camera.target, movement);
    this.camera.position = add(this.camera.position, movement);
    this.dirty = true;
    this.onCameraChange?.();
  }

  updateCameraMatrices() {
    const aspect = this.canvas.width / Math.max(this.canvas.height, 1);
    const { near, far } = cameraDepthRange(this.renderBounds, this.camera, this.reversedDepth);
    this.cameraDepthRange = { near, far };
    this.cameraForward = normalize(sub(this.camera.target, this.camera.position));
    if (this.camera.mode === "perspective") {
      perspective(this.projection, this.camera.fov, aspect, near, far, this.reversedDepth);
    } else {
      const vertical = this.camera.orthoScale;
      ortho(
        this.projection,
        -vertical * aspect,
        vertical * aspect,
        -vertical,
        vertical,
        near,
        far,
        this.reversedDepth,
      );
    }
    lookAt(this.view, this.camera.position, this.camera.target, this.camera.up);
    multiply(this.viewProjection, this.projection, this.view);
    invert(this.inverseViewProjection, this.viewProjection);
  }

}

const VERTEX_SHADER = `#version 300 es
precision highp float;
layout(location=0) in vec3 aPosition;
layout(location=1) in vec4 aModel0;
layout(location=2) in vec4 aModel1;
layout(location=3) in vec4 aModel2;
layout(location=4) in vec4 aModel3;
layout(location=5) in vec4 aColor;
layout(location=6) in float aRecord;
uniform mat4 uProjection;
uniform mat4 uView;
uniform bool uBaked;
uniform sampler2D uVisibility;
uniform int uVisibilityWidth;
uniform bool uPlainBatch;
out vec3 vWorld;
out vec4 vColor;
out float vDepth;
flat out float vRecord;
flat out float vVisible;
flat out float vSelected;
void main() {
  mat4 model = mat4(aModel0, aModel1, aModel2, aModel3);
  vec4 world = uBaked ? vec4(aPosition, 1.0) : model * vec4(aPosition, 1.0);
  vec4 view = uView * world;
  vWorld = world.xyz;
  vColor = aColor;
  vDepth = length(view.xyz);
  vRecord = aRecord;
  // Skipping the per-vertex fetch matters: most batches are wholly visible and unselected.
  if (uPlainBatch) {
    vVisible = 1.0;
    vSelected = 0.0;
  } else {
    int record = int(aRecord + 0.5);
    ivec2 visibilityCoordinate = ivec2(record % uVisibilityWidth, record / uVisibilityWidth);
    vec2 recordState = texelFetch(uVisibility, visibilityCoordinate, 0).rg;
    vVisible = recordState.r;
    vSelected = recordState.g;
  }
  gl_Position = uProjection * view;
  // Fully hidden triangles are clipped before rasterisation, including mixed batches.
  if (vVisible < 0.5) gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
}`;

const FRAGMENT_SHADER = `#version 300 es
precision highp float;
in vec3 vWorld;
in vec4 vColor;
in float vDepth;
flat in float vRecord;
flat in float vVisible;
flat in float vSelected;
uniform bool uPicking;
uniform bool uDepthOnly;
uniform int uStyle;
uniform bool uSectionActive;
uniform int uSectionAxis;
uniform float uSectionValue;
uniform float uSectionSign;
uniform float uFogDensity;
uniform vec3 uFogColor;
uniform vec3 uCameraPosition;
out vec4 outColor;
vec3 safeNormalize(vec3 value, vec3 fallback) {
  float magnitudeSquared = dot(value, value);
  return magnitudeSquared > 1e-20 ? value * inversesqrt(magnitudeSquared) : fallback;
}
void main() {
  #ifdef SECTION
  float coordinate = uSectionAxis == 0 ? vWorld.x : (uSectionAxis == 1 ? vWorld.y : vWorld.z);
  if (uSectionActive && (coordinate - uSectionValue) * uSectionSign > 0.0) discard;
  #endif
  if (uDepthOnly) { outColor = vec4(0.0); return; }
  if (uPicking) {
    float id = vRecord + 1.0;
    vec3 bytes = vec3(mod(id, 256.0), mod(floor(id / 256.0), 256.0), mod(floor(id / 65536.0), 256.0));
    outColor = vec4(bytes / 255.0, 1.0);
    return;
  }
  vec3 toCamera = safeNormalize(uCameraPosition - vWorld, vec3(0.0, 0.0, 1.0));
  vec3 normal = safeNormalize(cross(dFdx(vWorld), dFdy(vWorld)), toCamera);
  // Face the derivative normal toward the camera rather than trust IFC winding.
  if (dot(normal, toCamera) < 0.0) normal = -normal;
  vec3 lightA = normalize(vec3(0.42, -0.58, 0.70));
  vec3 lightB = normalize(vec3(-0.65, 0.20, 0.42));
  float hemisphere = mix(0.38, 0.58, normal.z * 0.5 + 0.5);
  float diffuse = hemisphere + max(dot(normal, lightA), 0.0) * 0.52 + max(dot(normal, lightB), 0.0) * 0.16;
  // IGP colours are display-space values: light in roughly linear space and convert back.
  vec3 color = vColor.rgb * pow(max(diffuse, 0.0), 1.0 / 2.2);
  if (vSelected > 0.5) color = mix(color, vec3(1.0, 0.28, 0.12), 0.72);
  else if (vSelected > 0.0) color = mix(color, vec3(0.36, 0.74, 1.0), min(vSelected * 1.5, 0.75));
  float alpha = vColor.a;
  if (uStyle == 1) alpha = min(alpha, 0.30);
  if (uStyle == 2) { color = mix(color, vec3(0.08, 0.14, 0.15), 0.45); alpha = min(alpha, 0.78); }
  float fog = 1.0 - exp(-uFogDensity * uFogDensity * vDepth * vDepth);
  color = mix(color, uFogColor, clamp(fog, 0.0, 0.92));
  outColor = vec4(color, alpha);
}`;

const CAP_VERTEX_SHADER = `#version 300 es
in vec3 aPosition;
uniform mat4 uProjection;
void main() {
  gl_Position = uProjection * vec4(aPosition, 1.0);
}
`;

const CAP_FRAGMENT_SHADER = `#version 300 es
precision highp float;
uniform vec3 uColor;
out vec4 fragColor;
void main() {
  fragColor = vec4(uColor, 1.0);
}
`;

function createProgram(gl, vertexSource, fragmentSource) {
  const compile = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const message = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader);
      throw new Error(`IFC renderer shader failed: ${message}`);
    }
    return shader;
  };
  const vertex = compile(gl.VERTEX_SHADER, vertexSource);
  const fragment = compile(gl.FRAGMENT_SHADER, fragmentSource);
  const program = gl.createProgram();
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const message = gl.getProgramInfoLog(program);
    gl.deleteProgram(program);
    throw new Error(`IFC renderer could not link: ${message}`);
  }
  return program;
}

function uniforms(gl, program, names) {
  return Object.fromEntries(names.map((name) => [name, gl.getUniformLocation(program, name)]));
}

/** A tight, precision-aware clip range around the model bounds. */
export function cameraDepthRange(bounds, camera, reversedDepth = false) {
  const radius = Math.max(bounds.radius, 0.001);
  const viewDirection = normalize(sub(camera.target, camera.position));
  const centerDepth = dot(sub(bounds.center, camera.position), viewDirection);
  const padding = radius * 1.2;
  const minimumDepthSpan = Math.max(radius * 0.01, 0.001);
  let near = Math.max(0.001, centerDepth - padding);
  if (camera.mode === "perspective" && !reversedDepth) {
    const cameraDistance = Math.max(length(sub(camera.position, camera.target)), 0.001);
    const precisionFloor = clamp(Math.min(radius / 512, cameraDistance / 100), 0.001, 0.25);
    near = Math.max(near, precisionFloor);
  }
  const far = Math.max(near + minimumDepthSpan, centerDepth + padding);
  return { near, far };
}

/** Stable u32 colour ranks, independent of instance and batch order. */
export function planDepthRanks(colors, recordCount = Math.floor(colors.length / 4)) {
  const count = Math.max(0, Math.min(Math.floor(recordCount), Math.floor(colors.length / 4)));
  const keys = new Uint32Array(count);
  const distinct = new Set();
  for (let record = 0; record < count; record += 1) {
    if (colors.active?.[record] === 0) continue;
    const at = record * 4;
    const key = (
      colors[at] * 0x1000000 +
      colors[at + 1] * 0x10000 +
      colors[at + 2] * 0x100 +
      colors[at + 3]
    ) >>> 0;
    keys[record] = key;
    distinct.add(key);
  }
  const ordered = [...distinct].sort((left, right) => left - right);
  const byColor = new Map(ordered.map((key, rank) => [key, rank]));
  const ranks = new Uint32Array(count);
  for (let record = 0; record < count; record += 1) if (colors.active?.[record] !== 0) ranks[record] = byColor.get(keys[record]);
  return { ranks, colors: ordered.length };
}

function packedRgbaKey(colors, record) {
  const at = record * 4;
  return (
    colors[at] * 0x1000000 +
    colors[at + 1] * 0x10000 +
    colors[at + 2] * 0x100 +
    colors[at + 3]
  ) >>> 0;
}

function validDepthBounds(bounds) {
  return (
    bounds?.min?.length >= 3 &&
    bounds?.max?.length >= 3 &&
    [0, 1, 2].every(
      (axis) =>
        Number.isFinite(bounds.min[axis]) &&
        Number.isFinite(bounds.max[axis]) &&
        bounds.min[axis] <= bounds.max[axis],
    )
  );
}

function depthBoundsOverlap(left, right) {
  for (let axis = 0; axis < 3; axis += 1) {
    if (left.max[axis] < right.min[axis] || right.max[axis] < left.min[axis]) return false;
  }
  return true;
}

// Coincident faces fight; interpenetrating solids do not. Two boxes can only
// hold a shared surface where one of their face planes meets another's.
export const COINCIDENT_PLANE_FRACTION = 2e-5;

export function boundsShareFacePlane(left, right, tolerance) {
  for (let axis = 0; axis < 3; axis += 1) {
    const lowLeft = left.min[axis], highLeft = left.max[axis];
    const lowRight = right.min[axis], highRight = right.max[axis];
    if (
      Math.abs(lowLeft - lowRight) <= tolerance ||
      Math.abs(highLeft - highRight) <= tolerance ||
      Math.abs(lowLeft - highRight) <= tolerance ||
      Math.abs(highLeft - lowRight) <= tolerance
    ) return true;
  }
  return false;
}

// Past this many pair tests the sweep stops and every opaque material counts
// as contested: extra draws instead of fighting surfaces on a hostile file.
export const MAX_DEPTH_PAIR_TESTS = 4_000_000;

// Overlapping record pairs are kept for later visibility changes up to this many.
export const MAX_DEPTH_RECORD_PAIRS = 1_000_000;

/**
 * Plan deterministic material winners for coincident opaque records; AABB overlap is the broad phase.
 * With `options.collectPairs` the overlapping record pairs come back too, for `restrictDepthPlan`.
 */
export function planDepthMaterials(
  colors,
  recordBounds,
  recordCount = Math.floor(colors.length / 4),
  isVisible = () => true,
  options = {},
) {
  const rankPlan = planDepthRanks(colors, recordCount);
  const count = rankPlan.ranks.length;
  const keys = new Uint32Array(count);
  const records = [];
  const opaqueColors = new Set();
  const extentMin = [Infinity, Infinity, Infinity];
  const extentMax = [-Infinity, -Infinity, -Infinity];
  for (let record = 0; record < count; record += 1) {
    if (!isVisible(record)) continue;
    const key = packedRgbaKey(colors, record);
    keys[record] = key;
    if (colors[record * 4 + 3] < 255) continue;
    opaqueColors.add(key);
    const candidate = recordBounds?.[record];
    const bounds = candidate?.bounds ?? candidate;
    if (!validDepthBounds(bounds)) continue;
    records.push({ record, key, bounds });
    for (let axis = 0; axis < 3; axis += 1) {
      if (bounds.min[axis] < extentMin[axis]) extentMin[axis] = bounds.min[axis];
      if (bounds.max[axis] > extentMax[axis]) extentMax[axis] = bounds.max[axis];
    }
  }
  const diagonal = Math.hypot(
    Math.max(0, extentMax[0] - extentMin[0]),
    Math.max(0, extentMax[1] - extentMin[1]),
    Math.max(0, extentMax[2] - extentMin[2]),
  );
  const planeTolerance = Number.isFinite(options.planeTolerance)
    ? options.planeTolerance
    : diagonal * COINCIDENT_PLANE_FRACTION;

  records.sort((left, right) => {
    const x = left.bounds.min[0] - right.bounds.min[0];
    if (x) return x;
    const maximum = left.bounds.max[0] - right.bounds.max[0];
    if (maximum) return maximum;
    if (left.key !== right.key) return left.key - right.key;
    return left.record - right.record;
  });

  const active = [];
  const contestedColors = new Set();
  const conflictKeys = new Set();
  let pairs = options.collectPairs ? [] : null;
  let tests = 0;
  let exhausted = false;
  for (const current of records) {
    let write = 0;
    for (let index = 0; index < active.length; index += 1) {
      if (active[index].bounds.max[0] >= current.bounds.min[0]) {
        active[write] = active[index];
        write += 1;
      }
    }
    active.length = write;
    tests += active.length;
    if (tests > MAX_DEPTH_PAIR_TESTS) {
      exhausted = true;
      break;
    }
    for (const other of active) {
      if (other.key === current.key || !depthBoundsOverlap(other.bounds, current.bounds)) continue;
      if (!boundsShareFacePlane(other.bounds, current.bounds, planeTolerance)) continue;
      const low = Math.min(other.key, current.key);
      const high = Math.max(other.key, current.key);
      conflictKeys.add(`${low}:${high}`);
      contestedColors.add(other.key);
      contestedColors.add(current.key);
      if (pairs) {
        if (pairs.length < MAX_DEPTH_RECORD_PAIRS * 2) pairs.push(other.record, current.record);
        else pairs = null;
      }
    }
    active.push(current);
  }
  if (exhausted) {
    conflictKeys.clear();
    contestedColors.clear();
    for (const key of opaqueColors) contestedColors.add(key);
    pairs = null;
  }

  // Only a record with an overlapping partner needs the overlay; without the
  // pairs the whole colour is taken, because any of its records may be the one.
  const contested = new Uint8Array(count);
  if (pairs) {
    for (const record of pairs) contested[record] = 1;
  } else {
    for (let record = 0; record < count; record += 1) {
      if (colors[record * 4 + 3] === 255 && contestedColors.has(keys[record])) contested[record] = 1;
    }
  }
  return {
    ranks: rankPlan.ranks,
    contested,
    colors: rankPlan.colors,
    opaqueColors: opaqueColors.size,
    contestedColors: contestedColors.size,
    contestedRecords: contested.reduce((total, flag) => total + flag, 0),
    conflictPairs: conflictKeys.size,
    exhausted,
    pairs: pairs ? Uint32Array.from(pairs) : null,
  };
}

/**
 * The plan for a visible set that is a subset of the one `base` was planned over: the
 * pairs with both records still visible decide the contested colours, without another sweep.
 * Null when the base carries no pairs.
 */
export function restrictDepthPlan(base, colors, recordCount, isVisible) {
  const pairs = base?.pairs;
  if (!pairs || base.exhausted) return null;
  const count = Math.max(0, Math.min(Math.floor(recordCount), Math.floor(colors.length / 4)));
  const contestedColors = new Set();
  const conflictKeys = new Set();
  const contested = new Uint8Array(count);
  for (let at = 0; at + 1 < pairs.length; at += 2) {
    const first = pairs[at];
    const second = pairs[at + 1];
    if (first >= count || second >= count || !isVisible(first) || !isVisible(second)) continue;
    const firstKey = packedRgbaKey(colors, first);
    const secondKey = packedRgbaKey(colors, second);
    conflictKeys.add(firstKey < secondKey ? `${firstKey}:${secondKey}` : `${secondKey}:${firstKey}`);
    contestedColors.add(firstKey);
    contestedColors.add(secondKey);
    contested[first] = 1;
    contested[second] = 1;
  }
  const opaqueColors = new Set();
  for (let record = 0; record < count; record += 1) {
    if (colors[record * 4 + 3] !== 255 || !isVisible(record)) continue;
    opaqueColors.add(packedRgbaKey(colors, record));
  }
  return {
    ranks: base.ranks,
    contested,
    colors: base.colors,
    opaqueColors: opaqueColors.size,
    contestedColors: contestedColors.size,
    contestedRecords: contested.reduce((total, flag) => total + flag, 0),
    conflictPairs: conflictKeys.size,
    exhausted: false,
    pairs,
  };
}
/** The least opaque a product may be drawn; exporters style whole windows fully transparent. */
export const MINIMUM_VISIBLE_ALPHA = 46;

/** The darkest a product may be drawn; scaled, not added, so dark red stays red. */
export const MINIMUM_VISIBLE_LUMINANCE = 58;

function luminanceOf(red, green, blue) {
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

/** The pack's colours with both floors applied; the input is never modified. */
export function displayColors(
  pack,
  minimumAlpha = MINIMUM_VISIBLE_ALPHA,
  minimumLuminance = MINIMUM_VISIBLE_LUMINANCE,
) {
  const colors = Uint8Array.from(pack.instances.colors);
  if (pack.instances.active) colors.active = pack.instances.active;
  const floors = applyDisplayFloors(colors, 0, pack.instances.count, minimumAlpha, minimumLuminance, pack.instances.active);
  colors.raisedCount = floors.raised;
  colors.brightenedCount = floors.brightened;
  return colors;
}

/** Apply both floors in place to records `from..to`, returning how many moved. */
function applyDisplayFloors(colors, from, to, minimumAlpha, minimumLuminance, active = null) {
  let raised = 0;
  let brightened = 0;
  for (let record = from; record < to; record += 1) {
    if (active?.[record] === 0) continue;
    const at = record * 4;
    if (colors[at + 3] < minimumAlpha) {
      colors[at + 3] = minimumAlpha;
      raised += 1;
    }
    const luminance = luminanceOf(colors[at], colors[at + 1], colors[at + 2]);
    if (luminance < minimumLuminance) {
      if (luminance > 0.5) {
        const scale = minimumLuminance / luminance;
        colors[at] = Math.min(255, Math.round(colors[at] * scale));
        colors[at + 1] = Math.min(255, Math.round(colors[at + 1] * scale));
        colors[at + 2] = Math.min(255, Math.round(colors[at + 2] * scale));
      } else {
        colors[at] = minimumLuminance;
        colors[at + 1] = minimumLuminance;
        colors[at + 2] = minimumLuminance;
      }
      brightened += 1;
    }
  }
  return { raised, brightened };
}

/** Reused opaque geometry is instanced only once its copies would cost this many vertices. */
export const INSTANCE_MIN_VERTICES = 4096;

// Batches are culled whole, so records are grouped by locality first: a batch
// spread over the model has a bounding box the size of the model.
const SPATIAL_GRID_BITS = 8;

function spreadBits(value) {
  let bits = value & 0xff;
  bits = (bits | (bits << 8)) & 0x00f00f;
  bits = (bits | (bits << 4)) & 0x0c30c3;
  bits = (bits | (bits << 2)) & 0x249249;
  return bits;
}

/** Interleaved cell index of `bounds`' centre inside `extent`, so near records sort together. */
export function spatialKey(bounds, extent) {
  const cells = (1 << SPATIAL_GRID_BITS) - 1;
  const cell = (axis) => {
    const span = extent.max[axis] - extent.min[axis];
    if (!(span > 0)) return 0;
    const centre = (bounds.min[axis] + bounds.max[axis]) / 2;
    const at = Math.floor(((centre - extent.min[axis]) / span) * cells);
    return Math.max(0, Math.min(cells, Number.isFinite(at) ? at : 0));
  };
  return (spreadBits(cell(0)) | (spreadBits(cell(1)) << 1) | (spreadBits(cell(2)) << 2)) >>> 0;
}

// A batch is worth splitting only once it carries enough work to pay for the draw.
const LOCALITY_CELLS_PER_AXIS = 8;
const MIN_SPLIT_VERTEX_FRACTION = 1 / 32;

function boundsSpan(min, max) {
  return Math.hypot(max[0] - min[0], max[1] - min[1], max[2] - min[2]);
}

/** Grow batches until they run out of vertex room or leave their neighbourhood. */
function localityChunker(extent, vertexLimit) {
  const span = extent ? boundsSpan(extent.min, extent.max) / LOCALITY_CELLS_PER_AXIS : Infinity;
  const minimumVertices = vertexLimit * MIN_SPLIT_VERTEX_FRACTION;
  let min = [Infinity, Infinity, Infinity], max = [-Infinity, -Infinity, -Infinity];
  return {
    /** True when `bounds` belongs in the next batch instead of this one. */
    breaks(bounds, vertices, count, accumulated) {
      if (!count) return false;
      if (accumulated + vertices > vertexLimit) return true;
      if (!bounds || !extent || count < 2 || accumulated < minimumVertices) return false;
      return boundsSpan(
        [Math.min(min[0], bounds.min[0]), Math.min(min[1], bounds.min[1]), Math.min(min[2], bounds.min[2])],
        [Math.max(max[0], bounds.max[0]), Math.max(max[1], bounds.max[1]), Math.max(max[2], bounds.max[2])],
      ) > span;
    },
    add(bounds) {
      if (!bounds) return;
      for (let axis = 0; axis < 3; axis += 1) {
        if (bounds.min[axis] < min[axis]) min[axis] = bounds.min[axis];
        if (bounds.max[axis] > max[axis]) max[axis] = bounds.max[axis];
      }
    },
    reset() {
      min = [Infinity, Infinity, Infinity];
      max = [-Infinity, -Infinity, -Infinity];
    },
  };
}

function recordExtent(locations, first, last) {
  const extent = { min: [Infinity, Infinity, Infinity], max: [-Infinity, -Infinity, -Infinity] };
  for (let record = first; record < last; record += 1) {
    const bounds = locations?.[record]?.bounds;
    if (!bounds) continue;
    for (let axis = 0; axis < 3; axis += 1) {
      if (bounds.min[axis] < extent.min[axis]) extent.min[axis] = bounds.min[axis];
      if (bounds.max[axis] > extent.max[axis]) extent.max[axis] = bounds.max[axis];
    }
  }
  return Number.isFinite(extent.min[0]) ? extent : null;
}

/** Plan batches for `pack`, or its `range`; `options.instanceMinVertices` overrides `INSTANCE_MIN_VERTICES`. */
export function planRenderBatches(pack, vertexLimit = BATCH_VERTEX_LIMIT, colors = null, range = null, options = {}) {
  const geometryById = options.geometryById ?? geometryIndex(pack);
  const shade = colors ?? displayColors(pack);
  // A batch is redrawn as one unit, so a contested record must not carry
  // uncontested neighbours into the overlay pass.
  const contested = options.contested ?? null;
  const contestKey = (record) => (contested?.[record] ? 1 : 0);
  const locations = options.recordLocations ?? null;
  const instanceMinVertices = Number.isFinite(options.instanceMinVertices)
    ? Math.max(0, options.instanceMinVertices)
    : INSTANCE_MIN_VERTICES;
  const byGeometry = new Map();
  const opaqueGeometryUse = new Map();
  const transparentGeometryUse = new Map();
  const transparentItems = [];
  const sourceGroups = new Set();
  const first = range ? Math.max(0, range.from) : 0;
  const last = range ? Math.min(pack.instances.count, range.to) : pack.instances.count;
  for (let record = first; record < last; record += 1) {
    if (!isActiveRecord(pack, record) || options.records && !options.records.has(record)) continue;
    const geometryId = pack.instances.geometryIds[record];
    if (!geometryById.has(geometryId)) continue;
    const transparent = shade[record * 4 + 3] < 255;
    sourceGroups.add(`${geometryId}:${transparent ? 1 : 0}`);
    if (transparent) {
      transparentGeometryUse.set(
        geometryId,
        (transparentGeometryUse.get(geometryId) ?? 0) + 1,
      );
      transparentItems.push({
        record,
        geometry: geometryById.get(geometryId),
      });
      continue;
    }
    opaqueGeometryUse.set(geometryId, (opaqueGeometryUse.get(geometryId) ?? 0) + 1);
    const colorKey = Array.from(shade.subarray(record * 4, record * 4 + 4)).join(",");
    const key = `${geometryId}:0:${colorKey}:${contestKey(record)}`;
    if (!byGeometry.has(key)) {
      byGeometry.set(key, { geometry: geometryById.get(geometryId), transparent: false, records: [] });
    }
    byGeometry.get(key).records.push(record);
  }

  const extent = locations ? recordExtent(locations, first, last) : null;
  const keyed = new Map();
  const localityKey = (record) => {
    if (!extent) return record;
    let key = keyed.get(record);
    if (key === undefined) {
      const bounds = locations[record]?.bounds;
      key = bounds ? spatialKey(bounds, extent) : 0xffffffff;
      keyed.set(record, key);
    }
    return key;
  };
  const byLocality = (left, right) => localityKey(left) - localityKey(right) || left - right;

  const instanced = [];
  const byColor = new Map();
  for (const group of byGeometry.values()) {
    const uses = opaqueGeometryUse.get(group.geometry.id) ?? 0;
    const vertices = group.geometry.positions.length / 3;
    if (uses > 1 && uses * vertices >= instanceMinVertices) {
      if (!extent || group.records.length < 2) {
        instanced.push(group);
        continue;
      }
      group.records.sort(byLocality);
      const chunker = localityChunker(extent, vertexLimit);
      let records = [];
      for (const record of group.records) {
        const bounds = locations[record]?.bounds;
        if (chunker.breaks(bounds, vertices, records.length, records.length * vertices)) {
          instanced.push({ ...group, records });
          records = [];
          chunker.reset();
        }
        records.push(record);
        chunker.add(bounds);
      }
      if (records.length) instanced.push({ ...group, records });
      continue;
    }
    // Small reused geometry joins the colour batches one copy per record.
    for (const record of group.records) {
      const color = Array.from(shade.subarray(record * 4, record * 4 + 4));
      const key = `${color.join(",")}:${contestKey(record)}`;
      if (!byColor.has(key)) byColor.set(key, { color, items: [] });
      byColor.get(key).items.push({ record, geometry: group.geometry });
    }
  }

  const baked = [];
  for (const material of byColor.values()) {
    if (extent) material.items.sort((left, right) => byLocality(left.record, right.record));
    const chunker = localityChunker(extent, vertexLimit);
    let items = [];
    let vertexCount = 0;
    const flush = () => {
      if (!items.length) return;
      baked.push({
        color: material.color,
        transparent: material.color[3] < 255,
        items,
        vertexCount,
      });
      items = [];
      vertexCount = 0;
      chunker.reset();
    };
    for (const item of material.items) {
      const vertices = item.geometry.positions.length / 3;
      const bounds = locations?.[item.record]?.bounds;
      if (chunker.breaks(bounds, vertices, items.length, vertexCount)) flush();
      items.push(item);
      vertexCount += vertices;
      chunker.add(bounds);
    }
    flush();
  }

  // Alpha blending is order-dependent, so translucent products stay separately
  // sortable: reused geometry shares buffers, unique geometry stays baked.
  for (const item of transparentItems) {
    if ((transparentGeometryUse.get(item.geometry.id) ?? 0) > 1) {
      instanced.push({
        geometry: item.geometry,
        transparent: true,
        records: [item.record],
      });
    } else {
      const color = Array.from(shade.subarray(item.record * 4, item.record * 4 + 4));
      baked.push({
        color,
        transparent: true,
        items: [item],
        vertexCount: item.geometry.positions.length / 3,
      });
    }
  }
  return {
    instanced,
    baked,
    drawCalls: instanced.length + baked.length,
    sourceDrawCalls: sourceGroups.size,
  };
}

// Both filters are quality passes; a huge mesh skips them rather than stalling.
const MAX_FILTERED_TRIANGLES = 200_000;

function prepareGeometryForGpu(geometry, suppressCoplanarPatches = true) {
  // IGP may carry f64 positions; WebGL wants four-byte floats.
  const positions =
    geometry.positions instanceof Float32Array
      ? geometry.positions
      : Float32Array.from(geometry.positions);
  // The coverage pass is opaque-only; transparent surfaces keep their layer count.
  const indices =
    geometry.indices.length > MAX_FILTERED_TRIANGLES * 3
      ? geometry.indices
      : suppressCoplanarPatches
        ? filterCoplanarPatchTriangles(geometry.positions, geometry.indices)
        : filterCoincidentTriangles(geometry.positions, geometry.indices);
  return {
    positions,
    indices,
    suppressedTriangles: (geometry.indices.length - indices.length) / 3,
  };
}

function axisAlignedTriangle(positions, indices, offset) {
  const vertexCount = Math.floor(positions.length / 3);
  const ia = indices[offset];
  const ib = indices[offset + 1];
  const ic = indices[offset + 2];
  if (ia >= vertexCount || ib >= vertexCount || ic >= vertexCount) return null;
  const ax = positions[ia * 3], ay = positions[ia * 3 + 1], az = positions[ia * 3 + 2];
  const bx = positions[ib * 3], by = positions[ib * 3 + 1], bz = positions[ib * 3 + 2];
  const cx = positions[ic * 3], cy = positions[ic * 3 + 1], cz = positions[ic * 3 + 2];
  // Most triangles of curved geometry share no axis plane: decided before anything is allocated.
  if (!((ax === bx && ax === cx) || (ay === by && ay === cy) || (az === bz && az === cz))) return null;
  if (
    !Number.isFinite(ax) || !Number.isFinite(ay) || !Number.isFinite(az) ||
    !Number.isFinite(bx) || !Number.isFinite(by) || !Number.isFinite(bz) ||
    !Number.isFinite(cx) || !Number.isFinite(cy) || !Number.isFinite(cz)
  ) return null;
  const a = [ax, ay, az];
  const b = [bx, by, bz];
  const c = [cx, cy, cz];

  let result = null;
  for (let axis = 0; axis < 3; axis += 1) {
    if (a[axis] !== b[axis] || a[axis] !== c[axis]) continue;
    const u = (axis + 1) % 3;
    const v = (axis + 2) % 3;
    const points = [
      [a[u], a[v]],
      [b[u], b[v]],
      [c[u], c[v]],
    ];
    const twiceArea = signedAreaTwice(points);
    if (twiceArea === 0 || !Number.isFinite(twiceArea)) continue;
    if (!result || Math.abs(twiceArea) > result.area * 2) {
      result = {
        offset,
        axis,
        plane: a[axis],
        orientation: Math.sign(twiceArea),
        points,
        area: Math.abs(twiceArea) / 2,
        bounds: bounds2d(points),
      };
    }
  }
  return result;
}

function bounds2d(points) {
  const bounds = {
    minX: Number.POSITIVE_INFINITY,
    minY: Number.POSITIVE_INFINITY,
    maxX: Number.NEGATIVE_INFINITY,
    maxY: Number.NEGATIVE_INFINITY,
  };
  for (const point of points) {
    bounds.minX = Math.min(bounds.minX, point[0]);
    bounds.minY = Math.min(bounds.minY, point[1]);
    bounds.maxX = Math.max(bounds.maxX, point[0]);
    bounds.maxY = Math.max(bounds.maxY, point[1]);
  }
  return bounds;
}

function boundsOverlap2d(left, right) {
  return (
    left.maxX >= right.minX &&
    right.maxX >= left.minX &&
    left.maxY >= right.minY &&
    right.maxY >= left.minY
  );
}

function signedAreaTwice(points) {
  if (!points.length) return 0;
  // Translate first to avoid cancellation far from the map origin.
  const originX = points[0][0];
  const originY = points[0][1];
  let area = 0;
  for (let index = 0; index < points.length; index += 1) {
    const next = (index + 1) % points.length;
    const x = points[index][0] - originX;
    const y = points[index][1] - originY;
    const nextX = points[next][0] - originX;
    const nextY = points[next][1] - originY;
    area += x * nextY - y * nextX;
  }
  return area;
}

function edgeSide(a, b, point) {
  return (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
}

function edgeTolerance(a, b, point) {
  const edgeScale = Math.abs(b[0] - a[0]) + Math.abs(b[1] - a[1]);
  const pointScale = Math.abs(point[0] - a[0]) + Math.abs(point[1] - a[1]);
  return Number.EPSILON * 64 * Math.max(1, edgeScale * pointScale);
}

function segmentLineIntersection(start, end, clipStart, clipEnd) {
  const rayX = end[0] - start[0];
  const rayY = end[1] - start[1];
  const edgeX = clipEnd[0] - clipStart[0];
  const edgeY = clipEnd[1] - clipStart[1];
  const denominator = rayX * edgeY - rayY * edgeX;
  const scale = Math.max(1, Math.abs(rayX) + Math.abs(rayY), Math.abs(edgeX) + Math.abs(edgeY));
  if (Math.abs(denominator) <= Number.EPSILON * 64 * scale * scale) return null;
  const offsetX = clipStart[0] - start[0];
  const offsetY = clipStart[1] - start[1];
  const amount = (offsetX * edgeY - offsetY * edgeX) / denominator;
  const clamped = Math.max(0, Math.min(1, amount));
  return [start[0] + rayX * clamped, start[1] + rayY * clamped];
}

function triangleIntersectionArea(subject, clip) {
  const clipOrientation = Math.sign(signedAreaTwice(clip));
  if (!clipOrientation) return Number.NaN;
  let output = subject.map((point) => point.slice());
  for (let edge = 0; edge < clip.length; edge += 1) {
    const clipStart = clip[edge];
    const clipEnd = clip[(edge + 1) % clip.length];
    const input = output;
    output = [];
    if (!input.length) break;
    let start = input[input.length - 1];
    let startInside =
      clipOrientation * edgeSide(clipStart, clipEnd, start) >=
      -edgeTolerance(clipStart, clipEnd, start);
    for (const end of input) {
      const endInside =
        clipOrientation * edgeSide(clipStart, clipEnd, end) >=
        -edgeTolerance(clipStart, clipEnd, end);
      if (endInside !== startInside) {
        const intersection = segmentLineIntersection(start, end, clipStart, clipEnd);
        if (!intersection) return Number.NaN;
        output.push(intersection);
      }
      if (endInside) output.push(end.slice());
      start = end;
      startInside = endInside;
    }
  }
  if (output.length < 3) return 0;
  const area = Math.abs(signedAreaTwice(output)) / 2;
  return Number.isFinite(area) ? area : Number.NaN;
}

function areaTolerance(area) {
  return Math.max(PATCH_AREA_ABSOLUTE_EPSILON, Math.abs(area) * PATCH_AREA_RELATIVE_EPSILON);
}

function areasMatch(left, right) {
  return Math.abs(left - right) <= areaTolerance(right);
}

function hasPositiveAreaOverlap(triangles) {
  for (let left = 0; left < triangles.length; left += 1) {
    for (let right = left + 1; right < triangles.length; right += 1) {
      if (!boundsOverlap2d(triangles[left].bounds, triangles[right].bounds)) continue;
      const overlap = triangleIntersectionArea(triangles[left].points, triangles[right].points);
      if (!Number.isFinite(overlap)) return true;
      if (overlap > areaTolerance(Math.min(triangles[left].area, triangles[right].area))) return true;
    }
  }
  return false;
}

/** Remove one raster copy of axis-aligned, opposite-facing patches; source indices are untouched. */
export function filterCoplanarPatchTriangles(positions, indices) {
  if (indices.length < 6) return filterCoincidentTriangles(positions, indices);
  const planes = [new Map(), new Map(), new Map()];
  for (let offset = 0; offset + 2 < indices.length; offset += 3) {
    const triangle = axisAlignedTriangle(positions, indices, offset);
    if (!triangle) continue;
    let group = planes[triangle.axis].get(triangle.plane);
    if (!group) {
      group = { positive: [], negative: [] };
      planes[triangle.axis].set(triangle.plane, group);
    }
    (triangle.orientation > 0 ? group.positive : group.negative).push(triangle);
  }

  const suppressed = new Set();
  for (const group of planes.flatMap((byPlane) => [...byPlane.values()])) {
    const positive = group.positive;
    const negative = group.negative;
    if (!positive.length || !negative.length) continue;
    if (
      positive.length + negative.length > MAX_PATCH_TRIANGLES ||
      positive.length * negative.length > MAX_PATCH_PAIR_TESTS
    ) continue;
    // Area sums measure the union only when neither orientation overlaps itself.
    if (hasPositiveAreaOverlap(positive) || hasPositiveAreaOverlap(negative)) continue;
    const positiveArea = positive.reduce((total, triangle) => total + triangle.area, 0);
    const negativeArea = negative.reduce((total, triangle) => total + triangle.area, 0);
    if (Math.min(positiveArea, negativeArea) <= MIN_PATCH_AREA) continue;
    let overlapArea = 0;
    for (const left of positive) {
      for (const right of negative) {
        if (!boundsOverlap2d(left.bounds, right.bounds)) continue;
        overlapArea += triangleIntersectionArea(left.points, right.points);
      }
    }
    const positiveCovered = areasMatch(overlapArea, positiveArea);
    const negativeCovered = areasMatch(overlapArea, negativeArea);
    let remove = null;
    if (positiveCovered && negativeCovered) remove = negative;
    else if (positiveCovered) remove = positive;
    else if (negativeCovered) remove = negative;
    if (remove) for (const triangle of remove) suppressed.add(triangle.offset);
  }

  if (!suppressed.size) return filterCoincidentTriangles(positions, indices);
  const retained = new indices.constructor(indices.length - suppressed.size * 3);
  let write = 0;
  for (let offset = 0; offset + 2 < indices.length; offset += 3) {
    if (suppressed.has(offset)) continue;
    retained[write] = indices[offset];
    retained[write + 1] = indices[offset + 1];
    retained[write + 2] = indices[offset + 2];
    write += 3;
  }
  return filterCoincidentTriangles(positions, retained);
}

/**
 * Source triangle index to prepared triangle index, or null when the prepared list is the
 * source. The GPU filters keep a subsequence of the source in order, so one walk maps them.
 */
export function preparedTriangleMap(source, prepared) {
  if (source === prepared || source.length === prepared.length) return null;
  const map = new Int32Array(Math.floor(source.length / 3)).fill(-1);
  let at = 0;
  for (let t = 0; t < map.length && at * 3 + 2 < prepared.length; t += 1) {
    if (source[t * 3] === prepared[at * 3] && source[t * 3 + 1] === prepared[at * 3 + 1] && source[t * 3 + 2] === prepared[at * 3 + 2]) {
      map[t] = at;
      at += 1;
    }
  }
  return map;
}

/** Keep one GPU triangle per exact coincident face pair; the source mesh keeps both. */
export function filterCoincidentTriangles(positions, indices) {
  if (indices.length < 6) return indices;
  const vertexCount = Math.floor(positions.length / 3);
  const canonicalVertices = canonicalVertexIds(positions, vertexCount);
  const triangleCount = Math.floor(indices.length / 3);
  let size = 1;
  while (size < triangleCount * 2) size *= 2;
  const table = new Int32Array(size).fill(-1);
  const mask = size - 1;
  const keys = new Uint32Array(triangleCount * 3);
  const filtered = new indices.constructor(indices.length);
  let kept = 0;
  for (let offset = 0; offset + 2 < indices.length; offset += 3) {
    const a = indices[offset];
    const b = indices[offset + 1];
    const c = indices[offset + 2];
    if (a >= vertexCount || b >= vertexCount || c >= vertexCount) continue;
    let x = canonicalVertices[a];
    let y = canonicalVertices[b];
    let z = canonicalVertices[c];
    if (x === y || y === z || z === x) continue;
    if (x > y) [x, y] = [y, x];
    if (y > z) [y, z] = [z, y];
    if (x > y) [x, y] = [y, x];
    let slot = hashTriple(x, y, z) & mask;
    let duplicate = false;
    for (;;) {
      const other = table[slot];
      if (other === -1) {
        table[slot] = kept;
        break;
      }
      const at = other * 3;
      if (keys[at] === x && keys[at + 1] === y && keys[at + 2] === z) {
        duplicate = true;
        break;
      }
      slot = (slot + 1) & mask;
    }
    if (duplicate) continue;
    keys[kept * 3] = x;
    keys[kept * 3 + 1] = y;
    keys[kept * 3 + 2] = z;
    filtered[kept * 3] = a;
    filtered[kept * 3 + 1] = b;
    filtered[kept * 3 + 2] = c;
    kept += 1;
  }
  return kept * 3 === indices.length ? indices : filtered.slice(0, kept * 3);
}

/** One id per distinct f32 position, from an open-addressing table over the raw bits. */
function canonicalVertexIds(positions, vertexCount) {
  const floats = positions instanceof Float32Array ? positions : Float32Array.from(positions);
  const bits = new Uint32Array(floats.buffer, floats.byteOffset, vertexCount * 3);
  // Negative zero is the same coordinate as zero.
  const word = (at) => (bits[at] === 0x80000000 ? 0 : bits[at]);
  const ids = new Uint32Array(vertexCount);
  let size = 1;
  while (size < vertexCount * 2) size *= 2;
  const table = new Int32Array(size).fill(-1);
  const mask = size - 1;
  let next = 0;
  for (let vertex = 0; vertex < vertexCount; vertex += 1) {
    const at = vertex * 3;
    const x = word(at);
    const y = word(at + 1);
    const z = word(at + 2);
    let slot = hashTriple(x, y, z) & mask;
    for (;;) {
      const other = table[slot];
      if (other === -1) {
        table[slot] = vertex;
        ids[vertex] = next;
        next += 1;
        break;
      }
      const otherAt = other * 3;
      if (word(otherAt) === x && word(otherAt + 1) === y && word(otherAt + 2) === z) {
        ids[vertex] = ids[other];
        break;
      }
      slot = (slot + 1) & mask;
    }
  }
  return ids;
}

function hashTriple(x, y, z) {
  let hash = Math.imul(x, 0x9e3779b1) ^ Math.imul(y ^ 0x85ebca77, 0xc2b2ae3d) ^ Math.imul(z, 0x27d4eb2f);
  hash ^= hash >>> 15;
  hash = Math.imul(hash, 0x2c1b3c6d);
  hash ^= hash >>> 12;
  return hash >>> 0;
}

/** A set of vertex pairs in typed arrays sized for `indexCount` edges; `add` reports a new pair. */
function createEdgeSet(indexCount) {
  let size = 1;
  while (size < indexCount * 2) size *= 2;
  const table = new Int32Array(size).fill(-1);
  const lows = new Uint32Array(indexCount);
  const highs = new Uint32Array(indexCount);
  const mask = size - 1;
  let count = 0;
  return {
    add(low, high) {
      let slot = hashTriple(low, high, 0x51ed27) & mask;
      for (;;) {
        const other = table[slot];
        if (other === -1) {
          table[slot] = count;
          lows[count] = low;
          highs[count] = high;
          count += 1;
          return true;
        }
        if (lows[other] === low && highs[other] === high) return false;
        slot = (slot + 1) & mask;
      }
    },
  };
}

/** The meshes of a pack by id, built once per pass rather than once per caller. */
function geometryIndex(pack) {
  return new Map(pack.geometry.map((item) => [item.id, item]));
}

function isActiveRecord(pack, record) {
  return Boolean(pack && Number.isInteger(record) && record >= 0 && record < pack.instances.count && pack.instances.active?.[record] !== 0);
}

function activePredicate(pack, predicate) {
  return (record) => isActiveRecord(pack, record) && predicate(record);
}

function computeModelBounds(pack, isVisible = () => true) {
  const bounds = emptyBounds();
  const geometry = geometryIndex(pack);
  for (let record = 0; record < pack.instances.count; record += 1) {
    if (!isActiveRecord(pack, record) || !isVisible(record)) continue;
    const item = geometry.get(pack.instances.geometryIds[record]);
    if (!item) continue;
    const record_bounds = transformedBounds(item.bbox, pack.instances.transforms, record * 16);
    // Sentinel bounds from all-non-finite geometry would push the camera to infinity.
    if (!validDepthBounds(record_bounds)) continue;
    includeBounds(bounds, record_bounds);
  }
  finishBounds(bounds);
  return bounds;
}

function finishBounds(bounds) {
  if (!Number.isFinite(bounds.min[0]) || !Number.isFinite(bounds.max[0])) {
    bounds.min = [-1, -1, -1];
    bounds.max = [1, 1, 1];
  }
  bounds.center = boundsCenter(bounds);
  bounds.radius = boundsRadius(bounds);
}

function emptyBounds() {
  return { min: [Infinity, Infinity, Infinity], max: [-Infinity, -Infinity, -Infinity], center: [0, 0, 0], radius: 1 };
}

function offsetBounds(bounds, offset) {
  const min = add(bounds.min, offset);
  const max = add(bounds.max, offset);
  return { min, max, center: boundsCenter({ min, max }), radius: bounds.radius };
}

function transformPositions(target, targetOffset, source, matrix, origin) {
  for (let offset = 0; offset < source.length; offset += 3) {
    const x = source[offset];
    const y = source[offset + 1];
    const z = source[offset + 2];
    const w = matrix[3] * x + matrix[7] * y + matrix[11] * z + matrix[15];
    target[targetOffset + offset] =
      (matrix[0] * x + matrix[4] * y + matrix[8] * z + matrix[12]) / w - origin[0];
    target[targetOffset + offset + 1] =
      (matrix[1] * x + matrix[5] * y + matrix[9] * z + matrix[13]) / w - origin[1];
    target[targetOffset + offset + 2] =
      (matrix[2] * x + matrix[6] * y + matrix[10] * z + matrix[14]) / w - origin[2];
  }
}

function renderTransform(transforms, record, origin) {
  const matrix = Float32Array.from(transforms.subarray(record * 16, record * 16 + 16));
  matrix[12] -= origin[0];
  matrix[13] -= origin[1];
  matrix[14] -= origin[2];
  return matrix;
}

function transformedBounds(bbox, matrices, offset) {
  const result = emptyBounds();
  const matrix = matrices.subarray(offset, offset + 16);
  for (let mask = 0; mask < 8; mask += 1) {
    const point = [
      bbox[mask & 1 ? 3 : 0],
      bbox[mask & 2 ? 4 : 1],
      bbox[mask & 4 ? 5 : 2],
    ];
    includePoint(result, transformPoint(matrix, point));
  }
  return result;
}

function includePoint(bounds, point) {
  for (let axis = 0; axis < 3; axis += 1) {
    bounds.min[axis] = Math.min(bounds.min[axis], point[axis]);
    bounds.max[axis] = Math.max(bounds.max[axis], point[axis]);
  }
}

function includeBounds(target, source) {
  includePoint(target, source.min);
  includePoint(target, source.max);
}

function rayBounds(origin, direction, bounds) {
  let near = 0;
  let far = Infinity;
  for (let axis = 0; axis < 3; axis += 1) {
    if (Math.abs(direction[axis]) < 1e-12) {
      if (origin[axis] < bounds.min[axis] || origin[axis] > bounds.max[axis]) return null;
      continue;
    }
    const inverse = 1 / direction[axis];
    let first = (bounds.min[axis] - origin[axis]) * inverse;
    let second = (bounds.max[axis] - origin[axis]) * inverse;
    if (first > second) [first, second] = [second, first];
    near = Math.max(near, first);
    far = Math.min(far, second);
    if (near > far) return null;
  }
  return far >= 0 ? near : null;
}

function indexByteWidth(type, gl) {
  return type === gl.UNSIGNED_INT ? 4 : 2;
}

/** Run `callback` when the main thread is idle; a timer stands in where idle callbacks are missing. */
function whenIdle(callback) {
  if (typeof requestIdleCallback === "function") return { idle: true, id: requestIdleCallback(callback, { timeout: 1000 }) };
  return { idle: false, id: setTimeout(callback, 50) };
}

function cancelWhenIdle(handle) {
  if (handle.idle) cancelIdleCallback(handle.id);
  else clearTimeout(handle.id);
}

/** A record, a list of them, or nothing, as a list. */
function recordList(records) {
  if (Number.isInteger(records)) return [records];
  return Array.isArray(records) ? records.filter(Number.isInteger) : [];
}

function boundsCenter(bounds) {
  return bounds.min.map((value, axis) => (value + bounds.max[axis]) / 2);
}

function boundsHalfExtents(bounds) {
  return bounds.min.map((value, axis) => Math.max(0, bounds.max[axis] - value) / 2);
}

function boundsRadius(bounds) {
  return length(sub(bounds.max, bounds.min)) / 2;
}

function transformPoint(matrix, point) {
  const x = point[0], y = point[1], z = point[2];
  const w = matrix[3] * x + matrix[7] * y + matrix[11] * z + matrix[15];
  return [
    (matrix[0] * x + matrix[4] * y + matrix[8] * z + matrix[12]) / w,
    (matrix[1] * x + matrix[5] * y + matrix[9] * z + matrix[13]) / w,
    (matrix[2] * x + matrix[6] * y + matrix[10] * z + matrix[14]) / w,
  ];
}

function rayTriangleIndexed(origin, direction, positions, aIndex, bIndex, cIndex) {
  const a = aIndex * 3;
  const b = bIndex * 3;
  const c = cIndex * 3;
  const edge1x = positions[b] - positions[a];
  const edge1y = positions[b + 1] - positions[a + 1];
  const edge1z = positions[b + 2] - positions[a + 2];
  const edge2x = positions[c] - positions[a];
  const edge2y = positions[c + 1] - positions[a + 1];
  const edge2z = positions[c + 2] - positions[a + 2];
  const px = direction[1] * edge2z - direction[2] * edge2y;
  const py = direction[2] * edge2x - direction[0] * edge2z;
  const pz = direction[0] * edge2y - direction[1] * edge2x;
  const determinant = edge1x * px + edge1y * py + edge1z * pz;
  if (Math.abs(determinant) < 1e-10) return null;
  const inverse = 1 / determinant;
  const tx = origin[0] - positions[a];
  const ty = origin[1] - positions[a + 1];
  const tz = origin[2] - positions[a + 2];
  const u = (tx * px + ty * py + tz * pz) * inverse;
  if (u < 0 || u > 1) return null;
  const qx = ty * edge1z - tz * edge1y;
  const qy = tz * edge1x - tx * edge1z;
  const qz = tx * edge1y - ty * edge1x;
  const v = (direction[0] * qx + direction[1] * qy + direction[2] * qz) * inverse;
  if (v < 0 || u + v > 1) return null;
  const distance = (edge2x * qx + edge2y * qy + edge2z * qz) * inverse;
  return distance > 1e-8 ? distance : null;
}

function unproject(inverse, x, y, z) {
  const point = transformPoint(inverse, [x, y, z]);
  return point;
}

function mat4() {
  return new Float32Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
}

/** A perspective matrix, optionally with the depth range reversed. */
function perspective(out, fov, aspect, near, far, reversed = false) {
  const f = 1 / Math.tan(fov / 2);
  out.fill(0);
  out[0] = f / aspect;
  out[5] = f;
  out[10] = (far + near) / (near - far);
  out[11] = -1;
  out[14] = 2 * far * near / (near - far);
  if (reversed) {
    out[10] = near / (far - near);
    out[14] = far * near / (far - near);
  }
}

function ortho(out, left, right, bottom, top, near, far, reversed = false) {
  out.fill(0);
  out[0] = 2 / (right - left);
  out[5] = 2 / (top - bottom);
  out[10] = -2 / (far - near);
  out[12] = -(right + left) / (right - left);
  out[13] = -(top + bottom) / (top - bottom);
  out[14] = -(far + near) / (far - near);
  out[15] = 1;
  if (reversed) {
    out[10] = 1 / (far - near);
    out[14] = far / (far - near);
  }
}

function lookAt(out, eye, target, up) {
  const z = normalize(sub(eye, target));
  const x = normalize(cross(up, z));
  const y = cross(z, x);
  out.set([
    x[0], y[0], z[0], 0,
    x[1], y[1], z[1], 0,
    x[2], y[2], z[2], 0,
    -dot(x, eye), -dot(y, eye), -dot(z, eye), 1,
  ]);
}

function multiply(out, a, b) {
  const result = new Float32Array(16);
  for (let column = 0; column < 4; column += 1) {
    for (let row = 0; row < 4; row += 1) {
      result[column * 4 + row] =
        a[row] * b[column * 4] +
        a[4 + row] * b[column * 4 + 1] +
        a[8 + row] * b[column * 4 + 2] +
        a[12 + row] * b[column * 4 + 3];
    }
  }
  out.set(result);
}

function invert(out, matrix) {
  const m = matrix;
  const b00 = m[0] * m[5] - m[1] * m[4];
  const b01 = m[0] * m[6] - m[2] * m[4];
  const b02 = m[0] * m[7] - m[3] * m[4];
  const b03 = m[1] * m[6] - m[2] * m[5];
  const b04 = m[1] * m[7] - m[3] * m[5];
  const b05 = m[2] * m[7] - m[3] * m[6];
  const b06 = m[8] * m[13] - m[9] * m[12];
  const b07 = m[8] * m[14] - m[10] * m[12];
  const b08 = m[8] * m[15] - m[11] * m[12];
  const b09 = m[9] * m[14] - m[10] * m[13];
  const b10 = m[9] * m[15] - m[11] * m[13];
  const b11 = m[10] * m[15] - m[11] * m[14];
  let determinant = b00 * b11 - b01 * b10 + b02 * b09 + b03 * b08 - b04 * b07 + b05 * b06;
  if (!determinant) return false;
  determinant = 1 / determinant;
  out[0] = (m[5] * b11 - m[6] * b10 + m[7] * b09) * determinant;
  out[1] = (-m[1] * b11 + m[2] * b10 - m[3] * b09) * determinant;
  out[2] = (m[13] * b05 - m[14] * b04 + m[15] * b03) * determinant;
  out[3] = (-m[9] * b05 + m[10] * b04 - m[11] * b03) * determinant;
  out[4] = (-m[4] * b11 + m[6] * b08 - m[7] * b07) * determinant;
  out[5] = (m[0] * b11 - m[2] * b08 + m[3] * b07) * determinant;
  out[6] = (-m[12] * b05 + m[14] * b02 - m[15] * b01) * determinant;
  out[7] = (m[8] * b05 - m[10] * b02 + m[11] * b01) * determinant;
  out[8] = (m[4] * b10 - m[5] * b08 + m[7] * b06) * determinant;
  out[9] = (-m[0] * b10 + m[1] * b08 - m[3] * b06) * determinant;
  out[10] = (m[12] * b04 - m[13] * b02 + m[15] * b00) * determinant;
  out[11] = (-m[8] * b04 + m[9] * b02 - m[11] * b00) * determinant;
  out[12] = (-m[4] * b09 + m[5] * b07 - m[6] * b06) * determinant;
  out[13] = (m[0] * b09 - m[1] * b07 + m[2] * b06) * determinant;
  out[14] = (-m[12] * b03 + m[13] * b01 - m[14] * b00) * determinant;
  out[15] = (m[8] * b03 - m[9] * b01 + m[10] * b00) * determinant;
  return true;
}

const add = (a, b) => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const scale = (a, value) => [a[0] * value, a[1] * value, a[2] * value];
const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const length = (a) => Math.hypot(a[0], a[1], a[2]);
const normalize = (a) => {
  const magnitude = length(a) || 1;
  return scale(a, 1 / magnitude);
};
const clamp = (value, minimum, maximum) => Math.max(minimum, Math.min(maximum, value));
const mix = (a, b, amount) => a + (b - a) * amount;
