// SPDX-License-Identifier: Apache-2.0
// The browser script engine against the Node kernel build: STEP values, the
// entity API, snapshots the kernel accepts, and error reporting.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { pavilionIfc } from "../../../viewer/test/fixture.mjs";
import {
  Enum, Ref, Typed, containsRef, createScriptEngine, decodeStepString, encodeStepString, formatValue, indexRecords, newGuid, parseValue,
  runScript, splitArguments,
} from "../src/script-engine.js";

let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

// ------------------------------------------------------------ values

assert.deepEqual(splitArguments("'a,b',(1,2),$,#3"), ["'a,b'", "(1,2)", "$", "#3"]);
assert.deepEqual(splitArguments(""), []);
ok(true, "arguments split at top-level commas only");

assert.equal(parseValue("$").value, null);
assert.ok(parseValue("#12").value instanceof Ref);
assert.equal(parseValue("#12").value.id, 12);
assert.equal(parseValue("'it''s \\X2\\00E9\\X0\\'").value, "it's é");
assert.equal(parseValue(".AREA.").value.value, "AREA");
assert.equal(parseValue(".T.").value, true);
assert.equal(parseValue("1.E-05").value, 0.00001);
assert.equal(parseValue(".5").value, 0.5);
assert.deepEqual(parseValue("(.5,-.25)").value, [0.5, -0.25]);
assert.throws(() => parseValue(".AREA"), /Unterminated enumeration/);
assert.deepEqual(parseValue("((1.,2.),(3.,4.))").value, [[1, 2], [3, 4]]);
const typed = parseValue("IFCLABEL('x')").value;
assert.ok(typed instanceof Typed);
assert.equal(typed.type, "IFCLABEL");
assert.equal(typed.value, "x");
ok(true, "STEP values parse: null, references, escaped strings, enumerations, booleans, reals, nested lists, typed values");

assert.equal(decodeStepString("caf\\X\\E9"), "café");
assert.equal(encodeStepString("it's é\u{1F600}"), "'it''s \\X2\\00E9\\X0\\\\X4\\0001F600\\X0\\'");
assert.equal(decodeStepString(encodeStepString("Grüße").slice(1, -1)), "Grüße");
ok(true, "strings round-trip through STEP escapes");

assert.equal(formatValue(3, { name: "Depth", base: "real" }), "3.");
assert.equal(formatValue(0.5), "0.5");
assert.equal(formatValue(1e-7), "1.E-7");
assert.equal(formatValue(3, { name: "Dimension", base: "integer" }), "3");
assert.equal(formatValue("AREA", { name: "ProfileType", base: "enumeration" }), ".AREA.");
assert.equal(formatValue(new Enum("area")), ".AREA.");
assert.equal(formatValue(true), ".T.");
assert.equal(formatValue(null), "$");
assert.equal(formatValue([new Ref(4), 2], { name: "Items", base: "real" }), "(#4,2.)");
assert.equal(formatValue(new Typed("IFCLABEL", "x")), "IFCLABEL('x')");
assert.throws(() => formatValue("x", { name: "NominalValue", base: "select" }), /ifc\.typed/);
assert.throws(() => formatValue(Number.NaN), /finite/);
ok(true, "values serialize by declared base type");

const guid = newGuid();
assert.match(guid, /^[0-3][0-9A-Za-z_$]{21}$/);
assert.notEqual(guid, newGuid());
ok(true, "new GlobalIds have 22 characters and a leading 0-3");

