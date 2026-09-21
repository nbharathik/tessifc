#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// Generate the TypeScript declarations of the JavaScript packages from their
// JSDoc, then check that every export entry of each package names a
// declaration file that exists. `--check` only type-checks and verifies.
//
//   node scripts/build-types.mjs              every package
//   node scripts/build-types.mjs bindings/edit  one package
//   node scripts/build-types.mjs --check      verify without writing

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PACKAGES = ["bindings/edit", "bindings/mcp", "bindings/viewer", "adapters/three"];

const args = process.argv.slice(2);
const check = args.includes("--check");
const targets = args.filter((item) => !item.startsWith("--")).map((item) => resolve(item));
const packages = targets.length ? targets : PACKAGES.map((item) => join(root, item));

const kernelTypes = join(root, "bindings/wasm/pkg/tessifc_wasm.d.ts");
if (!existsSync(kernelTypes)) {
  console.error("The kernel declarations are missing; build the WASM package first: python scripts/build-wasm.py --target both");
  process.exit(1);
}
const tsc = join(root, "node_modules/typescript/bin/tsc");
if (!existsSync(tsc)) {
  console.error("TypeScript is not installed; run npm ci at the repository root.");
  process.exit(1);
}

/** Every `types` file an exports map names, whatever its nesting. */
function typeFiles(entry, found = []) {
  if (!entry || typeof entry !== "object") return found;
  for (const [condition, value] of Object.entries(entry)) {
    if (condition === "types" && typeof value === "string") found.push(value);
    else typeFiles(value, found);
  }
  return found;
}

let failed = false;
for (const dir of packages) {
  const manifest = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  const result = spawnSync(process.execPath, [tsc, "-p", join(dir, "tsconfig.json"), ...(check ? ["--noEmit"] : [])], { stdio: "inherit" });
  if (result.status !== 0) {
    console.error(`FAIL  ${manifest.name}: tsc exited with ${result.status}`);
    failed = true;
    continue;
  }
  const expected = [manifest.types, ...typeFiles(manifest.exports)].filter(Boolean);
  const missing = expected.filter((file) => !existsSync(join(dir, file)));
  // A JSON export such as a contract needs no declaration.
  const untyped = Object.entries(manifest.exports ?? {})
    .filter(([, value]) => (typeof value === "string" ? value.endsWith(".js") : !typeFiles(value).length))
    .map(([key]) => key);
  if (missing.length || untyped.length) {
    for (const file of missing) console.error(`FAIL  ${manifest.name}: ${file} is missing`);
    for (const key of untyped) console.error(`FAIL  ${manifest.name}: export "${key}" names no types file`);
    failed = true;
    continue;
  }
  if (!manifest.files?.includes("types/")) {
    console.error(`FAIL  ${manifest.name}: "files" does not ship types/`);
    failed = true;
    continue;
  }
  console.log(`ok    ${manifest.name}: ${expected.length} declaration files ${check ? "verified" : "generated"} in ${relative(root, dir)}`);
}
process.exit(failed ? 1 : 0);
