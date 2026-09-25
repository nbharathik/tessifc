// SPDX-License-Identifier: Apache-2.0
// Scripts and model text, without a browser. The kernel worker's lockdown runs
// in a worker thread of its own: afterwards a script compiled the way the
// engine compiles one reaches no network, no module loader and no code
// generation, while ordinary scripts still run. The Selection button's line
// keeps model text inside its comment.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Worker, isMainThread, parentPort } from "node:worker_threads";
import { runScript } from "../../bindings/edit/src/script-engine.js";
import { EGRESS_GLOBALS, lockdownScriptScope, scriptRefusal } from "../../bindings/viewer/src/script-lockdown.js";
import { commentText, selectionLine } from "../src/session-panel.js";

// Each probe is a script body; the worker reports `ok <stdout>` or `error <message>`.
const PROBES = {
  plain: 'print(typeof ifc, [1, 2].map((x) => x * 2).join(","), JSON.stringify({ a: 1 }))',
  instanceChecks: "print(print instanceof Function, (async () => 1).constructor.name, (function* () {}).constructor.name)",
  timerWithFunction: 'clearTimeout(setTimeout(() => {}, 0)); print("timer")',
  eval: 'eval("1")',
  functionCall: 'Function("return 1")()',
  functionNew: 'new Function("return 1")',
  arrowConstructor: '(() => {}).constructor("return 1")()',
  asyncConstructor: '(async () => {}).constructor("return 1")',
  generatorConstructor: '(function* () {}).constructor("return 1")',
  asyncGeneratorConstructor: '(async function* () {}).constructor("return 1")',
  reflectConstruct: 'Reflect.construct(Function, ["return 1"])',
  prototypeConstructor: 'Object.getPrototypeOf(print).constructor("return 1")',
  stringTimeout: 'setTimeout("print(1)", 0)',
  stringInterval: 'setInterval("print(1)", 0)',
  redefineFetch: 'Object.defineProperty(globalThis, "fetch", { value: () => 1 })',
  escapedImport: '\\u0069mport("node:fs")',
};
for (const name of EGRESS_GLOBALS) PROBES[`global ${name}`] = `const found = ${name}; print(typeof found);`;

/** A stand-in engine: the lockdown concerns the realm, not the model. */
function engine() {
  const lines = [];
  return {
    api: Object.freeze({}),
    resolve: () => [],
    print: (...values) => lines.push(values.join(" ")),
    outputText: () => lines.join("\n"),
    changed: () => false,
    operations: () => ({ created: 0, modified: 0, deleted: 0 }),
  };
}

