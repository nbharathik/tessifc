// SPDX-License-Identifier: Apache-2.0
import { createServer } from "node:http";
import { createReadStream, statSync, realpathSync } from "node:fs";
import { resolve, sep, extname } from "node:path";
import { fileURLToPath } from "node:url";

/** Serve the checkout on a loopback-only, ephemeral port for browser tests. */
export async function serveViewer() {
  const root = realpathSync(fileURLToPath(new URL("../../", import.meta.url)));
  const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" };
  const server = createServer((request, response) => {
    try {
      const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
      let path = resolve(root, "." + pathname);
      if (statSync(path).isDirectory()) path = resolve(path, "index.html");
      path = realpathSync(path);
      if (!path.startsWith(root + sep) || !statSync(path).isFile()) throw new Error("outside server root");
      response.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
      createReadStream(path).on("error", () => response.destroy()).pipe(response);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((done) => server.close(done)) };
}

/** Expose module state only in browser tests, without shipping a console API. */
export async function instrumentViewer(page) {
  await page.route("**/viewer/src/main.js", async (route) => {
    const response = await route.fetch();
    const source = await response.text();
    await route.fulfill({ response, body: source + `\nwindow.__tessifc = {
      renderer, state, scheduleRender, tree, tools, inspector, shell,
      selectRecord, selectExpressId, receiveEntityInfo, receiveEntityError, receiveRevision, runPanelWork, panelWork,
      runBrowserScript, browserHistoryAction, updateFromFile,
      render: () => renderer.render(true),
      pack: () => state.model?.pack ?? null,
      ready: () => Boolean(state.model),
      loadStatus: () => ({ state: state.loadOutcome, finished: !state.converting && state.loadOutcome !== "loading" }),
      streaming: () => Boolean(state.stream?.assembler),
      overlayState: () => state.model?.overlayAnalysis?.state ?? null,
      overlaySettled: () => Boolean(state.model?.overlayAnalysis) && state.model.overlayAnalysis.state !== "pending",
    };\n` });
  });
}
