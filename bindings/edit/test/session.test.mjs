// SPDX-License-Identifier: Apache-2.0
// The editing session end to end in Node: evaluate, script, snapshot, attribute
// edits, direct refresh, undo and redo, the agent tools over the session, and
// the three.js retained scene consuming the deltas through a THREE stub.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { pavilionIfc } from "../../../viewer/test/fixture.mjs";
import { createEditingSession, createAgentTools, createModel, createSceneMirror, describeModel, readIgp, runAgentTurn, toChatTools, toMessagesTools, toolsFor, verifyRevision } from "../src/index.js";
import { createRetainedModel } from "../../../adapters/three/src/retained.js";
import { JAVASCRIPT_EXAMPLES } from "../src/examples.js";

let passed = 0;
function ok(condition, label) {
  assert.ok(condition, label);
  passed += 1;
  console.log(`ok    ${label}`);
}

const pkg = fileURLToPath(new URL("../../wasm/pkg-node/tessifc_wasm.js", import.meta.url));
if (!existsSync(pkg)) {
  console.log("skip  build the Node package first (python scripts/build-wasm.py --target both)");
  process.exit(0);
}
const { Kernel } = createRequire(import.meta.url)(pkg);

/** Just enough of three.js for the retained model: geometry, material, mesh and group bookkeeping. */
function stubThree() {
  class BufferAttribute {
    constructor(array, itemSize) {
      this.array = array;
      this.itemSize = itemSize;
    }
  }
  class BufferGeometry {
    constructor() {
      this.attributes = {};
      this.disposed = false;
    }
    setAttribute(name, attribute) {
      this.attributes[name] = attribute;
    }
    setIndex(attribute) {
      this.index = attribute;
    }
    computeVertexNormals() {
      this.normals = true;
    }
    dispose() {
      this.disposed = true;
    }
  }
  class Color {
    constructor(r, g, b) {
      this.r = r;
      this.g = g;
      this.b = b;
    }
  }
  class MeshLambertMaterial {
    constructor(parameters) {
      Object.assign(this, parameters);
      this.disposed = false;
    }
    dispose() {
      this.disposed = true;
    }
  }
  class Matrix4 {
    fromArray(array, offset = 0) {
      this.elements = Array.from(array.subarray(offset, offset + 16));
      return this;
    }
  }
  class Mesh {
    constructor(geometry, material) {
      this.geometry = geometry;
      this.material = material;
      this.matrix = new Matrix4();
      this.visible = true;
      this.userData = {};
    }
  }
  class Group {
    constructor() {
      this.children = [];
    }
    add(child) {
      this.children.push(child);
    }
    remove(child) {
      this.children = this.children.filter((item) => item !== child);
    }
    removeFromParent() {}
    clear() {
      this.children = [];
    }
  }
  return { BufferAttribute, BufferGeometry, Color, MeshLambertMaterial, Mesh, Group, DoubleSide: 2 };
}

const kernel = new Kernel();
const modelId = kernel.openModel(Buffer.from(pavilionIfc()));
const session = createEditingSession(kernel, modelId, { settings: { includeOpenings: true } });
assert.throws(() => session.runScript("print(1)"), /Establish the scene first/);
const initial = session.evaluate();
ok(initial.pack.instances.count === 18 && session.modelOffset.length === 3 && session.nextGeometryId > 0, "evaluate() returns the initial pack and establishes the scene basis");

const THREE = stubThree();
const model = createRetainedModel(THREE, initial.pack);
ok(model.meshCount === 18 && model.productIds().length === 18, "the retained model holds one mesh per placed instance");
const hidden = model.meshesOf(kernel.getIdsOfType(modelId, "IfcOpeningElement")[0]);
ok(hidden.length === 1 && hidden[0].visible === false, "helper geometry starts invisible");
ok(model.geometryCount === initial.pack.geometry.length, "one BufferGeometry per IGP geometry, shared by its instances");

const wallId = kernel.getIdsOfType(modelId, "IfcWall")[0];
const door = session.runScript(`
  const wall = selected;
  const solid = wall.Representation.Representations[0].Items[0];
  const opening = ifc.addBox("IfcOpeningElement", "Door opening", { at: [1, 0, 0], size: [0.9, solid.SweptArea.YDim + 0.1, 2.1], relativeTo: wall });
  const door = ifc.addBox("IfcDoor", "New door", { at: [1, 0, 0], size: [0.9, 0.05, 2.1], relativeTo: wall });
  ifc.void(wall, opening);
  ifc.fill(opening, door);
  ifc.contain(door, ifc.container(wall));
  print(door.id);
`, { ids: [wallId] });
const doorId = Number(door.report.stdout);
ok(door.delta?.kind === "selective" && door.delta.revision === "1" && door.delta.affectedProducts.length === 3 && !door.delta.fullRebuild,
  "a script publishes a selective delta with the wall, the opening and the door");
