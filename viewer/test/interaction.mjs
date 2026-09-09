// SPDX-License-Identifier: Apache-2.0
/** Navigation and responsive-layout assertions against the running renderer. */
export async function checkInteraction(page, check) {
  const navigation = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    r.fit(); r.render(true);
    const rect = r.canvas.getBoundingClientRect();
    let pointer;
    for (let y = 0.25; y < 0.8 && !pointer; y += 0.1) {
      for (let x = 0.25; x < 0.8 && !pointer; x += 0.1) {
        const px = rect.left + x * rect.width, py = rect.top + y * rect.height;
        const hit = r.pickSurface(px, py);
        if (hit) pointer = { px, py, point: hit.point };
      }
    }
    if (!pointer) throw new Error("fixture has no pickable surface");
    const before = r.project(pointer.point);
    r.canvas.dispatchEvent(new WheelEvent("wheel", { clientX: pointer.px, clientY: pointer.py, deltaY: -100, cancelable: true }));
    r.render(true);
    const after = r.project(pointer.point);
    for (let step = 0; step < 25; step++) r.zoomAt(0.7, pointer.px, pointer.py, true);
    const closeDistance = r.camera.distance;
    r.fit(); r.render(true);
    const saved = JSON.parse(JSON.stringify(r.camera));
    const height = r.canvas.height;
    r.pan(20, 0);
    const firstPan = r.camera.target.slice();
    r.camera = JSON.parse(JSON.stringify(saved));
    r.canvas.height = height * 2;
    r.pan(20, 0);
    const secondPan = r.camera.target.slice();
    r.canvas.height = height;
    r.resizeDirty = true;
    r.fit(); r.render(true);
    return { drift: Math.hypot(before.x - after.x, before.y - after.y), closeDistance, panDifference: Math.hypot(...firstPan.map((value, i) => value - secondPan[i])) };
  });
  check(navigation.drift < 0.5, `zoom keeps a picked surface under the cursor (${navigation.drift.toFixed(3)} px)`);
  check(navigation.closeDistance < 0.01, "zoom reaches small details below the old one-centimetre pivot limit");
  check(navigation.panDifference < 1e-9, "pan distance is independent of drawing-buffer resolution");
  await page.waitForTimeout(150);
  const before = await page.evaluate(() => window.__tessifc.renderer.camera.distance);
  await page.click("#dock-zoom-in");
  const after = await page.evaluate(() => window.__tessifc.renderer.camera.distance);
  check(after < before, "the zoom button moves the camera");
  await page.locator("#viewport").focus();
  await page.keyboard.press("-");
  check(await page.evaluate(() => window.__tessifc.renderer.camera.distance) > after, "the minus shortcut zooms out");
  const viewport = page.viewportSize();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.waitForTimeout(100);
  check(await page.evaluate(() => ["outliner", "inspector", "editor"].every((id) => document.getElementById(id).classList.contains("collapsed"))), "narrow layouts start with the model unobstructed");
  const zoom = await page.locator("#dock-zoom-in").boundingBox();
  check(await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.closest("#dock-zoom-in") !== null, { x: zoom.x + zoom.width / 2, y: zoom.y + zoom.height / 2 }), "mobile zoom controls are not covered by a folded panel");
  await page.click("#outliner-open");
  await page.click('#rail [data-panel="properties"]');
  check(await page.locator("#outliner").evaluate((element) => element.classList.contains("collapsed")), "mobile panels open one at a time");
  await page.click("#inspector-close");
  check(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "the mobile page has no horizontal overflow");
  await page.evaluate(() => { window.__tessifc.renderer.fit(); window.__tessifc.render(); });
  const touchStart = await page.evaluate(() => window.__tessifc.renderer.camera.distance);
  const session = await page.context().newCDPSession(page);
  await session.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: 130, y: 400, id: 1 }, { x: 250, y: 400, id: 2 }] });
  await session.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: 100, y: 420, id: 1 }, { x: 280, y: 420, id: 2 }] });
  const pinched = await page.evaluate(() => window.__tessifc.renderer.camera.distance);
  await session.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await session.detach();
  await page.waitForTimeout(80);
  check(pinched < touchStart, "a two-finger pinch zooms the camera");
  check(await page.evaluate(() => Math.abs(visualViewport.scale - 1) < 0.001), "pinching the model does not also zoom the browser page");
  check(await page.evaluate(() => window.__tessifc.renderer.pointers.size === 0 && !window.__tessifc.renderer.drag), "touch release clears all gesture state");
  await page.setViewportSize(viewport);
  await page.evaluate(() => { window.__tessifc.renderer.fit(); window.__tessifc.render(); });
}
