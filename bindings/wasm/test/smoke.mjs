// SPDX-License-Identifier: Apache-2.0
// The WASM smoke test: IFC crosses the boundary and comes back as meshes.
// TESSIFC_TEST_MODEL names an optional IFC file to compare against the CLI.
// TESSIFC_SLIM=1 expects a build without the edit and ifczip features.

import { execFileSync } from "node:child_process";
import { deflateRawSync } from "node:zlib";
import { existsSync, readFileSync, statSync, mkdtempSync, unlinkSync, rmdirSync } from "node:fs";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";
import { readIgp } from "../../../viewer/src/igp.js";
import { createPackAssembler } from "../../../viewer/src/stream.js";
import { createEditingSession } from "../../edit/src/session.js";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..", "..", "..");
const pkg = resolve(here, "..", "pkg-node");

function check(condition, message) {
  if (!condition) fail(message);
  console.log(`ok    ${message}`);
}

function fail(message) {
  console.error(`FAIL  ${message}`);
  process.exit(1);
}

if (!existsSync(join(pkg, "tessifc_wasm.js"))) {
  console.error("The wasm package is not built.");
  console.error("Run:  python scripts/build-wasm.py");
  process.exit(2);
}

// The nodejs target is CommonJS; accept a default export or named exports.
const loaded = await import(`file://${join(pkg, "tessifc_wasm.js").replaceAll("\\", "/")}`);
const { Kernel, version, simplifyMesh } = loaded.Kernel ? loaded : loaded.default;
const SLIM = process.env.TESSIFC_SLIM === "1";

console.log(`tessifc-wasm ${version()}${SLIM ? " (slim build expected)" : ""}`);

// ---------------------------------------------------------------- unit checks

const kernel = new Kernel();

const tiny = new TextEncoder().encode(
  "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n" +
    "#1=IFCWALL('guid',$,'Wall A',$,$,$,$,$,.SOLIDWALL.);\n" +
    "ENDSEC;\nEND-ISO-10303-21;\n",
);

const tinyId = kernel.openModel(tiny);
const tinyInfo = JSON.parse(kernel.getModelInfo(tinyId));
if (tinyInfo.schema !== "IFC4") fail(`schema was ${tinyInfo.schema}, expected IFC4`);
if (tinyInfo.entities !== 1) fail(`entities was ${tinyInfo.entities}, expected 1`);
if (tinyInfo.products.IfcWall !== 1) fail("expected one IfcWall");
if (kernel.getClassName(tinyId, 1) !== "IfcWall") fail("getClassName disagrees");
if (kernel.getProductCategory(tinyId, 1) !== "physical") {
  fail("getProductCategory disagrees");
}
console.log("ok    a minimal file parses through wasm");

// Schema introspection answers by class name in the model's schema, in argument order.
const doorDefinition = JSON.parse(kernel.getClassAttributes(tinyId, "IfcDoor"));
check(doorDefinition.class === "IfcDoor" && !doorDefinition.abstract, "getClassAttributes names the class");
check(doorDefinition.attributes[0].name === "GlobalId" && !doorDefinition.attributes[0].optional, "GlobalId is the first required attribute");
check(doorDefinition.attributes.find((a) => a.name === "OverallHeight")?.base === "real", "attribute bases are reported");
check(JSON.parse(kernel.getClassAttributes(tinyId, "IfcProduct")).abstract === true, "abstract classes are flagged");
check(kernel.getClassAttributes(tinyId, "IfcSpaceship") === undefined, "an unknown class is undefined");
const doorChain = JSON.parse(kernel.getClassSupertypes(tinyId, "IfcDoor"));
check(doorChain[0] === "IfcDoor" && doorChain.includes("IfcProduct") && doorChain.at(-1) === "IfcRoot", "getClassSupertypes walks to IfcRoot");
check(kernel.getClassSupertypes(tinyId, "IfcSpaceship") === undefined, "supertypes of an unknown class are undefined");

