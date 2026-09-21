// SPDX-License-Identifier: Apache-2.0

//! The worker entry of `createViewer`'s worker mode. A module worker resolves
//! no bare specifier, so the kernel and `@tessifc/edit` are looked up next to
//! this package; a host names another kernel through the worker's `name`.

const options = readOptions(self.name);
const here = import.meta.url;
const editBase = options.editUrl ?? new URL("../../edit/src/", here).href;
// The checkout keeps the kernel under `wasm`, an installed tree under `core`.
const kernelUrls = options.wasmUrl
  ? [String(options.wasmUrl)]
  : [new URL("../../wasm/pkg/tessifc_wasm.js", here).href, new URL("../../core/pkg/tessifc_wasm.js", here).href];

try {
  const [{ startKernelWorker }, session, engine, describe, glue] = await Promise.all([
    import("./kernel-worker-core.js"),
    import(new URL("session.js", editBase).href),
    import(new URL("script-engine.js", editBase).href),
    import(new URL("describe.js", editBase).href),
    firstThatLoads(kernelUrls),
  ]);
  startKernelWorker({
    glue,
    edit: {
      createEditingSession: session.createEditingSession,
      createScriptEngine: engine.createScriptEngine,
      runScript: engine.runScript,
      storeysOf: describe.storeysOf,
      lengthUnitOf: describe.lengthUnitOf,
    },
    settings: options.settings ?? undefined,
  });
} catch (error) {
  self.postMessage({ type: "boot-error", message: error instanceof Error ? error.message : String(error) });
}

/** The options a client passes through the worker's name, JSON or nothing. */
function readOptions(name) {
  if (typeof name !== "string" || !name.startsWith("{")) return {};
  try {
    const parsed = JSON.parse(name);
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

/** The first of several module URLs that imports; the last failure is the error. */
async function firstThatLoads(urls) {
  let failure = null;
  for (const url of urls) {
    try {
      return await import(url);
    } catch (error) {
      failure = error;
    }
  }
  throw failure ?? new Error("no kernel module to load");
}
