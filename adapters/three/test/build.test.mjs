// SPDX-License-Identifier: Apache-2.0
/**
 * Tests for the batching: draw-call grouping, express-id recovery, index types
 * and transparent shapes. Not rendering.
 *
 * The first half drives buildBatches from a stub kernel, so the batch limits
 * and the wide-index path are exercised without a model. The second half runs
 * the real kernel over an embedded IFC fragment, or over TESSIFC_TEST_MODEL
 * when one is given.
 */

import { existsSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { BATCH_VERTEX_LIMIT, buildBatches, frame } from "../src/build.js";
import { disposeModel, frameCamera } from "../src/index.js";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..", "..", "..");
const pkg = resolve(repo, "bindings", "wasm", "pkg-node");
const requested = process.env.TESSIFC_TEST_MODEL
  ? resolve(repo, process.env.TESSIFC_TEST_MODEL)
  : null;
const haveModel = Boolean(requested) && existsSync(requested) && statSync(requested).isFile();

let failures = 0;
function check(condition, message) {
  if (condition) {
    console.log(`ok    ${message}`);
  } else {
    console.error(`FAIL  ${message}`);
    failures += 1;
  }
}

// ------------------------------------------------------------- stub kernel

/**
 * A kernel-shaped object over synthetic parts. buildBatches is duck-typed, so
 * this reaches the limits a small model never would.
 * @param parts one `{ expressId, class, color, vertices }` per coloured part
 */
function stubKernel(parts) {
  const positions = parts.map((part) => new Float32Array(part.vertices * 3));
  const indices = parts.map((part) => {
    const triangles = Math.max(1, Math.floor(part.vertices / 3));
    const array = new Uint32Array(triangles * 3);
    for (let i = 0; i < array.length; i += 1) array[i] = i % part.vertices;
    return array;
  });
  for (const [index, part] of parts.entries()) {
    for (let vertex = 0; vertex < part.vertices; vertex += 1) {
      positions[index][vertex * 3] = vertex;
      positions[index][vertex * 3 + 1] = index;
      positions[index][vertex * 3 + 2] = 0;
    }
  }
  return {
    shapeCount: () => parts.length,
    shapeExpressId: (_model, index) => parts[index].expressId,
    shapeClass: (_model, index) => parts[index].class,
    shapePartCount: () => 1,
    shapePositions: (_model, index) => positions[index],
    shapeIndices: (_model, index) => indices[index],
    shapeColor: (_model, index) => Uint8Array.from(parts[index].color),
  };
}

{
  const half = Math.ceil(BATCH_VERTEX_LIMIT / 2) + 3;
  const kernel = stubKernel([
    { expressId: 11, class: "IfcWall", color: [200, 200, 200, 255], vertices: half },
    { expressId: 12, class: "IfcWall", color: [200, 200, 200, 255], vertices: half },
    { expressId: 13, class: "IfcWindow", color: [90, 140, 200, 120], vertices: 90 },
  ]);
  const { batches, shapes, bounds } = buildBatches(kernel, 0);

  check(shapes.length === 3, "every part with triangles becomes a shape");
  check(
    batches.filter((batch) => !batch.transparent).length === 2,
    "one colour opens a second batch once the vertex limit is passed",
  );
  for (const batch of batches) {
    check(batch.vertexCount <= BATCH_VERTEX_LIMIT, "no batch exceeds the vertex limit");
    check(
      batch.positions.length / 3 === batch.vertexCount,
      `batch vertex count matches its positions (${batch.vertexCount})`,
    );
    check(batch.expressIds.length === batch.vertexCount, "one express id per vertex");
    let highest = 0;
    for (let i = 0; i < batch.indices.length; i += 1) {
      if (batch.indices[i] > highest) highest = batch.indices[i];
    }
    check(highest < batch.vertexCount, "indices are rebased into their own batch");
    const expected = batch.vertexCount > 65535 ? "Uint32Array" : "Uint16Array";
    check(
      batch.indices.constructor.name === expected,
      `index type is ${expected} for ${batch.vertexCount} vertices`,
    );
  }
  check(
    batches.every(
      (batch, index) => !batch.transparent || batches.slice(index).every((later) => later.transparent),
    ),
    "opaque batches come before transparent ones",
  );
  check(frame(bounds).radius > 0, "synthetic bounds still frame to a finite radius");
}

{
  const kernel = stubKernel([
    { expressId: 31, class: "IfcPlate", color: [90, 140, 200, 120], vertices: 9 },
    { expressId: 32, class: "IfcPlate", color: [90, 140, 200, 120], vertices: 9 },
    { expressId: 33, class: "IfcWall", color: [200, 200, 200, 255], vertices: BATCH_VERTEX_LIMIT + 20 },
  ]);
  const { batches } = buildBatches(kernel, 0);
  check(batches.filter((batch) => batch.transparent).length === 2, "transparent products can be sorted independently");
  check(batches.every((batch) => batch.vertexCount <= BATCH_VERTEX_LIMIT), "even a single oversized shape respects the batch limit");
  check(batches.reduce((sum, batch) => sum + batch.indices.length, 0) === kernel.shapeIndices(0, 0).length + kernel.shapeIndices(0, 1).length + kernel.shapeIndices(0, 2).length, "oversized splitting preserves every triangle");
  const large = batches.filter((batch) => !batch.transparent);
  const coordinates = large.flatMap((batch) => Array.from(batch.indices, (index) => batch.positions[index * 3]));
  check(coordinates.every((value, index) => value === index), "split triangle indices still reference their original positions");
}
{
  const camera = { fov: 60, aspect: 0.5, position: { set(...values) { this.values = values; } }, updateProjectionMatrix() {}, lookAt() {} };
  frameCamera(camera, { min: [-1, -1, -1], max: [1, 1, 1] });
  const portrait = Math.hypot(...camera.position.values);
  camera.aspect = 2;
  frameCamera(camera, { min: [-1, -1, -1], max: [1, 1, 1] });
  check(portrait > Math.hypot(...camera.position.values), "the three.js camera fits both viewport dimensions");
  let disposed = 0, cleared = false, detached = false;
  const resource = { dispose() { disposed++; } };
  const group = { traverse(fn) { fn({ geometry: resource, material: [resource, resource] }); }, removeFromParent() { detached = true; }, clear() { cleared = true; } };
  disposeModel(group);
  check(disposed === 2 && detached && cleared, "model disposal releases each geometry and material once and detaches the group");
}

// -------------------------------------------------------------- real kernel

// A wall and a slab, the smallest file that still produces two solids.
const FRAGMENT = [
  "ISO-10303-21;",
  "HEADER;",
  "FILE_SCHEMA(('IFC4'));",
  "ENDSEC;",
  "DATA;",
  "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,0.3);",
  "#2=IFCDIRECTION((0.,0.,1.));",
  "#3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.5);",
  "#4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));",
  "#5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));",
  "#6=IFCWALL('1wall',$,'Wall A',$,$,$,#5,$,$);",
  "#11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,4.);",
  "#12=IFCEXTRUDEDAREASOLID(#11,$,#2,0.2);",
  "#13=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#12));",
  "#14=IFCPRODUCTDEFINITIONSHAPE($,$,(#13));",
  "#15=IFCSLAB('1slab',$,'Slab A',$,$,$,#14,$,$);",
  "ENDSEC;",
  "END-ISO-10303-21;",
].join("\n");

if (!existsSync(join(pkg, "tessifc_wasm.js"))) {
  console.log("skip  kernel checks: the wasm package is not built");
  console.log(failures === 0 ? "\nPASS" : `\n${failures} FAILED`);
  process.exit(failures === 0 ? 0 : 1);
}

const loaded = await import(`file://${join(pkg, "tessifc_wasm.js").replaceAll("\\", "/")}`);
const { Kernel } = loaded.Kernel ? loaded : loaded.default;

const source = haveModel
  ? new Uint8Array(readFileSync(requested))
  : new TextEncoder().encode(FRAGMENT);
const label = haveModel ? basename(requested) : "the embedded fragment";

const kernel = new Kernel();
const id = kernel.openModel(source);
const summary = JSON.parse(kernel.evaluateGeometry(id, JSON.stringify({})));
const { batches, shapes, bounds } = buildBatches(kernel, id);

check(batches.length > 0, `${label}: ${batches.length} batches from ${shapes.length} shapes`);

// A product can own several coloured parts, a door's frame and its panel for
// example, so the bound is the part count and not the shape count.
let parts = 0;
for (let index = 0; index < kernel.shapeCount(id); index += 1) {
  parts += kernel.shapePartCount?.(id, index) ?? 1;
}
check(
  batches.length <= parts + Math.ceil(summary.triangles * 3 / BATCH_VERTEX_LIMIT),
  `batching adds calls only to split oversized parts (${parts} parts)`,
);

let triangles = 0;
for (const batch of batches) {
  triangles += batch.indices.length / 3;
  check(
    batch.positions.length / 3 === batch.vertexCount,
    `batch vertex count matches its positions (${batch.vertexCount})`,
  );
  check(
    batch.expressIds.length === batch.vertexCount,
    "one express id per vertex, so a hit can be resolved",
  );
  check(batch.vertexCount <= BATCH_VERTEX_LIMIT, "no batch exceeds the vertex limit");
  let highest = 0;
  for (let i = 0; i < batch.indices.length; i += 1) {
    if (batch.indices[i] > highest) highest = batch.indices[i];
  }
  check(
    highest < batch.vertexCount,
    "no index points past the end of its batch, which would draw garbage",
  );
  const expected = batch.vertexCount > 65535 ? "Uint32Array" : "Uint16Array";
  check(
    batch.indices.constructor.name === expected,
    `index type is ${expected} for ${batch.vertexCount} vertices`,
  );
}

check(
  triangles === summary.triangles,
  `every triangle survives batching: ${triangles} of ${summary.triangles}`,
);

const transparentAfterOpaque = batches.every(
  (batch, index) => !batch.transparent || batches.slice(index).every((later) => later.transparent),
);
check(transparentAfterOpaque, "opaque batches come before transparent ones");

check(
  new Set(batches.map((b) => b.color.join(","))).size <= batches.length,
  "a batch has exactly one colour",
);

const { centre, radius } = frame(bounds);
// Bounded, not exact: a wall and a building are both legitimate.
check(radius > 0 && radius < 100_000, `the model frames to a finite radius (${radius.toFixed(1)} m)`);
check(centre.every(Number.isFinite), "and a finite centre");

// Every shape's express id must be findable, or picking silently fails.
const ids = new Set();
for (const batch of batches) for (const id of batch.expressIds) ids.add(id);
check(
  shapes.every((shape) => ids.has(shape.expressId)),
  "every shape's express id survives into a batch",
);

kernel.closeAll();
console.log(failures === 0 ? "\nPASS" : `\n${failures} FAILED`);
process.exit(failures === 0 ? 0 : 1);