// Garbage must not throw, and must be reported rather than swallowed.
const junkId = kernel.openModel(new Uint8Array([0, 1, 2, 3, 255, 254]));
const junkInfo = JSON.parse(kernel.getModelInfo(junkId));
if (junkInfo.entities !== 0) fail("garbage produced entities");
if (junkInfo.diagnostics.errors < 1) fail("garbage produced no error diagnostic");
const junkDiagnostics = JSON.parse(kernel.getDiagnostics(junkId));
if (!Array.isArray(junkDiagnostics) || junkDiagnostics.length === 0) {
  fail("getDiagnostics returned nothing for garbage");
}
const diagnosticKeys = Object.keys(junkDiagnostics[0]).sort().join(",");
if (diagnosticKeys !== "code,expressId,line,message,severity") {
  fail(`a diagnostic carries ${diagnosticKeys}, which is not what docs/sdk.md documents`);
}
console.log(`ok    garbage is diagnosed, not thrown (${junkDiagnostics[0].code})`);

for (const settings of ['{', '[]', '{"circleSegments":4294967297}', '{"chordToleranceM":0}', '{"includeSpaces":"yes"}', '{"maxSurfaceVertices":0}', '{"repairSurfaceCurves":1}']) {
  assert.throws(() => kernel.evaluateGeometry(tinyId, settings), /settings/);
  assert.throws(() => kernel.beginGeometryStream(tinyId, settings), /settings/);
  if (!SLIM) assert.throws(() => kernel.evaluateProducts(tinyId, Uint32Array.of(1), settings), /settings/);
}
kernel.beginGeometryStream(junkId, '{}');
const emptyChunk = readIgp(kernel.nextGeometryChunk(junkId, 0, 1, 0));
assert.equal(emptyChunk.index.stream.final, true);
assert(emptyChunk.index.diagnostics.some(d => d.code === 'E_NOT_A_STEP_FILE'));
assert.equal(kernel.nextGeometryChunk(junkId, 0, 1, 0), undefined);
assert.equal(JSON.parse(kernel.streamProgress(junkId)).finished, true);
assert.equal(kernel.cancelGeometryStream(junkId), true);
console.log('ok    malformed geometry settings throw and empty streams deliver final diagnostics');

// Models are independent, and closing one does not disturb the other.
if (kernel.modelCount() !== 2) fail("expected two open models");
if (!kernel.closeModel(tinyId)) fail("closeModel returned false for an open model");
if (kernel.getModelInfo(tinyId) !== undefined && kernel.getModelInfo(tinyId) !== null) {
  fail("a closed model still answers");
}
if (JSON.parse(kernel.getModelInfo(junkId)).entities !== 0) fail("the other model was disturbed");
kernel.closeAll();
if (kernel.modelCount() !== 0) fail("closeAll left something open");
console.log("ok    models are independent and closable");

// ------------------------------------------------------------------- geometry

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

const fragment = new TextEncoder().encode(FRAGMENT);

{
  const kernel = new Kernel();
  const id = kernel.openModel(fragment);
  const summary = JSON.parse(kernel.evaluateGeometry(id, JSON.stringify({ includeOpenings: true })));
  check(summary.products === 2, `${summary.products} shapes evaluated`);
  check(summary.triangles > 0, `${summary.triangles} triangles`);

  const positions = kernel.shapePositions(id, 0);
  const indices = kernel.shapeIndices(id, 0);
  check(positions instanceof Float32Array, "positions come back as a Float32Array");
  check(indices instanceof Uint32Array, "indices come back as a Uint32Array");
  check(positions.length % 3 === 0 && indices.length % 3 === 0, "both are triples");
  check(
    Math.max(...indices) < positions.length / 3,
    "no index points past the end of the positions, which would draw garbage",
  );

  const colour = kernel.shapeColor(id, 0);
  check(colour.length === 4, "a colour is four bytes");

  const pack = kernel.getPack(id);
  check(pack.length > 24, "the pack has more than a header");
  const magic = new DataView(pack.buffer, pack.byteOffset).getUint32(0, true);
  check(magic === 0x00504749, `IGP magic, got 0x${magic.toString(16)}`);

  const transferred = kernel.takePack(id);
  check(transferred instanceof Uint8Array, "takePack returns a Uint8Array");
  check(
    transferred.length === pack.length && transferred.every((byte, index) => byte === pack[index]),
    "takePack is byte-identical to getPack",
  );
  check(kernel.shapeCount(id) === 0, "taking the pack releases the geometry");
  kernel.closeAll();
}

