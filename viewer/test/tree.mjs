// SPDX-License-Identifier: Apache-2.0

/** The structure panel against the running viewer: one frame per click, sliced expansion, skipped lists, keyboard. */
export async function checkTree(page, check) {
  const viewport = page.viewportSize();
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.waitForTimeout(120);
  // Both side panels open, whatever the earlier narrow-layout checks left behind.
  if (await page.locator("#outliner.collapsed").count()) await page.click("#outliner-open");
  if (await page.locator("#inspector.collapsed").count()) await page.click('#rail [data-panel="properties"]');
  await page.waitForTimeout(120);
  await page.evaluate(() => { window.__tessifc.renderer.fit(); window.__tessifc.render(); });

  const point = await page.evaluate(() => {
    const r = window.__tessifc.renderer, rect = r.canvas.getBoundingClientRect();
    for (let y = 0.2; y < 0.8; y += 0.1) for (let x = 0.2; x < 0.8; x += 0.1) {
      const px = rect.left + x * rect.width, py = rect.top + y * rect.height;
      if (document.elementFromPoint(px, py) !== r.canvas) continue;
      const hit = r.pick(px, py, false);
      if (hit) return { x: px, y: py, record: hit.record };
    }
    throw new Error("fixture has no unobstructed pickable point");
  });
  await page.evaluate(() => {
    const r = window.__tessifc.renderer, render = r.render;
    window.__treeTest = { renders: 0, restore: () => { r.render = render; delete window.__treeTest; } };
    r.render = function (...args) {
      if (this.dirty || args[0]) window.__treeTest.renders += 1;
      return render.apply(this, args);
    };
  });
  await page.mouse.click(point.x, point.y);
  await page.waitForFunction((record) => window.__tessifc.state.selection?.record === record, point.record, { polling: 20 });
  await page.waitForTimeout(250);
  const click = await page.evaluate(() => {
    const renders = window.__treeTest.renders;
    window.__treeTest.restore();
    const host = document.getElementById("tree-spatial");
    const row = host.querySelector(".trow.selected");
    const a = row?.getBoundingClientRect(), b = host.getBoundingClientRect();
    return { renders, revealed: Boolean(row) && a.top >= b.top - 1 && a.bottom <= b.bottom + 1 };
  });
  check(click.renders === 1, `a click selects in its release handler and paints one frame (${click.renders})`);
  check(click.revealed, "the selected element's row is opened and scrolled into view in the structure panel");

  await page.click("#tree-expand");
  await page.waitForFunction(() => !window.__tessifc.tree.expanding(), null, { polling: 20 });
  const expanded = await page.evaluate(() => ({
    closed: document.querySelectorAll("#tree-spatial .tnode.class-group:not(.open)").length,
    rows: document.querySelectorAll("#tree-spatial .tnode.element").length,
    products: window.__tessifc.state.model.index.recordsByExpressId.size,
  }));
  check(expanded.closed === 0 && expanded.rows === expanded.products,
    `expanding every branch opens every class group and lists every product (${expanded.rows} of ${expanded.products})`);

  // A short panel puts most lists off screen, where the browser skips them; their rows must still be right when they scroll in.
  const rowsAgree = async (label) => {
    const mismatches = await page.evaluate(async () => {
      const host = document.getElementById("tree-spatial");
      const T = window.__tessifc, r = T.renderer, index = T.state.model.index;
      const settle = () => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(done, 30))));
      const wrong = [];
      for (const fraction of [0, 0.5, 1]) {
        host.scrollTop = fraction * (host.scrollHeight - host.clientHeight);
        await settle();
        const view = host.getBoundingClientRect();
        for (const item of host.querySelectorAll(".tnode.element")) {
          const box = item.getBoundingClientRect();
          if (box.bottom < view.top || box.top > view.bottom || box.height === 0) continue;
          const id = Number(item.getAttribute("aria-label").match(/#(\d+)$/)?.[1]);
          const records = index.recordsByExpressId.get(id) ?? [];
          const shown = records.some((record) => r.visibility[record * 2] >= 128);
          if (item.classList.contains("off") === shown) wrong.push(id);
        }
      }
      return wrong;
    });
    check(mismatches.length === 0, `${label}: every row on screen shows the viewport's state (${mismatches.length} wrong)`);
  };
  await page.evaluate(() => { document.getElementById("tree-spatial").style.maxHeight = "72px"; });
  await page.waitForTimeout(100);
  await page.click("#dock-isolate");
  await page.waitForTimeout(100);
  await rowsAgree("after isolating the selection");
  await page.click("#dock-isolate");
  await page.waitForTimeout(100);
  await rowsAgree("after showing everything again");
  await page.evaluate(() => { document.getElementById("tree-spatial").style.maxHeight = ""; });

  const keyboard = await page.evaluate(() => {
    const first = document.querySelector("#tree-spatial .tnode");
    first.focus();
    return document.activeElement === first;
  });
  await page.keyboard.press("ArrowDown");
  const moved = await page.evaluate(() => {
    const first = document.querySelector("#tree-spatial .tnode");
    const active = document.activeElement;
    return active !== first && active.classList.contains("tnode") && active.tabIndex === 0 && first.tabIndex === -1;
  });
  check(keyboard && moved, "arrow keys step to the next tree item and move the single tab stop with them");

  await page.click("#tree-collapse");
  check(await page.evaluate(() => document.querySelectorAll("#tree-spatial .tnode.class-group.open").length === 0),
    "collapsing returns to the spatial containers");
  await page.setViewportSize(viewport);
  await page.waitForTimeout(120);
  await page.evaluate(() => { window.__tessifc.renderer.fit(); window.__tessifc.render(); });
}