ok(door.delta.pack.instances.count === 3 && door.delta.hierarchy.nodes.some((node) => node.expressId === doorId), "the delta carries the replacement geometry and the new hierarchy");

let applied = model.applyDelta(door.delta);
ok(applied.removed === 1 && applied.added === 3 && model.meshCount === 20, "applying the delta replaces the wall and adds the opening and the door");
ok(model.meshesOf(doorId).length === 1 && model.meshesOf(doorId)[0].userData.class === "IfcDoor", "the new door is addressable by express id");

const before = model.meshesOf(wallId)[0];
const rename = session.setAttributes([{ expressId: wallId, attribute: "Name", value: "Renamed wall", raw: false }]);
ok(rename.revision === "2" && rename.affectedProducts.length === 0 && rename.metadataProducts.includes(wallId), "attribute edits publish a metadata-only delta");
applied = model.applyDelta(rename);
ok(applied.removed === 0 && applied.added === 0 && model.meshesOf(wallId)[0] === before, "a metadata delta leaves every mesh alone");
ok(session.entity(wallId).fields.find((field) => field.name === "Name").value === "Renamed wall", "the session reads the committed attributes back");

const direct = session.refreshProducts([wallId, doorId]);
ok(direct.kind === "direct" && direct.revision === "2" && direct.pack.instances.count === 2 && direct.emptyProducts.length === 0,
  "refreshProducts re-tessellates named products without a new revision");
applied = model.applyDelta(direct);
ok(applied.removed === 2 && applied.added === 2 && model.meshCount === 20, "a direct refresh swaps exactly those meshes");

const snapshot = new TextDecoder("latin1").decode(session.export()).replace("'Renamed wall'", "'Snapshot wall'");
const external = session.applySnapshot(Buffer.from(snapshot, "latin1"));
ok(external.revision === "3" && external.metadataProducts.includes(wallId), "an external snapshot goes through the same delta path");

const undone = session.undo();
ok(undone.revision === "4" && undone.label === "undo" && session.history.undo === 2 && session.history.redo === 1, "undo republishes the previous source as a new revision");
const redone = session.redo();
ok(redone.revision === "5" && session.history.redo === 0, "redo restores it again");
assert.throws(() => session.redo(), /Nothing to redo/);

const failing = session.runScript("selected.Name = 'x'; nope();", { ids: [wallId] });
ok(!failing.report.ok && failing.delta === null && session.revision === "5", "a failing script publishes nothing");
const readOnly = session.runScript("selected.Name = 'peek'", { ids: [wallId] }, { commit: false });
ok(readOnly.delta === null && readOnly.report.changed === false && session.entity(wallId).fields.find((f) => f.name === "Name").value === "Snapshot wall",
  "a read-only run discards its edits");

