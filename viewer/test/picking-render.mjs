// SPDX-License-Identifier: Apache-2.0

/** Compare accelerated picks with the original scan through renderer state changes. */
export async function checkPicking(page, check) {
  const result = await page.evaluate(async () => {
    const { buildBoundsTree } = await import("/viewer/src/picking.js");
    const r = window.__tessifc.renderer;
    const pack = r.pack;
    const saved = {
      camera: structuredClone(r.camera), section: { ...r.section }, style: r.style,
      predicate: r.visibilityPredicate, selected: r.selected.slice(), touched: r.cameraTouched,
    };
    let comparisons = 0, hits = 0, differences = 0;
    const rays = () => {
      const rect = r.canvas.getBoundingClientRect(), output = [];
      for (let y = .2; y <= .8; y += .15) {
        for (let x = .2; x <= .8; x += .15) output.push(r.pointerRay(rect.left + x * rect.width, rect.top + y * rect.height));
      }
      for (let record = 0; record < Math.min(r.recordLocations.length, 32); record++) {
        const box = r.recordLocations[record]?.bounds;
        if (!box) continue;
        const origin = r.camera.position.slice();
        const direction = box.min.map((low, axis) => (low + box.max[axis]) / 2 - origin[axis]);
        const length = Math.hypot(...direction);
        if (length) output.push({ origin, direction: direction.map((value) => value / length) });
      }
      return output;
    };
    const compare = (tree) => {
      const originalTree = r.pickTree;
      try {
        for (const ray of rays()) {
          r.pickTree = tree;
          const accelerated = r.pickRecord(ray);
          r.pickTree = null;
          const linear = r.pickRecord(ray);
          comparisons++;
          if (linear) hits++;
          if (JSON.stringify(accelerated) !== JSON.stringify(linear)) differences++;
        }
      } finally {
        r.pickTree = originalTree;
      }
    };
    let loadedTree = false, reloadedTree = false, replacementTree = false, clearedTree = false, streamFallback = false, finishedTree = false;
    try {
      loadedTree = Boolean(r.pickTree && r.pickTree.count === r.recordLocations.length);
      for (const mode of ["perspective", "top"]) {
        r.fit(mode);
        for (const filter of [saved.predicate, (record) => saved.predicate(record) && record % 3 !== 0]) {
          r.setVisibility(filter);
          for (const section of [0, 1, 2]) {
            r.setSection(section !== 0, "z", r.sectionValue("z", .5), section === 2);
            compare(r.pickTree);
          }
        }
      }
      const firstTree = r.pickTree;
      r.reload(pack, saved.predicate);
      reloadedTree = Boolean(r.pickTree && r.pickTree !== firstTree);
      r.setSection(false, "z", 0);
      compare(r.pickTree);
      const replacement = { ...pack, instances: { ...pack.instances, transforms: pack.instances.transforms.slice() } };
      const record = r.recordLocations.findIndex((location, index) => location && saved.predicate(index));
      if (record >= 0) replacement.instances.transforms[record * 16 + 12] += Math.max(1, r.bounds.radius * 2);
      const beforeReplacement = r.pickTree;
      r.load(replacement, saved.predicate);
      replacementTree = Boolean(r.pickTree && r.pickTree !== beforeReplacement);
      compare(r.pickTree);
      r.beginStream();
      clearedTree = r.pickTree === null;
      r.appendStream(pack, 0, pack.instances.count, saved.predicate);
      r.fit();
      streamFallback = r.pickTree === null;
      compare(buildBoundsTree(r.recordLocations.length, (index) => r.recordLocations[index]?.bounds));
      r.finishStream(pack, saved.predicate);
      finishedTree = Boolean(r.pickTree && r.pickTree.count === r.recordLocations.length);
      compare(r.pickTree);
      return { comparisons, hits, differences, loadedTree, reloadedTree, replacementTree, clearedTree, streamFallback, finishedTree };
    } finally {
      r.load(pack, saved.predicate);
      r.camera = saved.camera;
      r.section = saved.section;
      r.style = saved.style;
      r.cameraTouched = saved.touched;
      r.select(saved.selected);
      r.render(true);
    }
  });
  check(result.comparisons > 0 && result.hits > 0 && result.differences === 0,
    "accelerated picking preserves records and exact surface hits across cameras, visibility and section planes");
  check(result.loadedTree && result.reloadedTree && result.replacementTree,
    "picking bounds are rebuilt when loading, reloading and replacing a model");
  check(result.clearedTree && result.streamFallback && result.finishedTree,
    "streaming clears stale picking bounds, uses the original scan and builds fresh bounds when complete");
}
