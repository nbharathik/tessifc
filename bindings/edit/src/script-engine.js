// SPDX-License-Identifier: Apache-2.0

//! Browser scripts: JavaScript over the open model with a small IFC API, run
//! inside the geometry worker. Edits become a new IFC snapshot for the
//! revision path, so the kernel decides which products to rebuild.

import { createHelpers } from "./script-helpers.js";

const ENTITY = Symbol("entity");
const DERIVED = Symbol("derived");
const GUID_ALPHABET = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";
const MAX_OUTPUT = 64_000;

/** A reference to an entity by express id. */
export class Ref {
  constructor(id) {
    this.id = id;
  }
  toString() {
    return `#${this.id}`;
  }
}

/** An enumeration value, written `.VALUE.` in the file. */
export class Enum {
  constructor(value) {
    this.value = String(value).toUpperCase();
  }
  toString() {
    return this.value;
  }
}

/** A typed value such as `IFCLABEL('x')`. */
export class Typed {
  constructor(type, value) {
    this.type = String(type).toUpperCase();
    this.value = value;
  }
  toString() {
    return String(this.value);
  }
}

/** An integer written without a decimal point. */
export class Int {
  constructor(value) {
    this.value = Math.trunc(value);
  }
  toString() {
    return String(this.value);
  }
}

/** The reference documented to scripts and to the assistant. */
export const API_REFERENCE = `JavaScript with these names defined:
- ifc.byType("IfcWall") -> entities of that class or its subtypes; ifc.get(id); ifc.byGuid("guid").
- entity.Name, entity.ObjectPlacement, entity.Representation.Representations[0].Items[0].Depth: attributes by IFC name.
  References resolve to entities, lists to arrays, enumerations and strings to strings, numbers to numbers, $ to null.
  entity.id, entity.type, entity.is("IfcProduct"), entity.attributes() (a plain object).
- entity.Name = "x" or solid.Depth = solid.Depth + 0.5 changes an attribute; assign entities, arrays, numbers, strings, null.
- ifc.add("IfcCartesianPoint", { Coordinates: [1, 2, 0] }) creates an entity from attributes by name (or a positional array);
  required attributes must be given, optional ones default to $. Enumerations take plain strings ("AREA"), booleans true/false,
  select values need ifc.typed("IFCLABEL", "x"). Returns the new entity.
- ifc.remove(entity) deletes it and detaches every reference to it; a relationship left empty is removed too.
- ifc.inverses(entity, "IfcRelVoidsElement") -> entities referencing it; ifc.container(product) -> its storey or null.
- ifc.addBox(className, name, { at: [x, y, z], size: [width, depth, height], relativeTo: entity, rotation: degrees }) creates
  a placed rectangular extrusion (centred on x/y at its placement, rising from z) with a Body representation.
- Building helpers (lengths in the model unit, z up, all contained in a storey and returned as entities):
  ifc.addStorey({ name, elevation }); ifc.storeys() lowest first; ifc.byName("IfcWall", "Name").
  ifc.addWall({ from: [x, y], to: [x, y], height, thickness, storey, name }) along the line between the points.
  ifc.addSlab({ polygon: [[x, y], ...] or size: [w, d], at: [x, y, z], thickness, type: "FLOOR" | "ROOF" | "BASESLAB", storey }).
  ifc.addDoor({ in: wall, at: [x, 0, 0], size: [width, height] }) and ifc.addWindow({ in: wall, at: [x, 0, sill], size })
  cut their own opening; x runs along the wall from its centre, so use half the length for the ends.
  ifc.addColumn({ at: [x, y], size: [w, d], height, storey }); ifc.addBeam({ from: [x, y, z], to: [x, y, z], size: [depth, width] }).
  ifc.addProperties(entity, "Pset_WallCommon", { IsExternal: true, FireRating: "REI60" }) creates or extends the set.
  ifc.setColor(entity, [r, g, b, a]) with components 0..1; ifc.describe() -> schema, unit, product counts, storeys.
- ifc.contain(product, storey), ifc.void(host, opening), ifc.fill(opening, element), ifc.aggregate(parent, child).
- ifc.newGuid(), ifc.enum("AREA"), ifc.typed(type, value), ifc.int(n), ifc.context(), ifc.schema.
- selected (the viewer selection or null), selection (array), print(...values).
Lengths are in the model's length unit; the script ends when it returns and its edits are published together.`;

// -------------------------------------------------------------- STEP text

