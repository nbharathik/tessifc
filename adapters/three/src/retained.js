// SPDX-License-Identifier: Apache-2.0
/**
 * A retained three.js scene over IGP packs: one mesh per placed instance,
 * one BufferGeometry per IGP geometry shared by every instance of it, and
 * deltas that replace only the products a revision touched. three.js is
 * passed in, as everywhere in this package.
 *
 * ```js
 * const model = createRetainedModel(THREE, readIgp(packBytes));
 * scene.add(model.group);
 * const { report, delta } = session.runScript(source);
 * if (delta) model.applyDelta(delta);
 * ```
 */

// IGP instance flags for helper geometry: openings, spaces and reference items.
const HELPER_FLAGS = (1 << 1) | (1 << 2) | (1 << 4);

/**
 * Build a retained model from a parsed IGP pack (the `readIgp` output).
 * @param THREE the three.js namespace
 * @param pack a parsed IGP pack
 * @param options `{ hiddenFlags }`: instance flags whose meshes start invisible (helpers by default)
 */
export function createRetainedModel(THREE, pack, options = {}) {
  const hiddenFlags = options.hiddenFlags ?? HELPER_FLAGS;
  const group = new THREE.Group();
  group.name = "tessifc";
  // Geometry id -> { geometry, users }; a shared geometry lives while one mesh uses it.
  const geometries = new Map();
  // Colour key -> material; materials are shared and disposed with the model.
  const materials = new Map();
  // Express id -> the meshes of that product.
  const products = new Map();
  let classes = [];

  function material(colors, offset) {
    const [red, green, blue, alpha] = colors.subarray(offset, offset + 4);
    const key = `${red},${green},${blue},${alpha}`;
    let found = materials.get(key);
    if (!found) {
      found = new THREE.MeshLambertMaterial({
        color: new THREE.Color(red / 255, green / 255, blue / 255),
        transparent: alpha < 255,
        opacity: alpha / 255,
        side: THREE.DoubleSide,
        depthWrite: alpha === 255,
      });
      materials.set(key, found);
    }
    return found;
  }

  function geometry(source) {
    let entry = geometries.get(source.id);
    if (!entry) {
      const built = new THREE.BufferGeometry();
      built.setAttribute("position", new THREE.BufferAttribute(source.positions, 3));
      built.setIndex(new THREE.BufferAttribute(source.indices, 1));
      built.computeVertexNormals();
      entry = { geometry: built, users: 0 };
      geometries.set(source.id, entry);
    }
    entry.users += 1;
    return entry.geometry;
  }

  function release(id) {
    const entry = geometries.get(id);
    if (!entry) return;
    entry.users -= 1;
    if (entry.users <= 0) {
      entry.geometry.dispose();
      geometries.delete(id);
    }
  }

  /** Add every instance of a pack; records of products already present are added beside them. */
  function addPack(source) {
    const byId = new Map(source.geometry.map((item) => [item.id, item]));
    classes = source.index.classes ?? classes;
    let added = 0;
    for (let record = 0; record < source.instances.count; record += 1) {
      const geometryId = source.instances.geometryIds[record];
      const definition = byId.get(geometryId);
      if (!definition) continue;
      const expressId = source.instances.expressIds[record];
      const flags = source.instances.flags[record];
      const mesh = new THREE.Mesh(geometry(definition), material(source.instances.colors, record * 4));
      mesh.matrixAutoUpdate = false;
      mesh.matrix.fromArray(source.instances.transforms, record * 16);
      mesh.matrixWorldNeedsUpdate = true;
      mesh.visible = !(flags & hiddenFlags);
      mesh.userData = { expressId, geometryId, flags, classId: source.instances.classIds[record],
        class: String(classes[source.instances.classIds[record]] ?? "IfcProduct") };
      mesh.name = `#${expressId}`;
      group.add(mesh);
      const list = products.get(expressId) ?? [];
      list.push(mesh);
      products.set(expressId, list);
      added += 1;
    }
    return added;
  }

  /** Remove every mesh of these products and release what nothing else uses. */
  function removeProducts(expressIds) {
    let removed = 0;
    for (const expressId of expressIds) {
      const list = products.get(expressId);
      if (!list) continue;
      for (const mesh of list) {
        group.remove(mesh);
        release(mesh.userData.geometryId);
        removed += 1;
      }
      products.delete(expressId);
    }
    return removed;
  }

  /**
   * Apply a scene delta from `@tessifc/edit`: affected and removed products
   * lose their meshes, the delta's pack supplies the replacements, and a full
   * rebuild replaces everything.
   */
  function applyDelta(delta) {
    const source = delta.pack;
    if (!source) throw new Error("applyDelta needs the delta's parsed pack.");
    if (delta.fullRebuild) {
      const removed = removeProducts([...products.keys()]);
      return { removed, added: addPack(source) };
    }
    const removed = removeProducts([...(delta.affectedProducts ?? []), ...(delta.removedProducts ?? [])]);
    return { removed, added: addPack(source) };
  }

  function setVisible(expressId, visible) {
    for (const mesh of products.get(expressId) ?? []) mesh.visible = visible;
  }

  function dispose() {
    removeProducts([...products.keys()]);
    for (const entry of geometries.values()) entry.geometry.dispose();
    geometries.clear();
    for (const item of materials.values()) item.dispose();
    materials.clear();
    group.removeFromParent();
    group.clear();
  }

  addPack(pack);

  return {
    group,
    applyDelta,
    setVisible,
    dispose,
    meshesOf: (expressId) => [...(products.get(expressId) ?? [])],
    productIds: () => [...products.keys()],
    get geometryCount() {
      return geometries.size;
    },
    get meshCount() {
      return group.children.length;
    },
  };
}