// The agent tools over the session, driven by a scripted provider.
const seen = [];
const tools = createAgentTools(session, { policy: "review", selection: { ids: [wallId] }, onDelta: (delta) => seen.push(delta.revision) });
ok(toolsFor("ask").length === 1 && toolsFor("edit").length === 3, "ask mode exposes inspection only");
ok(toChatTools()[0].function.name === "inspect_model" && toMessagesTools()[1].strict === true, "definitions map to both wire shapes");
let turn = 0;
const provider = async ({ tools: offered, messages }) => {
  turn += 1;
  const last = messages[messages.length - 1];
  if (last.role === "user") {
    return { text: "", toolCalls: [{ id: "c1", name: "inspect_model", input: { code: 'print("walls", ifc.byType("IfcWall").length)' } }], stop: "tool_use" };
  }
  const result = last.results[0];
  if (result.name === "inspect_model" && offered.some((tool) => tool.name === "propose_edit")) {
    return { text: "", toolCalls: [{ id: "c2", name: "propose_edit", input: { script: 'selected.Name = "Agent wall"; print("done");', summary: "Renames the wall." } }], stop: "tool_use" };
  }
  return { text: `Answer: ${result.content}`, toolCalls: [], stop: "end_turn" };
};
const asked = await runAgentTurn({ complete: provider, tools, prompt: "How many walls?", mode: "ask", context: "test" });
ok(asked.answer === "Answer: walls 2" && asked.rounds === 2 && asked.proposals.length === 0, "an ask turn inspects and answers without editing");
const edited = await runAgentTurn({ complete: provider, tools, prompt: "Rename the wall", mode: "edit", context: "test" });
ok(edited.proposals.length === 1 && edited.proposals[0].run === null && session.revision === "5", "a review-policy edit records a proposal and changes nothing");
const ran = await tools.run(edited.proposals[0]);
ok(ran.delta.revision === "6" && seen.includes("6") && session.entity(wallId).fields.find((f) => f.name === "Name").value === "Agent wall", "running the proposal publishes it and reports the delta");
const auto = createAgentTools(session, { policy: "auto", selection: { ids: [wallId] } });
const automatic = await runAgentTurn({ complete: provider, tools: auto, prompt: "Rename again", mode: "edit", context: "test" });
ok(automatic.proposals[0].run?.delta?.revision === "7" && /revision/.test(automatic.answer), "the automatic policy runs the proposal inside the turn");
const undoResult = await auto.execute("undo_edit", {});
ok(!undoResult.isError && session.revision === "8", "the undo tool publishes a new revision");

// The loop reports usage and events, accepts one proposal per turn, and can be aborted.
const events = [];
const greedy = async ({ messages }) => {
  const last = messages[messages.length - 1];
  if (last.role === "user") {
    return { text: "", stop: "tool_use", usage: { input: 7, output: 3 }, toolCalls: [
      { id: "p1", name: "propose_edit", input: { script: 'print("one")', summary: "First." } },
      { id: "p2", name: "propose_edit", input: { script: 'print("two")', summary: "Second." } },
    ] };
  }
  return { text: `Results: ${last.results.map((r) => `${r.isError ? "error" : "ok"}`).join(",")}`, toolCalls: [], stop: "end_turn", usage: { input: 5, output: 1 } };
};
const reviewed = createAgentTools(session, { policy: "review" });
const greedyTurn = await runAgentTurn({ complete: greedy, tools: reviewed, prompt: "Two things", mode: "edit", onEvent: (event) => events.push(event.type) });
ok(greedyTurn.proposals.length === 1 && greedyTurn.answer === "Results: ok,error" && greedyTurn.policy === "review",
  "a second proposal in one turn is refused and the executing policy comes from the tools");
ok(greedyTurn.usage.inputTokens === 12 && greedyTurn.usage.outputTokens === 4 && greedyTurn.rounds === 2 && greedyTurn.stop === "end_turn", "usage and the stop reason are summed over the rounds");
ok(events.join(",") === "round,tool,proposal,tool,tool,tool,round,answer", `events follow the rounds, tools and proposals (${events.join(",")})`);
const again = await runAgentTurn({ complete: greedy, tools: reviewed, prompt: "Again", mode: "edit" });
ok(again.proposals.length === 1 && reviewed.proposals.length === 2, "the next turn accepts a proposal again");
const controller = new AbortController();
controller.abort();
await assert.rejects(() => runAgentTurn({ complete: greedy, tools: reviewed, prompt: "x", mode: "ask", signal: controller.signal }), (error) => error.name === "AbortError");
ok(true, "an aborted signal stops the turn before the provider is called");
const cut = await runAgentTurn({ complete: async () => ({ text: "", toolCalls: [], stop: "max_tokens" }), tools: reviewed, prompt: "x" });
ok(cut.answer === "The response was cut short.", "a truncated reply is reported");

const context = describeModel(session, { selection: { ids: [wallId] } });
ok(/^File: model\.ifc \(IFC4\), revision 8, lengths in m\./.test(context) && /Products by class: .*IfcColumn 6/.test(context)
  && /Storeys: #\d+ Ground floor\./.test(context) && /Selected in the viewer \(the `selected` entity\): #\d+ IfcWall with GlobalId=/.test(context),
  "describeModel names the file, schema, revision, unit, classes, storeys and the selection");
ok(describeModel(session).endsWith("Nothing is selected in the viewer."), "without a selection the last line says so");