/** Decode the payload of a STEP string literal, without its quotes. */
export function decodeStepString(inner) {
  let text = inner.replace(/''/g, "'");
  text = text.replace(/\\X2\\([0-9A-Fa-f]+)\\X0\\/g, (_, hex) => {
    let out = "";
    for (let i = 0; i + 4 <= hex.length; i += 4) out += String.fromCharCode(parseInt(hex.slice(i, i + 4), 16));
    return out;
  });
  text = text.replace(/\\X4\\([0-9A-Fa-f]+)\\X0\\/g, (_, hex) => {
    let out = "";
    for (let i = 0; i + 8 <= hex.length; i += 8) out += String.fromCodePoint(parseInt(hex.slice(i, i + 8), 16));
    return out;
  });
  text = text.replace(/\\X\\([0-9A-Fa-f]{2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)));
  text = text.replace(/\\S\\(.)/g, (_, char) => String.fromCharCode(char.charCodeAt(0) + 128));
  return text;
}

/** Encode a string as a STEP literal with quotes; non-ASCII goes through \\X2\\ escapes. */
export function encodeStepString(text) {
  let out = "'";
  let run = "";
  const flush = () => {
    if (run) out += `\\X2\\${run}\\X0\\`;
    run = "";
  };
  for (const char of String(text)) {
    const code = char.codePointAt(0);
    if (code >= 32 && code < 127) {
      flush();
      out += char === "'" ? "''" : char === "\\" ? "\\\\" : char;
    } else if (code > 0xffff) {
      flush();
      out += `\\X4\\${code.toString(16).toUpperCase().padStart(8, "0")}\\X0\\`;
    } else {
      run += code.toString(16).toUpperCase().padStart(4, "0");
    }
  }
  flush();
  return out + "'";
}

/**
 * Parse one STEP value starting at `index`; returns the value and the index after it.
 * @param {string} text
 * @param {number} [index]
 * @returns {{ value: unknown, end: number }}
 */
export function parseValue(text, index = 0) {
  let i = index;
  while (i < text.length && /\s/.test(text[i])) i += 1;
  const char = text[i];
  if (char === "$") return { value: null, end: i + 1 };
  if (char === "*") return { value: DERIVED, end: i + 1 };
  if (char === "#") {
    let j = i + 1;
    while (j < text.length && /[0-9]/.test(text[j])) j += 1;
    return { value: new Ref(Number(text.slice(i + 1, j))), end: j };
  }
  if (char === "'") {
    let j = i + 1;
    while (j < text.length) {
      if (text[j] === "'") {
        if (text[j + 1] === "'") {
          j += 2;
          continue;
        }
        break;
      }
      j += 1;
    }
    return { value: decodeStepString(text.slice(i + 1, j)), end: j + 1 };
  }
  if (char === "." && /[0-9]/.test(text[i + 1] ?? "")) {
    let j = i + 1;
    while (j < text.length && /[0-9.eE+-]/.test(text[j])) j += 1;
    return { value: Number(text.slice(i, j)), end: j };
  }
  if (char === ".") {
    const j = text.indexOf(".", i + 1);
    if (j < 0) throw new Error("Unterminated enumeration in STEP value");
    const name = text.slice(i + 1, j);
    if (name === "T") return { value: true, end: j + 1 };
    if (name === "F") return { value: false, end: j + 1 };
    return { value: new Enum(name), end: j + 1 };
  }
  if (char === "(") {
    const items = [];
    let j = i + 1;
    for (;;) {
      while (j < text.length && /[\s,]/.test(text[j])) j += 1;
      if (text[j] === ")") return { value: items, end: j + 1 };
      if (j >= text.length) throw new Error("Unterminated list in STEP value");
      const item = parseValue(text, j);
      items.push(item.value);
      j = item.end;
    }
  }
  if (/[-+0-9]/.test(char)) {
    let j = i + 1;
    while (j < text.length && /[0-9.eE+-]/.test(text[j])) j += 1;
    return { value: Number(text.slice(i, j)), end: j };
  }
  if (/[A-Za-z_]/.test(char)) {
    let j = i;
    while (j < text.length && /[A-Za-z0-9_]/.test(text[j])) j += 1;
    const name = text.slice(i, j);
    if (text[j] !== "(") throw new Error(`Unexpected token ${name} in STEP value`);
    const inner = parseValue(text, j + 1);
    let k = inner.end;
    while (k < text.length && /\s/.test(text[k])) k += 1;
    if (text[k] !== ")") throw new Error(`Unterminated typed value ${name}`);
    return { value: new Typed(name, inner.value), end: k + 1 };
  }
  throw new Error(`Unexpected character ${JSON.stringify(char ?? "")} in STEP value`);
}

