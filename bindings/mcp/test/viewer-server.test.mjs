// SPDX-License-Identifier: Apache-2.0
// The loopback viewer server over a model host: status and token, Host and
// Origin checks, snapshots by version, the long poll, scripts, undo, the
// selection and applied reports, and static file containment. Node only.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
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
const get = (path, headers = {}) => fetch(`${base}${path}`, { headers: { host: hostHeader, ...headers } });
const post = (path, body, headers = {}) => fetch(`${base}${path}`, {
  method: "POST", headers: { host: hostHeader, origin, "content-type": "application/json", "x-tessifc-token": server.token, ...headers }, body: JSON.stringify(body),
});
try {
  const status = await (await get("/__tessifc/session")).json();
  ok(status.token === server.token && status.revision === "0" && status.capabilities.authoring === "javascript" && status.capabilities.selection === true
    && status.examples.some((example) => example.title === "Build a small house"), "the status carries the token, the revision, the JavaScript capability and the examples");
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
  ok((await rawStatus({ host: "evil.example:1" })) === 403 && (await rawStatus({ host: hostHeader })) === 200, "a foreign Host is refused");
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

  ok((await get("/")).status === 302 || (await get("/")).redirected, "the root redirects to the viewer");
  ok((await get("/viewer/index.html")).status === 200 && (await get("/bindings/edit/src/session.js")).status === 200, "the viewer and the packages are served");
  ok((await get("/bindings/mcp/src/cli.js")).status === 404 && (await get("/viewer/node_modules/playwright/package.json")).status === 404 && (await get("/../Cargo.toml")).status === 404,
    "files outside the static roots, node_modules and traversals are refused");
} finally {
  await server.close();
  host.dispose();
  rmSync(dir, { recursive: true, force: true });
}
console.log(`PASS ${passed} checks`);