// A candidate the kernel refuses names the reason and leaves everything as it was.
const rejection = (() => {
  try {
    session.runScript("selected.Representation.Representations[0].Items[0].SweptArea = null;", { ids: [wallId] });
    return null;
  } catch (error) {
    return error;
  }
})();
ok(rejection && /rejected/.test(rejection.message) && rejection.impact?.evaluationAccepted === false && !rejection.committed,
  "a rejected candidate throws with the kernel's report");
ok(session.revision === "8" && kernel.getPreparedRevisionInfo(modelId) === undefined, "the rejected candidate is discarded and the revision stands");

// A failure after the commit reports it, and the history follows the model, not the delta.
const historyBefore = session.history;
const realHierarchy = kernel.getSpatialHierarchy.bind(kernel);
let hierarchyCalls = 0;
kernel.getSpatialHierarchy = (id) => {
  hierarchyCalls += 1;
  if (hierarchyCalls === 1) throw new Error("hierarchy unavailable");
  return realHierarchy(id);
};
const committedFailure = (() => {
  try {
    session.undo();
    return null;
  } catch (error) {
    return error;
  }
})();
delete kernel.getSpatialHierarchy;
ok(committedFailure?.committed === true && committedFailure.revision === "9" && session.revision === "9", "an error after the commit carries the new revision");
ok(session.history.undo === historyBefore.undo - 1 && session.history.redo === historyBefore.redo + 1, "the undo entry is consumed and the redo entry recorded");
const redoAfter = session.redo();
ok(redoAfter.revision === "10" && session.history.redo === historyBefore.redo, "redo after a committed failure restores the content it recorded");

// A bounded history never loses an entry to a failed restore.
const bounded = createEditingSession(kernel, modelId, { settings: { includeOpenings: true }, historyLimit: 1 });
bounded.adopt({ modelOffset: session.modelOffset, nextGeometryId: session.nextGeometryId });
bounded.setAttributes([{ expressId: wallId, attribute: "Name", value: "First", raw: false }]);
bounded.setAttributes([{ expressId: wallId, attribute: "Name", value: "Second", raw: false }]);
kernel.prepareRevision = () => {
  throw new Error("prepare unavailable");
};
assert.throws(() => bounded.undo(), /prepare unavailable/);
delete kernel.prepareRevision;
ok(bounded.history.undo === 1 && bounded.history.redo === 0, "a failed restore keeps the bounded history intact");
const boundedUndo = bounded.undo();
ok(boundedUndo.label === "undo" && bounded.history.undo === 0 && bounded.history.redo === 1
  && session.entity(wallId).fields.find((f) => f.name === "Name").value === "First", "the kept entry restores afterwards");
bounded.close();

model.dispose();
ok(model.meshCount === 0 && model.geometryCount === 0, "disposing the retained model releases every geometry");

// The panel's examples run on the pavilion; the house one builds a complete small building.
const houseExample = JAVASCRIPT_EXAMPLES.find((example) => example.title === "Build a small house");
const houseRun = session.runScript(houseExample.source, { ids: [wallId] });
ok(houseRun.report.ok && houseRun.delta?.affectedProducts.length === 14 && !houseRun.delta.fullRebuild && /built a house/.test(houseRun.report.stdout),
  `the house example adds fourteen products to the pavilion (${houseRun.report.error ?? ""})`);
for (const example of JAVASCRIPT_EXAMPLES) {
  if (example.title === "Build a small house") continue;
  const run = session.runScript(example.source, { ids: [wallId] });
  assert.equal(run.report.ok, true, `${example.title}: ${run.report.error}`);
}
ok(true, "every panel example runs on the pavilion");