/** Split a record's argument text at top-level commas. */
export function splitArguments(text) {
  const parts = [];
  let depth = 0;
  let quoted = false;
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const char = text[i];
    if (quoted) {
      if (char === "'") quoted = false;
    } else if (char === "'") quoted = true;
    else if (char === "(") depth += 1;
    else if (char === ")") depth -= 1;
    else if (char === "," && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
  }
  if (text.trim()) parts.push(text.slice(start));
  return parts;
}

/** Whether a parsed STEP value references `#id` anywhere inside it. */
export function containsRef(value, id) {
  if (value instanceof Ref) return value.id === id;
  if (Array.isArray(value)) return value.some((item) => containsRef(item, id));
  if (value instanceof Typed) return containsRef(value.value, id);
  return false;
}

const isSpace = (char) => char === " " || char === "\t" || char === "\r" || char === "\n";
const isNameChar = (char) => /[A-Za-z0-9_]/.test(char);

/**
 * Scan STEP text once, outside string literals and comments. Returns every
 * `#id=` record with its spans (`records`, by id) and, for every referenced
 * entity, the ids of the records that reference it (`users`).
 */
export function indexRecords(source) {
  const records = new Map();
  const users = new Map();
  const length = source.length;
  let current = null;
  let depth = 0;
  let i = 0;
  while (i < length) {
    const char = source[i];
    if (char === "'") {
      i += 1;
      while (i < length) {
        if (source[i] === "'") {
          if (source[i + 1] === "'") {
            i += 2;
            continue;
          }
          break;
        }
        i += 1;
      }
      i += 1;
      continue;
    }
    if (char === "/" && source[i + 1] === "*") {
      const close = source.indexOf("*/", i + 2);
      i = close < 0 ? length : close + 2;
      continue;
    }
    if (char === "#") {
      let j = i + 1;
      while (j < length && source[j] >= "0" && source[j] <= "9") j += 1;
      if (j === i + 1) {
        i += 1;
        continue;
      }
      const id = Number(source.slice(i + 1, j));
      if (current) {
        let set = users.get(id);
        if (!set) users.set(id, (set = new Set()));
        set.add(current.id);
        i = j;
        continue;
      }
      let k = j;
      while (k < length && isSpace(source[k])) k += 1;
      if (source[k] !== "=") {
        i = j;
        continue;
      }
      let lineStart = i;
      while (lineStart > 0 && source[lineStart - 1] !== "\n") lineStart -= 1;
      let m = k + 1;
      while (m < length && isSpace(source[m])) m += 1;
      let n = m;
      while (n < length && isNameChar(source[n])) n += 1;
      let p = n;
      while (p < length && isSpace(source[p])) p += 1;
      // A complex instance has no class name and its arguments are the outer list.
      const className = source.slice(m, n);
      current = { id, className, start: i, argsStart: p + 1, argsEnd: -1, end: -1, lineStart, lineEnd: -1 };
      depth = 0;
      i = p;
      continue;
    }
    if (current) {
      if (char === "(") depth += 1;
      else if (char === ")") {
        depth -= 1;
        if (depth === 0 && current.argsEnd < 0) current.argsEnd = i;
      } else if (char === ";" && depth <= 0) {
        current.end = i + 1;
        let lineEnd = i + 1;
        if (source[lineEnd] === "\r") lineEnd += 1;
        if (source[lineEnd] === "\n") lineEnd += 1;
        current.lineEnd = lineEnd;
        if (current.argsEnd < 0) current.argsEnd = i;
        records.set(current.id, current);
        current = null;
      }
    }
    i += 1;
  }
  return { records, users };
}

function formatReal(value) {
  if (!Number.isFinite(value)) throw new Error(`${value} is not a finite number`);
  if (Number.isInteger(value)) return `${value}.`;
  let text = String(value);
  if (text.includes("e")) {
    let [mantissa, exponent] = text.split("e");
    if (!mantissa.includes(".")) mantissa += ".";
    text = `${mantissa}E${exponent}`;
  }
  return text;
}

