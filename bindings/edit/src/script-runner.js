// SPDX-License-Identifier: Apache-2.0

//! Scripts under a time limit, for Node hosts: each runs in a worker thread
//! with its own kernel over a copy of the model, and one that overruns is
//! stopped by ending the thread. The host's kernel never runs script code.

import { createHash } from "node:crypto";
import { Worker } from "node:worker_threads";

export const DEFAULT_SCRIPT_TIMEOUT_MS = 30_000;

/** @typedef {import("./types.js").ScriptRunner} ScriptRunner */

/**
 * Create a runner. `kernelModule` is the path of the Node kernel module the
 * worker loads; `timeoutMs` is the limit per script, 0 for none. The worker
 * stays warm between runs and reloads the model only when its bytes changed.
 * @param {{ kernelModule: string, timeoutMs?: number }} options
 * @returns {ScriptRunner}
 */
export function createScriptRunner({ kernelModule, timeoutMs = DEFAULT_SCRIPT_TIMEOUT_MS }) {
  if (!kernelModule) throw new Error("createScriptRunner needs the path of the Node kernel module.");
  if (!Number.isFinite(timeoutMs) || timeoutMs < 0) throw new Error("timeoutMs must be zero or a positive number of milliseconds.");
  let worker = null;
  let nextId = 0;
  let queue = Promise.resolve();
  let disposed = false;

  function spawn() {
    const spawned = new Worker(new URL("./script-worker.js", import.meta.url), { workerData: { kernelModule } });
    // An idle worker must not keep the host process alive.
    spawned.unref();
    return spawned;
  }

  function drop() {
    const dropped = worker;
    worker = null;
    return dropped ? dropped.terminate() : Promise.resolve();
  }

  function stopped(why) {
    const error = why === "timedOut"
      ? `ScriptTimeout: the script ran longer than ${timeoutMs} ms and was stopped`
      : "AbortError: the script was cancelled";
    return { ok: false, timedOut: why === "timedOut", aborted: why === "aborted", error, traceback: "", stdout: "", changed: false,
      operations: { created: 0, modified: 0, deleted: 0 }, snapshot: null };
  }

  function execute(bytes, source, selection, { commit, signal }) {
    return new Promise((resolve, reject) => {
      if (signal?.aborted) {
        resolve(stopped("aborted"));
        return;
      }
      if (!worker) worker = spawn();
      const active = worker;
      const id = ++nextId;
      const hash = createHash("sha256").update(bytes).digest("hex");
      const copy = bytes.slice();
      let timer = null;
      const cleanup = () => {
        clearTimeout(timer);
        active.off("message", onMessage);
        active.off("error", onError);
        active.off("exit", onExit);
        signal?.removeEventListener("abort", onAbort);
        active.unref();
      };
      const settle = (outcome) => {
        cleanup();
        resolve(outcome);
      };
      const stop = (why) => {
        drop();
        settle(stopped(why));
      };
      function onMessage(message) {
        if (message?.id !== id) return;
        if (message.type === "started") {
          if (timeoutMs > 0) timer = setTimeout(() => stop("timedOut"), timeoutMs);
        } else if (message.type === "result") {
          settle({ ...message.report, snapshot: message.snapshot ?? null });
        }
      }
      function onError(error) {
        drop();
        cleanup();
        reject(error instanceof Error ? error : new Error(String(error)));
      }
      function onExit(code) {
        if (worker === active) worker = null;
        cleanup();
        reject(new Error(`The script worker exited with code ${code} before answering.`));
      }
      function onAbort() {
        stop("aborted");
      }
      active.ref();
      active.on("message", onMessage);
      active.on("error", onError);
      active.on("exit", onExit);
      signal?.addEventListener("abort", onAbort, { once: true });
      active.postMessage({ type: "run", id, hash, buffer: copy.buffer, source, selection, commit }, [copy.buffer]);
    });
  }

  /**
   * Run `source` against the model `bytes` hold; resolves with the script
   * report plus `snapshot`, the edited file when the script changed it and
   * `commit` is true. A stopped script resolves with `ok: false` and
   * `timedOut` or `aborted` set. Runs are serialised; `bytes` are copied.
   * @param {Uint8Array} bytes
   * @param {string} source
   * @param {import("./types.js").Selection | null} [selection]
   * @param {{ commit?: boolean, signal?: AbortSignal | null }} [options]
   * @returns {Promise<import("./types.js").ScriptReport & { snapshot: Uint8Array | null }>}
   */
  function run(bytes, source, selection = null, { commit = true, signal = null } = {}) {
    if (disposed) return Promise.reject(new Error("The script runner is disposed."));
    if (!(bytes instanceof Uint8Array)) return Promise.reject(new TypeError("run() needs the model bytes."));
    const turn = queue.then(() => execute(bytes, String(source ?? ""), selection, { commit, signal }));
    queue = turn.catch(() => {});
    return turn;
  }

  /** End the worker; a script still running is stopped. */
  async function dispose() {
    disposed = true;
    await drop();
  }

  return {
    run,
    dispose,
    get timeoutMs() {
      return timeoutMs;
    },
    get warm() {
      return worker !== null;
    },
  };
}
