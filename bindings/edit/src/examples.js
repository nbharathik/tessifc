// SPDX-License-Identifier: Apache-2.0

//! Ready-to-run JavaScript scripts for the session panel and the MCP server.

export const JAVASCRIPT_EXAMPLES = [
  {
    title: "Add a door to a wall",
    source: `// Cuts an opening into the selected wall (or the first wall) and fills it with a door.
const wall = selected?.is("IfcWall") ? selected : ifc.byType("IfcWall")[0];
if (!wall) throw new Error("The model has no wall");
const storey = ifc.container(wall) ?? ifc.byType("IfcBuildingStorey")[0];
const solid = wall.Representation?.Representations?.[0]?.Items?.[0];
const thickness = solid?.SweptArea?.YDim ?? 0.3;

// Local coordinates of the wall: x along its length, y across, z up.
const opening = ifc.addBox("IfcOpeningElement", "Door opening",
  { at: [1.0, 0, 0], size: [0.9, thickness + 0.1, 2.1], relativeTo: wall });
const door = ifc.addBox("IfcDoor", "New door",
  { at: [1.0, 0, 0], size: [0.9, 0.05, 2.1], relativeTo: wall,
    attributes: { OverallHeight: 2.1, OverallWidth: 0.9 } });
ifc.void(wall, opening);
ifc.fill(opening, door);
if (storey) ifc.contain(door, storey);
print("added", door, "into", wall.Name);
`,
  },
  {
    title: "Raise the selected wall",
    source: `// Makes the selected extrusion (or the first wall) half a unit taller.
const target = selected ?? ifc.byType("IfcWall")[0];
const solid = target.Representation.Representations[0].Items[0];
if (solid.type !== "IfcExtrudedAreaSolid") throw new Error(\`\${target.Name} is not a simple extrusion\`);
solid.Depth = solid.Depth + 0.5;
print("raised", target.Name, "to", solid.Depth);
`,
  },
  {
    title: "Move the selection",
    source: `// Shifts the selected product by one unit along x through its placement point.
const target = selected ?? ifc.byType("IfcColumn")[0] ?? ifc.byType("IfcWall")[0];
const point = target.ObjectPlacement.RelativePlacement.Location;
const [x, y, z] = point.Coordinates;
point.Coordinates = [x + 1, y, z];
print("moved", target.Name, "to", point.Coordinates);
`,
  },
  {
    title: "Add a column",
    source: `// Places a new 0.3 x 0.3 x 3 column at the origin of the first storey.
const storey = ifc.byType("IfcBuildingStorey")[0];
const column = ifc.addBox("IfcColumn", "Scripted column", { at: [0.5, 0.5, 0], size: [0.3, 0.3, 3] });
if (storey) ifc.contain(column, storey);
print("added", column);
`,
  },
  {
    title: "Rename the selection",
    source: `// Names are metadata: the viewer updates its tree without tessellating anything.
const target = selected ?? ifc.byType("IfcWall")[0];
target.Name = (target.Name ?? "Element") + " (renamed)";
target.Description = "Edited in the TessIFC session panel";
print("renamed", target, "to", target.Name);
`,
  },
  {
    title: "Delete the selection",
    source: `// Removes the selected product and every relationship that referenced it.
if (!selected) throw new Error("Select an element first");
print("removing", selected, selected.Name);
ifc.remove(selected);
`,
  },
  {
    title: "Build a small house",
    source: `// A 6 by 4 metre house at x = 20 on the lowest storey: walls, slabs, a door, windows, a column, a beam.
const x0 = 20;
const storey = ifc.storeys()[0];
if (!storey) throw new Error("The model has no storey; add one with ifc.addStorey");
const corners = [[x0, 0], [x0 + 6, 0], [x0 + 6, 4], [x0, 4]];
const walls = corners.map((from, i) => ifc.addWall({ from, to: corners[(i + 1) % 4], height: 2.8, thickness: 0.3, storey, name: \`House wall \${i + 1}\` }));
ifc.addSlab({ polygon: corners, thickness: 0.2, at: [0, 0, -0.2], type: "BASESLAB", storey, name: "House base slab" });
ifc.addSlab({ polygon: corners, thickness: 0.25, at: [0, 0, 2.8], type: "ROOF", storey, name: "House roof" });
const door = ifc.addDoor({ in: walls[0], at: [-1.5, 0, 0], size: [0.9, 2.1], name: "House door" });
const windows = [ifc.addWindow({ in: walls[0], at: [1.5, 0, 0.9], name: "House window 1" }), ifc.addWindow({ in: walls[2], at: [0, 0, 0.9], name: "House window 2" })];
const column = ifc.addColumn({ at: [x0 + 3, 2], size: [0.25, 0.25], height: 2.8, storey, name: "House column" });
ifc.addBeam({ from: [x0 + 0.3, 2, 2.6], to: [x0 + 5.7, 2, 2.6], size: [0.2, 0.15], storey, name: "House beam" });
for (const wall of walls) {
  ifc.addProperties(wall, "Pset_WallCommon", { IsExternal: true, LoadBearing: true });
  ifc.setColor(wall, [0.85, 0.8, 0.7]);
}
for (const window of windows) ifc.setColor(window, [0.4, 0.6, 0.9, 0.5]);
ifc.setColor(door, [0.5, 0.3, 0.15]);
ifc.setColor(column, [0.35, 0.35, 0.38]);
print("built a house from", walls.length, "walls, a door and", windows.length, "windows in", storey.Name);
`,
  },
  {
    title: "List walls (read only)",
    source: `// Prints without changing anything; no revision is published.
for (const wall of ifc.byType("IfcWall")) {
  const solid = wall.Representation?.Representations?.[0]?.Items?.[0];
  print(wall, wall.Name, "height", solid?.Depth ?? "?", "in", ifc.container(wall)?.Name ?? "no storey");
}
`,
  },
];
