// SPDX-License-Identifier: Apache-2.0
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const root = new URL("./", import.meta.url);
const read = (name) => readFileSync(new URL(name, root));
const manifest = JSON.parse(read("package.json"));
const names = ["package.json", "tessifc_wasm.js", "tessifc_wasm.d.ts", "tessifc_wasm_bg.wasm", "tessifc_wasm_bg.wasm.d.ts"];
for (const [directory, type] of [["pkg", "module"], ["pkg-node", "commonjs"]]) {
  assert.deepEqual(readdirSync(new URL(directory, root)).sort(), names.slice().sort(), `${directory}: unexpected or missing generated files`);
  for (const name of names) assert(statSync(new URL(`${directory}/${name}`, root)).size > 0, `${directory}/${name} is empty`);
  assert.equal(JSON.parse(read(`${directory}/package.json`)).type, type);
  assert(WebAssembly.validate(read(`${directory}/tessifc_wasm_bg.wasm`)), `${directory}: invalid WebAssembly`);
}
const hash = (name) => createHash("sha256").update(read(name)).digest("hex");
assert.equal(hash("pkg/tessifc_wasm_bg.wasm"), hash("pkg-node/tessifc_wasm_bg.wasm"), "browser and Node artifacts must come from the same kernel build");
for (const name of ["LICENSE", "NOTICE"]) {
  assert(read(name).equals(readFileSync(new URL(`../../${name}`, root))), `${name} differs from repository licensing`);
}
const require = createRequire(import.meta.url);
assert.equal(require(fileURLToPath(new URL("pkg-node/tessifc_wasm.js", root))).version(), manifest.version, "WASM version differs from package.json; rebuild before packing");
console.error(`PASS  @tessifc/core ${manifest.version}: web, Node, declarations and licensing`);
