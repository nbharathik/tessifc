// SPDX-License-Identifier: Apache-2.0

//! The client of a local editing session (the Python session server or
//! tessifc-mcp): follows completed snapshots of one IFC file and sends
//! scripts, undo and assistant requests to the session's origin.

const WAIT_SECONDS = 20;
const RETRY_MS = 1000;
const TOKEN_KEY = "tessifc-session-token";
const NO_TOKEN = "This page has no session token: open the address the session server printed.";
const REFUSED = "The session refused this page: open the address the session server printed.";

/**
 * The session token this page was opened with. A `#token=` in the address is
 * taken, removed from the address bar and kept for reloads of this tab.
 * @returns {string | null}
 */
export function pageSessionToken() {
  if (typeof location === "undefined") return null;
  const fragment = new URLSearchParams(location.hash.slice(1));
  const given = fragment.get("token") || null;
  try {
    if (fragment.has("token")) {
      fragment.delete("token");
      const rest = fragment.toString();
      history.replaceState(history.state, "", `${location.pathname}${location.search}${rest ? `#${rest}` : ""}`);
    }
    if (given) sessionStorage.setItem(TOKEN_KEY, given);
    return given ?? sessionStorage.getItem(TOKEN_KEY);
  } catch {
    // Blocked storage: the token from the address still serves this load.
    return given;
  }
}

function samePage(baseUrl) {
  if (!baseUrl) return true;
  try {
    return typeof location !== "undefined" && new URL(baseUrl, location.href).origin === location.origin;
  } catch {
    return false;
  }
}

/**
 * Follow the session; `status` receives every server status seen. A changed
 * `generation` (the host opened another model) reopens instead of updating,
 * and hosts that ask for it get the applied report and the selection.
 * `token` goes with every request; without one, a client of the page's own
 * origin uses the token the page was opened with (see `pageSessionToken`).
 * @param {{ baseUrl?: string, token?: string | null, fetch?: any, ready: any, loaded: any, open: any, update: any, report: any, status: any }} options
 */
export function createSessionClient({ baseUrl = "", token, fetch: fetchImpl = globalThis.fetch.bind(globalThis), ready, loaded, open, update, report, status }) {
  const secret = token === undefined ? (samePage(baseUrl) ? pageSessionToken() : null) : token;
  const auth = secret ? { "X-Tessifc-Token": secret } : {};
  let stopped = false;
  let version = null;
  let generation = null;
  let known = null;
  let timer;
  let previousError = "";
  let selectionTimer = null;
  let lastSelection = "";
  const controller = new AbortController();
  // Waiters for a content version the viewer has not applied yet. Versions are
  // content hashes, so undo brings an old one back: the sequence tells them apart.
  const waiters = new Map();
  const applied = new Map();
  let sequence = 0;

  async function fetchStatus(wait) {
    const query = wait && version ? `?after=${encodeURIComponent(version)}&timeout=${WAIT_SECONDS}` : "";
    const response = await fetchImpl(`${baseUrl}/__tessifc/session${query}`, { cache: "no-store", headers: auth, signal: controller.signal });
    if (response.status === 403) throw new Error(REFUSED);
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
      if (!secret) throw new Error(NO_TOKEN);
      const session = await fetchStatus(true);
      if (session.version === version) return;
      const source = await fetchImpl(`${baseUrl}/__tessifc/model.ifc?version=${encodeURIComponent(session.version)}`,
        { cache: "no-store", headers: auth, signal: controller.signal });
      if (source.status === 409) return;
      if (source.status === 403) throw new Error(REFUSED);
      if (!source.ok) throw new Error("The edited IFC snapshot is unavailable.");
      const file = new File([await source.arrayBuffer()], session.name, { type: "application/octet-stream" });
      if (stopped || !ready()) return;
      attemptedVersion = session.version;
      const reopen = !loaded() || (session.generation != null && generation != null && session.generation !== generation);
      const result = reopen ? await open(file) : await update(file);
      version = session.version;
      generation = session.generation ?? generation;
      previousError = "";
      settle(session.version, null, result ?? null);
      if (known?.capabilities?.applied) {
        post("applied", { version: session.version, revision: result?.revision ?? null, affectedProducts: result?.affectedProducts ?? [],
          removedProducts: result?.removedProducts ?? [], fullRebuild: Boolean(result?.fullRebuild) }).catch(() => {});
      }
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
    if (!secret) throw new Error(NO_TOKEN);
    if (!known) throw new Error("The editing session is not connected.");
    const marker = sequence;
    const response = await fetchImpl(`${baseUrl}/__tessifc/${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...auth },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (response.status === 204) return { marker };
    if (response.status === 403) throw new Error(REFUSED);
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.error ?? `The session returned ${response.status}.`);
    if (payload.status) {
      known = payload.status;
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

  /** Tell a host that wants it what is selected; repeated and rapid reports are coalesced. */
  function reportSelection(summary) {
    if (!known?.capabilities?.selection) return;
    const body = summary
      ? { ids: [summary.expressId], guids: summary.globalId ? [summary.globalId] : [], className: summary.className ?? null, name: summary.name ?? null }
      : { ids: [], guids: [], className: null, name: null };
    const key = JSON.stringify(body);
    if (key === lastSelection) return;
    lastSelection = key;
    clearTimeout(selectionTimer);
    selectionTimer = setTimeout(() => {
      if (!stopped) post("selection", body).catch(() => {});
    }, 150);
  }

  poll();
  return {
    stop() {
      stopped = true;
      clearTimeout(timer);
      clearTimeout(selectionTimer);
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
    reportSelection,
  };
}

/** The reference application's entry: the session at the page's own origin. */
export function startFileSession(options) {
  return createSessionClient(options);
}