const TRICKY = [
  "ISO-10303-21;",
  "HEADER;",
  "FILE_DESCRIPTION(('#1=IFCWALL in the header'),'2;1');",
  "ENDSEC;",
  "DATA;",
  "/* #9=IFCWALL('g9',$,$,$,$,#8,$,$,$); a commented-out record */",
  "#5=IFCCARTESIANPOINT((0.,0.,0.));",
  "#7=IFCWALL('g7',$,'Named #8',' #5= it''s',$,#8,$,$,$);",
  "#8 = IFCLOCALPLACEMENT($,#5);\r",
  "#10=(IFCNAMEDUNIT(*)IFCSIUNIT(.LENGTHUNIT.,$,.METRE.));",
  "#11=IFCPROPERTYSINGLEVALUE('p',$,IFCLABEL('#7'),$);",
  "ENDSEC;",
  "END-ISO-10303-21;",
].join("\n");
const scanned = indexRecords(TRICKY);
assert.deepEqual([...scanned.records.keys()], [5, 7, 8, 10, 11]);
assert.equal(scanned.records.get(7).className, "IFCWALL");
assert.equal(TRICKY.slice(scanned.records.get(7).argsStart, scanned.records.get(7).argsEnd), "'g7',$,'Named #8',' #5= it''s',$,#8,$,$,$");
assert.equal(TRICKY.slice(scanned.records.get(8).lineStart, scanned.records.get(8).lineEnd), "#8 = IFCLOCALPLACEMENT($,#5);\r\n");
assert.equal(scanned.records.get(10).className, "");
assert.deepEqual([...scanned.users.get(8)], [7]);
assert.deepEqual([...scanned.users.get(5)], [8]);
assert.equal(scanned.users.get(7), undefined);
assert.equal(scanned.users.get(9), undefined);
assert.ok(containsRef(parseValue("(#1,IFCLABEL('x'),(#2))").value, 2));
assert.ok(!containsRef(parseValue("'#2'").value, 2));
ok(true, "the record scanner ignores strings, comments and the header, and maps reference users");

// ------------------------------------------------------------ kernel

