// SPDX-License-Identifier: Apache-2.0

/** Real WebGL allocations and picking survive selective scene revisions. */
export async function checkDeltaRendering(page, check) {
  const result = await page.evaluate(async () => {
    const { createPackAssembler } = await import("/viewer/src/stream.js");
    const { deltaChunk, deltaTriangle } = await import("/viewer/test/delta-fixture.mjs");
    const r = window.__tessifc.renderer, gl = r.gl, originalPack = r.pack;
    const saved = {
      camera: structuredClone(r.camera), section: { ...r.section }, style: r.style,
      predicate: r.visibilityPredicate, selected: r.selected.slice(), touched: r.cameraTouched, lodPixels: r.lodPixels,
    };
    const originalUpload = gl.bufferData;
    let uploads = 0;
    gl.bufferData = function (...args) { uploads++; return originalUpload.apply(this, args); };
    try {
      const assembler = createPackAssembler();
      assembler.append(deltaChunk([deltaTriangle(0), deltaTriangle(1, 20)], [
        { geometry: 0, id: 10 }, { geometry: 1, id: 11, color: [80, 80, 200, 255] },
      ]));
      r.load(assembler.pack());
      r.setSection(false, "z", 0);
      r.setLodPixels(0);
      r.select([0, 1]);
      const camera = JSON.stringify(r.camera), origin = r.renderOrigin.slice();
      const untouched = r.batches.find((batch) => batch.records.includes(1));
      const retired = r.batches.find((batch) => batch.records.includes(0));
      const ray = (x) => ({ origin: [x - origin[0], .2 - origin[1], 2 - origin[2]], direction: [0, 0, -1] });
      const patch = assembler.replaceProducts([10], deltaChunk([deltaTriangle(2, 5, 2)], [{ geometry: 2, id: 10 }]));
      const stats = r.applyDelta(assembler.pack(), patch);
      r.render(true);
      const resourceReuse = r.batches.includes(untouched) && untouched.buffers.every((buffer) => gl.isBuffer(buffer)) &&
        retired.buffers.every((buffer) => !gl.isBuffer(buffer));
      const movedPicking = r.pickRecord(ray(.2)) === null && r.pickRecord(ray(5.2))?.record === 2 && r.pickRecord(ray(20.2))?.record === 1;
      const stateKept = camera === JSON.stringify(r.camera) && JSON.stringify(origin) === JSON.stringify(r.renderOrigin) &&
        JSON.stringify(r.selected) === JSON.stringify([1, 2]);
      const noChange = assembler.replaceProducts([10], deltaChunk([deltaTriangle(3, 5, 2)], [{ geometry: 3, id: 10 }]));
      const beforeNoop = uploads;
      r.applyDelta(assembler.pack(), noChange);
      const noUploads = !noChange.changed && uploads === beforeNoop;
      const deletion = assembler.replaceProducts([10], deltaChunk([], []));
      r.applyDelta(assembler.pack(), deletion);
      r.setVisibility(() => true);
      r.render(true);
      const deleted = r.pickRecord(ray(5.2)) === null && r.batches.length === 1 && r.batches[0] === untouched;
      for (let i = 0; i < 20; i++) {
        const id = assembler.nextGeometryId();
        const update = assembler.replaceProducts([10], deltaChunk([deltaTriangle(id, 5, 2 + i)], [{ geometry: id, id: 10 }]));
        r.applyDelta(assembler.pack(), update);
      }
      const bounded = assembler.geometryCount === 2 && r.batches.length === 2 &&
        r.batches.includes(untouched) && assembler.pack().instances.activeCount === 2;
      // load is also the context-restoration path; it must omit every retired slot.
      r.reload(assembler.pack());
      const restored = r.batches.every((batch) => [...batch.records].every((record) => assembler.pack().instances.active[record])) &&
        r.recordLocations.filter(Boolean).length === 2;
      r.render(true);
      return { resourceReuse, movedPicking, stateKept, noUploads, deleted, bounded, restored, stats: stats.patchStats, glError: gl.getError() };
    } finally {
      gl.bufferData = originalUpload;
      r.load(originalPack, saved.predicate);
      r.camera = saved.camera;
      r.section = saved.section;
      r.style = saved.style;
      r.cameraTouched = saved.touched;
      r.setLodPixels(saved.lodPixels);
      r.select(saved.selected);
      r.render(true);
    }
  });
  check(result.resourceReuse && result.stats.reusedBatches === 1 && result.stats.rebuiltBatches === 1,
    "geometry deltas retain unrelated WebGL buffers and release only affected batches");
  check(result.movedPicking && result.deleted, "replacement and deletion update picking without resurrecting old geometry");
  check(result.stateKept && result.noUploads, "selective updates preserve camera, origin and selection; exact no-ops upload nothing");
  check(result.bounded && result.restored && result.glError === 0,
    "repeated updates bound live geometry and batches, and restoration excludes retired slots");
}
