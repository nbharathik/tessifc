// SPDX-License-Identifier: Apache-2.0

//! Build the demo house step by step with the editing session, printing the
//! kernel's report after every step; `--serve` lets the viewer follow along.
//!
//!   node examples/agent-building/build-house.mjs [house.ifc] [--serve] [--port 8000] [--schema IFC4] [--pause 1500]

import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { createModelHost } from "../../bindings/mcp/src/session-host.js";
import { createViewerServer } from "../../bindings/mcp/src/viewer-server.js";
import { MODEL, STEPS, EXPECTED, EXPECTED_TOTAL } from "./steps.mjs";

const root = resolve(fileURLToPath(new URL("../../", import.meta.url)));
const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    serve: { type: "boolean", default: false },
    port: { type: "string", default: "8000" },
    schema: { type: "string", default: MODEL.schema },
    pause: { type: "string", default: "1500" },
  },
});
const kernelModule = resolve(root, "bindings/wasm/pkg-node/tessifc_wasm.js");
if (!existsSync(kernelModule)) {
  console.error("Build the Node kernel first: python scripts/build-wasm.py --target both");
  process.exit(1);
}
const { Kernel } = createRequire(import.meta.url)(kernelModule);
const file = resolve(positionals[0] ?? "house.ifc");
const pause = Number(values.pause) || 0;

const host = createModelHost({ Kernel, log: () => {} });
await host.newModel({ ...MODEL, schema: values.schema }, { path: file, force: true });
console.log(`New ${values.schema} model with ${MODEL.storeys.length} storeys, saved to ${file}`);

let viewer = null;
if (values.serve) {
  viewer = createViewerServer(host, { root, port: Number(values.port) || 0 });
  await viewer.listen();
  console.log(`Open ${viewer.viewerUrl} and watch; the build starts in 5 seconds`);
  await new Promise((done) => setTimeout(done, 5000));
}

for (const [index, step] of STEPS.entries()) {
  const { report, delta } = await host.run(step.script, null, { label: step.title });
  if (!report.ok) {
    console.error(`${index + 1}. ${step.title}: ${report.error}\n${report.traceback ?? ""}`);
    process.exit(1);
  }
  const summary = delta
    ? `revision ${delta.revision}, ${delta.affectedProducts.length} products rebuilt, ${delta.removedProducts.length} removed, ` +
      `${delta.metadataProducts.length - delta.affectedProducts.length} metadata only, ${report.operations.created} records created`
    : "no change";
  console.log(`${index + 1}. ${step.title}: ${report.stdout.trim()} (${summary})`);
  if (viewer && pause) await new Promise((done) => setTimeout(done, pause));
}

const verified = host.verify();
const info = host.session.info();
const counts = Object.fromEntries(Object.keys(EXPECTED).map((name) => [name, info.products?.[name] ?? 0]));
const complete = Object.entries(EXPECTED).every(([name, count]) => counts[name] === count);
console.log(`Verification: ${verified.ok ? "the scene matches a fresh evaluation" : `${verified.mismatches.length} mismatches`}; ` +
  `${verified.products} placed products (expected ${EXPECTED_TOTAL}); ${complete ? "all counts as expected" : `counts ${JSON.stringify(counts)}`}`);
console.log(`Saved ${file} at revision ${host.session.revision}`);

if (viewer) {
  console.log("The viewer keeps following; press Ctrl+C to stop.");
  await new Promise((done) => process.once("SIGINT", done));
  await viewer.close();
}
host.dispose();
process.exit(verified.ok && complete ? 0 : 1);
