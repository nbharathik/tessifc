// SPDX-License-Identifier: Apache-2.0

//! The house the demo builds: a two-storey block, 10 by 8 metres, one script
//! per step. The same steps drive the deterministic build, the agent brief
//! and the test.

/** Options for `createModel`. */
export const MODEL = {
  schema: "IFC4",
  name: "Demo house",
  site: "Demo site",
  building: "Demo house",
  storeys: [{ name: "Ground floor", elevation: 0 }, { name: "Upper floor", elevation: 3 }],
};

const FOOTPRINT = "[[-0.15, -0.15], [10.15, -0.15], [10.15, 8.15], [-0.15, 8.15]]";

function exteriorWalls(storey) {
  return `
const storey = ${JSON.stringify(storey)};
const suffix = storey === "Ground floor" ? "" : " (upper)";
const walls = [
  ifc.addWall({ from: [0, 0], to: [10, 0], height: 2.8, thickness: 0.3, storey, name: "Exterior wall south" + suffix }),
  ifc.addWall({ from: [10, 0], to: [10, 8], height: 2.8, thickness: 0.3, storey, name: "Exterior wall east" + suffix }),
  ifc.addWall({ from: [10, 8], to: [0, 8], height: 2.8, thickness: 0.3, storey, name: "Exterior wall north" + suffix }),
  ifc.addWall({ from: [0, 8], to: [0, 0], height: 2.8, thickness: 0.3, storey, name: "Exterior wall west" + suffix }),
];
print("added", walls.length, "exterior walls on", storey);`;
}