/** Serialize a JavaScript value as STEP text; `attr` supplies the declared base type. */
export function formatValue(value, attr = null) {
  const base = attr?.base ?? "unknown";
  if (value === null || value === undefined) return "$";
  if (value === DERIVED) return "*";
  if (value instanceof Ref) return `#${value.id}`;
  if (value?.[ENTITY] !== undefined) return `#${value[ENTITY]}`;
  if (value instanceof Enum) return `.${value.value}.`;
  if (value instanceof Int) return String(value.value);
  if (value instanceof Typed) return `${value.type}(${formatValue(value.value)})`;
  if (typeof value === "boolean") return value ? ".T." : ".F.";
  if (typeof value === "number") return base === "integer" ? String(Math.trunc(value)) : formatReal(value);
  if (typeof value === "string") {
    if (base === "enumeration" || base === "boolean" || base === "logical") return `.${value.toUpperCase()}.`;
    if (base === "entity") throw new Error(`Expected an entity for ${attr.name}, not the string ${JSON.stringify(value)}`);
    if (base === "select") throw new Error(`${attr.name} is a select: use ifc.typed("IFCLABEL", ${JSON.stringify(value)}) or an entity`);
    return encodeStepString(value);
  }
  if (Array.isArray(value)) return `(${value.map((item) => formatValue(item, attr)).join(",")})`;
  throw new Error(`Cannot write a ${typeof value} as an IFC value`);
}

/** A fresh 22-character IFC GlobalId from 128 random bits. */
export function newGuid(random = crypto) {
  const bytes = new Uint8Array(16);
  random.getRandomValues(bytes);
  let number = 0n;
  for (const byte of bytes) number = (number << 8n) | BigInt(byte);
  let out = "";
  for (let i = 0; i < 21; i += 1) {
    out = GUID_ALPHABET[Number(number & 63n)] + out;
    number >>= 6n;
  }
  return GUID_ALPHABET[Number(number & 3n)] + out;
}

// ------------------------------------------------------------ the engine

/** @typedef {ReturnType<typeof createScriptEngine>} ScriptEngine */

/**
 * Create the script engine over an open model. `kernel` is the WASM kernel
 * holding `modelId`; the engine never changes it, it only builds a snapshot.
 * @param {import("./types.js").Kernel} kernel
 * @param {number} modelId
 */
