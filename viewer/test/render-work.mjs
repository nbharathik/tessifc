// SPDX-License-Identifier: Apache-2.0

/** Compare culled and unculled frames and count actual GPU submissions. */
export async function checkRenderWork(page, check) {
  await page.waitForFunction(() => window.__tessifc.renderer.batches.every((batch) => batch.wireBuffer), null, { timeout: 5000 }).catch(() => {});
  check(await page.evaluate(() => window.__tessifc.renderer.batches.every((batch) => batch.wireBuffer)),
    "wire indices of a small model are prepared in idle time after the load");
  const result = await page.evaluate(() => {
    const r = window.__tessifc.renderer, gl = r.gl;
    const saved = { camera: structuredClone(r.camera), style: r.style, section: { ...r.section }, predicate: r.visibilityPredicate, prepass: r.contestedDepthPrepass };
    let calls = 0;
    const draw = gl.drawElementsInstanced.bind(gl);
    gl.drawElementsInstanced = (...args) => { calls++; return draw(...args); };
    const capture = (cull, prepass = saved.prepass) => {
      r.cullBatches = cull;
      r.contestedDepthPrepass = prepass;
      calls = 0;
      r.render(true);
      const pixels = new Uint8Array(r.canvas.width * r.canvas.height * 4);
      gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      return { pixels, calls };
    };
    const comparisons = [];
    try {
      for (const style of ["shaded", "wire", "xray"]) {
        r.setStyle(style);
        for (const mode of ["perspective", "top"]) {
          r.fit(mode);
          for (const section of [false, true]) {
            r.setSection(section, "z", r.sectionValue("z", .5));
            r.setVisibility((record) => saved.predicate(record) && record % 3 !== 0);
            r.camera.position[0] += r.renderBounds.radius * .7;
            r.camera.target[0] += r.renderBounds.radius * .7;
            const a = capture(false), b = capture(true), original = capture(true, false);
            let differences = 0, prepassDifferences = 0;
            for (let i = 0; i < a.pixels.length; i++) if (a.pixels[i] !== b.pixels[i]) differences++;
            for (let i = 0; i < b.pixels.length; i++) if (b.pixels[i] !== original.pixels[i]) prepassDifferences++;
            comparisons.push({ style, mode, section, differences, prepassDifferences, before: a.calls, after: b.calls });
          }
        }
      }
      r.setSection(false, "z", 0); r.setStyle("shaded"); r.fit();
      r.setVisibility(() => false);
      const hidden = capture(true);
      r.setVisibility(saved.predicate);
      // Move the model entirely outside the lateral clip volume.
      r.camera.position[0] += r.renderBounds.radius * 100;
      r.camera.target[0] += r.renderBounds.radius * 100;
      const outsideA = capture(false), outsideB = capture(true);
      let outsideDifferences = 0;
      for (let i = 0; i < outsideA.pixels.length; i++) if (outsideA.pixels[i] !== outsideB.pixels[i]) outsideDifferences++;
      return { comparisons, hiddenCalls: hidden.calls, outsideCalls: outsideB.calls, outsideBefore: outsideA.calls, outsideDifferences };
    } finally {
      gl.drawElementsInstanced = draw;
      r.cullBatches = true;
      r.contestedDepthPrepass = saved.prepass;
      r.setVisibility(saved.predicate); r.camera = saved.camera; r.style = saved.style; r.section = saved.section;
      r.render(true);
    }
  });
  check(result.comparisons.every((row) => row.differences === 0), "culling preserves pixels across shading, wire, xray, orthographic views and sections");
  check(result.comparisons.every((row) => row.prepassDifferences === 0),
    "the contested depth prepass preserves pixels across display styles, cameras, hidden records and sections");
  check(result.hiddenCalls === 0, "hidden geometry issues no GPU mesh draws");
  check(result.outsideCalls === 0 && result.outsideBefore > 0 && result.outsideDifferences === 0, "offscreen geometry issues no GPU mesh draws and preserves the frame");
  const shaderParity = await page.evaluate(() => {
    const r = window.__tessifc.renderer, gl = r.gl;
    const saved = { style: r.style, section: { ...r.section }, selected: r.selected.slice(), predicate: r.visibilityPredicate };
    let differences = 0, programsDiffer = false;
    const pixels = () => {
      r.render(true);
      const data = new Uint8Array(r.canvas.width * r.canvas.height * 4);
      gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, data);
      return data;
    };
    try {
      r.select([0]);
      for (const style of ["shaded", "wire", "xray"]) {
        r.setStyle(style);
        r.setVisibility((record) => saved.predicate(record) && record % 3 !== 0);
        r.setSection(false, "z", r.bounds.max[2] + r.bounds.radius, false, false);
        const surface = pixels(), surfaceProgram = r.program;
        // A cut beyond the model keeps every fragment but exercises the section shader.
        r.section.active = true;
        const section = pixels();
        programsDiffer ||= surfaceProgram !== r.program;
        for (let at = 0; at < surface.length; at++) if (surface[at] !== section[at]) differences++;
      }
      return { differences, programsDiffer, error: gl.getError() };
    } finally {
      r.setVisibility(saved.predicate); r.select(saved.selected);
      r.style = saved.style; r.section = saved.section; r.render(true);
    }
  });
  check(shaderParity.programsDiffer && shaderParity.differences === 0 && shaderParity.error === 0,
    "the fast surface shader preserves section-shader pixels, selection and hidden geometry across display styles");
  // Occlusion culling: from behind the service wall a moving frame leaves the
  // hidden clusters out and still paints the same pixels; a rest frame draws everything.
  const occlusionWas = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const was = r.occlusionCulling;
    r.setOcclusionCulling(true);
    return was;
  });
  await page.waitForFunction(() => Boolean(window.__tessifc.renderer.occluders), null, { timeout: 10_000 }).catch(() => {});
  const occlusion = await page.evaluate((occlusionWas) => {
    const r = window.__tessifc.renderer, gl = r.gl;
    const saved = { camera: structuredClone(r.camera), style: r.style, section: { ...r.section }, lodPixels: r.lodPixels, culling: occlusionWas };
    let calls = 0, indices = 0;
    const draw = gl.drawElementsInstanced.bind(gl);
    gl.drawElementsInstanced = (mode, count, type, offset, instances) => { calls++; indices += count * instances; return draw(mode, count, type, offset, instances); };
    const capture = (moving, culling) => {
      r.setOcclusionCulling(culling);
      r.interacting = moving;
      r.drag = null;
      calls = 0; indices = 0;
      r.dirty = true;
      r.render(true);
      const pixels = new Uint8Array(r.canvas.width * r.canvas.height * 4);
      gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      const state = { ...r.displayInfo().occlusionState };
      return { pixels, calls, indices, state, reduced: r.lastFrameReduced };
    };
    const same = (a, b) => { for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false; return true; };
    const place = () => {
      // The camera just left of the service wall, inside the model's bounding sphere and
      // looking into the pavilion; columns and plinths sit behind the wall.
      const origin = r.renderOrigin;
      r.camera.mode = "perspective";
      r.camera.up = [0, 0, 1];
      r.camera.target = [0 - origin[0], 0 - origin[1], 1.5 - origin[2]];
      r.camera.position = [-6.5 - origin[0], 0 - origin[1], 1.7 - origin[2]];
      r.camera.distance = Math.hypot(6.5, 0, 0.2);
    };
    try {
      r.setStyle("shaded");
      r.setSection(false, "z", 0);
      r.setLodPixels(0);
      place();
      const occluders = r.occluders ? r.occluders.count : 0;
      const movingOn = capture(true, true);
      const movingOff = capture(true, false);
      const rest = capture(false, true);
      const restOff = capture(false, false);
      const styles = {};
      for (const style of ["xray", "wire"]) {
        r.setStyle(style);
        const on = capture(true, true), off = capture(true, false);
        styles[style] = { equal: on.indices === off.indices && on.calls === off.calls && same(on.pixels, off.pixels), active: on.state.active };
      }
      r.setStyle("shaded");
      r.setSection(true, "z", r.sectionValue("z", 0.5));
      const sectionOn = capture(true, true), sectionOff = capture(true, false);
      styles.section = { equal: sectionOn.indices === sectionOff.indices && same(sectionOn.pixels, sectionOff.pixels), active: sectionOn.state.active };
      r.setSection(false, "z", 0);
      return {
        occluders,
        movingOn: { calls: movingOn.calls, indices: movingOn.indices, state: movingOn.state, reduced: movingOn.reduced },
        movingOff: { calls: movingOff.calls, indices: movingOff.indices, active: movingOff.state.active, occluded: movingOff.state.occluded, occludersDrawn: movingOff.state.occludersDrawn },
        rest: { calls: rest.calls, indices: rest.indices, active: rest.state.active, reduced: rest.reduced },
        restOff: { calls: restOff.calls, indices: restOff.indices },
        pixelsOnOff: same(movingOn.pixels, movingOff.pixels),
        pixelsOnRest: same(movingOn.pixels, rest.pixels),
        styles,
        error: gl.getError(),
      };
    } finally {
      gl.drawElementsInstanced = draw;
      r.setOcclusionCulling(saved.culling);
      r.interacting = false;
      r.setLodPixels(saved.lodPixels);
      r.camera = saved.camera; r.style = saved.style; r.section = saved.section;
      r.dirty = true;
      r.render(true);
    }
  }, occlusionWas);
  check(occlusion.occluders > 0 && occlusion.movingOn.state.active && occlusion.movingOn.state.occluded > 0,
    `occluders are selected after the load and a moving frame behind the service wall hides clusters (${occlusion.occluders} occluders, ${occlusion.movingOn.state.occluded} of ${occlusion.movingOn.state.clusters} clusters occluded)`);
  check(occlusion.movingOn.indices < occlusion.movingOff.indices && occlusion.movingOn.reduced,
    `the reduced moving frame submits fewer indices (${occlusion.movingOn.indices} of ${occlusion.movingOff.indices})`);
  check(occlusion.pixelsOnOff && occlusion.pixelsOnRest && occlusion.error === 0,
    "the reduced frame paints the same pixels as the unreduced and the rest frame");
  check(!occlusion.rest.active && !occlusion.rest.reduced && occlusion.rest.indices === occlusion.movingOff.indices &&
    occlusion.rest.calls === occlusion.restOff.calls && occlusion.rest.indices === occlusion.restOff.indices,
    "a rest frame draws everything, with occlusion on or off");
  check(occlusion.movingOff.occluded === 0 && occlusion.movingOff.occludersDrawn === 0 && Object.values(occlusion.styles).every((s) => s.equal && !s.active),
    "occlusion off occludes nothing, and x-ray, wireframe and sectioned moving frames are not culled at all");

  // Motion level of detail: a synthetic grid with a coarse level draws fewer indices on a
  // moving frame and the same pixels as at rest, since the level lies in the same plane.
  const lod = await page.evaluate(async () => {
    const { createPackAssembler } = await import("/viewer/src/stream.js");
    const r = window.__tessifc.renderer, gl = r.gl, originalPack = r.pack;
    const saved = { camera: structuredClone(r.camera), style: r.style, section: { ...r.section }, predicate: r.visibilityPredicate, lodPixels: r.lodPixels, motionLod: r.motionLod, occlusion: r.occlusionCulling };
    let indices = 0, calls = 0;
    const draw = gl.drawElementsInstanced.bind(gl);
    gl.drawElementsInstanced = (mode, count, type, offset, instances) => { calls++; indices += count * instances; return draw(mode, count, type, offset, instances); };
    const grid = (n, id) => {
      const positions = new Float32Array((n + 1) * (n + 1) * 3);
      for (let y = 0; y <= n; y++) for (let x = 0; x <= n; x++) positions.set([x / n * 4 - 2, y / n * 4 - 2, 0], (y * (n + 1) + x) * 3);
      const tris = [];
      const at = (x, y) => y * (n + 1) + x;
      for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) tris.push(at(x, y), at(x + 1, y), at(x + 1, y + 1), at(x, y), at(x + 1, y + 1), at(x, y + 1));
      return { id, positions, indices: Uint16Array.from(tris), bbox: [-2, -2, 0, 2, 2, 0], closed: false };
    };
    const fine = grid(64, 1);
    // The level uses every eighth grid vertex of the same array: 8x8 quads over the fine positions.
    const coarse = [];
    const step = 8, n = 64, at = (x, y) => y * (n + 1) + x;
    for (let y = 0; y < n; y += step) for (let x = 0; x < n; x += step) {
      coarse.push(at(x, y), at(x + step, y), at(x + step, y + step), at(x, y), at(x + step, y + step), at(x, y + step));
    }
    const level = { id: 2, positions: fine.positions, indices: Uint16Array.from(coarse), bbox: fine.bbox, closed: false, lod: { of: 1, level: 1 } };
    const chunk = {
      geometry: [fine, level],
      index: { classes: ["IfcSlab"], model_offset: [0, 0, 0] },
      instances: { count: 1, geometryIds: Uint32Array.from([1]), expressIds: Uint32Array.from([10]), classIds: new Uint16Array(1),
        transforms: Float32Array.from([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]), colors: Uint8Array.from([200, 120, 60, 255]), flags: new Uint16Array(1) },
      flags: 0, bytes: 0,
    };
    const assembler = createPackAssembler();
    assembler.append(chunk);
    const capture = (moving, motionLod) => {
      r.setMotionLod(motionLod);
      r.interacting = moving;
      r.drag = null;
      calls = 0; indices = 0;
      r.dirty = true;
      r.render(true);
      const pixels = new Uint8Array(r.canvas.width * r.canvas.height * 4);
      gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      return { pixels, calls, indices, coarseActive: r.displayInfo().coarseActive, reduced: r.lastFrameReduced };
    };
    const same = (a, b) => { for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false; return true; };
    const differing = (a, b) => { let n = 0; for (let i = 0; i < a.length; i += 4) if (a[i] !== b[i] || a[i + 1] !== b[i + 1] || a[i + 2] !== b[i + 2]) n++; return n; };
    try {
      r.setOcclusionCulling(false);
      r.load(assembler.pack());
      r.setStyle("shaded");
      r.setSection(false, "z", 0);
      r.setLodPixels(0);
      r.fit("perspective");
      const state = r.meshLevelState();
      const movingOn = capture(true, true);
      const movingOff = capture(true, false);
      const rest = capture(false, true);
      // A level that lands after the load is taken without re-uploading vertices.
      const late = createPackAssembler();
      late.append({ ...chunk, geometry: [fine] });
      r.load(late.pack());
      const before = capture(true, true);
      const uploads = { count: 0 };
      const bufferData = gl.bufferData;
      gl.bufferData = function (...args) { uploads.count += 1; return bufferData.apply(this, args); };
      const ids = late.addLodLevels([{ of: 1, level: 1, indices: Uint16Array.from(coarse) }]);
      const applied = r.applyLodLevels(late.pack(), ids);
      gl.bufferData = bufferData;
      const after = capture(true, true);
      return {
        state, movingOn: { indices: movingOn.indices, coarseActive: movingOn.coarseActive, reduced: movingOn.reduced },
        movingOff: { indices: movingOff.indices, coarseActive: movingOff.coarseActive },
        rest: { indices: rest.indices, coarseActive: rest.coarseActive, reduced: rest.reduced },
        pixelsOnOff: same(movingOn.pixels, movingOff.pixels), pixelsOnRest: same(movingOn.pixels, rest.pixels),
        differingOnOff: differing(movingOn.pixels, movingOff.pixels), differingOnRest: differing(movingOn.pixels, rest.pixels), total: movingOn.pixels.length / 4,
        late: { applied, uploads: uploads.count, beforeIndices: before.indices, afterIndices: after.indices, pixels: same(before.pixels, after.pixels), differing: differing(before.pixels, after.pixels) },
        error: gl.getError(),
      };
    } finally {
      gl.drawElementsInstanced = draw;
      r.setMotionLod(saved.motionLod);
      r.setOcclusionCulling(saved.occlusion);
      r.interacting = false;
      r.load(originalPack, saved.predicate);
      r.setLodPixels(saved.lodPixels);
      r.camera = saved.camera; r.style = saved.style; r.section = saved.section;
      r.dirty = true;
      r.render(true);
    }
  });
  check(lod.state.levels === 1 && lod.state.coarseBatches === 1 && lod.movingOn.coarseActive && lod.movingOn.reduced,
    `a pack's coarse level reaches the GPU and a moving frame draws it (${lod.state.levels} level, ${lod.state.coarseBatches} batch)`);
  check(lod.movingOn.indices === 8 * 8 * 6 && lod.movingOff.indices === 64 * 64 * 6 && lod.rest.indices === 64 * 64 * 6,
    `the coarse level submits the 8x8 grid, the fine frames the 64x64 one (${lod.movingOn.indices} of ${lod.movingOff.indices})`);
  // Two triangulations of one plane differ only where multisampling rounds along the rim: under a thousandth of the pixels.
  const rim = lod.total * 0.001;
  check(lod.differingOnOff <= rim && lod.differingOnRest <= rim && !lod.rest.coarseActive && !lod.rest.reduced && lod.error === 0,
    `the coarse frame paints the fine frame's pixels but for rim rounding, and the rest frame is never reduced (${lod.differingOnOff} and ${lod.differingOnRest} of ${lod.total} pixels differ)`);
  check(lod.late.applied.baked === 1 && lod.late.uploads === 1 && lod.late.beforeIndices === 64 * 64 * 6 && lod.late.afterIndices === 8 * 8 * 6 && lod.late.differing <= rim,
    `a level that lands after the load uploads its indices alone and takes effect (${lod.late.uploads} upload, ${lod.late.afterIndices} of ${lod.late.beforeIndices} indices)`);

  const viewport = page.viewportSize();
  await page.setViewportSize({ width: 1280, height: 900 });
  const quality = await page.evaluate(() => {
    const r = window.__tessifc.renderer, gl = r.gl;
    r.fit(); r.render(true);
    const original = [r.canvas.width, r.canvas.height];
    const batches = r.batches;
    const dense = Array(201).fill(batches[0]);
    r.batches = dense;
    r.prepareInteractionTarget();
    r.batches = batches;
    r.render(true);
    const methods = ["createFramebuffer", "createRenderbuffer", "deleteFramebuffer", "deleteRenderbuffer",
      "renderbufferStorage", "renderbufferStorageMultisample"];
    const originals = new Map(methods.map((name) => [name, gl[name]]));
    let allocations = 0, fixedCanvas = true, restored = true, fullQuality = true, painted = false;
    let differences = 0, restReduced = false;
    for (const name of methods) gl[name] = function (...args) {
      allocations++; return originals.get(name).apply(this, args);
    };
    try {
      for (let gesture = 0; gesture < 4; gesture++) {
        r.batches = dense; r.beginInteraction(); r.batches = batches;
        r.orbit(4, 2); r.render(true);
        fullQuality &&= r.target.frameWidth === original[0] && r.target.frameHeight === original[1];
        const movingTarget = r.target, movingSamples = r.target.samples;
        fixedCanvas &&= r.canvas.width === original[0] && r.canvas.height === original[1];
        const pixels = new Uint8Array(r.canvas.width * r.canvas.height * 4);
        gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
        for (let at = 4; at < pixels.length; at += 4) {
          if (Math.abs(pixels[at] - pixels[0]) + Math.abs(pixels[at + 1] - pixels[1]) +
            Math.abs(pixels[at + 2] - pixels[2]) > 30) { painted = true; break; }
        }
        r.interacting = false; r.dirty = true; r.render(true);
        restReduced ||= r.lastFrameReduced;
        restored &&= r.target === movingTarget && r.target.samples === movingSamples;
        const resting = new Uint8Array(pixels.length);
        gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, resting);
        for (let at = 0; at < pixels.length; at++) if (pixels[at] !== resting[at]) differences++;
      }
      const cacheSize = r.targetCache.size;
      const memory = r.renderTargetBytes();
      const accounted = [...r.targetCache.values()].reduce((sum, target) => sum + target.estimatedBytes, 0);
      const error = gl.getError();
      return { fixedCanvas, restored, fullQuality, differences, allocations, painted, cacheSize, memory, accounted, error, restReduced };
    } finally {
      r.interacting = false; r.batches = batches;
      for (const [name, fn] of originals) gl[name] = fn;
      r.releaseRenderTarget();
      r.render(true);
    }
  });
  check(quality.fullQuality && quality.fixedCanvas && quality.restored && quality.differences === 0 && !quality.restReduced,
    "dense gestures keep full resolution, antialiasing and identical pixels at the same camera position, and rest frames are never reduced");
  check(quality.allocations === 0, "repeated gestures allocate and delete no GPU render targets");
  check(quality.painted && quality.error === 0, "multisample presentation draws the moving model without WebGL errors");
  check(quality.cacheSize === 1 && quality.memory === quality.accounted,
    "render-target caching stays bounded and includes all target memory");

  // A panel folding and unfolding changes the canvas size; the targets grow at most once and are then reused.
  const countAllocations = async (action) => {
    await page.evaluate(() => {
      const gl = window.__tessifc.renderer.gl;
      const names = ["renderbufferStorage", "renderbufferStorageMultisample"];
      window.__folds = { count: 0, restore: null };
      const originals = names.map((name) => [name, gl[name]]);
      for (const [name, fn] of originals) gl[name] = function (...args) { window.__folds.count += 1; return fn.apply(this, args); };
      window.__folds.restore = () => { for (const [name, fn] of originals) gl[name] = fn; };
    });
    await action();
    await page.waitForTimeout(450);
    return page.evaluate(() => {
      const r = window.__tessifc.renderer, info = r.displayInfo();
      const count = window.__folds.count;
      window.__folds.restore();
      delete window.__folds;
      const fits = info.frameSize[0] === r.canvas.width && info.frameSize[1] === r.canvas.height &&
        (!info.targetSize || (info.targetSize[0] >= r.canvas.width && info.targetSize[1] >= r.canvas.height));
      return { count, fits };
    });
  };
  const toggleInspector = () => page.click('#rail [data-panel="properties"]');
  const folds = [await countAllocations(toggleInspector), await countAllocations(toggleInspector), await countAllocations(toggleInspector)];
  check(folds[1].count === 0 && folds[2].count === 0,
    `folding a panel back and forth reallocates no render target (${folds.map((f) => f.count).join(", ")})`);
  check(folds.every((f) => f.fits), "the frame follows the canvas inside a target at least as large");
  await page.setViewportSize(viewport);
  await page.evaluate(() => { window.__tessifc.renderer.fit(); window.__tessifc.render(); });
}