/** The steps, in order; every script is complete on its own. */
export const STEPS = [
  { title: "Ground floor exterior walls", script: exteriorWalls("Ground floor"), affected: 4 },
  {
    title: "Base slab",
    script: `const slab = ifc.addSlab({ polygon: ${FOOTPRINT}, thickness: 0.2, at: [0, 0, -0.2], type: "BASESLAB", storey: "Ground floor", name: "Base slab" });
print("added", slab.Name);`,
    affected: 1,
  },
  {
    title: "Interior walls",
    script: `const a = ifc.addWall({ from: [5, 0.15], to: [5, 7.85], height: 2.8, thickness: 0.12, storey: "Ground floor", name: "Interior wall 1" });
const b = ifc.addWall({ from: [5.06, 4], to: [9.85, 4], height: 2.8, thickness: 0.12, storey: "Ground floor", name: "Interior wall 2" });
print("added", a.Name, "and", b.Name);`,
    affected: 2,
  },
  {
    title: "Doors",
    script: `const south = ifc.byName("IfcWall", "Exterior wall south");
const interior = ifc.byName("IfcWall", "Interior wall 1");
const entrance = ifc.addDoor({ in: south, at: [-2, 0, 0], size: [1, 2.1], name: "Entrance door" });
const inner = ifc.addDoor({ in: interior, at: [1.5, 0, 0], size: [0.9, 2.1], name: "Interior door" });
print("added", entrance.Name, "and", inner.Name);`,
    affected: 6,
  },
  {
    title: "Ground floor windows",
    script: `const windows = [
  ifc.addWindow({ in: ifc.byName("IfcWall", "Exterior wall south"), at: [2.5, 0, 0.9], size: [1.5, 1.2], name: "South window" }),
  ifc.addWindow({ in: ifc.byName("IfcWall", "Exterior wall east"), at: [-1.5, 0, 0.9], size: [1.2, 1.2], name: "East window" }),
  ifc.addWindow({ in: ifc.byName("IfcWall", "Exterior wall north"), at: [2, 0, 0.9], size: [1.8, 1.2], name: "North window" }),
];
print("added", windows.length, "windows");`,
    affected: 9,
  },
  {
    title: "Upper floor slab",
    script: `const slab = ifc.addSlab({ polygon: ${FOOTPRINT}, thickness: 0.2, at: [0, 0, -0.2], type: "FLOOR", storey: "Upper floor", name: "Upper floor slab" });
print("added", slab.Name);`,
    affected: 1,
  },
  {
    title: "Upper floor walls and windows",
    script: `${exteriorWalls("Upper floor")}
const windows = [
  ifc.addWindow({ in: walls[0], at: [-2.5, 0, 0.9], size: [1.5, 1.2], name: "South window (upper)" }),
  ifc.addWindow({ in: walls[0], at: [2.5, 0, 0.9], size: [1.5, 1.2], name: "South window 2 (upper)" }),
  ifc.addWindow({ in: walls[2], at: [0, 0, 0.9], size: [1.8, 1.2], name: "North window (upper)" }),
];
print("added", windows.length, "windows upstairs");`,
    affected: 10,
  },
  {
    title: "Roof slab",
    script: `const roof = ifc.addSlab({ polygon: ${FOOTPRINT}, thickness: 0.25, at: [0, 0, 2.8], type: "ROOF", storey: "Upper floor", name: "Roof" });
print("added", roof.Name);`,
    affected: 1,
  },
  {
    title: "Columns and a beam",
    script: `const columns = [
  ifc.addColumn({ at: [2.5, 4], size: [0.3, 0.3], height: 2.8, storey: "Ground floor", name: "Column A" }),
  ifc.addColumn({ at: [7.5, 4], size: [0.3, 0.3], height: 2.8, storey: "Ground floor", name: "Column B" }),
];
const beam = ifc.addBeam({ from: [2.5, 4, 2.65], to: [7.5, 4, 2.65], size: [0.3, 0.2], storey: "Ground floor", name: "Beam AB" });
print("added", columns.length, "columns and", beam.Name);`,
    affected: 3,
  },
  {
    title: "Property sets",
    script: `let sets = 0;
for (const wall of ifc.byType("IfcWall")) {
  const external = wall.Name.startsWith("Exterior");
  ifc.addProperties(wall, "Pset_WallCommon", { IsExternal: external, LoadBearing: external, FireRating: external ? "REI60" : "REI30" });
  sets += 1;
}
for (const slab of ifc.byType("IfcSlab")) {
  ifc.addProperties(slab, "Pset_SlabCommon", { IsExternal: slab.Name === "Roof" || slab.Name === "Base slab", LoadBearing: true });
  sets += 1;
}
for (const door of ifc.byType("IfcDoor")) {
  ifc.addProperties(door, "Pset_DoorCommon", { IsExternal: door.Name === "Entrance door", FireRating: "none" });
  sets += 1;
}
print("wrote", sets, "property sets");`,
    affected: 0,
  },
  {
    title: "Colours",
    script: `let coloured = 0;
for (const wall of ifc.byType("IfcWall")) { ifc.setColor(wall, wall.Name.startsWith("Exterior") ? [0.85, 0.8, 0.7] : [0.95, 0.95, 0.9]); coloured += 1; }
for (const slab of ifc.byType("IfcSlab")) { ifc.setColor(slab, slab.Name === "Roof" ? [0.55, 0.25, 0.2] : [0.6, 0.6, 0.62]); coloured += 1; }
for (const window of ifc.byType("IfcWindow")) { ifc.setColor(window, [0.4, 0.6, 0.9, 0.5]); coloured += 1; }
for (const door of ifc.byType("IfcDoor")) { ifc.setColor(door, [0.5, 0.3, 0.15]); coloured += 1; }
for (const item of [...ifc.byType("IfcColumn"), ...ifc.byType("IfcBeam")]) { ifc.setColor(item, [0.35, 0.35, 0.38]); coloured += 1; }
print("coloured", coloured, "products");`,
    affected: 24,
  },
];

/** Product counts of the finished house, by class. */
export const EXPECTED = {
  IfcWall: 10,
  IfcSlab: 3,
  IfcDoor: 2,
  IfcWindow: 6,
  IfcOpeningElement: 8,
  IfcColumn: 2,
  IfcBeam: 1,
};

/** Every product with geometry, the sum of `EXPECTED`. */
export const EXPECTED_TOTAL = Object.values(EXPECTED).reduce((sum, value) => sum + value, 0);