export function createScriptEngine(kernel, modelId) {
  const infoCache = new Map();
  const classCache = new Map();
  const supertypeCache = new Map();
  const typeCache = new Map();
  const edits = new Map();
  const added = new Map();
  const removed = new Set();
  const output = [];
  let outputChars = 0;
  let dropped = 0;
  let sourceText = null;
  let recordIndex = null;
  let nextId = null;
  let contextRef = null;
  const schema = JSON.parse(kernel.getModelInfo(modelId)).schema;

  function text() {
    if (sourceText === null) {
      const bytes = kernel.exportModel(modelId);
      if (!bytes) throw new Error("The editable IFC source is no longer open.");
      sourceText = new TextDecoder("latin1").decode(bytes);
    }
    return sourceText;
  }

  /** Record spans and reference users of the source, scanned once outside strings and comments. */
  function index() {
    if (recordIndex === null) recordIndex = indexRecords(text());
    return recordIndex;
  }

  function allocateId() {
    if (nextId === null) {
      let max = 0;
      for (const id of index().records.keys()) if (id > max) max = id;
      nextId = max + 1;
    }
    const id = nextId;
    nextId += 1;
    return id;
  }

  function classDefinition(name) {
    const key = String(name).toUpperCase();
    if (!classCache.has(key)) {
      const json = kernel.getClassAttributes(modelId, String(name));
      classCache.set(key, json ? JSON.parse(json) : null);
    }
    const definition = classCache.get(key);
    if (!definition) throw new Error(`${name} is not a class of ${schema}`);
    return definition;
  }

  function info(id) {
    if (removed.has(id)) return null;
    if (added.has(id)) return added.get(id).info;
    if (!infoCache.has(id)) {
      const json = kernel.getEntityInfo(modelId, id);
      infoCache.set(id, json ? JSON.parse(json) : null);
    }
    return infoCache.get(id);
  }

  function className(id) {
    return info(id)?.class ?? null;
  }

  function idsOfType(name) {
    const key = String(name).toUpperCase();
    if (!typeCache.has(key)) typeCache.set(key, new Set(kernel.getIdsOfType(modelId, String(name))));
    return typeCache.get(key);
  }

  function isA(id, name) {
    if (added.has(id)) return supertypes(added.get(id).info.class).has(String(name).toUpperCase());
    return idsOfType(name).has(id);
  }

  /** The class and its supertypes, upper-cased, from the kernel's schema tables. */
  function supertypes(name) {
    const key = String(name).toUpperCase();
    if (!supertypeCache.has(key)) {
      const json = typeof kernel.getClassSupertypes === "function" ? kernel.getClassSupertypes(modelId, String(name)) : null;
      const names = json ? JSON.parse(json) : [String(name)];
      supertypeCache.set(key, new Set(names.map((item) => item.toUpperCase())));
    }
    return supertypeCache.get(key);
  }

  function decode(raw) {
    if (raw === undefined || raw === null || raw === "") return null;
    return wrap(parseValue(raw).value);
  }

  function wrap(value) {
    if (value instanceof Ref) return removed.has(value.id) ? null : entity(value.id);
    if (Array.isArray(value)) return value.map(wrap);
    if (value instanceof Enum) return value.value;
    return value;
  }

  function field(id, name) {
    const record = info(id);
    if (!record) return undefined;
    const upper = String(name).toUpperCase();
    return record.fields.find((item) => item.name.toUpperCase() === upper);
  }

  function attributeValue(id, name) {
    const item = field(id, name);
    if (!item) return undefined;
    const pending = edits.get(id)?.get(item.name);
    return decode(pending ?? item.raw);
  }

  function setAttribute(id, name, value) {
    const record = info(id);
    if (!record) throw new Error(`Entity #${id} does not exist`);
    if (record.complex) throw new Error(`#${id} is a complex instance; edit it by argument index in the attribute editor`);
    const item = field(id, name);
    if (!item) throw new Error(`${record.class} has no attribute ${name}`);
    const attr = { name: item.name, base: item.kind };
    const raw = formatValue(value, attr);
    if (added.has(id)) {
      added.get(id).args[item.index] = raw;
      item.raw = raw;
      return;
    }
    if (!edits.has(id)) edits.set(id, new Map());
    edits.get(id).set(item.name, raw);
  }

  function entity(id) {
    const target = { [ENTITY]: id };
    /** @param {string | symbol} prop */
    const read = (prop) => {
      if (prop === ENTITY || prop === "id") return id;
      if (prop === "type" || prop === "class") return className(id);
      if (prop === "is") return (name) => isA(id, name);
      if (prop === "attributes") return () => Object.fromEntries((info(id)?.fields ?? []).map((item) => [item.name, attributeValue(id, item.name)]));
      if (prop === "toString" || prop === "toJSON" || prop === Symbol.toPrimitive) return () => `#${id}`;
      if (prop === "inspect" || prop === Symbol.iterator || typeof prop !== "string") return undefined;
      return attributeValue(id, prop);
    };
    return new Proxy(target, {
      get(_, prop) {
        return read(prop);
      },
      set(_, prop, value) {
        if (typeof prop !== "string") return false;
        setAttribute(id, prop, value);
        return true;
      },
      has(_, prop) {
        return typeof prop === "string" && field(id, prop) !== undefined;
      },
      ownKeys() {
        return ["id", "type", ...(info(id)?.fields ?? []).map((item) => item.name)];
      },
      getOwnPropertyDescriptor(_, prop) {
        return { enumerable: true, configurable: true, value: read(prop) };
      },
    });
  }

  function resolveId(value) {
    if (value?.[ENTITY] !== undefined) return value[ENTITY];
    if (value instanceof Ref) return value.id;
    if (Number.isInteger(value)) return value;
    throw new Error("Expected an entity or an express id");
  }

  function add(name, attributes = {}) {
    const definition = classDefinition(name);
    if (definition.abstract) throw new Error(`${definition.class} is abstract`);
    const args = new Array(definition.attributes.length).fill("$");
    const provided = new Map();
    if (Array.isArray(attributes)) {
      if (attributes.length !== definition.attributes.length) {
        throw new Error(`${definition.class} takes ${definition.attributes.length} attributes, ${attributes.length} given`);
      }
      attributes.forEach((value, index) => provided.set(definition.attributes[index].name.toUpperCase(), value));
    } else {
      for (const [key, value] of Object.entries(attributes ?? {})) {
        const upper = key.toUpperCase();
        if (!definition.attributes.some((attribute) => attribute.name.toUpperCase() === upper)) {
          throw new Error(`${definition.class} has no attribute ${key}`);
        }
        provided.set(upper, value);
      }
    }
    const missing = [];
    definition.attributes.forEach((attribute, index) => {
      const upper = attribute.name.toUpperCase();
      if (attribute.derived) {
        args[index] = "*";
        return;
      }
      if (provided.has(upper) && provided.get(upper) !== undefined) {
        args[index] = formatValue(provided.get(upper), attribute);
      } else if (upper === "GLOBALID") {
        args[index] = encodeStepString(newGuid());
      } else if (!attribute.optional) {
        missing.push(attribute.name);
      }
    });
    // IFC2X3 requires an owner history on every rooted entity; reuse the file's first one.
    if (missing.includes("OwnerHistory")) {
      const history = ownerHistory();
      if (history !== null) {
        args[definition.attributes.findIndex((attribute) => attribute.name === "OwnerHistory")] = `#${history}`;
        missing.splice(missing.indexOf("OwnerHistory"), 1);
      }
    }
    if (missing.length) {
      const hint = missing.includes("OwnerHistory") ? "; this schema needs an IfcOwnerHistory and the model has none (createModel writes one)" : "";
      throw new Error(`${definition.class} needs ${missing.join(", ")}${hint}`);
    }
    const id = allocateId();
    const fields = definition.attributes.map((attribute, index) => ({
      index, name: attribute.name, kind: attribute.base, type: attribute.type, optional: attribute.optional, raw: args[index], value: null,
    }));
    added.set(id, { className: definition.class, args, info: { expressId: id, class: definition.class, complex: false, fields } });
    if (typeCache.has(definition.class.toUpperCase())) typeCache.get(definition.class.toUpperCase()).add(id);
    return entity(id);
  }

  function ownerHistory() {
    for (const [id, record] of added) if (record.className.toUpperCase() === "IFCOWNERHISTORY") return id;
    const [first] = [...idsOfType("IfcOwnerHistory")].filter((id) => !removed.has(id));
    return first ?? null;
  }

  // ----------------------------------------------------------- records

  function locate(id) {
    const record = index().records.get(id);
    return record && record.className ? record : null;
  }

  function referencingRecords(id) {
    const found = new Set();
    // Records with pending edits are judged by their current arguments, not the file.
    for (const otherId of index().users.get(id) ?? []) if (otherId !== id && !edits.has(otherId)) found.add(otherId);
    for (const otherId of edits.keys()) {
      if (otherId !== id && (currentArguments(otherId) ?? []).some((arg) => containsRef(parseValue(arg).value, id))) found.add(otherId);
    }
    for (const [otherId, record] of added) {
      if (otherId !== id && record.args.some((arg) => containsRef(parseValue(arg).value, id))) found.add(otherId);
    }
    for (const otherId of removed) found.delete(otherId);
    return [...found];
  }

  function currentArguments(id) {
    if (added.has(id)) return added.get(id).args.slice();
    const record = info(id);
    if (!record) return null;
    const pending = edits.get(id);
    return record.fields.map((item) => pending?.get(item.name) ?? item.raw);
  }

  function remove(target, { detach = true } = {}) {
    const id = resolveId(target);
    if (removed.has(id)) return;
    const record = info(id);
    if (!record) throw new Error(`Entity #${id} does not exist`);
    if (detach) {
      for (const otherId of referencingRecords(id)) {
        const other = info(otherId);
        if (!other || other.complex) continue;
        let emptied = false;
        for (const item of other.fields) {
          const raw = edits.get(otherId)?.get(item.name) ?? item.raw;
          const value = parseValue(raw).value;
          if (!containsRef(value, id)) continue;
          if (Array.isArray(value)) {
            const kept = value.filter((element) => !(element instanceof Ref && element.id === id));
            if (kept.length === 0) emptied = true;
            else setAttribute(otherId, item.name, kept);
          } else if (item.optional) {
            setAttribute(otherId, item.name, null);
          } else {
            emptied = true;
          }
        }
        // A relationship that lost a required end goes with the entity; anything else is refused.
        if (emptied) {
          if (other.class.toUpperCase().startsWith("IFCREL")) remove(otherId, { detach: true });
          else throw new Error(`Removing #${id} would leave ${other.class} #${otherId} without a required reference`);
        }
      }
    }
    removed.add(id);
    edits.delete(id);
    if (added.has(id)) added.delete(id);
  }

  function byGuid(guid) {
    for (const [id, record] of added) {
      if (record.args[0] === encodeStepString(guid)) return entity(id);
    }
    const literal = encodeStepString(guid).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const pattern = new RegExp(`#(\\d+)\\s*=\\s*[A-Za-z0-9_]+\\s*\\(\\s*${literal}\\s*,`, "g");
    const source = text();
    let match;
    while ((match = pattern.exec(source))) {
      // Only a real record start counts; the same text inside a string or a comment does not.
      const record = index().records.get(Number(match[1]));
      if (record && record.start === match.index) return removed.has(record.id) ? null : entity(record.id);
    }
    return null;
  }

  function byType(name) {
    const ids = new Set([...idsOfType(name)].filter((id) => !removed.has(id)));
    for (const id of added.keys()) if (isA(id, name)) ids.add(id);
    return [...ids].map(entity);
  }

  function inverses(target, filterClass = null) {
    const id = resolveId(target);
    const upper = filterClass ? String(filterClass).toUpperCase() : null;
    return referencingRecords(id)
      .filter((otherId) => !upper || isA(otherId, filterClass) || className(otherId)?.toUpperCase() === upper)
      .map(entity);
  }

  function context() {
    if (contextRef === null) {
      const contexts = byType("IfcGeometricRepresentationContext").filter((item) => item.type === "IfcGeometricRepresentationContext");
      contextRef = contexts.find((item) => item.ContextType === "Model") ?? contexts[0] ?? null;
      if (!contextRef) throw new Error("The model has no geometric representation context");
    }
    return contextRef;
  }

  function contain(product, structure) {
    const existing = inverses(structure, "IfcRelContainedInSpatialStructure").find((rel) => rel.RelatingStructure?.id === structure.id);
    if (existing) {
      existing.RelatedElements = [...existing.RelatedElements, product];
      return existing;
    }
    return add("IfcRelContainedInSpatialStructure", { RelatedElements: [product], RelatingStructure: structure });
  }

  function aggregate(parent, child) {
    const existing = inverses(parent, "IfcRelAggregates").find((rel) => rel.RelatingObject?.id === parent.id);
    if (existing) {
      existing.RelatedObjects = [...existing.RelatedObjects, child];
      return existing;
    }
    return add("IfcRelAggregates", { RelatingObject: parent, RelatedObjects: [child] });
  }

  function container(product) {
    const rel = inverses(product, "IfcRelContainedInSpatialStructure").find((item) => (item.RelatedElements ?? []).some((element) => element?.id === product.id));
    return rel?.RelatingStructure ?? null;
  }

  function print(...values) {
    const line = values.map((value) => (typeof value === "string" ? value : describe(value))).join(" ");
    if (outputChars + line.length > MAX_OUTPUT) {
      dropped += line.length;
      return;
    }
    outputChars += line.length + 1;
    output.push(line);
  }

  function describe(value) {
    if (value?.[ENTITY] !== undefined) return `#${value[ENTITY]} ${className(value[ENTITY]) ?? ""}`.trim();
    if (Array.isArray(value)) return `[${value.map(describe).join(", ")}]`;
    if (value instanceof Enum || value instanceof Typed || value instanceof Ref) return value.toString();
    if (value === null) return "null";
    if (typeof value === "object") {
      try {
        return JSON.stringify(value);
      } catch {
        return String(value);
      }
    }
    return String(value);
  }

  // ----------------------------------------------------------- results

  function changed() {
    return edits.size > 0 || added.size > 0 || removed.size > 0;
  }

  /** The new file text with every pending edit, addition and removal applied. */
  function snapshotText() {
    const source = text();
    const pieces = [];
    const replacements = [];
    for (const [id, pending] of edits) {
      const location = locate(id);
      if (!location) throw new Error(`Entity #${id} was not found in the source`);
      const record = info(id);
      const args = splitArguments(source.slice(location.argsStart, location.argsEnd));
      if (args.length !== record.fields.length) {
        throw new Error(`#${id} ${record.class} has ${args.length} arguments in the file, ${record.fields.length} in the schema`);
      }
      for (const item of record.fields) if (pending.has(item.name)) args[item.index] = pending.get(item.name);
      replacements.push({ start: location.argsStart, end: location.argsEnd, text: args.join(",") });
    }
    for (const id of removed) {
      if (added.has(id)) continue;
      const location = locate(id);
      if (location) replacements.push({ start: location.lineStart, end: location.lineEnd, text: "" });
    }
    replacements.sort((left, right) => left.start - right.start);
    let cursor = 0;
    for (const replacement of replacements) {
      if (replacement.start < cursor) throw new Error("Overlapping edits in one record");
      pieces.push(source.slice(cursor, replacement.start), replacement.text);
      cursor = replacement.end;
    }
    let result = pieces.join("") + source.slice(cursor);
    if (added.size) {
      const lines = [...added.values()].map((record) => `#${record.info.expressId}=${record.className.toUpperCase()}(${record.args.join(",")});`);
      const dataIndex = result.indexOf("DATA;");
      const endIndex = result.indexOf("ENDSEC;", dataIndex);
      if (dataIndex < 0 || endIndex < 0) throw new Error("The source has no DATA section");
      const newline = result.includes("\r\n") ? "\r\n" : "\n";
      result = result.slice(0, endIndex) + lines.join(newline) + newline + result.slice(endIndex);
    }
    return result;
  }

  function snapshotBytes() {
    const value = snapshotText();
    const bytes = new Uint8Array(value.length);
    for (let i = 0; i < value.length; i += 1) bytes[i] = value.charCodeAt(i) & 0xff;
    return bytes;
  }

  function operations() {
    return { created: added.size, modified: edits.size, deleted: [...removed].filter((id) => !added.has(id)).length };
  }

  function outputText() {
    const value = output.join("\n");
    return dropped ? `${value}\n... ${dropped} more characters` : value;
  }

  const voidRel = (host, opening) => add("IfcRelVoidsElement", { RelatingBuildingElement: host, RelatedOpeningElement: opening });
  const fillRel = (opening, element) => add("IfcRelFillsElement", { RelatingOpeningElement: opening, RelatedBuildingElement: element });
  const helpers = createHelpers({
    add, byType, inverses, contain, aggregate, container, context, classDefinition, schema,
    void: voidRel, fill: fillRel, typed: (type, value) => new Typed(type, value),
    isTyped: (value) => value instanceof Typed, isInt: (value) => value instanceof Int,
    modelInfo: () => JSON.parse(kernel.getModelInfo(modelId)),
  });
  const api = Object.freeze({
    schema,
    byType, get: (id) => {
      const numeric = resolveId(id);
      if (!info(numeric)) throw new Error(`Entity #${numeric} does not exist`);
      return entity(numeric);
    },
    byGuid, add, remove, inverses, container, contain, aggregate, context, ...helpers,
    void: voidRel, fill: fillRel,
    newGuid, enum: (value) => new Enum(value), typed: (type, value) => new Typed(type, value), int: (value) => new Int(value),
    ref: (id) => entity(resolveId(id)), derived: DERIVED, print,
  });

  return {
    api, print, entity, changed, snapshotBytes, operations, outputText,
    resolve: (selection) => {
      const ids = [];
      for (const guid of selection?.guids ?? []) {
        const found = byGuid(guid);
        if (found) ids.push(found.id);
      }
      if (!ids.length) for (const id of selection?.ids ?? []) if (info(Number(id))) ids.push(Number(id));
      return ids.map(entity);
    },
  };
}

