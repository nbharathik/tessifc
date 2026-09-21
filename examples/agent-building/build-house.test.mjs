// SPDX-License-Identifier: Apache-2.0
// The demo house through the model host: every step's affected products, the
// final counts, verification, a reopen of the saved file, and IFC2X3.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createModelHost } from "../../bindings/mcp/src/session-host.js";
import { MODEL, STEPS, EXPECTED, EXPECTED_TOTAL } from "./steps.mjs";

const root = resolve(fileURLToPath(new URL("../../", import.meta.url)));
const kernelModule = resolve(root, "bindings/wasm/pkg-node/tessifc_wasm.js");
if (!existsSync(kernelModule)) {
  console.log("skip  build the Node package first (python scripts/build-wasm.py --target both)");
  process.exit(0);
}
const { Kernel } = createRequire(import.meta.url)(kernelModule);
let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

const dir = mkdtempSync(join(tmpdir(), "tessifc-house-"));
try {
  for (const schema of ["IFC4", "IFC2X3"]) {
    const file = join(dir, `house-${schema}.ifc`);
    const host = createModelHost({ Kernel, log: () => {} });
    await host.newModel({ ...MODEL, schema }, { path: file });
    for (const [index, step] of STEPS.entries()) {
      const { report, delta } = await host.run(step.script, null, { label: step.title });
      assert.equal(report.ok, true, `${schema} step ${index + 1} ${step.title}: ${report.error}\n${report.traceback ?? ""}`);
      assert.ok(delta, `${schema} step ${index + 1} changed the model`);
      assert.equal(delta.affectedProducts.length, step.affected, `${schema} step ${index + 1} ${step.title}: affected products`);
      assert.equal(delta.fullRebuild, false, `${schema} step ${index + 1} is selective`);
      assert.equal(delta.revision, String(index + 1));
    }
    ok(true, `${schema}: every step publishes the expected affected products`);
    const info = host.session.info();
    for (const [name, count] of Object.entries(EXPECTED)) assert.equal(info.products?.[name] ?? 0, count, `${schema}: ${name}`);
    const verified = host.verify();
    ok(verified.ok && verified.products === EXPECTED_TOTAL, `${schema}: the mirror matches a fresh evaluation of ${EXPECTED_TOTAL} products (${JSON.stringify(verified.mismatches)})`);
    const reopened = createModelHost({ Kernel, save: false, log: () => {} });
    const opened = await reopened.openFile(file);
    ok(opened.products === EXPECTED_TOTAL && opened.info.products.IfcWindow === EXPECTED.IfcWindow, `${schema}: the saved file reopens with every product`);
    const psets = reopened.session.idsOfType("IfcPropertySet").length;
    ok(psets === 15, `${schema}: the property sets survive the save (${psets})`);
    reopened.dispose();
    host.dispose();
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}
console.log(`PASS ${passed} checks`);
