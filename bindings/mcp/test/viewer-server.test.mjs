// SPDX-License-Identifier: Apache-2.0
// The loopback viewer server over a model host: status, token and origin checks,
// snapshots, the long poll, scripts, undo, the reports, static file containment,
// the viewer's session client, a port in use, and the host's guards against
// overwriting files or edits.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { request as httpRequest } from "node:http";
import { createModelHost } from "../src/session-host.js";
import { createViewerServer } from "../src/viewer-server.js";

let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

const root = fileURLToPath(new URL("../../../", import.meta.url));
const pkg = resolve(root, "bindings/wasm/pkg-node/tessifc_wasm.js");
if (!existsSync(pkg)) {
  console.log("skip  build the Node package first (python scripts/build-wasm.py --target both)");
  process.exit(0);
}
const { Kernel } = createRequire(import.meta.url)(pkg);
const dir = mkdtempSync(join(tmpdir(), "tessifc-mcp-"));
const file = join(dir, "house.ifc");
const host = createModelHost({ Kernel, version: "test" });
await host.newModel({ name: "House", storeys: [{ name: "Ground floor", elevation: 0 }] }, { path: file });
ok(existsSync(file) && host.session.revision === "0" && host.generation === 1, "a new model is saved to its file at revision 0");

const server = createViewerServer(host, { root, port: 0 });
const base = await server.listen();
const hostHeader = new URL(base).host;
const origin = `http://${hostHeader}`;
const get = (path, headers = {}) => fetch(`${base}${path}`, { headers: { host: hostHeader, "x-tessifc-token": server.token, ...headers } });
const bare = (path, headers = {}) => fetch(`${base}${path}`, { headers: { host: hostHeader, ...headers }, redirect: "manual" });
const post = (path, body, headers = {}) => fetch(`${base}${path}`, {
  method: "POST", headers: { host: hostHeader, origin, "content-type": "application/json", "x-tessifc-token": server.token, ...headers }, body: JSON.stringify(body),
});
try {
  const status = await (await get("/__tessifc/session")).json();
  ok(!("token" in status) && !JSON.stringify(status).includes(server.token) && status.revision === "0" && status.capabilities.authoring === "javascript"
    && status.capabilities.selection === true && status.examples.some((example) => example.title === "Build a small house"),
  "the status carries the revision, the JavaScript capability and the examples, never the token");
  ok(server.viewerUrl === `${base}/viewer/?session=file#token=${server.token}`, "the address to open carries the token in its fragment");
  ok((await bare("/__tessifc/session")).status === 403 && (await bare(`/__tessifc/model.ifc?version=${status.version}`)).status === 403
    && (await bare("/__tessifc/session", { "x-tessifc-token": `${server.token}x` })).status === 403 && (await bare("/__tessifc/session", { "x-tessifc-token": "" })).status === 403,
  "the status and the snapshot need the token");
  const root302 = await bare("/");
  ok(root302.status === 302 && root302.headers.get("location") === "/viewer/?session=file", "the root redirects to the viewer without the token");
  // fetch drops a custom Host header, so the check goes through node:http.
  const rawStatus = (headers) => new Promise((done, fail) => {
    const url = new URL(`${base}/__tessifc/session`);
    const req = httpRequest({ host: url.hostname, port: url.port, path: url.pathname, headers }, (response) => {
      response.resume();
      done(response.statusCode);
    });
    req.on("error", fail);
    req.end();
  });
  ok((await rawStatus({ host: "evil.example:1", "x-tessifc-token": server.token })) === 403 && (await rawStatus({ host: hostHeader, "x-tessifc-token": server.token })) === 200,
    "a foreign Host is refused");
  ok((await get("/__tessifc/session", { origin: "http://evil.example" })).status === 403, "a foreign Origin is refused");
  ok((await fetch(`${base}/__tessifc/run`, { method: "POST", headers: { host: hostHeader, origin, "content-type": "application/json" }, body: "{}" })).status === 403,
    "a POST without the token is refused");
  ok((await fetch(`${base}/__tessifc/run`, { method: "POST", headers: { host: hostHeader, "content-type": "application/json", "x-tessifc-token": server.token }, body: "{}" })).status === 403,
    "a POST without an Origin is refused");

  const snapshot = await get(`/__tessifc/model.ifc?version=${status.version}`);
  ok(snapshot.status === 200 && (await snapshot.arrayBuffer()).byteLength === readFileSync(file).length, "the snapshot of the current version is served");
  ok((await get("/__tessifc/model.ifc?version=stale")).status === 409, "a stale version answers 409");

  const waiting = get(`/__tessifc/session?after=${status.version}&timeout=5`);
  const run = await (await post("/__tessifc/run", { script: 'const w = ifc.addWall({ from: [0, 0], to: [4, 0], height: 3 }); print("wall", w.id);' })).json();
  ok(run.ok && run.changed && run.revision === "1" && run.impact.affectedProducts.length === 1 && run.saved === true && run.status.undo === 1,
    "a script runs, publishes revision 1 with one affected product and saves the file");
  const woken = await (await waiting).json();
  ok(woken.version === run.version && woken.version !== status.version, "the long poll wakes with the new version");
  ok(readFileSync(file, "latin1").includes("IFCWALL"), "the saved file carries the wall");

  const failed = await post("/__tessifc/run", { script: "nope()" });
  const failure = await failed.json();
  ok(failed.status === 200 && failure.ok === false && /ReferenceError/.test(failure.error) && failure.revision === "1", "a failing script reports its error and publishes nothing");
  const rejected = await post("/__tessifc/run", { script: 'ifc.byType("IfcWall")[0].Representation.Representations[0].Items[0].SweptArea = null;' });
  ok(rejected.status === 400 && /rejected/.test((await rejected.json()).error), "a candidate the kernel refuses answers 400 with the reason");
  const undone = await (await post("/__tessifc/undo", {})).json();
  ok(undone.ok && undone.label === "undo" && undone.revision === "2" && undone.status.redo === 1, "undo publishes revision 2");
  const redone = await (await post("/__tessifc/redo", {})).json();
  ok(redone.revision === "3" && redone.status.redo === 0, "redo publishes revision 3");
  ok((await (await post("/__tessifc/undo", {}, {})).json()).revision === "4", "undo again");
  const empty = await post("/__tessifc/redo", {});
  await empty.json();
  ok((await (await post("/__tessifc/redo", {})).json()).error?.includes("Nothing to redo"), "nothing to redo answers with an error");

  ok((await post("/__tessifc/selection", { ids: [12], guids: ["abc"], className: "IfcWall", name: "W" })).status === 204 && host.selection.ids[0] === 12 && host.selection.reportedAt,
    "the page's selection is recorded");
  ok((await post("/__tessifc/applied", { version: host.version, revision: "5", affectedProducts: [12], removedProducts: [], fullRebuild: false })).status === 204
    && host.applied.revision === "5", "the page's applied report is recorded");
  ok((await post("/__tessifc/assistant", { prompt: "x" })).status === 404, "the assistant route says there is none");
  ok((await post("/__tessifc/run", "not json", {})).status === 400, "a non-object body answers 400");

  ok((await bare("/viewer/index.html")).status === 200 && (await bare("/bindings/edit/src/session.js")).status === 200, "the viewer and the packages are served without the token");
  ok((await get("/bindings/mcp/src/cli.js")).status === 404 && (await get("/viewer/node_modules/playwright/package.json")).status === 404 && (await get("/../Cargo.toml")).status === 404,
    "files outside the static roots, node_modules and traversals are refused");

  // The viewer's session client against this server, on a simulated page opened at the printed address.
  const { createSessionClient, pageSessionToken } = await import("../../viewer/src/session-client.js");
  const stored = new Map();
  let replaced = null;
  const page = {
    location: { hash: `#token=${server.token}&view=top`, pathname: "/viewer/", search: "?session=file", href: server.viewerUrl, origin: base },
    history: { state: null, replaceState: (_state, _title, url) => { replaced = url; } },
    sessionStorage: { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, String(value)) },
  };
  for (const [name, value] of Object.entries(page)) Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  try {
    ok(pageSessionToken() === server.token && replaced === "/viewer/?session=file#view=top" && stored.get("tessifc-session-token") === server.token,
      "the page takes the token from the fragment, removes it from the address and keeps it for the tab");
    page.location.hash = "";
    ok(pageSessionToken() === server.token, "a reload of the tab keeps the token");
    // A browser sends the page's origin with every POST; Node's fetch does not, so the wrapper adds it.
    let requests = 0;
    const browserFetch = (url, init = {}) => {
      requests += 1;
      return fetch(new URL(url, base), { ...init, headers: { ...init.headers, origin } });
    };
    const follow = async (token) => {
      const seen = { opened: 0, errors: [] };
      const client = createSessionClient({ baseUrl: "", ...(token === undefined ? {} : { token }), fetch: browserFetch, ready: () => true,
        loaded: () => seen.opened > 0, open: async () => { seen.opened += 1; return { revision: host.session.revision }; }, update: async () => ({}),
        report: (message) => seen.errors.push(message), status: () => {} });
      for (let attempt = 0; attempt < 100 && !seen.opened && !seen.errors.length; attempt += 1) await new Promise((done) => setTimeout(done, 50));
      return { client, seen };
    };
    const followed = await follow(undefined);
    const ran = await followed.client.run('print("from the page")', null);
    ok(followed.seen.opened === 1 && !("token" in followed.client.status()) && ran.ok && ran.stdout.includes("from the page"),
      "the client uses the page's token to follow the host and run a script");
    followed.client.stop();
    const wrong = await follow("not-the-token");
    ok(wrong.seen.opened === 0 && /refused this page/.test(wrong.seen.errors[0] ?? ""), "a wrong token is refused and the page says to open the printed address");
    wrong.client.stop();
    const before = requests;
    const none = await follow(null);
    ok(none.seen.opened === 0 && /no session token/.test(none.seen.errors[0] ?? "") && requests === before,
      "without a token the client sends nothing and says to open the printed address");
    none.client.stop();
  } finally {
    for (const name of Object.keys(page)) delete globalThis[name];
  }

  // A port in use rejects the listen, and the same server can then take a free port.
  const second = createViewerServer(host, { root, port: server.port });
  let code = null;
  await second.listen().catch((error) => { code = error.code; });
  const fallback = await second.listen(0);
  ok(code === "EADDRINUSE" && second.port !== server.port && (await fetch(`${fallback}/__tessifc/session`, { headers: { "x-tessifc-token": second.token } })).status === 200,
    "a port in use rejects with EADDRINUSE and a retry on port 0 serves");
  await second.close();
} finally {
  await server.close();
  host.dispose();
  rmSync(dir, { recursive: true, force: true });
}