/**
 * Run a script; returns `{ ok, stdout, error, traceback, changed, operations }` and leaves the snapshot in the engine.
 * @param {ScriptEngine} engine
 * @param {string} source
 * @param {import("./types.js").Selection | null} selection
 * @returns {import("./types.js").ScriptReport}
 */
export function runScript(engine, source, selection) {
  const selected = engine.resolve(selection);
  let compiled;
  try {
    compiled = new Function("ifc", "selected", "selection", "print", `"use strict";\n${source}`);
  } catch (error) {
    return failure(engine, error, source);
  }
  try {
    compiled(engine.api, selected[0] ?? null, selected, engine.print);
  } catch (error) {
    return failure(engine, error, source);
  }
  return { ok: true, stdout: engine.outputText(), changed: engine.changed(), operations: engine.operations() };
}

function failure(engine, error, source) {
  const message = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  // Chrome reports positions inside the Function body; two header lines precede the script.
  const match = /<anonymous>:(\d+):(\d+)/.exec(error?.stack ?? "");
  const line = match ? Number(match[1]) - 3 : null;
  const lines = source.split("\n");
  const traceback = line != null && line >= 1 && line <= lines.length ? `line ${line}: ${lines[line - 1].trim()}` : "";
  return { ok: false, stdout: engine.outputText(), error: message, traceback, changed: false, operations: engine.operations() };
}
