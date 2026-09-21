// SPDX-License-Identifier: Apache-2.0
// The retained scene over a hand-built IGP pack: materials from the pack's
// table, textures from its pixels and blobs, uv on the geometry, and the
// flat-colour path when textures are off. three.js runs headless here; a
// checkout without it still runs the pure parts.

import assert from "node:assert/strict";
import { readIgp } from "../../../bindings/edit/src/igp.js";
import { writeIgp } from "../../../viewer/test/igp-writer.mjs";
import { createRetainedModel, materialParameters, pixelsToRgba, resolveTextureUrl } from "../src/retained.js";

// ------------------------------------------------------------- pure parts

{
  const plain = materialParameters({ color: [255, 128, 0, 255], diffuse: null, specular: null, shininess: null, roughness: null, reflectance: null, texture: null, source: 1 });
  assert.equal(plain.color, 0xff8000, "the surface colour when the style gives no diffuse colour");
  assert.equal(plain.roughness, 1);
  assert.equal(plain.metalness, 0);
  assert.equal(plain.transparent, false);
  assert.equal(plain.opacity, 1);
  assert.equal(plain.depthWrite, true);
  assert.equal(plain.map, null);

  const shiny = materialParameters({ color: [0, 0, 0, 128], diffuse: [1, 0.5, 0], specular: null, shininess: 64, roughness: null, reflectance: "METAL", texture: 4, source: 2 }, "map");
  assert.equal(shiny.color, 0xff8000, "the diffuse colour wins over the surface colour");
  assert.equal(shiny.roughness, 0.5, "shininess maps onto roughness when no roughness is given");
  assert.equal(shiny.metalness, 1);
  assert.equal(shiny.transparent, true);
  assert.equal(shiny.depthWrite, false);
  assert.equal(shiny.map, "map");
  assert.equal(materialParameters({ color: [1, 1, 1, 255], roughness: 0.2, shininess: 128, reflectance: "mirror" }).roughness, 0.2, "roughness wins over shininess");
  assert.equal(materialParameters({ color: [1, 1, 1, 255], reflectance: "mirror" }).metalness, 1);
  assert.equal(materialParameters({ color: [1, 1, 1, 255], shininess: 1000 }).roughness, 0, "shininess is clamped");

  assert.deepEqual([...pixelsToRgba(Uint8Array.from([9, 200]), 1, 1, 2)], [9, 9, 9, 200]);
  assert.equal(pixelsToRgba(Uint8Array.from([9]), 2, 1, 1), null);
  const page = "https://viewer.example/app/";
  assert.equal(resolveTextureUrl("wood.png", { pageUrl: page }), "https://viewer.example/app/wood.png");
  assert.equal(resolveTextureUrl("https://cdn.example/wood.png", { pageUrl: page }), null);
  assert.equal(resolveTextureUrl("https://cdn.example/wood.png", { pageUrl: page, allowRemote: true }), "https://cdn.example/wood.png");
  assert.equal(resolveTextureUrl("javascript:alert(1)", { pageUrl: page, allowRemote: true }), null);
  console.log("ok    material parameters, pixel expansion and the texture policy");
}

// ------------------------------------------------------------ the scene

let THREE;
try {
  THREE = await import("three");
} catch {
  console.log("skip  install three to run the retained scene checks");
  console.log("PASS");
  process.exit(0);
}