const pkg = fileURLToPath(new URL("../../wasm/pkg-node/tessifc_wasm.js", import.meta.url));
if (!existsSync(pkg)) {
  console.log("skip  kernel checks: build the Node package first (python scripts/build-wasm.py --target both)");
} else {
  const { Kernel } = createRequire(import.meta.url)(pkg);
  const kernel = new Kernel();
  const modelId = kernel.openModel(Buffer.from(pavilionIfc()));
  const summary = JSON.parse(kernel.evaluateGeometry(modelId, JSON.stringify({ includeOpenings: true })));
  kernel.takePack(modelId);
  const settings = () => JSON.stringify({ includeOpenings: true, modelOffset: summary.modelOffset, firstGeometryId: 1_000_000 });

  function publish(source, selection = null) {
    const engine = createScriptEngine(kernel, modelId);
    const result = runScript(engine, source, selection);
    if (!result.ok || !result.changed) return { result, impact: null };
    const base = kernel.getModelRevision(modelId);
    const candidate = JSON.parse(kernel.prepareRevision(modelId, engine.snapshotBytes(), base));
    kernel.evaluatePreparedRevision(modelId, candidate.candidateToken, settings());
    const impact = JSON.parse(kernel.getPreparedRevisionInfo(modelId));
    assert.equal(impact.evaluationAccepted, true, `candidate accepted: ${JSON.stringify(impact.diagnostics ?? []).slice(0, 300)}`);
    kernel.commitRevision(modelId, base, candidate.candidateToken);
    return { result, impact };
  }

  const wallId = kernel.getIdsOfType(modelId, "IfcWall")[0];
  const read = createScriptEngine(kernel, modelId);
  const listing = runScript(read, `
    const wall = ifc.get(${wallId});
    print(wall.type, wall.Name, wall.is("IfcProduct"), wall.is("IfcColumn"));
    const solid = wall.Representation.Representations[0].Items[0];
    print(solid.type, solid.Depth, solid.SweptArea.XDim, solid.SweptArea.ProfileType);
    print(ifc.container(wall).Name, ifc.byType("IfcWall").length, ifc.inverses(wall, "IfcRelVoidsElement").length);
    print(Object.keys(wall.attributes()).length, ifc.schema);
  `, null);
  assert.equal(listing.ok, true, listing.error);
  assert.equal(listing.stdout, `IfcWall Gallery wall true false\nIfcExtrudedAreaSolid 3.4 10.8 AREA\nGround floor 2 1\n9 IFC4`);
  assert.equal(listing.changed, false);
  ok(true, "entities navigate attributes, references, lists, inverses and containers");

  const door = publish(`
    const wall = selected;
    const storey = ifc.container(wall);
    const solid = wall.Representation.Representations[0].Items[0];
    const opening = ifc.addBox("IfcOpeningElement", "Door opening", { at: [1, 0, 0], size: [0.9, solid.SweptArea.YDim + 0.1, 2.1], relativeTo: wall });
    const door = ifc.addBox("IfcDoor", "New door", { at: [1, 0, 0], size: [0.9, 0.05, 2.1], relativeTo: wall, attributes: { OverallHeight: 2.1 } });
    ifc.void(wall, opening);
    ifc.fill(opening, door);
    ifc.contain(door, storey);
    wall.Name = "Wall with door";
    print(door.id, door.OverallHeight, door.ObjectPlacement.PlacementRelTo.id === wall.ObjectPlacement.id);
  `, { ids: [wallId] });
  assert.equal(door.result.ok, true, door.result.error);
  assert.equal(door.result.operations.created, 20);
  assert.equal(door.result.operations.modified, 2);
  const doorId = Number(door.result.stdout.split(" ")[0]);
  assert.equal(door.result.stdout, `${doorId} 2.1 true`);
  assert.deepEqual([...door.impact.affectedProducts].sort((a, b) => a - b), [wallId, doorId - 9, doorId].sort((a, b) => a - b));
  assert.equal(door.impact.fullRebuild, false);
  assert.equal(kernel.getClassName(modelId, doorId), "IfcDoor");
  ok(true, "the door script creates placed geometry, relationships and containment the kernel accepts");

  const byGuid = createScriptEngine(kernel, modelId);
  const guidResult = runScript(byGuid, `const d = ifc.byType("IfcDoor")[0]; print(ifc.byGuid(d.GlobalId).id === d.id, ifc.byGuid("nope") === null);`, null);
  assert.equal(guidResult.stdout, "true true");
  ok(true, "byGuid finds rooted entities by their GlobalId");

  const rename = publish(`selected.Description = "Grüße"; selected.Tag = null;`, { ids: [wallId] });
  assert.equal(rename.impact.affectedProducts.length, 0);
  assert.equal(rename.impact.metadataProducts.length, 1);
  const description = JSON.parse(kernel.getEntityInfo(modelId, wallId)).fields.find((field) => field.name === "Description");
  assert.equal(description.value, "Grüße");
  assert.match(description.raw, /X2/);
  ok(true, "attribute edits keep the record intact and count as metadata");

  const removal = publish(`ifc.remove(ifc.byType("IfcDoor")[0]); print(ifc.byType("IfcDoor").length, ifc.byType("IfcRelFillsElement").length);`, null);
  assert.equal(removal.result.stdout, "0 0");
  assert.deepEqual(removal.impact.removedProducts, [doorId]);
  assert.equal(removal.result.operations.deleted, 2);
  ok(true, "removing a product detaches its relationships and removes the emptied ones");

  const failing = runScript(createScriptEngine(kernel, modelId), 'print("first");\nconst x = 1;\nx.missing.deeper;\n', null);
  assert.equal(failing.ok, false);
  assert.match(failing.error, /TypeError/);
  assert.equal(failing.traceback, "line 3: x.missing.deeper;");
  assert.equal(failing.stdout, "first");
  ok(true, "runtime errors report the script line and keep earlier output");

  const subtype = runScript(createScriptEngine(kernel, modelId), `
    const c = ifc.add("IfcWallStandardCase", { Name: "New" });
    print(c.is("IfcProduct"), c.is("IfcWall"), c.is("IfcColumn"), ifc.byType("IfcProduct").some((e) => e.id === c.id), ifc.byType("IfcWall").length);
  `, null);
  assert.equal(subtype.ok, true, subtype.error);
  assert.equal(subtype.stdout, "true true false true 3");
  ok(true, "added entities answer is() and byType() through the schema's supertypes");

  const trickyId = kernel.openModel(Buffer.from(TRICKY.replace("\r", "")));
  assert.equal(JSON.parse(kernel.getModelInfo(trickyId)).entities, 5);
  const tricky = createScriptEngine(kernel, trickyId);
  const trickyRun = runScript(tricky, `
    const wall = ifc.get(7);
    print(ifc.inverses(ifc.get(8)).map((e) => e.id), ifc.byGuid("g7").id, ifc.byGuid("g9"), wall.Name, wall.Description);
    ifc.remove(ifc.get(8));
    print(wall.ObjectPlacement, ifc.byType("IfcWall").length);
  `, null);
  assert.equal(trickyRun.ok, true, trickyRun.error);
  assert.equal(trickyRun.stdout, "[7] 7 null Named #8  #5= it's\nnull 1");
  const trickyText = new TextDecoder("latin1").decode(tricky.snapshotBytes());
  assert.match(trickyText, /'Named #8'/);
  assert.match(trickyText, /#7=IFCWALL\('g7',\$,'Named #8',' #5= it''s',\$,\$,\$,\$,\$\);/);
  assert.ok(!trickyText.includes("#8 = IFCLOCALPLACEMENT"));
  const trickyCandidate = JSON.parse(kernel.prepareRevision(trickyId, tricky.snapshotBytes(), kernel.getModelRevision(trickyId)));
  assert.deepEqual(trickyCandidate.deletedEntities, [8]);
  assert.deepEqual(trickyCandidate.modifiedEntities, [7]);
  kernel.discardRevision(trickyId, trickyCandidate.candidateToken);
  kernel.closeModel(trickyId);
  ok(true, "references inside strings and comments never count, so removal keeps the text that mentions them");

  const placed = runScript(createScriptEngine(kernel, modelId), `
    const wall = ifc.addWall({ from: [0, 0], to: [3, 4], height: 2.8, thickness: 0.2 });
    const beam = ifc.addBeam({ from: [0, 0, 3], to: [0, 5, 3], size: [0.4, 0.2] });
    const post = ifc.addBeam({ from: [1, 1, 0], to: [1, 1, 4] });
    const axes = (p) => p.ObjectPlacement.RelativePlacement;
    const solid = (p) => p.Representation.Representations[0].Items[0];
    print(JSON.stringify([axes(wall).RefDirection.DirectionRatios, axes(wall).Axis, solid(wall).SweptArea.XDim, solid(wall).Depth,
      axes(beam).Axis.DirectionRatios, axes(beam).RefDirection.DirectionRatios, solid(beam).Depth, solid(beam).SweptArea.XDim,
      axes(post).RefDirection.DirectionRatios, ifc.container(wall).Name]));
  `, null);
  assert.equal(placed.ok, true, placed.error);
  assert.deepEqual(JSON.parse(placed.stdout), [[0.6, 0.8, 0], null, 5, 2.8, [0, 1, 0], [0, 0, 1], 5, 0.4, [1, 0, 0], "Ground floor"]);
  ok(true, "walls follow their line and beams their axis, with the profile depth vertical");

  const invalid = runScript(createScriptEngine(kernel, modelId), 'ifc.add("IfcExtrudedAreaSolid", { Depth: 1 });', null);
  assert.equal(invalid.error, "Error: IfcExtrudedAreaSolid needs SweptArea, ExtrudedDirection");
  const unknown = runScript(createScriptEngine(kernel, modelId), 'ifc.add("IfcSpaceship", {});', null);
  assert.match(unknown.error, /not a class/);
  const wrongAttribute = runScript(createScriptEngine(kernel, modelId), `ifc.get(${wallId}).Colour = 1;`, null);
  assert.match(wrongAttribute.error, /no attribute Colour/);
  ok(true, "schema errors name the missing attributes, unknown classes and unknown attribute names");

  kernel.closeModel(modelId);
  kernel.free();
}

console.log(`PASS ${passed} checks`);
