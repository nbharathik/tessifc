// SPDX-License-Identifier: Apache-2.0

//! Ready-to-run scripts for the session panel, one set per engine.

export { JAVASCRIPT_EXAMPLES } from "../../bindings/edit/src/examples.js";

export const PYTHON_EXAMPLES = [
  {
    title: "Add a door to a wall",
    source: `# Cuts an opening into the selected wall (or the first wall) and fills it with a door.
wall = selected if selected is not None and selected.is_a("IfcWall") else model.by_type("IfcWall")[0]
storey = element.get_container(wall) or model.by_type("IfcBuildingStorey")[0]
context = [c for c in model.by_type("IfcGeometricRepresentationContext") if c.is_a() == "IfcGeometricRepresentationContext"][0]
solid = wall.Representation.Representations[0].Items[0]
thickness = solid.SweptArea.YDim if solid.is_a("IfcExtrudedAreaSolid") and solid.SweptArea.is_a("IfcRectangleProfileDef") else 0.3

def box(ifc_class, name, x, width, depth, height):
    point = model.create_entity("IfcCartesianPoint", Coordinates=(float(x), 0.0, 0.0))
    axes = model.create_entity("IfcAxis2Placement3D", Location=point)
    placement = model.create_entity("IfcLocalPlacement", PlacementRelTo=wall.ObjectPlacement, RelativePlacement=axes)
    profile = model.create_entity("IfcRectangleProfileDef", ProfileType="AREA", XDim=float(width), YDim=float(depth))
    direction = model.create_entity("IfcDirection", DirectionRatios=(0.0, 0.0, 1.0))
    shape_solid = model.create_entity("IfcExtrudedAreaSolid", SweptArea=profile, ExtrudedDirection=direction, Depth=float(height))
    shape = model.create_entity("IfcShapeRepresentation", ContextOfItems=context, RepresentationIdentifier="Body",
                                RepresentationType="SweptSolid", Items=[shape_solid])
    representation = model.create_entity("IfcProductDefinitionShape", Representations=[shape])
    return model.create_entity(ifc_class, GlobalId=guid.new(), Name=name, ObjectPlacement=placement, Representation=representation)

opening = box("IfcOpeningElement", "Door opening", 1.0, 0.9, thickness + 0.1, 2.1)
door = box("IfcDoor", "New door", 1.0, 0.9, 0.05, 2.1)
model.create_entity("IfcRelVoidsElement", GlobalId=guid.new(), RelatingBuildingElement=wall, RelatedOpeningElement=opening)
model.create_entity("IfcRelFillsElement", GlobalId=guid.new(), RelatingOpeningElement=opening, RelatedBuildingElement=door)
rel = next((r for r in model.by_type("IfcRelContainedInSpatialStructure") if r.RelatingStructure == storey), None)
if rel is not None:
    rel.RelatedElements = list(rel.RelatedElements) + [door]
else:
    model.create_entity("IfcRelContainedInSpatialStructure", GlobalId=guid.new(), RelatedElements=[door], RelatingStructure=storey)
print("added", door, "into", wall.Name)
`,
  },
  {
    title: "Raise the selected wall",
    source: `# Makes the selected extrusion (or the first wall) half a unit taller.
target = selected or model.by_type("IfcWall")[0]
solid = target.Representation.Representations[0].Items[0]
solid.Depth = solid.Depth + 0.5
print("raised", target.Name, "to", solid.Depth)
`,
  },
  {
    title: "Move the selection",
    source: `# Shifts the selected product by one unit along x through its placement point.
target = selected or model.by_type("IfcWall")[0]
point = target.ObjectPlacement.RelativePlacement.Location
x, y, z = point.Coordinates
point.Coordinates = (x + 1.0, y, z)
print("moved", target.Name, "to", point.Coordinates)
`,
  },
  {
    title: "Rename the selection",
    source: `# Names are metadata: the viewer updates its tree without tessellating anything.
target = selected or model.by_type("IfcWall")[0]
target.Name = (target.Name or "Element") + " (renamed)"
print("renamed", target.GlobalId, "to", target.Name)
`,
  },
  {
    title: "Delete the selection",
    source: `# Removes the selected product; IfcOpenShell detaches its relationships.
if selected is None:
    raise ValueError("Select an element first")
print("removing", selected.Name)
model.remove(selected)
`,
  },
  {
    title: "List walls (read only)",
    source: `# Prints without changing anything; no revision is published.
for wall in model.by_type("IfcWall"):
    solid = wall.Representation.Representations[0].Items[0] if wall.Representation else None
    container = element.get_container(wall)
    print(wall.id(), wall.Name, "height", getattr(solid, "Depth", "?"), "in", container.Name if container else "no storey")
`,
  },
];
