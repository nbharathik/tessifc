// SPDX-License-Identifier: Apache-2.0

/** Exercise actual input handlers when animation callbacks stop arriving. */
export async function checkScheduling(page, check) {
  const viewport = await page.locator("#viewport").boundingBox();
  const x = viewport.x + viewport.width * .5, y = viewport.y + viewport.height * .5;
  await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const request = window.requestAnimationFrame, cancel = window.cancelAnimationFrame;
    const draw = r.draw, pick = r.pick;
    const callbacks = new Map();
    const probe = window.__blockedAnimationTest = { frames: 0, gestureFrames: 0, picks: 0, paintedCamera: null };
    let next = 1;
    window.requestAnimationFrame = (callback) => { const id = next++; callbacks.set(id, callback); return id; };
    window.cancelAnimationFrame = (id) => callbacks.delete(id);
    r.draw = function (...args) {
      const result = draw.apply(this, args);
      if (!args[0]) {
        probe.frames++;
        if (this.target?.slot === "gesture") probe.gestureFrames++;
        probe.paintedCamera = JSON.stringify(this.camera);
      }
      return result;
    };
    r.pick = function (...args) { probe.picks++; return pick.apply(this, args); };
    probe.restore = () => {
      window.requestAnimationFrame = request; window.cancelAnimationFrame = cancel;
      r.draw = draw; r.pick = pick;
      for (const callback of callbacks.values()) request(callback);
      delete window.__blockedAnimationTest;
    };
  });
  try {
    const before = await page.evaluate(() => window.__tessifc.renderer.camera.position.slice());
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 15, y + 10);
    await page.waitForTimeout(70);
    await page.mouse.move(x + 30, y + 20);
    await page.waitForFunction(() => window.__blockedAnimationTest.frames > 0, null, { polling: 20 });
    const during = await page.evaluate(() => ({
      position: window.__tessifc.renderer.camera.position.slice(),
      frames: window.__blockedAnimationTest.frames,
    }));
    check(during.position.some((value, i) => Math.abs(value - before[i]) > 1e-6) && during.frames > 0,
      "pointer orbit paints while animation callbacks are held");
    const finalCamera = await page.evaluate(() => {
      const r = window.__tessifc.renderer;
      r.orbit(5, 3);
      const camera = JSON.stringify(r.camera);
      return { camera, pending: r.dirty && window.__blockedAnimationTest.paintedCamera !== camera };
    });
    check(finalCamera.pending, "the final camera update remains pending before release");
    await page.mouse.up();
    await page.waitForFunction((camera) => window.__blockedAnimationTest.paintedCamera === camera, finalCamera.camera, { polling: 20 });
    check(await page.evaluate(() => !window.__tessifc.renderer.interacting),
      "pointer release paints the final pending camera without an animation callback");

    const settledFrames = await page.evaluate(() => window.__blockedAnimationTest.frames);
    await page.mouse.down({ button: "right" });
    await page.mouse.move(x + 30, y + 20);
    await page.mouse.up({ button: "right" });
    await page.waitForTimeout(80);
    check(await page.evaluate((frames) => window.__blockedAnimationTest.frames === frames && !window.__tessifc.renderer.resizeDirty, settledFrames),
      "a stationary pointer gesture submits no frame and requests no canvas resize");

    await page.evaluate(() => {
      const r = window.__tessifc.renderer;
      r.setInteractionScale(.5);
      r.render(true);
    });
    try {
      await page.mouse.down({ button: "right" });
      await page.mouse.move(x + 45, y + 30);
      await page.waitForFunction(() => window.__tessifc.renderer.target?.slot === "gesture", null, { polling: 20 });
      await page.mouse.up({ button: "right" });
      await page.waitForFunction(() => window.__tessifc.renderer.target?.slot === "full", null, { polling: 20 });
      check(await page.evaluate(() => {
        const r = window.__tessifc.renderer;
        return r.target.frameWidth === r.canvas.width && r.target.frameHeight === r.canvas.height && !r.interacting;
      }), "an explicit reduced gesture scale restores full quality on release without an animation callback");
      const wheelBefore = await page.evaluate(({ x, y }) => {
        const r = window.__tessifc.renderer, probe = window.__blockedAnimationTest;
        const before = { frames: probe.frames, gestureFrames: probe.gestureFrames };
        r.canvas.dispatchEvent(new WheelEvent("wheel", { clientX: x, clientY: y, deltaY: -80, cancelable: true }));
        return before;
      }, { x: x + 45, y: y + 30 });
      await page.waitForFunction(() => {
        const r = window.__tessifc.renderer;
        return !r.wheelQualityTimer && !r.dirty && r.target?.slot === "full";
      }, null, { polling: 20 });
      check(await page.evaluate((before) => {
        const probe = window.__blockedAnimationTest;
        return probe.frames === before.frames + 2 && probe.gestureFrames === before.gestureFrames + 1;
      }, wheelBefore), "an explicit reduced wheel scale paints its moving and restored frames without animation callbacks");
    } finally {
      await page.mouse.up({ button: "right" });
      await page.evaluate(() => {
        const r = window.__tessifc.renderer;
        r.setInteractionScale(1);
        r.render(true);
      });
    }

    const point = await page.evaluate(() => {
      const r = window.__tessifc.renderer, rect = r.canvas.getBoundingClientRect();
      for (let y = .2; y < .8; y += .1) for (let x = .2; x < .8; x += .1) {
        const px = rect.left + x * rect.width, py = rect.top + y * rect.height;
        if (document.elementFromPoint(px, py) !== r.canvas) continue;
        const hit = r.pick(px, py, false);
        if (hit) return { x: px, y: py, record: hit.record };
      }
      throw new Error("fixture has no unobstructed pickable point");
    });
    const picks = await page.evaluate(() => window.__blockedAnimationTest.picks);
    await page.mouse.click(point.x, point.y);
    await page.waitForFunction(({ record, picks }) =>
      window.__blockedAnimationTest.picks > picks && window.__tessifc.state.selection?.record === record,
    { record: point.record, picks }, { polling: 20 });
    check(await page.evaluate(() => !document.body.classList.contains("picking")),
      "click selection completes without two animation callbacks or an idle callback");
  } finally {
    await page.mouse.up();
    await page.evaluate(() => window.__blockedAnimationTest.restore());
  }
}