const quad = {
  id: 1,
  positions: Float32Array.from([0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 1, 0]),
  indices: Uint16Array.from([0, 1, 2, 0, 2, 3]),
  uv: Float32Array.from([0, 0, 1, 0, 1, 1, 0, 1]),
  bbox: [0, 0, 0, 1, 1, 0],
};
const plain = { id: 2, positions: quad.positions, indices: quad.indices, bbox: quad.bbox };
const checker = Uint8Array.from([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
const materials = [
  { color: [200, 200, 200, 255], diffuse: null, specular: null, shininess: null, roughness: null, reflectance: null, texture: 40, source: 30 },
  { color: [90, 140, 200, 255], diffuse: [0.2, 0.4, 0.8], specular: null, shininess: 64, roughness: null, reflectance: "METAL", texture: null, source: 31 },
  { color: [90, 140, 200, 255], diffuse: null, specular: null, shininess: null, roughness: null, reflectance: null, texture: 42, source: 32 },
];
const textures = [
  { id: 40, pixels: { width: 2, height: 2, components: 3, bytes: checker }, repeat: [true, false], transform: [2, 0, 0, 2, 0.5, 0] },
  { id: 42, uri: "https://cdn.example/brick.png" },
];
const bytes = writeIgp({
  geometries: [quad, plain],
  instances: [
    { geometry: 1, expressId: 10, classId: 0, material: 0 },
    { geometry: 2, expressId: 11, classId: 0, material: 1, color: [90, 140, 200, 255] },
    { geometry: 1, expressId: 12, classId: 0, material: 2, color: [90, 140, 200, 255] },
    { geometry: 2, expressId: 13, classId: 0 },
  ],
  materials,
  textures,
});

{
  const model = createRetainedModel(THREE, readIgp(bytes), { textures: true });
  assert.equal(model.meshCount, 4);
  const [painted] = model.meshesOf(10);
  assert.ok(painted.material instanceof THREE.MeshStandardMaterial, "a material row gives a standard material");
  assert.ok(painted.material.map instanceof THREE.DataTexture, "pixel textures upload as data textures");
  assert.equal(painted.material.map.image.width, 2);
  assert.deepEqual([...painted.material.map.image.data.subarray(0, 8)], [255, 0, 0, 255, 0, 255, 0, 255]);
  assert.equal(painted.material.map.wrapS, THREE.RepeatWrapping);
  assert.equal(painted.material.map.wrapT, THREE.ClampToEdgeWrapping);
  assert.equal(painted.material.map.flipY, false, "pixel rows already start at the bottom");
  assert.equal(painted.material.map.matrixAutoUpdate, false);
  assert.deepEqual(painted.material.map.matrix.toArray(), [2, 0, 0, 0, 2, 0, 0.5, 0, 1], "the texture transform becomes the uv matrix");
  assert.ok(painted.geometry.getAttribute("uv"), "the geometry carries its uv attribute");
  assert.equal(painted.geometry.getAttribute("uv").count, 4);

  const [metal] = model.meshesOf(11);
  assert.ok(metal.material instanceof THREE.MeshStandardMaterial);
  assert.equal(metal.material.map, null, "a material without a texture has no map");
  assert.equal(metal.material.metalness, 1);
  assert.equal(metal.material.roughness, 0.5);
  assert.equal(metal.geometry.getAttribute("uv"), undefined, "a mesh without uv gets no attribute");

  const [remote] = model.meshesOf(12);
  assert.equal(remote.material.map, null, "a remote image is not fetched unless allowed");
  assert.ok(remote.material instanceof THREE.MeshStandardMaterial, "the material row still styles the mesh");

  const [bare] = model.meshesOf(13);
  assert.ok(bare.material instanceof THREE.MeshLambertMaterial, "no material row keeps the flat colour");
  assert.equal(model.textureCount, 1, "one texture decoded, the refused one counted out");
  model.dispose();
  assert.equal(model.meshCount, 0);
  console.log("ok    textures on: materials, pixel textures, uv and the fetch policy");
}

{
  const model = createRetainedModel(THREE, readIgp(bytes));
  for (const id of [10, 11, 12, 13]) {
    const [mesh] = model.meshesOf(id);
    assert.ok(mesh.material instanceof THREE.MeshLambertMaterial, `#${id} draws flat with textures off`);
  }
  assert.equal(model.textureCount, 0);
  const [painted] = model.meshesOf(10);
  assert.ok(painted.geometry.getAttribute("uv"), "uv stays on the geometry for a host that wants it");
  model.dispose();
  console.log("ok    textures off: every mesh keeps its flat colour");
}

{
  // A delta pack carries its own tables; a texture seen before is reused.
  const model = createRetainedModel(THREE, readIgp(bytes), { textures: true });
  const delta = readIgp(writeIgp({
    geometries: [quad],
    instances: [{ geometry: 1, expressId: 10, classId: 0, material: 0 }],
    materials: [materials[0]],
    textures: [textures[0]],
  }));
  const report = model.applyDelta({ affectedProducts: [10], removedProducts: [13], pack: delta });
  assert.deepEqual(report, { removed: 2, added: 1 });
  assert.equal(model.textureCount, 1, "the same texture id is not decoded twice");
  const [painted] = model.meshesOf(10);
  assert.ok(painted.material.map instanceof THREE.DataTexture);
  model.dispose();
  console.log("ok    deltas reuse decoded textures");
}

console.log("PASS");
