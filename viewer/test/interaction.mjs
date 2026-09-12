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
    // Zoom must leave the orbit target alone, or rotating after it swings the
    // model around a pivot that is no longer on it.
    const world = () => r.camera.target.map((value, axis) => value + r.renderOrigin[axis]);
    const pivotBefore = world();
    const screenBefore = r.project(pivotBefore);
    r.canvas.dispatchEvent(new WheelEvent("wheel", { clientX: pointer.px, clientY: pointer.py, deltaY: 240, cancelable: true }));
    r.render(true);
    const pivotAfter = world();
    const screenAfter = r.project(pivotAfter);
    const pivotMoved = Math.hypot(...pivotBefore.map((value, axis) => value - pivotAfter[axis]));
    const pivotDrift = Math.hypot(screenBefore.x - screenAfter.x, screenBefore.y - screenAfter.y);
    let orbitDrift = 0;
    for (let step = 0; step < 24; step++) {
      r.orbit(40, 7);
      r.render(true);
      const at = r.project(world());
      orbitDrift = Math.max(orbitDrift, Math.hypot(at.x - screenAfter.x, at.y - screenAfter.y));
    }
    r.fit(); r.render(true);
    for (let step = 0; step < 25; step++) r.zoomAt(0.7);
    const closeDistance = r.camera.distance;
    // A click pivots at the depth of the surface it hit without turning the camera,
    // so zooming afterwards reaches that surface rather than the model centre.
    r.fit(); r.render(true);
    const forwardBefore = r.cameraBasis().forward;
    const positionBefore = r.camera.position.slice();
    const pivoted = r.setPivot(pointer.point);
    const forwardAfter = r.cameraBasis().forward;
    const pivotTurn = Math.hypot(...forwardBefore.map((value, axis) => value - forwardAfter[axis]));
    const pivotCameraMoved = Math.hypot(...positionBefore.map((value, axis) => value - r.camera.position[axis]));
    const hitLocal = pointer.point.map((value, axis) => value - r.renderOrigin[axis]);
    const hitDepth = hitLocal.reduce((sum, value, axis) => sum + (value - r.camera.position[axis]) * forwardAfter[axis], 0);
    const pivotDepthError = Math.abs(hitDepth - r.camera.distance);
    for (let step = 0; step < 40; step++) r.zoomAt(0.7);
    const reachedDistance = hitLocal.reduce((sum, value, axis) => sum + (value - r.camera.position[axis]) * forwardAfter[axis], 0);
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
    return { pivotMoved, pivotDrift, orbitDrift, closeDistance, pivoted, pivotTurn, pivotCameraMoved, pivotDepthError, reachedDistance,
      panDifference: Math.hypot(...firstPan.map((value, i) => value - secondPan[i])) };
  });
  check(navigation.pivoted && navigation.pivotTurn < 1e-9 && navigation.pivotCameraMoved < 1e-9, `a click pivot keeps the camera and its direction (turn ${navigation.pivotTurn.toExponential(1)})`);
  check(navigation.pivotDepthError < 1e-6, `the pivot sits at the depth of the clicked surface (${navigation.pivotDepthError.toExponential(1)} m off)`);
  check(navigation.reachedDistance < 0.05, `zooming after a click reaches the depth of the clicked surface (${navigation.reachedDistance.toFixed(3)} m short)`);
  check(navigation.pivotMoved < 1e-9, `zooming out leaves the orbit target where it was (${navigation.pivotMoved.toExponential(1)} m)`);
  check(navigation.pivotDrift < 0.5, `the orbit target holds its place on screen through a zoom (${navigation.pivotDrift.toFixed(3)} px)`);
  check(navigation.orbitDrift < 0.5, `a zoomed-out orbit turns about the target instead of swinging it (${navigation.orbitDrift.toFixed(3)} px)`);
  check(navigation.closeDistance < 0.01, "zoom reaches small details below the old one-centimetre pivot limit");
  check(navigation.panDifference < 1e-9, "pan distance is independent of drawing-buffer resolution");
  // The right side holds one panel at a time: the edit and session panels take the
  // inspector's place and give it back when they close. Wide layouts only; narrow ones
  // already open one panel at a time.
  const originalViewport = page.viewportSize();
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.waitForTimeout(100);
  const panels = await page.evaluate(() => {
    const { shell } = window.__tessifc;
    const visible = () => ["inspector", "editor", "session"].filter((name) => shell.panelVisible(name));
    shell.setPanel("inspector", true);
    shell.setPanel("editor", true);
    const editorOnly = visible();
    shell.setPanel("session", true);
    const sessionOnly = visible();
    shell.setPanel("session", false);
    const inspectorBack = visible();
    shell.setPanel("editor", true);
    shell.setPanel("inspector", true);
    const inspectorWins = visible();
    // Every right-side panel is the same width, so swapping them leaves the viewport alone.
    const widths = {};
    const canvasWidths = {};
    for (const name of ["inspector", "editor", "session"]) {
      shell.setPanel(name, true);
      widths[name] = document.getElementById(name).getBoundingClientRect().width;
      canvasWidths[name] = document.getElementById("viewport").getBoundingClientRect().width;
    }
    shell.setPanel("inspector", true);
    return { editorOnly, sessionOnly, inspectorBack, inspectorWins, widths, canvasWidths };
  });
  check(JSON.stringify(panels.editorOnly) === '["editor"]' && JSON.stringify(panels.sessionOnly) === '["session"]', `an edit or session panel replaces the inspector (${panels.editorOnly}, ${panels.sessionOnly})`);
  check(JSON.stringify(panels.inspectorBack) === '["inspector"]' && JSON.stringify(panels.inspectorWins) === '["inspector"]', `closing the overlay gives the inspector back (${panels.inspectorBack}, ${panels.inspectorWins})`);
  const sameWidth = new Set(Object.values(panels.widths)).size === 1 && new Set(Object.values(panels.canvasWidths)).size === 1;
  check(sameWidth && panels.widths.inspector > 0, `right-side panels share one width and the viewport keeps its size (${JSON.stringify(panels.widths)}, viewport ${JSON.stringify(panels.canvasWidths)})`);
  await page.setViewportSize(originalViewport);
  await page.waitForTimeout(100);
  await page.waitForTimeout(150);
  const unchanged = await page.evaluate(() => {
    const r = window.__tessifc.renderer;
    const camera = JSON.stringify(r.camera);
    r.render(true);
    r.orbit(0, 0); r.pan(0, 0);
    return !r.dirty && JSON.stringify(r.camera) === camera;
  });
  check(unchanged, "zero-distance orbit and pan preserve the camera and its painted frame");
  const wheelNotifications = await page.evaluate(() => {
    const r = window.__tessifc.renderer, rect = r.canvas.getBoundingClientRect();
    const draw = r.draw, resize = r.resize, change = r.onCameraChange;
    const probe = window.__wheelFrameTest = { frames: 0, resizes: 0, changes: 0 };
    r.draw = function (...args) {
      if (!args[0]) probe.frames++;
      return draw.apply(this, args);
    };
    r.resize = function (...args) { probe.resizes++; return resize.apply(this, args); };
    r.onCameraChange = function (...args) { probe.changes++; return change.apply(this, args); };
    probe.restore = () => { r.draw = draw; r.resize = resize; r.onCameraChange = change; delete window.__wheelFrameTest; };
    r.canvas.dispatchEvent(new WheelEvent("wheel", {
      clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2, deltaY: -100, cancelable: true,
    }));
    return probe.changes;
  });
  try {
    await page.waitForTimeout(220);
    const wheel = await page.evaluate(() => ({
      frames: window.__wheelFrameTest.frames,
      resizes: window.__wheelFrameTest.resizes,
      interacting: window.__tessifc.renderer.interacting,
    }));
    check(wheelNotifications === 1 && wheel.frames === 1 && wheel.resizes === 0 && !wheel.interacting,
      "one wheel movement paints once without a duplicate settled frame or canvas resize");
  } finally {
    await page.evaluate(() => window.__wheelFrameTest.restore());
  }
  const dragStart = await page.evaluate(() => {
    const r = window.__tessifc.renderer, rect = r.canvas.getBoundingClientRect();
    window.__dragPickCount = 0;
    window.__dragOriginalPick = r.pick;
    r.pick = function (...args) { window.__dragPickCount++; return window.__dragOriginalPick.apply(this, args); };
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  });
  let dragPicks;
  try {
    await page.mouse.move(dragStart.x, dragStart.y);
    await page.mouse.down();
    await page.mouse.move(dragStart.x + 35, dragStart.y + 12);
    await page.mouse.move(dragStart.x, dragStart.y);
    await page.mouse.up();
    dragPicks = await page.evaluate(() => window.__dragPickCount);
  } finally {
    await page.evaluate(() => {
      window.__tessifc.renderer.pick = window.__dragOriginalPick;
      delete window.__dragOriginalPick; delete window.__dragPickCount;
    });
  }
  check(dragPicks === 0, "an orbit that returns to its starting point does not pick or select on release");
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
