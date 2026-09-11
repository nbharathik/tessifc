// SPDX-License-Identifier: Apache-2.0
// The browser script engine against the Node kernel build: STEP values, the
// entity API, snapshots the kernel accepts, and error reporting.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { pavilionIfc } from "../../../viewer/test/fixture.mjs";
import {
  Enum, Ref, Typed, createScriptEngine, decodeStepString, encodeStepString, formatValue, newGuid, parseValue, runScript, splitArguments,
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
