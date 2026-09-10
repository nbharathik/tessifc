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
    let differences = 0;
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
        restored &&= r.target === movingTarget && r.target.samples === movingSamples;
        const resting = new Uint8Array(pixels.length);
        gl.readPixels(0, 0, r.canvas.width, r.canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, resting);
        for (let at = 0; at < pixels.length; at++) if (pixels[at] !== resting[at]) differences++;
      }
      const cacheSize = r.targetCache.size;
      const memory = r.renderTargetBytes();
      const accounted = [...r.targetCache.values()].reduce((sum, target) => sum + target.estimatedBytes, 0);
      const error = gl.getError();
      return { fixedCanvas, restored, fullQuality, differences, allocations, painted, cacheSize, memory, accounted, error };
    } finally {
      r.interacting = false; r.batches = batches;
      for (const [name, fn] of originals) gl[name] = fn;
      r.releaseRenderTarget();
      r.render(true);
    }
  });
  check(quality.fullQuality && quality.fixedCanvas && quality.restored && quality.differences === 0,
    "dense gestures keep full resolution, antialiasing and identical pixels at the same camera position");
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
