// SPDX-License-Identifier: Apache-2.0
// The editing session end to end in Node: evaluate, script, snapshot, attribute
// edits, direct refresh, undo and redo, the agent tools over the session, and
// the three.js retained scene consuming the deltas through a THREE stub.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { pavilionIfc } from "../../../viewer/test/fixture.mjs";
import { createEditingSession, createAgentTools, runAgentTurn, toChatTools, toMessagesTools, toolsFor } from "../src/index.js";
import { createRetainedModel } from "../../../adapters/three/src/retained.js";

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

model.dispose();
ok(model.meshCount === 0 && model.geometryCount === 0, "disposing the retained model releases every geometry");
session.close();
kernel.closeModel(modelId);
kernel.free();
console.log(`PASS ${passed} checks`);