// The host refuses to overwrite files or drop unsaved edits without force.
const scratch = mkdtempSync(join(tmpdir(), "tessifc-mcp-host-"));
const guarded = createModelHost({ Kernel, version: "test" });
try {
  const existing = join(scratch, "existing.ifc");
  writeFileSync(existing, "keep me");
  const refused = async (work) => work.then(() => null, (error) => error.message);
  ok(/\.ifc/.test(await refused(guarded.newModel({}, { path: join(scratch, "profile.txt") }))) && !existsSync(join(scratch, "profile.txt")),
    "a new model refuses a path without .ifc");
  ok(/exists/.test(await refused(guarded.newModel({}, { path: existing }))) && readFileSync(existing, "utf8") === "keep me", "a new model refuses an existing file");
  await guarded.newModel({ name: "Scratch" }, { path: existing, force: true });
  ok(readFileSync(existing, "latin1").includes("IFCPROJECT"), "force overwrites it");
  await guarded.newModel({ name: "In memory" });
  await guarded.run('ifc.addWall({ from: [0, 0], to: [3, 0], height: 3 });');
  ok(guarded.describe().saved === false && guarded.session.revision === "1", "an edit to a model in memory is unsaved");
  ok(/not saved/.test(await refused(guarded.openFile(existing))) && guarded.session.revision === "1", "opening a file refuses to drop unsaved edits");
  ok(/not saved/.test(await refused(guarded.newModel({}, { path: join(scratch, "fresh.ifc") }))) && guarded.session.revision === "1" && !existsSync(join(scratch, "fresh.ifc")),
    "a new model refuses to drop unsaved edits");
  ok((await guarded.openFile(existing, { force: true })).generation === 3, "force drops them");
} finally {
  guarded.dispose();
  rmSync(scratch, { recursive: true, force: true });
}
console.log(`PASS ${passed} checks`);
