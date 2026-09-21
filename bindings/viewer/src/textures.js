// SPDX-License-Identifier: Apache-2.0

//! GPU textures for an IGP pack: one per texture id, a white pixel until the
//! image has arrived, and the policy deciding which references may be fetched.

// The longest texture reference a pack may name.
const MAX_TEXTURE_URI = 4096;

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
 * The column-major 3x3 matrix a pack's texture transform `[a, b, c, d, tx, ty]`
 * applies to texture coordinates; identity when there is none.
 * @param {number[] | null | undefined} transform
 */
export function uvMatrix(transform) {
  if (!Array.isArray(transform) || transform.length !== 6 || !transform.every(Number.isFinite)) {
    return Float32Array.from([1, 0, 0, 0, 1, 0, 0, 0, 1]);
  }
  const [a, b, c, d, tx, ty] = transform;
  return Float32Array.from([a, b, 0, c, d, 0, tx, ty, 1]);
}

/**
 * @typedef {object} TextureEntry
 * @property {WebGLTexture | null} handle The decoded texture, null until it arrives or when it never will.
 * @property {boolean} ready
 * @property {boolean} failed
 * @property {Float32Array} transform The uv matrix.
 */

/**
 * A cache of GPU textures over a pack's `textures` table.
 * @param {WebGL2RenderingContext} gl
 * @param {{ allowRemote?: boolean, baseUrl?: string | null, onDirty?: () => void }} [options]
 */
export function createTextureCache(gl, options = {}) {
  const allowRemote = options.allowRemote ?? false;
  const baseUrl = options.baseUrl ?? null;
  const onDirty = options.onDirty ?? null;
  const placeholder = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, placeholder);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([255, 255, 255, 255]));
  gl.bindTexture(gl.TEXTURE_2D, null);
  /** @type {Map<number, TextureEntry>} */
  const entries = new Map();
  let disposed = false;

  function upload(entry, record, source, flip, crisp) {
    if (disposed) return;
    const handle = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, handle);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, flip);
    if (source.data) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, source.width, source.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, source.data);
    } else {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, source);
    }
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    const repeat = record.repeat ?? [true, true];
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, repeat[0] === false ? gl.CLAMP_TO_EDGE : gl.REPEAT);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, repeat[1] === false ? gl.CLAMP_TO_EDGE : gl.REPEAT);
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
    // Explicit pixels stay as pixels; an image is smoothed.
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, crisp ? gl.NEAREST : gl.LINEAR);
    gl.bindTexture(gl.TEXTURE_2D, null);
    entry.handle = handle;
    entry.ready = true;
    onDirty?.();
  }

  function fail(entry) {
    entry.failed = true;
  }

  function start(entry, record) {
    if (record.pixels) {
      const { width, height, components, bytes } = record.pixels;
      const rgba = pixelsToRgba(bytes, width, height, components);
      if (!rgba || width < 1 || height < 1) return fail(entry);
      return upload(entry, record, { width, height, data: rgba }, false, true);
    }
    if (record.blob) {
      if (typeof createImageBitmap !== "function" || typeof Blob !== "function") return fail(entry);
      const blob = new Blob([record.blob], { type: record.mime ?? "image/png" });
      // Rows are wanted bottom first, which a bitmap decides at creation.
      createImageBitmap(blob, { imageOrientation: "flipY" }).then(
        (bitmap) => {
          if (entries.get(record.id) === entry) upload(entry, record, bitmap, false, false);
          bitmap.close?.();
        },
        () => fail(entry),
      );
      return undefined;
    }
    if (typeof record.uri === "string") {
      const url = resolveTextureUrl(record.uri, { baseUrl, allowRemote });
      if (!url || typeof Image !== "function") return fail(entry);
      const image = new Image();
      image.crossOrigin = "anonymous";
      image.onload = () => {
        if (entries.get(record.id) === entry) upload(entry, record, image, true, false);
      };
      image.onerror = () => fail(entry);
      image.src = url;
      return undefined;
    }
    return fail(entry);
  }

  /**
   * The entry for texture `id`, started from `record` on first use.
   * @param {number} id
   * @param {() => (Record<string, any> | null | undefined)} recordOf
   */
  function get(id, recordOf) {
    let entry = entries.get(id);
    if (entry) return entry;
    entry = { handle: null, ready: false, failed: false, transform: uvMatrix(null) };
    entries.set(id, entry);
    const record = recordOf();
    if (!record) fail(entry);
    else {
      entry.transform = uvMatrix(record.transform);
      start(entry, record);
    }
    return entry;
  }

  function dispose() {
    if (disposed) return;
    disposed = true;
    for (const entry of entries.values()) if (entry.handle) gl.deleteTexture(entry.handle);
    entries.clear();
    gl.deleteTexture(placeholder);
  }

  return {
    get,
    /** The texture to sample for `entry`: its own once decoded, white until then. */
    handleOf: (entry) => entry.handle ?? placeholder,
    dispose,
    get count() {
      return entries.size;
    },
    get readyCount() {
      return [...entries.values()].filter((entry) => entry.ready).length;
    },
  };
}
