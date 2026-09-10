// SPDX-License-Identifier: Apache-2.0

/** Keep the final native gesture and its overlay while GPU work is deferred. */
export async function checkGpuPacing(page, check) {
  const viewport = page.viewportSize();
  try {
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.evaluate(async () => {
      await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
      const r = window.__tessifc.renderer;
      r.resize(true); r.render(true);
    });
    await page.waitForFunction(() => {
      const r = window.__tessifc.renderer;
      return !r.resizeDirty && !r.resizeSettleTimer && !r.dirty
        && r.canvasCssWidth === r.container.clientWidth && r.canvasCssHeight === r.container.clientHeight;
    }, null, { polling: 20 });
    const { x, y } = await page.evaluate(() => {
      const canvas = window.__tessifc.renderer.canvas, rect = canvas.getBoundingClientRect();
      for (const fy of [.5, .4, .6, .3, .7]) for (const fx of [.5, .4, .6, .3, .7]) {
        const x = rect.left + rect.width * fx, y = rect.top + rect.height * fy;
        let clear = true;
        for (let dy = 0; dy <= 20; dy += 5) for (let dx = 0; dx <= 45; dx += 5) {
          clear &&= document.elementFromPoint(x + dx, y + dy) === canvas;
        }
        if (clear) return { x, y };
      }
      throw new Error("No unobstructed canvas path is available for the GPU pacing gestures");
    });
    await page.mouse.move(x, y);
    const filled = await page.evaluate(() => {
      const T = window.__tessifc, r = T.renderer, gl = r.gl;
      if (!r.gpuPacing?.info().supported || typeof r.onFrameReady !== "function") {
        throw new Error("GPU pacing and its application callback must be installed");
      }
      if (typeof T.tools?.drawOverlay !== "function") throw new Error("The browser test harness must expose tools.drawOverlay");
      const saved = {
        wait: gl.clientWaitSync, draw: r.draw, ready: r.onFrameReady, overlay: T.tools.drawOverlay,
        limit: r.gpuPacing.info().limit, camera: structuredClone(r.camera),
        interactionScale: r.interactionScale, gestureScale: r.gestureScale, cameraTouched: r.cameraTouched,
      };
      const P = window.__gpuPacingTest = { busy: true, draws: 0, callbacks: 0, lastDraw: null, overlay: null };
      P.restore = () => {
        r.gpuPacing.reset();
        gl.clientWaitSync = saved.wait; r.draw = saved.draw; r.onFrameReady = saved.ready;
        T.tools.drawOverlay = saved.overlay;
        r.gpuPacing.setLimit(saved.limit);
        r.camera = saved.camera; r.interacting = false; r.gestureScale = saved.gestureScale;
        r.cameraTouched = saved.cameraTouched; r.setInteractionScale(saved.interactionScale);
        r.resize(true); r.render(true); T.tools.drawOverlay();
        delete window.__gpuPacingTest;
      };
      gl.clientWaitSync = function (...args) { return P.busy ? gl.TIMEOUT_EXPIRED : saved.wait.apply(this, args); };
      r.draw = function (...args) {
        const result = saved.draw.apply(this, args);
        if (!args[0]) {
          P.draws++;
          P.lastDraw = { camera: JSON.stringify(this.camera), count: P.draws,
            frame: [this.target?.frameWidth ?? this.canvas.width, this.target?.frameHeight ?? this.canvas.height],
            canvas: [this.canvas.width, this.canvas.height], slot: this.target?.slot ?? null };
        }
        return result;
      };
      r.onFrameReady = function (...args) { P.callbacks++; return saved.ready.apply(this, args); };
      T.tools.drawOverlay = function (...args) {
        const result = saved.overlay.apply(this, args);
        P.overlay = { camera: JSON.stringify(r.camera), count: P.draws };
        return result;
      };
      P.fill = () => {
        P.busy = true;
        r.gpuPacing.setLimit(2);
        const interacting = r.interacting, drag = r.drag;
        try {
          r.interacting = true; r.drag = null;
          r.render(true); r.dirty = true; r.render();
          return { count: P.draws, pending: r.gpuPacing.info().pending };
        } finally {
          r.interacting = interacting; r.drag = drag;
        }
      };
      r.interacting = false; r.setInteractionScale(1); r.resize(true);
      return P.fill();
    });
    check(filled.pending === 2, "two submitted frames fill the GPU queue");
    await page.mouse.down();
    if (!await page.evaluate(() => window.__tessifc.renderer.pointers.size > 0)) {
      throw new Error("The native orbit pointerdown did not reach the canvas");
    }
    await page.mouse.move(x + 25, y + 10);
    await page.mouse.move(x + 40, y + 15);
    await page.mouse.up();
    await page.waitForFunction(() => window.__gpuPacingTest.callbacks > 0, null, { polling: 20 });
    const blocked = await page.evaluate(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      return { dirty: r.dirty, interacting: r.interacting, draws: P.draws,
        cameraPending: P.lastDraw.camera !== JSON.stringify(r.camera), pending: r.gpuPacing.info().pending };
    });
    check(blocked.dirty && !blocked.interacting && blocked.cameraPending && blocked.pending === 2 && blocked.draws === filled.count,
      "orbit release preserves the pending camera without submitting past the GPU queue limit");
    await page.evaluate(() => { window.__gpuPacingTest.busy = false; });
    await page.waitForFunction(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      return !r.dirty && P.lastDraw.camera === JSON.stringify(r.camera)
        && P.overlay?.count === P.lastDraw.count && P.overlay.camera === P.lastDraw.camera;
    }, null, { polling: 20 });
    check(await page.evaluate(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      return P.lastDraw.frame.every((value, axis) => value === P.lastDraw.canvas[axis])
        && P.overlay.camera === JSON.stringify(r.camera);
    }), "the application retry submits the final camera at full resolution and updates its overlay");
    const idle = await page.evaluate(async () => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      const draws = P.draws, polls = r.gpuPacing.info().polls;
      await new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)));
      return P.draws === draws && r.gpuPacing.info().polls === polls && !r.dirty;
    });
    check(idle, "settled full-quality GPU pacing submits no idle frames or fence polls");

    const resized = await page.evaluate(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest, filled = P.fill();
      // Resetting the backing store clears it even while earlier work is pending.
      r.canvas.width += 1; r.dirty = true; r.render();
      const result = { drawn: P.draws === filled.count + 1, dirty: r.dirty,
        width: P.lastDraw.canvas[0], actualWidth: r.canvas.width, pending: r.gpuPacing.info().pending };
      P.busy = false; r.resize(true); r.render(true);
      return result;
    });
    check(resized.drawn && !resized.dirty && resized.width === resized.actualWidth && resized.pending === 1,
      "a reset canvas backing store bypasses the busy queue and receives a frame");

    await page.mouse.move(x, y);
    await page.evaluate(() => window.__tessifc.renderer.setInteractionScale(.5));
    await page.mouse.down({ button: "right" });
    if (!await page.evaluate(() => window.__tessifc.renderer.pointers.size > 0)) {
      throw new Error("The native pan pointerdown did not reach the canvas");
    }
    const reduced = await page.evaluate(() => {
      const P = window.__gpuPacingTest;
      const filled = P.fill();
      return { ...filled, slot: P.lastDraw.slot };
    });
    await page.mouse.move(x + 20, y + 10);
    await page.mouse.up({ button: "right" });
    const release = await page.evaluate(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      r.render();
      return { dirty: r.dirty, interacting: r.interacting, draws: P.draws, slot: P.lastDraw.slot };
    });
    check(reduced.slot === "gesture" && release.slot === "gesture" && release.dirty && !release.interacting && release.draws === reduced.count,
      "a reduced-quality gesture keeps its full-quality release pending while the GPU is busy");
    await page.evaluate(() => { window.__gpuPacingTest.busy = false; });
    await page.waitForFunction(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      return !r.dirty && P.lastDraw.slot === "full" && P.lastDraw.camera === JSON.stringify(r.camera)
        && P.lastDraw.frame.every((value, axis) => value === P.lastDraw.canvas[axis]);
    }, null, { polling: 20 });
    check(true, "the deferred gesture release eventually restores the full-resolution final camera");
    const preparation = await page.evaluate(() => {
      const r = window.__tessifc.renderer, P = window.__gpuPacingTest;
      const target = r.target, dirty = r.dirty, draws = P.draws;
      for (const cached of [...r.targetCache.values()]) if (cached.slot === "gesture") r.deleteRenderTarget(cached);
      r.prepareInteractionTarget(false);
      const clean = !dirty && !r.dirty && r.target === target && P.draws === draws
        && [...r.targetCache.values()].some((cached) => cached.slot === "gesture");
      r.dirty = true;
      r.prepareInteractionTarget(false);
      const pending = r.dirty;
      r.dirty = dirty;
      return { clean, pending };
    });
    check(preparation.clean && preparation.pending,
      "background gesture-target allocation preserves both clean frames and pending redraws");
  } finally {
    try {
      await page.mouse.up();
      await page.mouse.up({ button: "right" });
    } finally {
      try { await page.evaluate(() => window.__gpuPacingTest?.restore()); }
      finally { if (viewport) await page.setViewportSize(viewport); }
    }
  }
}
