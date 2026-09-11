// SPDX-License-Identifier: Apache-2.0
/**
 * three.js meshes from a TessIFC model. three.js is passed in, not imported,
 * so the package has no opinion about your version and adds no second copy.
 *
 * ```js
 * import init, { Kernel } from "@tessifc/core";
 * import { loadModel } from "@tessifc/three";
 *
 * await init();
 * const kernel = new Kernel();
 * const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));
 * const { group, bounds } = loadModel(THREE, kernel, id);
 * scene.add(group);
 * ```
 */

import { buildBatches, frame } from "./build.js";
import { createRetainedModel } from "./retained.js";

export { buildBatches, frame, createRetainedModel };

/**
 * Evaluate a model, unless already evaluated, and build meshes for it.
 * @param THREE the three.js namespace
 * @param kernel a TessIFC `Kernel`
 * @param modelId the model id
 * @param options `{ settings, evaluate }`; `settings` goes to `evaluateGeometry`, `evaluate: false` reuses an evaluation
 * @returns `{ group, bounds, shapes, summary, outcomes, batches }`
 */
export function loadModel(THREE, kernel, modelId, options = {}) {
  const summary =
    options.evaluate === false
      ? null
      : JSON.parse(kernel.evaluateGeometry(modelId, JSON.stringify(options.settings ?? {})));

  const { batches, shapes, bounds } = buildBatches(kernel, modelId);
  const outcomes = JSON.parse(kernel.getProductOutcomes?.(modelId) ?? "null");
  const group = new THREE.Group();
  group.name = "tessifc";

  for (const batch of batches) {
    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute("position", new THREE.BufferAttribute(batch.positions, 3));
    // Shared vertices give smooth normals; double-sided materials also show open shells.
    geometry.setIndex(new THREE.BufferAttribute(batch.indices, 1));
    geometry.computeVertexNormals();
    geometry.setAttribute("expressId", new THREE.BufferAttribute(batch.expressIds, 1));

    const [red, green, blue, alpha] = batch.color;
    const material = new THREE.MeshLambertMaterial({
      color: new THREE.Color(red / 255, green / 255, blue / 255),
      transparent: batch.transparent,
      opacity: alpha / 255,
      // Open and clipped shells are single-sided; draw both so walls stay visible from inside.
      side: THREE.DoubleSide,
      depthWrite: !batch.transparent,
    });

    const mesh = new THREE.Mesh(geometry, material);
    mesh.frustumCulled = true;
    group.add(mesh);
  }

  return { group, bounds, shapes, summary, outcomes, batches: batches.length };
}

/** Release the GPU resources owned by a loaded group and detach it from its scene. */
export function disposeModel(group) {
  const geometries = new Set(), materials = new Set();
  group.traverse((object) => {
    if (object.geometry) geometries.add(object.geometry);
    for (const material of Array.isArray(object.material) ? object.material : [object.material]) {
      if (material) materials.add(material);
    }
  });
  for (const geometry of geometries) geometry.dispose();
  for (const material of materials) material.dispose();
  group.removeFromParent();
  group.clear();
}

/** The express id under a raycast intersection, or `null`. */
export function expressIdAt(intersection) {
  const attribute = intersection?.object?.geometry?.getAttribute?.("expressId");
  if (!attribute) return null;
  const vertex = intersection.face?.a;
  if (vertex === undefined) return null;
  return attribute.getX(vertex);
}

/**
 * Point a perspective camera at a model.
 * @param camera a `THREE.PerspectiveCamera`
 * @param bounds from [`loadModel`]
 * @param controls optional orbit controls, whose target is moved too
 */
export function frameCamera(camera, bounds, controls) {
  const { centre, radius } = frame(bounds);
  const fov = (camera.fov * Math.PI) / 180;
  const halfFov = Math.atan(Math.tan(fov / 2) * Math.min(Math.max(camera.aspect || 1, 0.01), 1));
  const distance = (radius / Math.sin(halfFov)) * 1.25;
  const unit = distance / Math.hypot(1, 0.6, 1);
  camera.position.set(centre[0] + unit, centre[1] + unit * 0.6, centre[2] + unit);
  camera.near = Math.max(radius / 1000, 0.0001);
  camera.far = distance * 4 + radius * 4;
  camera.updateProjectionMatrix();
  camera.lookAt(centre[0], centre[1], centre[2]);
  if (controls) {
    controls.target.set(centre[0], centre[1], centre[2]);
    controls.update();
  }
}