if (!isMainThread) {
  // Names Node lacks, and a copy on a prototype, must be covered too.
  globalThis.XMLHttpRequest = class {};
  Object.defineProperty(Object.getPrototypeOf(globalThis), "EventSource", { value: class {}, configurable: true, writable: true });
  const gaps = lockdownScriptScope(globalThis);
  const results = {};
  for (const [name, source] of Object.entries(PROBES)) {
    const report = runScript(engine(), source, null);
    results[name] = report.ok ? `ok ${report.stdout}` : `error ${report.error}`;
  }
  parentPort.postMessage({
    gaps,
    again: lockdownScriptScope(globalThis),
    prototypeCopy: Object.hasOwn(Object.getPrototypeOf(globalThis), "EventSource"),
    results,
  });
} else {
  const outcome = await new Promise((resolve, reject) => {
    const worker = new Worker(new URL(import.meta.url));
    worker.once("message", resolve);
    worker.once("error", reject);
  });
  assert.deepEqual(outcome.gaps, [], "every global is locked");
  assert.deepEqual(outcome.again, [], "a second lockdown changes nothing");
  assert.equal(outcome.prototypeCopy, false, "a copy on a prototype is removed as well");
  const results = outcome.results;
  assert.equal(results.plain, 'ok object 2,4 {"a":1}', "an ordinary script still runs");
  assert.equal(results.instanceChecks, "ok true AsyncFunction GeneratorFunction", "instanceof and constructor names still work");
  assert.equal(results.timerWithFunction, "ok timer", "a timer with a function still runs");
  for (const name of ["eval", "functionCall", "functionNew", "arrowConstructor", "asyncConstructor", "generatorConstructor",
    "asyncGeneratorConstructor", "reflectConstruct", "prototypeConstructor", "stringTimeout", "stringInterval"]) {
    assert.match(results[name], /^error EvalError: /, `${name} cannot compile code`);
  }
  assert.match(results.redefineFetch, /^error TypeError: Cannot redefine property: fetch/, "a script cannot put fetch back");
  assert.match(results.escapedImport, /^error SyntaxError: /, "an escaped import keyword does not parse");
  for (const name of EGRESS_GLOBALS) {
    assert.match(results[`global ${name}`], new RegExp(`^error ReferenceError: ${name} is not available`), `${name} is gone`);
  }
  console.log("ok    after the lockdown a script reaches no network, no new global and no code generation");

  assert.equal(scriptRefusal('print("important imports", ifc.byType("IfcWall").length)'), null, "words that merely contain import are fine");
  assert.equal(scriptRefusal("\\u0069mport('x')"), null, "the escaped form is left to the parser, which rejects it");
  for (const source of ['import("data:text/javascript,export default 1")', "import/**/('x')", "import.meta", 'print(1); // import later', "x\nimport ('y')"]) {
    assert.match(scriptRefusal(source), /^ScriptRefused: /, `refused: ${JSON.stringify(source)}`);
  }
  const core = readFileSync(new URL("../../bindings/viewer/src/kernel-worker-core.js", import.meta.url), "utf8");
  assert.match(core, /kernel = new glue\.Kernel\(\);[\s\S]{0,200}?lockdownScriptScope\(globalThis\);\s*post\(\{ type: "ready"/,
    "the worker locks its global before it reports ready");
  assert.match(core, /scriptRefusal\(text\);[\s\S]{0,800}?edit\.runScript\(engine, text, selection\)/, "every script is checked before it compiles");
  assert.match(core, /startKernelWorker\(\{[^)]*lockdown = true/, "the lockdown is on by default");
  console.log("ok    the kernel worker locks down before its first script and refuses module loading");

  // Model text cannot end the Selection button's comment and add code.
  const hostile = {
    expressId: 7,
    className: "IfcWall\u2028print('class')",
    globalId: "2O2Fr$t4X7Zf8NOew3FLOH",
    name: "Wall\u2028print('injected')\u2029print('again')\r\nprint('more')",
  };
  assert.equal(commentText("a\u2028b\u2029c\nd\re\u0085f\u0000g"), "a b c d e f g");
  for (const python of [false, true]) {
    const line = selectionLine(hostile, python);
    assert.equal(line.split(/[\n\r\u2028\u2029\u0085]/).filter(Boolean).length, 1, `one ${python ? "Python" : "JavaScript"} line`);
    assert.ok(line.endsWith("\n"));
  }
  const calls = [];
  const ifc = { byGuid: (guid) => calls.push(`byGuid ${guid}`), get: (id) => calls.push(`get ${id}`) };
  const print = (...values) => calls.push(`print ${values.join(" ")}`);
  new Function("ifc", "print", selectionLine(hostile, false))(ifc, print);
  new Function("ifc", "print", selectionLine({ ...hostile, globalId: null }, false))(ifc, print);
  assert.deepEqual(calls, ["byGuid 2O2Fr$t4X7Zf8NOew3FLOH", "get 7"], "nothing after the comment runs");
  assert.match(selectionLine(hostile, true), /^target = model\.by_guid\("2O2Fr\$t4X7Zf8NOew3FLOH"\) {2}# IfcWall print\('class'\) "Wall print/);
  console.log("ok    the Selection line keeps model text inside its comment");
}
