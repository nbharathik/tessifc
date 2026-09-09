// SPDX-License-Identifier: Apache-2.0

/** Exercise actual input handlers when animation callbacks stop arriving. */
export async function checkScheduling(page, check) {
  const viewport = await page.locator("#viewport").boundingBox();
  const x = viewport.x + viewport.width * .5, y = viewport.y + viewport.height * .5;
  await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const request = window.requestAnimationFrame, cancel = window.cancelAnimationFrame;
    const render = r.render, pick = r.pick;
    const callbacks = new Map();
    const probe = window.__blockedAnimationTest = { frames: 0, picks: 0 };
    let next = 1;
    window.requestAnimationFrame = (callback) => { const id = next++; callbacks.set(id, callback); return id; };
    window.cancelAnimationFrame = (id) => callbacks.delete(id);
    r.render = function (...args) { probe.frames++; return render.apply(this, args); };
    r.pick = function (...args) { probe.picks++; return pick.apply(this, args); };
    probe.restore = () => {
      window.requestAnimationFrame = request; window.cancelAnimationFrame = cancel;
      r.render = render; r.pick = pick;
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
    await page.mouse.up();
    await page.waitForFunction((frames) => window.__blockedAnimationTest.frames > frames, during.frames, { polling: 20 });
    check(await page.evaluate(() => !window.__tessifc.renderer.interacting),
      "pointer release restores the resting frame without an animation callback");

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
