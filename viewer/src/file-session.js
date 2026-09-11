// SPDX-License-Identifier: Apache-2.0

//! The local editing session: follows completed snapshots of one IFC file
//! and sends scripts, undo and assistant requests to the same origin.

const WAIT_SECONDS = 20;
const RETRY_MS = 1000;

/** Follow the session; `status` receives every server status seen. */
export function startFileSession({ ready, loaded, open, update, report, status }) {
  let stopped = false;
  let version = null;
  let known = null;
  let timer;
  let previousError = "";
  const controller = new AbortController();
  // Waiters for a content version the viewer has not applied yet. Versions are
  // content hashes, so undo brings an old one back: the sequence tells them apart.
  const waiters = new Map();
  const applied = new Map();
  let sequence = 0;

  async function fetchStatus(wait) {
    const query = wait && version ? `?after=${encodeURIComponent(version)}&timeout=${WAIT_SECONDS}` : "";
    const response = await fetch(`/__tessifc/session${query}`, { cache: "no-store", signal: controller.signal });
    if (!response.ok) throw new Error("The local IFC file session is unavailable.");
    const session = await response.json();
    known = session;
    status?.(session);
    return session;
  }

  function settle(target, error, result) {
    sequence += 1;
    for (const waiter of waiters.get(target) ?? []) error ? waiter.reject(error) : waiter.resolve(result);
    waiters.delete(target);
    if (!error) {
      applied.delete(target);
      applied.set(target, { sequence, result });
      if (applied.size > 8) applied.delete(applied.keys().next().value);
    }
  }

  async function poll() {
    if (stopped) return;
    let attemptedVersion = null;
    let idle = true;
    try {
      if (!ready()) return;
      idle = false;
      const session = await fetchStatus(true);
      if (session.version === version) return;
      const source = await fetch(`/__tessifc/model.ifc?version=${encodeURIComponent(session.version)}`,
        { cache: "no-store", signal: controller.signal });
      if (source.status === 409) return;
      if (!source.ok) throw new Error("The edited IFC snapshot is unavailable.");
      const file = new File([await source.arrayBuffer()], session.name, { type: "application/octet-stream" });
      if (stopped || !ready()) return;
      attemptedVersion = session.version;
      const result = loaded() ? await update(file) : await open(file);
      version = session.version;
      previousError = "";
      settle(session.version, null, result ?? null);
    } catch (error) {
      idle = true;
      if (attemptedVersion !== null && error.revisionRejected) {
        version = attemptedVersion;
        settle(attemptedVersion, error);
      }
      if (!stopped && error.name !== "AbortError" && error.message !== previousError) {
        previousError = error.message;
        report(error.message);
      }
    } finally {
      if (!stopped) timer = setTimeout(poll, idle ? RETRY_MS : 0);
    }
  }

  async function post(path, body) {
    if (!known?.token) throw new Error("The editing session is not connected.");
    const marker = sequence;
    const response = await fetch(`/__tessifc/${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Tessifc-Token": known.token },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.error ?? `The session returned ${response.status}.`);
    if (payload.status) {
      known = { ...payload.status, token: known.token };
      status?.(known);
    }
    payload.marker = marker;
    return payload;
  }

  /** Resolves with the viewer's update report once `target` is applied after `marker`. */
  function whenApplied(target, marker = -1) {
    const done = applied.get(target);
    if (done && done.sequence > marker) return Promise.resolve(done.result ?? null);
    return new Promise((resolve, reject) => {
      const list = waiters.get(target) ?? [];
      list.push({ resolve, reject });
      waiters.set(target, list);
    });
  }

  poll();
  return {
    stop() {
      stopped = true;
      clearTimeout(timer);
      controller.abort();
      for (const target of [...waiters.keys()]) settle(target, new Error("The session was closed."));
    },
    status: () => known,
    version: () => version,
    run: (script, selection) => post("run", { script, selection }),
    undo: () => post("undo", {}),
    redo: () => post("redo", {}),
    assistant: (request) => post("assistant", request),
    whenApplied,
  };
}
