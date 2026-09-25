// SPDX-License-Identifier: Apache-2.0

//! A loopback HTTP server the viewer follows: the snapshot and its content
//! version, a long poll, scripts, undo and redo, the page's reports and the
//! checkout's static files. The same routes as the Python session server.

import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer } from "node:http";
import { realpath, readFile, stat } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { SessionBusy } from "./session-host.js";

const MAX_BODY_BYTES = 4 << 20;
const MAX_WAIT_SECONDS = 25;
const STATIC_SUFFIXES = new Set([".html", ".js", ".mjs", ".css", ".wasm", ".svg"]);
const MIME = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".svg": "image/svg+xml" };
const DEFAULT_ROOTS = ["viewer", "bindings/wasm/pkg", "bindings/edit/src", "bindings/viewer/src", "examples"];

function json(response, payload, status = 200) {
  const body = Buffer.from(JSON.stringify(payload));
  response.writeHead(status, {
    "content-type": "application/json",
    "content-length": body.length,
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  response.end(body);
}

function bytesOut(response, payload, mime, status = 200) {
  response.writeHead(status, { "content-type": mime, "content-length": payload.length, "cache-control": "no-store", "x-content-type-options": "nosniff" });
  response.end(payload);
}

function readJson(request) {
  return new Promise((done, fail) => {
    const length = Number(request.headers["content-length"] ?? 0);
    if (!Number.isFinite(length) || length <= 0 || length > MAX_BODY_BYTES) {
      fail(new Error("The request body is empty or too large."));
      request.resume();
      return;
    }
    const chunks = [];
    let total = 0;
    request.on("data", (chunk) => {
      total += chunk.length;
      if (total > MAX_BODY_BYTES) {
        fail(new Error("The request body is too large."));
        request.destroy();
        return;
      }
      chunks.push(chunk);
    });
    request.on("end", () => {
      try {
        const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        if (!body || typeof body !== "object" || Array.isArray(body)) throw new Error("object");
        done(body);
      } catch {
        fail(new Error("The request body must be a UTF-8 JSON object."));
      }
    });
    request.on("error", fail);
  });
}

/** The Python result record for a host run, plus the kernel's impact. */
function resultRecord(outcome) {
  const { report, delta } = outcome;
  return {
    ok: report.ok,
    label: report.label ?? "script",
    changed: Boolean(delta),
    version: outcome.version,
    revision: outcome.revision,
    stdout: report.stdout ?? "",
    operations: report.operations ?? { created: 0, modified: 0, deleted: 0 },
    elapsedMs: Math.round((report.elapsedMs ?? 0) * 10) / 10,
    ...(report.error ? { error: report.error, traceback: report.traceback ?? "" } : {}),
    impact: delta ? {
      revision: delta.revision, affectedProducts: delta.affectedProducts, removedProducts: delta.removedProducts,
      metadataProducts: delta.metadataProducts, fullRebuild: delta.fullRebuild, kind: delta.kind,
    } : null,
    saved: outcome.saved,
  };
}

/** @typedef {ReturnType<typeof createViewerServer>} ViewerServer */

/**
 * Create the server over a model host. `root` is the checkout that holds the
 * viewer and the WASM package; `port` 0 picks a free one. Call `listen()`.
 * Every `/__tessifc/` route needs the `x-tessifc-token` header; `viewerUrl`
 * is the address to open, with the token in its fragment.
 * @param {import("./session-host.js").ModelHost} host
 * @param {{ root: string, port?: number, staticRoots?: string[], log?: (line: string) => void }} options
 */
export function createViewerServer(host, { root, port = 8000, staticRoots = DEFAULT_ROOTS, log = () => {} }) {
  if (!root) throw new Error("createViewerServer needs the checkout root.");
  const token = randomBytes(18).toString("base64url");
  const expected = Buffer.from(token);
  const checkout = resolve(root);
  const allowedRoots = staticRoots.map((name) => resolve(checkout, name));
  // Compared with real paths, so a different drive-letter case still matches on Windows.
  let realRoots = null;
  let server = null;
  let boundPort = port;

  function hosts() {
    return [`127.0.0.1:${boundPort}`, `localhost:${boundPort}`];
  }

  function originOk(request, requireOrigin) {
    const allowed = hosts();
    if (!allowed.includes(request.headers.host ?? "")) return false;
    const origin = request.headers.origin;
    if (origin === undefined) return !requireOrigin;
    return allowed.some((name) => origin === `http://${name}`);
  }

  function tokenOk(request) {
    const given = Buffer.from(String(request.headers["x-tessifc-token"] ?? ""));
    return given.length === expected.length && timingSafeEqual(given, expected);
  }

  async function serveStatic(pathname, response) {
    try {
      const relative = decodeURIComponent(pathname).replace(/^\/+/, "");
      let file = await realpath(resolve(checkout, relative));
      realRoots ??= await Promise.all(allowedRoots.map((base) => realpath(base).catch(() => base)));
      const inside = (target) => realRoots.some((base) => target === base || target.startsWith(base + sep));
      if (!inside(file) || file.split(sep).includes("node_modules")) throw new Error("outside");
      if ((await stat(file)).isDirectory()) file = await realpath(resolve(file, "index.html"));
      if (!STATIC_SUFFIXES.has(extname(file)) || !inside(file)) throw new Error("suffix");
      bytesOut(response, await readFile(file), MIME[extname(file)] ?? "application/octet-stream");
    } catch {
      response.writeHead(404).end();
    }
  }

  async function handleGet(request, response, url) {
    if (url.pathname.startsWith("/__tessifc/") && !tokenOk(request)) {
      response.writeHead(403).end("Missing session token");
      return;
    }
    if (url.pathname === "/__tessifc/session") {
      const after = url.searchParams.get("after");
      if (after !== null) {
        const timeout = Math.min(Number(url.searchParams.get("timeout") ?? 20) || 0, MAX_WAIT_SECONDS);
        await host.waitForChange(after, timeout * 1000);
      }
      json(response, host.describe());
      return;
    }
    if (url.pathname === "/__tessifc/model.ifc") {
      if (!host.session) {
        response.writeHead(503).end("No model is open");
        return;
      }
      const snapshot = host.snapshot();
      if (url.searchParams.get("version") !== snapshot.version) {
        response.writeHead(409).end("The IFC snapshot changed; request its current version");
        return;
      }
      bytesOut(response, Buffer.from(snapshot.bytes), "application/octet-stream");
      return;
    }
    if (url.pathname === "/") {
      response.writeHead(302, { location: "/viewer/?session=file", "content-length": 0 });
      response.end();
      return;
    }
    await serveStatic(url.pathname, response);
  }

  async function handlePost(request, response, url) {
    if (!tokenOk(request)) {
      response.writeHead(403).end("Missing session token");
      return;
    }
    let body;
    try {
      body = await readJson(request);
    } catch (error) {
      json(response, { error: error.message }, 400);
      return;
    }
    try {
      /** @type {Record<string, any>} */
      let result;
      if (url.pathname === "/__tessifc/run") {
        result = resultRecord(await host.run(String(body.script ?? ""), body.selection ?? null));
      } else if (url.pathname === "/__tessifc/undo") {
        result = resultRecord(await host.undo());
      } else if (url.pathname === "/__tessifc/redo") {
        result = resultRecord(await host.redo());
      } else if (url.pathname === "/__tessifc/selection") {
        host.selection = body.ids?.length || body.guids?.length ? { ids: body.ids ?? [], guids: body.guids ?? [], className: body.className ?? null, name: body.name ?? null } : null;
        response.writeHead(204).end();
        return;
      } else if (url.pathname === "/__tessifc/applied") {
        host.applied = { version: body.version ?? null, revision: body.revision ?? null, affectedProducts: body.affectedProducts ?? [],
          removedProducts: body.removedProducts ?? [], fullRebuild: Boolean(body.fullRebuild) };
        response.writeHead(204).end();
        return;
      } else if (url.pathname === "/__tessifc/assistant") {
        json(response, { error: "This session has no assistant; it is driven by an MCP client." }, 404);
        return;
      } else {
        response.writeHead(404).end();
        return;
      }
      result.status = host.describe();
      json(response, result);
    } catch (error) {
      if (error instanceof SessionBusy) json(response, { error: error.message }, 409);
      else if (error?.code === "ENOENT" || error?.code === "EACCES" || error?.code === "EPERM") json(response, { error: `The IFC file could not be read or written: ${error.message}` }, 503);
      else json(response, { error: error?.message ?? String(error), impact: error?.impact ?? null }, 400);
    }
  }

  server = createServer((request, response) => {
    const url = new URL(request.url ?? "/", "http://localhost");
    const post = request.method === "POST";
    if (!originOk(request, post)) {
      response.writeHead(403).end();
      request.resume();
      return;
    }
    const work = post ? handlePost(request, response, url) : request.method === "GET" ? handleGet(request, response, url) : Promise.resolve(response.writeHead(405).end());
    work.catch((error) => {
      log(`request failed: ${error?.message ?? error}`);
      if (!response.headersSent) response.writeHead(500).end();
    });
  });

  return {
    token,
    /**
     * Listen on `listenPort` (the constructor's port by default) and resolve with the base URL.
     * A failed attempt, such as a port in use, rejects and may be retried with another port.
     * @param {number} [listenPort]
     */
    listen(listenPort = port) {
      return new Promise((done, fail) => {
        const failed = (error) => {
          server.off("listening", listening);
          fail(error);
        };
        const listening = () => {
          server.off("error", failed);
          boundPort = /** @type {import("node:net").AddressInfo} */ (server.address()).port;
          done(this.url);
        };
        server.once("error", failed);
        server.once("listening", listening);
        server.listen(listenPort, "127.0.0.1");
      });
    },
    get port() {
      return boundPort || null;
    },
    get url() {
      return boundPort ? `http://127.0.0.1:${boundPort}` : null;
    },
    /** The address to open: the viewer following this session, with the token in the fragment. */
    get viewerUrl() {
      return boundPort ? `http://127.0.0.1:${boundPort}/viewer/?session=file#token=${token}` : null;
    },
    /** Stop listening and drop the open long polls, so the promise settles. */
    close() {
      return new Promise((done) => {
        server.close(() => done());
        server.closeAllConnections?.();
      });
    },
  };
}