// A model from nothing, then a small building through the helpers, in every schema.
const HOUSE = `
  const upper = ifc.addStorey({ name: "Upper floor", elevation: 3 });
  const south = ifc.addWall({ from: [0, 0], to: [6, 0], height: 3, thickness: 0.3, name: "South wall" });
  const east = ifc.addWall({ from: [6, 0], to: [6, 4], height: 3, thickness: 0.3, storey: "Ground floor", name: "East wall" });
  const base = ifc.addSlab({ polygon: [[0, 0], [6, 0], [6, 4], [0, 4]], thickness: 0.2, at: [0, 0, -0.2], type: "BASESLAB", name: "Base slab" });
  const floor = ifc.addSlab({ size: [6, 4], at: [3, 2, 0], thickness: 0.2, storey: upper, name: "Upper slab" });
  const door = ifc.addDoor({ in: south, at: [-1.5, 0, 0], size: [0.9, 2.1], name: "Front door" });
  const window = ifc.addWindow({ in: south, at: [1.5, 0, 0.9], size: [1.2, 1.2], name: "Front window" });
  const column = ifc.addColumn({ at: [1, 3], size: [0.3, 0.3], height: 3, name: "Column" });
  const beam = ifc.addBeam({ from: [1, 3, 3], to: [5, 3, 3], size: [0.3, 0.2], name: "Beam" });
  ifc.addProperties(south, "Pset_WallCommon", { IsExternal: true, FireRating: "REI60", LoadBearing: false });
  ifc.addProperties(south, "Pset_WallCommon", { FireRating: "REI90", Reference: "W1" });
  ifc.setColor(south, [0.8, 0.2, 0.2, 1]);
  ifc.setColor(window, [0.4, 0.6, 0.9, 0.5]);
  print(JSON.stringify({ storeys: ifc.storeys().map((s) => s.Name), wall: south.id, window: window.id, door: door.id, beam: beam.id,
    byName: ifc.byName("IfcWall", "East wall")?.id === east.id, unit: ifc.describe().lengthUnit, container: ifc.container(door)?.Name ?? null }));
`;
for (const schema of ["IFC4", "IFC2X3", "IFC4X3"]) {
  const houseId = kernel.openModel(createModel({ schema, name: "House", storeys: [{ name: "Ground floor", elevation: 0 }], timestamp: "2026-09-14T12:00:00" }));
  const house = createEditingSession(kernel, houseId, { settings: { includeOpenings: true } });
  const empty = house.evaluate();
  ok(empty.pack.instances.count === 0 && JSON.parse(kernel.getModelInfo(houseId)).entities > 0 && house.revision === "0",
    `${schema}: createModel opens as an empty scene`);
  const mirror = createSceneMirror(empty.pack);
  const built = house.runScript(HOUSE);
  assert.equal(built.report.ok, true, built.report.error);
  mirror.applyDelta(built.delta);
  const second = house.runScript('ifc.byName("IfcWall", "South wall").Representation.Representations[0].Items[0].Depth = 3.5;');
  mirror.applyDelta(second.delta);
  const verified = verifyRevision({ Kernel, session: house, mirror });
  ok(verified.ok && verified.products === 10 && verified.revision === "2", `${schema}: the mirror of two deltas matches a fresh evaluation (${JSON.stringify(verified.mismatches)})`);
  house.undo();
  const printed = JSON.parse(built.report.stdout);
  ok(built.delta.kind === "selective" && built.delta.revision === "1" && !built.delta.fullRebuild && house.revision === "3", `${schema}: the house publishes one selective revision`);
  ok(built.delta.pack.instances.count === 10, `${schema}: ten placed products (2 walls, 2 slabs, door, window, their openings, column, beam)`);
  ok(printed.storeys.join(",") === "Ground floor,Upper floor" && printed.byName && printed.unit === "m" && printed.container === "Ground floor",
    `${schema}: storeys, byName, describe and containment answer`);
  const fresh = new Kernel();
  const freshId = fresh.openModel(house.export());
  fresh.evaluateGeometry(freshId, JSON.stringify({ includeOpenings: true }));
  const freshPack = readIgp(fresh.takePack(freshId));
  ok(freshPack.instances.count === 10 && JSON.parse(fresh.getDiagnostics(freshId)).every((d) => d.severity !== "error"),
    `${schema}: the exported file reopens with the same products and no parse errors`);
  fresh.free();
  const psets = house.idsOfType("IfcPropertySet");
  const values = house.idsOfType("IfcPropertySingleValue").map((id) => house.entity(id).fields.find((f) => f.name === "Name").value).sort();
  ok(psets.length === 1 && values.join(",") === "FireRating,IsExternal,LoadBearing,Reference", `${schema}: addProperties extends the existing set`);
  const record = Array.from(built.delta.pack.instances.expressIds).indexOf(printed.wall);
  const colour = Array.from(built.delta.pack.instances.colors.slice(record * 4, record * 4 + 4));
  ok(colour[0] === 204 && colour[1] === 51 && colour[3] === 255, `${schema}: setColor reaches the pack (${colour})`);
  const beamFields = house.entity(printed.beam).fields;
  ok(beamFields.some((f) => f.name === "Name" && f.value === "Beam"), `${schema}: the beam is addressable`);
  house.close();
  kernel.closeModel(houseId);
}
session.close();
kernel.closeModel(modelId);
kernel.free();
console.log(`PASS ${passed} checks`);
