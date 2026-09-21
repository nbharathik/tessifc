// SPDX-License-Identifier: Apache-2.0

//! A retained three.js scene over IGP packs: one mesh per placed instance, one
//! BufferGeometry per IGP geometry, and deltas that replace only the products
//! a revision touched. three.js is passed in, as everywhere in this package.
//!
//!   const model = createRetainedModel(THREE, readIgp(packBytes));
//!   scene.add(model.group);
//!   const { report, delta } = session.runScript(source);
//!   if (delta) model.applyDelta(delta);

// IGP instance flags for helper geometry: openings, spaces and reference items.
const HELPER_FLAGS = (1 << 1) | (1 << 2) | (1 << 4);

// The `material` column value of an instance without a material row.
const NO_MATERIAL = 0xffffffff;

// The longest texture reference a pack may name.
const MAX_TEXTURE_URI = 4096;

/** @typedef {ReturnType<typeof createRetainedModel>} RetainedModel */

/**
 * @typedef {object} RetainedModelOptions
 * @property {number} [hiddenFlags] Instance flags whose meshes start invisible (helpers by default).
 * @property {boolean} [textures] Use the pack's materials and textures; off, every mesh is a flat colour.
 * @property {string} [textureBaseUrl] Where relative texture paths resolve; the page's URL by default.
 * @property {boolean} [allowRemoteTextures] Fetch textures from other origins too; off by default.
 * @property {(id: number) => void} [onTexture] Called once a texture's image has arrived, to render again.
 */

/**
 * The URL a texture reference may be fetched from, or null when the policy
 * refuses it: relative paths resolve against `baseUrl` or the page, absolute
 * URLs need `allowRemote` unless they share an origin with either, and only
 * http, https and data images are ever loaded.
 * @param {string} uri
 * @param {{ baseUrl?: string | null, allowRemote?: boolean, pageUrl?: string | null }} [policy]
 * @returns {string | null}
 */
