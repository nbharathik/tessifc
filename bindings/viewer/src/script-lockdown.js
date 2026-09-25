// SPDX-License-Identifier: Apache-2.0

//! Takes the network, module loading and code generation away from a worker
//! before it runs scripts: a script can read and change the model, but it
//! cannot send it anywhere.

/** Worker globals that reach another origin or start a fresh global of their own. */
export const EGRESS_GLOBALS = Object.freeze([
  "fetch", "XMLHttpRequest", "WebSocket", "WebSocketStream", "WebTransport", "EventSource",
  "importScripts", "Worker", "SharedWorker", "BroadcastChannel", "caches", "FontFace", "FontFaceSet",
  "fonts", "Notification", "RTCPeerConnection", "webkitRTCPeerConnection", "ShadowRealm",
]);

// Captured when the module loads, before anything can replace them.
/** @type {Array<[string, any]>} */
const COMPILERS = [
  ["Function", Function],
  ["AsyncFunction", Object.getPrototypeOf(async function () {}).constructor],
  ["GeneratorFunction", Object.getPrototypeOf(function* () {}).constructor],
  ["AsyncGeneratorFunction", Object.getPrototypeOf(async function* () {}).constructor],
];

// `import()` is syntax, so no global can take it away; the source is refused instead.
const MODULE_LOAD = /\bimport\b/;

const locked = new WeakSet();

/**
 * Lock the global of the realm this module runs in before scripts run there:
 * every name in `EGRESS_GLOBALS` becomes a getter that throws, and `eval`,
 * the function constructors and string timers refuse to compile code. A host
 * keeps its own references from before the call. Dynamic `import()` stays;
 * check each script with `scriptRefusal`. Returns the names it could not
 * lock, empty when the lockdown is complete.
 * @param {any} [scope] the realm's global object
 * @returns {string[]}
 */
export function lockdownScriptScope(scope = globalThis) {
  if (locked.has(scope)) return [];
  const gaps = [];
  for (const name of EGRESS_GLOBALS) {
    const unavailable = () => {
      throw new ReferenceError(`${name} is not available: scripts run without network access`);
    };
    if (!replaceGlobal(scope, name, { get: unavailable, enumerable: false, configurable: false })) gaps.push(name);
  }
  for (const [name, constructor] of COMPILERS) {
    const stub = refusing(name, `${name} cannot compile code in a script`);
    // Keeps `instanceof` and `constructor.name` checks working.
    stub.prototype = constructor.prototype;
    if (!defineFixed(constructor.prototype, "constructor", stub)) gaps.push(`${name}.prototype.constructor`);
    if (name === "Function" && !replaceGlobal(scope, name, fixed(stub))) gaps.push(name);
  }
  if (!replaceGlobal(scope, "eval", fixed(refusing("eval", "eval cannot run code in a script")))) gaps.push("eval");
  for (const name of ["setTimeout", "setInterval"]) {
    const original = scope[name];
    if (typeof original !== "function") continue;
    const guarded = function (handler, ...rest) {
      if (typeof handler !== "function") throw new EvalError(`${name} runs a function, not a string of code`);
      return original.call(scope, handler, ...rest);
    };
    Object.defineProperty(guarded, "name", { value: name });
    if (!replaceGlobal(scope, name, fixed(guarded))) gaps.push(name);
  }
  if (!gaps.length) locked.add(scope);
  return gaps;
}

/**
 * Why a script may not run in a locked worker, or null. `import()` loads code
 * from any origin, so the word `import` is refused anywhere in the source,
 * strings and comments included.
 * @param {string} source
 * @returns {string | null}
 */
export function scriptRefusal(source) {
  return MODULE_LOAD.test(String(source ?? ""))
    ? "ScriptRefused: browser scripts cannot load modules, so the word import is not allowed anywhere in a script, strings and comments included."
    : null;
}

/**
 * A constructible stand-in, named like the function it replaces, that throws.
 * @returns {any}
 */
function refusing(name, message) {
  const stub = function () {
    throw new EvalError(message);
  };
  Object.defineProperty(stub, "name", { value: name });
  return stub;
}

function fixed(value) {
  return { value, writable: false, enumerable: false, configurable: false };
}

function defineFixed(target, name, value) {
  try {
    Object.defineProperty(target, name, fixed(value));
    return true;
  } catch {
    return false;
  }
}

/** Remove `name` from the global and its prototypes, then define it on the global; false when a copy stays. */
function replaceGlobal(scope, name, descriptor) {
  try {
    for (let owner = scope; owner; owner = Object.getPrototypeOf(owner)) {
      if (Object.hasOwn(owner, name)) delete owner[name];
    }
    Object.defineProperty(scope, name, descriptor);
    return true;
  } catch {
    return false;
  }
}
