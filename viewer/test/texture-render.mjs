// SPDX-License-Identifier: Apache-2.0

/** A pixel texture reaches the screen the right way up when textures are on, and not at all when off. */
export async function checkTextureRendering(page, check) {
  const result = await page.evaluate(async () => {
    const { createPackAssembler } = await import("/viewer/src/stream.js");
    const { readIgp } = await import("/viewer/src/igp.js");
    const { writeIgp } = await import("/viewer/test/igp-writer.mjs");
    const r = window.__tessifc.renderer, gl = r.gl, originalPack = r.pack;
    const saved = {
      camera: structuredClone(r.camera), style: r.style, section: { ...r.section }, predicate: r.visibilityPredicate,
      touched: r.cameraTouched, lodPixels: r.lodPixels, textures: r.textures,
    };
    try {
      // A 2 m quad in the ground plane with its uv square laid over it, white so the texture shows as is.
      const quad = {
        id: 1,
        positions: Float32Array.from([-1, -1, 0, 1, -1, 0, 1, 1, 0, -1, 1, 0]),
        indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
        uv: Float32Array.from([0, 0, 1, 0, 1, 1, 0, 1]),
        bbox: [-1, -1, 0, 1, 1, 0],
      };
      // Rows from the bottom: red and green below, blue and white above.
      const checker = Uint8Array.from([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
      const bytes = writeIgp({
        geometries: [quad],
        instances: [{ geometry: 1, expressId: 10, classId: 0, color: [255, 255, 255, 255], material: 0 }],
        materials: [{ color: [255, 255, 255, 255], diffuse: null, specular: null, shininess: null, roughness: null, reflectance: null, texture: 40, source: 30 }],
        textures: [{ id: 40, pixels: { width: 2, height: 2, components: 3, bytes: checker } }],
      });
      const assembler = createPackAssembler();
      assembler.append(readIgp(bytes));
      const pack = assembler.pack();
      const width = r.canvas.width, height = r.canvas.height;
      const step = Math.floor(Math.min(width, height) / 8);
      const corners = { red: [-1, -1], green: [1, -1], blue: [-1, 1], white: [1, 1] };
      const classify = ([red, green, blue]) => {
        const high = (value) => value > 150, low = (value) => value < 90;
        if (high(red) && high(green) && high(blue)) return "white";
        if (high(red) && low(green) && low(blue)) return "red";
        if (low(red) && high(green) && low(blue)) return "green";
        if (low(red) && low(green) && high(blue)) return "blue";
        return `other(${red},${green},${blue})`;
      };
      const capture = (textures) => {
        r.load(pack);
        r.setTextures(textures);
        r.setStyle("shaded");
        r.setSection(false, "z", 0);
        r.setLodPixels(0);
        r.fit("top");
        r.render(true);
        gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
        const seen = {};
        for (const [name, [sx, sy]] of Object.entries(corners)) {
          const pixel = new Uint8Array(4);
          gl.readPixels(Math.floor(width / 2 + sx * step), Math.floor(height / 2 + sy * step), 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
          seen[name] = classify(pixel);
        }
        return { seen, info: r.displayInfo(), glError: gl.getError() };
      };
      const on = capture(true);
      const off = capture(false);
      return { on, off };
    } finally {
      r.setTextures(saved.textures);
      r.load(originalPack, saved.predicate);
      r.camera = saved.camera;
      r.section = saved.section;
      r.style = saved.style;
      r.cameraTouched = saved.touched;
      r.setLodPixels(saved.lodPixels);
      r.render(true);
    }
  });
  const { on, off } = result;
  check(
    on.seen.red === "red" && on.seen.green === "green" && on.seen.blue === "blue" && on.seen.white === "white",
    `a pixel texture paints the quad with its rows the right way up (${JSON.stringify(on.seen)})`,
  );
  check(on.info.texturesActive && on.info.texturedBatches === 1 && on.info.texturesLoaded === 1 && on.glError === 0,
    "the textured batch draws through the textured program without GL errors");
  check(
    Object.values(off.seen).every((name) => name === "white") && !off.info.texturesActive && off.info.texturedBatches === 0,
    `textures off draws the quad in its flat colour (${JSON.stringify(off.seen)})`,
  );
}