export function resolveTextureUrl(uri, policy = {}) {
  const { baseUrl = null, allowRemote = false } = policy;
  const pageUrl = policy.pageUrl === undefined ? (globalThis.location?.href ?? null) : policy.pageUrl;
  if (typeof uri !== "string" || uri.length === 0 || uri.length > MAX_TEXTURE_URI) return null;
  if (/^data:image\//i.test(uri)) return uri;
  let url;
  try {
    const base = baseUrl ?? pageUrl;
    url = base ? new URL(uri, base) : new URL(uri);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (allowRemote) return url.href;
  const origins = [];
  for (const candidate of [baseUrl, pageUrl]) {
    try {
      if (candidate) origins.push(new URL(candidate).origin);
    } catch {
      // A base that is not a URL grants nothing.
    }
  }
  return origins.includes(url.origin) ? url.href : null;
}

/**
 * Raw pixel rows as RGBA: one component is grey, two are grey and alpha,
 * three are RGB and four are copied.
 * @param {Uint8Array} bytes
 * @param {number} width
 * @param {number} height
 * @param {number} components
 * @returns {Uint8Array | null} null when the bytes do not match the size
 */
export function pixelsToRgba(bytes, width, height, components) {
  const count = width * height;
  if (components < 1 || components > 4 || bytes.length !== count * components) return null;
  const out = new Uint8Array(count * 4);
  for (let pixel = 0; pixel < count; pixel += 1) {
    const at = pixel * components;
    const to = pixel * 4;
    if (components >= 3) {
      out[to] = bytes[at];
      out[to + 1] = bytes[at + 1];
      out[to + 2] = bytes[at + 2];
      out[to + 3] = components === 4 ? bytes[at + 3] : 255;
    } else {
      out[to] = bytes[at];
      out[to + 1] = bytes[at];
      out[to + 2] = bytes[at];
      out[to + 3] = components === 2 ? bytes[at + 1] : 255;
    }
  }
  return out;
}

/**
 * The `MeshStandardMaterial` parameters of an IGP material row: the diffuse
 * colour when the style gives one, roughness from the style's roughness or
 * its shininess, metal and mirror reflectance as metal, and the texture as
 * the colour map.
 * @param {import("@tessifc/edit/types").PackMaterial} material
 * @param {import("three").Texture | null} [texture] a three.js texture, or nothing
 */
export function materialParameters(material, texture = null) {
  const [red, green, blue, alpha] = material.color ?? [204, 204, 204, 255];
  const diffuse = material.diffuse ?? [red / 255, green / 255, blue / 255];
  const clamp = (value) => Math.min(1, Math.max(0, value));
  let roughness = 1;
  if (typeof material.roughness === "number" && Number.isFinite(material.roughness)) {
    roughness = clamp(material.roughness);
  } else if (typeof material.shininess === "number" && Number.isFinite(material.shininess)) {
    roughness = 1 - clamp(material.shininess / 128);
  }
  const reflectance = String(material.reflectance ?? "").toUpperCase();
  const metalness = reflectance === "METAL" || reflectance === "MIRROR" ? 1 : 0;
  const color = (Math.round(clamp(diffuse[0]) * 255) << 16)
    | (Math.round(clamp(diffuse[1]) * 255) << 8)
    | Math.round(clamp(diffuse[2]) * 255);
  return {
    color,
    roughness,
    metalness,
    transparent: alpha < 255,
    opacity: alpha / 255,
    depthWrite: alpha === 255,
    map: texture ?? null,
  };
}

/**
 * Build a retained model from a parsed IGP pack (the `readIgp` output).
 * @param {typeof import("three")} THREE the three.js namespace
 * @param {import("@tessifc/edit/types").Pack} pack a parsed IGP pack
 * @param {RetainedModelOptions} [options]
 */
export function createRetainedModel(THREE, pack, options = {}) {
  const hiddenFlags = options.hiddenFlags ?? HELPER_FLAGS;
  const useTextures = options.textures ?? false;
  const textureBaseUrl = options.textureBaseUrl ?? null;
  const allowRemote = options.allowRemoteTextures ?? false;
  const onTexture = options.onTexture ?? null;
  const group = new THREE.Group();
  group.name = "tessifc";
  // Geometry id -> { geometry, users }; a shared geometry lives while one mesh uses it.
  const geometries = new Map();
  // Colour or material key -> material; materials are shared and disposed with the model.
  const materials = new Map();
  // Texture id -> three.js texture, or null when it could not be loaded.
  const textures = new Map();
  // Express id -> the meshes of that product.
  const products = new Map();
  let classes = [];

  function plainMaterial(red, green, blue, alpha) {
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

  function texture(source, id) {
    if (textures.has(id)) return textures.get(id);
    const record = (source.index.textures ?? []).find((entry) => entry.id === id);
    const built = record ? buildTexture(record) : null;
    textures.set(id, built);
    return built;
  }

  function buildTexture(record) {
    let built = null;
    if (record.pixels) {
      const { width, height, components, bytes } = record.pixels;
      const rgba = pixelsToRgba(bytes, width, height, components);
      if (!rgba) return null;
      // Pixel rows start at the bottom, which is three.js's unflipped order.
      built = new THREE.DataTexture(rgba, width, height, THREE.RGBAFormat);
      built.flipY = false;
      built.needsUpdate = true;
    } else if (record.blob) {
      if (typeof createImageBitmap !== "function" || typeof Blob !== "function") return null;
      built = new THREE.Texture();
      built.flipY = false;
      const image = new Blob([record.blob], { type: record.mime ?? "image/png" });
      createImageBitmap(image, { imageOrientation: "flipY" }).then((bitmap) => {
        if (textures.get(record.id) !== built) return;
        built.image = bitmap;
        built.needsUpdate = true;
        onTexture?.(record.id);
      }, () => {});
    } else if (record.uri) {
      const url = resolveTextureUrl(record.uri, { baseUrl: textureBaseUrl, allowRemote });
      if (!url || typeof document === "undefined") return null;
      built = new THREE.TextureLoader().load(url, () => onTexture?.(record.id));
    } else {
      return null;
    }
    built.wrapS = record.repeat?.[0] === false ? THREE.ClampToEdgeWrapping : THREE.RepeatWrapping;
    built.wrapT = record.repeat?.[1] === false ? THREE.ClampToEdgeWrapping : THREE.RepeatWrapping;
    if (record.transform) {
      const [a, b, c, d, tx, ty] = record.transform;
      built.matrixAutoUpdate = false;
      built.matrix.set(a, c, tx, b, d, ty, 0, 0, 1);
    }
    if ("colorSpace" in built && THREE.SRGBColorSpace) built.colorSpace = THREE.SRGBColorSpace;
    return built;
  }

  function material(source, record) {
    const offset = record * 4;
    const [red, green, blue, alpha] = source.instances.colors.subarray(offset, offset + 4);
    const row = useTextures && source.instances.material ? source.instances.material[record] : NO_MATERIAL;
    const definition = row === NO_MATERIAL ? null : (source.index.materials?.[row] ?? null);
    if (!definition) return plainMaterial(red, green, blue, alpha);
    const key = `m:${JSON.stringify(definition)}:${red},${green},${blue},${alpha}`;
    let found = materials.get(key);
    if (!found) {
      const map = typeof definition.texture === "number" ? texture(source, definition.texture) : null;
      found = new THREE.MeshStandardMaterial({ ...materialParameters(definition, map), side: THREE.DoubleSide });
      materials.set(key, found);
    }
    return found;
  }

  function geometry(source) {
    let entry = geometries.get(source.id);
    if (!entry) {
      const built = new THREE.BufferGeometry();
      built.setAttribute("position", new THREE.BufferAttribute(source.positions, 3));
      if (source.uv) built.setAttribute("uv", new THREE.BufferAttribute(source.uv, 2));
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
      const mesh = new THREE.Mesh(geometry(definition), material(source, record));
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
   * @param {Partial<import("@tessifc/edit/types").Delta> & { pack: import("@tessifc/edit/types").Pack }} delta
   * @returns {{ removed: number, added: number }}
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

  /** @param {number} expressId @param {boolean} visible */
  function setVisible(expressId, visible) {
    for (const mesh of products.get(expressId) ?? []) mesh.visible = visible;
  }

  function dispose() {
    removeProducts([...products.keys()]);
    for (const entry of geometries.values()) entry.geometry.dispose();
    geometries.clear();
    for (const item of materials.values()) item.dispose();
    materials.clear();
    for (const item of textures.values()) item?.dispose();
    textures.clear();
    group.removeFromParent();
    group.clear();
  }

  addPack(pack);

  return {
    group,
    applyDelta,
    setVisible,
    dispose,
    /** @param {number} expressId */
    meshesOf: (expressId) => [...(products.get(expressId) ?? [])],
    productIds: () => [...products.keys()],
    get geometryCount() {
      return geometries.size;
    },
    get meshCount() {
      return group.children.length;
    },
    get textureCount() {
      return [...textures.values()].filter(Boolean).length;
    },
  };
}