// ----------------------------------------------------- IFCZIP archives

if (!SLIM) {
  // A minimal ZIP writer: one deflated entry, a central directory, the end record.
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc32 = bytes => {
    let c = 0xffffffff;
    for (const b of bytes) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const le16 = v => [v & 0xff, (v >>> 8) & 0xff];
  const le32 = v => [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff];
  const zipOf = (name, data) => {
    const deflated = deflateRawSync(data);
    const nameBytes = new TextEncoder().encode(name);
    const crc = crc32(data);
    const local = [0x50, 0x4b, 3, 4, ...le16(20), ...le16(0), ...le16(8), ...le16(0), ...le16(0),
      ...le32(crc), ...le32(deflated.length), ...le32(data.length), ...le16(nameBytes.length), ...le16(0),
      ...nameBytes, ...deflated];
    const central = [0x50, 0x4b, 1, 2, ...le16(20), ...le16(20), ...le16(0), ...le16(8), ...le16(0), ...le16(0),
      ...le32(crc), ...le32(deflated.length), ...le32(data.length), ...le16(nameBytes.length), ...le16(0), ...le16(0),
      ...le16(0), ...le16(0), ...le32(0), ...le32(0), ...nameBytes];
    const end = [0x50, 0x4b, 5, 6, ...le16(0), ...le16(0), ...le16(1), ...le16(1), ...le32(central.length),
      ...le32(local.length), ...le16(0)];
    return new Uint8Array([...local, ...central, ...end]);
  };
  const kernel = new Kernel();
  const archive = zipOf("model.ifc", fragment);
  const id = kernel.openModel(archive);
  const info = JSON.parse(kernel.getModelInfo(id));
  check(info.entities > 0 && !JSON.parse(kernel.getDiagnostics(id)).some(d => d.code.startsWith("E_IFCZIP")),
    "an IFCZIP archive opens to the model inside it");
  check(new TextDecoder().decode(kernel.exportModel(id)) === FRAGMENT,
    "the export of an archive is the plain IFC it held");
  const summary = JSON.parse(kernel.evaluateGeometry(id, JSON.stringify({ includeOpenings: true })));
  check(summary.products === 2, "and its geometry evaluates as usual");
  const truncated = kernel.openModel(archive.slice(0, archive.length - 5));
  check(JSON.parse(kernel.getDiagnostics(truncated)).some(d => d.code === "E_IFCZIP_MALFORMED"),
    "a truncated archive says so");
  const tooBig = kernel.openModel(archive, JSON.stringify({ maxIfczipBytes: 16 }));
  check(JSON.parse(kernel.getDiagnostics(tooBig)).some(d => d.code === "E_IFCZIP_TOO_LARGE"),
    "an archive over the configured size is refused");
  kernel.closeAll();
}

// ----------------------------------------------------- global budgets

{
  const kernel = new Kernel();
  const id = kernel.openModel(fragment);
  const settings = JSON.stringify({ includeOpenings: true, maxTotalTriangles: 1 });
  const summary = JSON.parse(kernel.evaluateGeometry(id, settings));
  check(summary.products === 1 && summary.limitReached?.which === "maxTotalTriangles",
    "a triangle budget stops the whole-model evaluation after one product");
  check(summary.limitReached.productsSkipped === 1, "the other product is skipped");
  const outcomes = JSON.parse(kernel.getProductOutcomes(id));
  check(outcomes.some(o => o.state === "skipped_by_limit"), "the skipped product says so");
  const pack = readIgp(kernel.takePack(id));
  check(pack.index.stats.limit_reached === 1 && pack.index.stats.products_skipped === 1,
    "the pack's stats carry the stop");
  check(pack.index.diagnostics.some(d => d.code === "E_GEOMETRY_LIMIT_REACHED"),
    "and the model-level diagnostic");
  assert.throws(() => kernel.evaluateGeometry(id, '{"maxGeometryMs":0}'), /maxGeometryMs/);

  kernel.beginGeometryStream(id, settings);
  let last;
  for (let chunk; (chunk = kernel.nextGeometryChunk(id, 0, 1, 0)) !== undefined;) last = readIgp(chunk);
  const progress = JSON.parse(kernel.streamProgress(id));
  check(progress.finished && progress.limitReached?.productsSkipped === 1,
    "a stream reports the stop in its progress");
  check(last.index.stats.limit_reached === 1, "and in its final chunk");
  kernel.closeAll();
}

// ------------------------------------------------------ textures on request

{
  // A textured quad: an image texture by path, mapped by an indexed triangle texture map.
  const TEXTURED = [
    "ISO-10303-21;",
    "HEADER;",
    "FILE_SCHEMA(('IFC4'));",
    "ENDSEC;",
    "DATA;",
    "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.)));",
    "#2=IFCTRIANGULATEDFACESET(#1,$,.F.,((1,2,3),(1,3,4)),$);",
    "#3=IFCTEXTUREVERTEXLIST(((0.,0.),(1.,0.),(1.,1.),(0.,1.)));",
    "#4=IFCINDEXEDTRIANGLETEXTUREMAP((#10),#2,#3,((1,2,3),(1,3,4)));",
    "#10=IFCIMAGETEXTURE(.T.,.F.,$,$,$,'textures/brick.png');",
    "#13=IFCCOLOURRGB($,0.8,0.2,0.1);",
    "#14=IFCSURFACESTYLERENDERING(#13,0.,$,$,$,$,$,IFCSPECULAREXPONENT(32.),.BLINN.);",
    "#16=IFCSURFACESTYLEWITHTEXTURES((#10));",
    "#17=IFCSURFACESTYLE('brick',.BOTH.,(#14,#16));",
    "#18=IFCSTYLEDITEM(#2,(#17),$);",
    "#20=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#2));",
    "#21=IFCPRODUCTDEFINITIONSHAPE($,$,(#20));",
    "#22=IFCWALL('w',$,$,$,$,$,#21,$,$);",
    "ENDSEC;",
    "END-ISO-10303-21;",
  ].join("\n");
  const kernel = new Kernel();
  const id = kernel.openModel(new TextEncoder().encode(TEXTURED));
  const flat = JSON.parse(kernel.evaluateGeometry(id, "{}"));
  check(flat.products === 1 && !flat.materials && !flat.textures, "textures off: the summary counts none");
  check(kernel.shapeUv(id, 0, 0) === undefined, "textures off: a part carries no texture coordinates");
  const flatPack = readIgp(kernel.takePack(id));
  check(flatPack.instances.material === null && flatPack.index.materials === undefined && flatPack.geometry[0].uv === null,
    "textures off: the pack has none of the optional members");

  const summary = JSON.parse(kernel.evaluateGeometry(id, JSON.stringify({ textures: true })));
  check(summary.materials === 1 && summary.textures === 1, `textures on: the summary counts the material and its texture (${summary.materials}, ${summary.textures})`);
  const uv = kernel.shapeUv(id, 0, 0);
  check(uv instanceof Float32Array && uv.length === 8, "textures on: the array path hands out two floats per vertex");
  check(uv && uv[2] === 1 && uv[3] === 0 && uv[4] === 1 && uv[5] === 1, "the coordinates are the file's texture vertices");
  const pack = readIgp(kernel.takePack(id));
  check(pack.geometry[0].uv?.length === 8, "textures on: the pack's geometry carries uv");
  check(pack.instances.material?.[0] === 0, "the instance names its material row");
  const material = pack.index.materials?.[0];
  check(material?.texture === 10 && material.shininess === 32 && material.reflectance === "BLINN", "the material row carries the rendering extras");
  const texture = pack.index.textures?.[0];
  check(texture?.id === 10 && texture.uri === "textures/brick.png" && texture.mime === "image/png", "an image texture keeps its path and media type; nothing is fetched");
  check(Array.isArray(texture?.repeat) && texture.repeat[0] === true && texture.repeat[1] === false, "repeat flags follow the file");
  kernel.closeAll();
  console.log("ok    textures are read, packed and handed out only when the setting asks");
}

// -------------------------------------------------------- coarse mesh levels

{
  // A column with hundreds of segments is large enough for a level; a quad is not.
  const COLUMN = [
    "ISO-10303-21;", "HEADER;", "FILE_SCHEMA(('IFC4'));", "ENDSEC;", "DATA;",
    "#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,1.);",
    "#2=IFCDIRECTION((0.,0.,1.));",
    "#3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);",
    "#4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));",
    "#5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));",
    "#6=IFCCOLUMN('c',$,$,$,$,$,#5,$,$);",
    "ENDSEC;", "END-ISO-10303-21;",
  ].join("\n");
  const kernel = new Kernel();
  const id = kernel.openModel(new TextEncoder().encode(COLUMN));
  kernel.evaluateGeometry(id, JSON.stringify({ circleSegments: 512 }));
  const plain = readIgp(kernel.takePack(id));
  check(plain.geometry.length === 1 && plain.geometry[0].lod === null, "without lodLevels the pack has no level");
  kernel.evaluateGeometry(id, JSON.stringify({ circleSegments: 512, lodLevels: 1 }));
  const levelled = readIgp(kernel.takePack(id));
  const level = levelled.geometry.find((mesh) => mesh.lod);
  check(levelled.geometry.length === 2 && level?.lod.of === levelled.geometry[0].id && level.lod.level === 1, "lodLevels: 1 writes one level per large mesh");
  check(level && level.indices.length * 4 <= levelled.geometry[0].indices.length * 3, `the level holds at most three quarters of the triangles (${level?.indices.length / 3} of ${levelled.geometry[0].indices.length / 3})`);
  check(level && level.positions.byteOffset === levelled.geometry[0].positions.byteOffset, "the level shares the base's positions");
  check(levelled.instances.count === 1 && levelled.index.stats.triangles === plain.index.stats.triangles, "no instance places the level and the stats count the base");
  assert.throws(() => kernel.evaluateGeometry(id, JSON.stringify({ lodLevels: 3 })), /lodLevels/);
  const fine = levelled.geometry[0];
  const coarse = simplifyMesh(fine.positions, Uint32Array.from(fine.indices), JSON.stringify({ level: 1 }));
  check(coarse instanceof Uint32Array && coarse.length === level.indices.length, "simplifyMesh computes the same level the pack carries");
  check(simplifyMesh(new Float32Array(9), new Uint32Array([0, 1, 2]), undefined) === undefined, "a small mesh has no level");
  assert.throws(() => simplifyMesh(fine.positions, Uint32Array.from(fine.indices), JSON.stringify({ level: 5 })), /level/);
  kernel.closeAll();
  console.log("ok    coarse levels are written on request and computed on demand");
}

// ----------------------------------------------------- staged live revisions

if (SLIM) {
  // A build without the edit feature reads and draws; the session says so.
  const kernel = new Kernel();
  const id = kernel.openModel(fragment);
  for (const name of ["prepareRevision", "prepareAttributeEdits", "commitRevision", "setAttribute", "exportModel", "getModelRevision", "evaluateProducts"]) {
    check(typeof kernel[name] === "undefined", `the slim build has no ${name}`);
  }
  check(JSON.parse(kernel.evaluateGeometry(id, "{}")).products === 2, "the slim build still evaluates geometry");
  const session = createEditingSession(kernel, id, { modelOffset: [0, 0, 0], firstGeometryId: 1 });
  assert.throws(() => session.runScript("print(1)"), /no editing support/);
  assert.throws(() => session.applySnapshot(fragment), /no editing support/);
  console.log("ok    a slim build has no editing methods and the session refuses to edit on it");
  kernel.closeAll();
} else {
  const kernel = new Kernel();
  const encode = text => new TextEncoder().encode(text);
  const id = kernel.openModel(fragment);
  const candidateToken = () => JSON.parse(kernel.getPreparedRevisionInfo(id)).candidateToken;
  assert.equal(kernel.getModelRevision(id), '0');
  kernel.evaluateGeometry(id);
  const originalPack = readIgp(kernel.takePack(id));
  const changed = FRAGMENT.replace('#2,2.5)', '#2,4.)');
  const impact = JSON.parse(kernel.prepareRevision(id, encode(changed), '0'));
  assert.deepEqual(impact.affectedProducts, [6]);
  assert.equal(impact.revision, '1');
  assert.equal(new TextDecoder().decode(kernel.exportModel(id)), FRAGMENT);
  assert.throws(() => kernel.commitRevision(id, '0', impact.candidateToken), /evaluation/);
  assert.throws(() => kernel.prepareRevision(id, fragment, '0'), /discard/);
  assert.throws(() => kernel.evaluatePreparedRevision(id, impact.candidateToken,
    '{"circleSegments":40}'), /settings differ/);
  assert.throws(() => kernel.evaluatePreparedRevision(id, impact.candidateToken,
    '{"modelOffset":[1e300,0,0]}'), /modelOffset differs/);
  const patch = readIgp(kernel.evaluatePreparedRevision(id, impact.candidateToken, JSON.stringify({
    modelOffset: originalPack.index.model_offset, firstGeometryId: 100,
  })));
  assert.deepEqual([...patch.instances.expressIds], [6]);
  assert.equal(patch.geometry[0].id, 100);
  assert.equal(Math.max(...patch.geometry[0].positions.filter((_, i) => i % 3 === 2)), 4);
  const evaluated = JSON.parse(kernel.getPreparedRevisionInfo(id));
  assert.equal(evaluated.evaluationAccepted, true);
  assert.deepEqual(evaluated.productOutcomes.map(o => [o.express_id, o.state]), [[6, 'emitted']]);
  assert.equal(kernel.commitRevision(id, '0', impact.candidateToken), '1');
  assert.equal(kernel.getModelRevision(id), '1');
  assert.equal(new TextDecoder().decode(kernel.exportModel(id)), changed);
  assert.throws(() => kernel.prepareRevision(id, fragment, '0'), /stale/);
  assert.throws(() => kernel.commitRevision(id, '0', impact.candidateToken), /prepared|stale/);

  const names = JSON.parse(kernel.prepareAttributeEdits(id, JSON.stringify([
    { expressId: 6, attribute: 'Name', value: "O'Brien wall" },
    { expressId: 15, attribute: 'Name', value: 'Renamed slab' },
  ]), '1'));
  assert.deepEqual(names.affectedProducts, []);
  const metadataPack = readIgp(kernel.evaluatePreparedRevision(id, names.candidateToken));
  assert.equal(metadataPack.instances.count, 0);
  assert.equal(kernel.commitRevision(id, '1', names.candidateToken), '2');
  assert.equal(JSON.parse(kernel.getProductOutcomes(id)).filter(o => o.state === 'emitted').length, 2);
  assert.equal(JSON.parse(kernel.getSpatialHierarchy(id)).nodes.filter(n => n.rendered).length, 2);
  const namedSource = new TextDecoder().decode(kernel.exportModel(id));
  assert(namedSource.includes("'O''Brien wall'"));
  assert(namedSource.includes("'Renamed slab'"));

  // A new product may share an existing representation. Deletion changes count too.
  const added = namedSource.replace('ENDSEC;\nEND-ISO',
    "#25=IFCWALL('new-wall',$,'New wall',$,$,$,#5,$,$);\nENDSEC;\nEND-ISO");
  const creation = JSON.parse(kernel.prepareRevision(id, encode(added), '2'));
  assert.deepEqual(creation.createdEntities, [25]);
  assert(creation.affectedProducts.includes(25));
  kernel.evaluatePreparedRevision(id, creation.candidateToken);
  assert.equal(kernel.commitRevision(id, '2', creation.candidateToken), '3');
  const removal = JSON.parse(kernel.prepareRevision(id, encode(namedSource), '3'));
  assert.deepEqual(removal.deletedEntities, [25]);
  assert(removal.removedProducts.includes(25));
  kernel.evaluatePreparedRevision(id, removal.candidateToken);
  assert.equal(kernel.commitRevision(id, '3', removal.candidateToken), '4');

  for (const malformed of [
    namedSource.replace('END-ISO-10303-21;', ''),
    namedSource.replace('ENDSEC;\nEND-ISO', 'END-ISO'),
    namedSource.replace('END-ISO-10303-21;', '/* END-ISO-10303-21; */'),
    namedSource.replace('#2,4.)', '#999,4.)'),
    namedSource.replace('#2,4.)', '#2,4.,5.)'),
  ]) {
    assert.throws(() => kernel.prepareRevision(id, encode(malformed), '4'), /candidate/);
    assert.equal(kernel.getModelRevision(id), '4');
    assert.equal(new TextDecoder().decode(kernel.exportModel(id)), namedSource);
  }

  // Valid STEP can still have failed geometry: source and revision remain unchanged.
  const broken = namedSource.replace('#1,$,#2,4.', '$,$,#2,4.');
  kernel.prepareRevision(id, encode(broken), '4');
  kernel.evaluatePreparedRevision(id, candidateToken());
  const refused = JSON.parse(kernel.getPreparedRevisionInfo(id));
  assert.equal(refused.evaluationAccepted, false);
  assert(refused.productOutcomes.some(o => o.state === 'empty_or_failed'));
  assert.throws(() => kernel.commitRevision(id, '4', candidateToken()), /evaluation/);
  assert.equal(new TextDecoder().decode(kernel.exportModel(id)), namedSource);
  assert.equal(kernel.discardRevision(id, candidateToken()), true);

  const abandoned = JSON.parse(kernel.prepareRevision(id, encode(namedSource), '4'));
  kernel.discardRevision(id, abandoned.candidateToken);
  kernel.prepareRevision(id, encode(namedSource), '4');
  assert.throws(() => kernel.evaluatePreparedRevision(id, abandoned.candidateToken), /stale candidate/);
  assert.throws(() => kernel.commitRevision(id, '4', abandoned.candidateToken), /stale candidate/);
  assert.throws(() => kernel.discardRevision(id, abandoned.candidateToken), /stale candidate/);
  assert(kernel.getPreparedRevisionInfo(id), 'stale cleanup preserves the newer candidate');
  kernel.setAttribute(id, 6, 'Name', 'Legacy edit', false);
  assert.equal(kernel.getModelRevision(id), '5');
  assert.equal(kernel.getPreparedRevisionInfo(id), undefined);
  assert.equal(kernel.discardRevision(id, abandoned.candidateToken), false);
  kernel.closeAll();
  assert.equal(kernel.getModelRevision(id), undefined);
  assert.equal(kernel.getPreparedRevisionInfo(id), undefined);
  console.log('ok    staged revisions publish only affected geometry, batch metadata, create/delete, and reject stale or failed updates');
}

// -------------------------------------------------- agreement with the CLI

// Point TESSIFC_TEST_MODEL at an IFC file to compare a whole model against the
// native CLI. A skip proves nothing, so a gated run turns a skip into a failure.
const model = process.env.TESSIFC_TEST_MODEL
  ? resolve(repo, process.env.TESSIFC_TEST_MODEL)
  : null;
const requireModel = process.env.TESSIFC_REQUIRE_MODEL === "1";

if (!model || !existsSync(model) || !statSync(model).isFile()) {
  if (requireModel) fail("TESSIFC_REQUIRE_MODEL is set but TESSIFC_TEST_MODEL names no file");
  console.log("skip  native comparison: set TESSIFC_TEST_MODEL to an IFC file");
  console.log("PASS");
  process.exit(0);
}

const cli =
  process.platform === "win32"
    ? resolve(repo, "target", "release", "tessifc.exe")
    : resolve(repo, "target", "release", "tessifc");

if (!existsSync(cli)) {
  if (requireModel) fail("TESSIFC_REQUIRE_MODEL is set but the native CLI is not built");
  console.log("skip  native CLI not built; run cargo build --release -p tessifc-cli");
  console.log("PASS");
  process.exit(0);
}

const native = JSON.parse(execFileSync(cli, ["info", model, "--json"], { encoding: "utf8" }));

const kernel2 = new Kernel();
const id = kernel2.openModel(new Uint8Array(readFileSync(model)));
const wasm = JSON.parse(kernel2.getModelInfo(id));

if (wasm.schema !== native.schema) {
  fail(`schema: wasm ${wasm.schema}, native ${native.schema}`);
}
if (wasm.entities !== native.entities) {
  fail(`entities: wasm ${wasm.entities}, native ${native.entities}`);
}
if (wasm.productTotal !== native.product_total) {
  fail(`products: wasm ${wasm.productTotal}, native ${native.product_total}`);
}
for (const [name, count] of Object.entries(native.classes)) {
  if (wasm.classes[name] !== count) {
    fail(`class ${name}: wasm ${wasm.classes[name]}, native ${count}`);
  }
}
if (Object.keys(wasm.classes).length !== Object.keys(native.classes).length) {
  fail("wasm reported a different number of populated classes");
}
function comparePacks(actual, expected) {
  assert.equal(actual.instances.count, expected.instances.count);
  for (const key of ['expressIds', 'geometryIds', 'flags', 'colors']) {
    if (actual.instances[key]) assert.deepEqual(actual.instances[key], expected.instances[key], key);
  }
  for (let i = 0; i < actual.instances.count; i++) {
    assert.equal(actual.index.classes[actual.instances.classIds[i]],expected.index.classes[expected.instances.classIds[i]], `class of record ${i}`);
    if (actual.instances.provenance && expected.instances.provenance) {
      assert.deepEqual(actual.index.provenance[actual.instances.provenance[i]], expected.index.provenance[expected.instances.provenance[i]], `source of record ${i}`);
    }
  }
  assert.equal(actual.geometry.length, expected.geometry.length);
  for (let i = 0; i < actual.geometry.length; i++) {
    const a = actual.geometry[i], b = expected.geometry[i];
    assert.equal(a.id, b.id);
    assert.deepEqual(a.indices, b.indices, `mesh ${a.id} indices`);
    assert.equal(a.positions.length, b.positions.length);
    for (let j = 0; j < a.positions.length; j++) {
      assert(Math.abs(a.positions[j] - b.positions[j]) <= 1e-5 * Math.max(1, Math.abs(b.positions[j])), `mesh ${a.id} position ${j}`);
    }
  }
  for (let j = 0; j < actual.instances.transforms.length; j++) {
    assert(Math.abs(actual.instances.transforms[j] - expected.instances.transforms[j]) <= 1e-5 * Math.max(1,Math.abs(expected.instances.transforms[j])), `transform ${j}`);
  }
}
const directory = mkdtempSync(join(tmpdir(), 'tessifc-smoke-'));
const output = join(directory, 'geometry.igp');
try {
  for (const settings of [{ chordToleranceM: 0.002 }, { circleSegments: 36, weld: false }, { repairSurfaceCurves: true, maxSurfaceVertices: 262144 }]) {
    const json = JSON.stringify(settings);
    const report = JSON.parse(execFileSync(cli, ['convert', model, '--json', '--jobs', '1', '--settings', json, '-o', output], { encoding:'utf8', maxBuffer:64*1024*1024 }));
    const summary = JSON.parse(kernel2.evaluateGeometry(id,json));
    assert.equal(summary.triangles,report.triangles);
    assert.deepEqual(summary.effectiveSettings,report.effective_settings);
    const nativePack = readIgp(readFileSync(output));
    const whole = readIgp(kernel2.takePack(id));
    comparePacks(whole,nativePack);
    const stream = createPackAssembler();
    kernel2.beginGeometryStream(id,json);
    let chunk;
    while ((chunk=kernel2.nextGeometryChunk(id,0,1,0))) stream.append(readIgp(chunk));
    comparePacks(stream.pack(),whole);
    assert.deepEqual(JSON.parse(kernel2.getProductOutcomes(id)),report.product_outcomes);
  }
} finally {
  if (existsSync(output)) unlinkSync(output);
  rmdirSync(directory);
}
console.log('ok    native, WASM whole-model and streamed geometry agree under three settings profiles');
kernel2.closeAll();

console.log(
  `ok    ${basename(model)}: ${wasm.entities} entities, ` +
    `${wasm.productTotal} products, identical to the native CLI`,
);

console.log("PASS");
