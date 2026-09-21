# SPDX-License-Identifier: Apache-2.0
"""Create and edit a small demonstration IFC using an installed IfcOpenShell."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import tempfile

import ifcopenshell
import ifcopenshell.guid


def box(model, context, tag, name, x, width, thickness, height, ifc_class="IfcWall"):
    """Create a placed rectangular extrusion with an independent profile."""
    point = model.create_entity("IfcCartesianPoint", Coordinates=(float(x), 0.0, 0.0))
    axes = model.create_entity("IfcAxis2Placement3D", Location=point)
    placement = model.create_entity("IfcLocalPlacement", RelativePlacement=axes)
    profile = model.create_entity("IfcRectangleProfileDef", ProfileType="AREA", XDim=float(width), YDim=float(thickness))
    direction = model.create_entity("IfcDirection", DirectionRatios=(0.0, 0.0, 1.0))
    solid = model.create_entity("IfcExtrudedAreaSolid", SweptArea=profile, ExtrudedDirection=direction, Depth=float(height))
    shape = model.create_entity("IfcShapeRepresentation", ContextOfItems=context, RepresentationIdentifier="Body",
                                RepresentationType="SweptSolid", Items=[solid])
    representation = model.create_entity("IfcProductDefinitionShape", Representations=[shape])
    return model.create_entity(ifc_class, GlobalId=ifcopenshell.guid.new(), Name=name, Tag=tag,
                               ObjectPlacement=placement, Representation=representation)


def create_model():
    """Build first-party geometry suitable for selective edit demonstrations."""
    model = ifcopenshell.file(schema="IFC4")
    origin = model.create_entity("IfcCartesianPoint", Coordinates=(0.0, 0.0, 0.0))
    axes = model.create_entity("IfcAxis2Placement3D", Location=origin)
    context = model.create_entity("IfcGeometricRepresentationContext", ContextType="Model",
                                  CoordinateSpaceDimension=3, Precision=0.00001, WorldCoordinateSystem=axes)
    metre = model.create_entity("IfcSIUnit", UnitType="LENGTHUNIT", Name="METRE")
    units = model.create_entity("IfcUnitAssignment", Units=[metre])
    project = model.create_entity("IfcProject", GlobalId=ifcopenshell.guid.new(), Name="Incremental editing", UnitsInContext=units,
                                  RepresentationContexts=[context])
    storey = model.create_entity("IfcBuildingStorey", GlobalId=ifcopenshell.guid.new(), Name="Demo level", Elevation=0.0)
    model.create_entity("IfcRelAggregates", GlobalId=ifcopenshell.guid.new(), RelatingObject=project, RelatedObjects=[storey])
    wall = box(model, context, "demo-wall", "Editable wall", 0, 5, 0.3, 3)
    neighbor = box(model, context, "demo-neighbor", "Unchanged wall", 8, 5, 0.3, 3)
    opening = box(model, context, "demo-opening", "Opening", 0, 1, 0.6, 2, "IfcOpeningElement")
    model.create_entity("IfcRelVoidsElement", GlobalId=ifcopenshell.guid.new(), RelatingBuildingElement=wall, RelatedOpeningElement=opening)
    model.create_entity("IfcRelContainedInSpatialStructure", GlobalId=ifcopenshell.guid.new(),
                        RelatingStructure=storey, RelatedElements=[wall, neighbor])
    return model


def edit_model(model, action):
    """Apply one deterministic operation; support entities are changed directly."""
    tagged = {element.Tag: element for element in model.by_type("IfcElement") if element.Tag}
    wall = tagged["demo-wall"]
    if action == "raise":
        wall.Representation.Representations[0].Items[0].Depth += 0.5
    elif action == "move":
        point = tagged["demo-neighbor"].ObjectPlacement.RelativePlacement.Location
        x, y, z = point.Coordinates
        point.Coordinates = (x, y + 1.0, z)
    elif action == "opening":
        solid = tagged["demo-opening"].Representation.Representations[0].Items[0]
        solid.SweptArea.XDim += 0.25
    elif action == "rename":
        wall.Name = "Renamed wall" if wall.Name != "Renamed wall" else "Editable wall"
    elif action == "create":
        if "demo-added" in tagged:
            raise ValueError("The demonstration column already exists; delete it before creating another.")
        context = model.by_type("IfcGeometricRepresentationContext")[0]
        column = box(model, context, "demo-added", "Added column", 4, 0.4, 0.4, 3.5, "IfcColumn")
        relation = model.by_type("IfcRelContainedInSpatialStructure")[0]
        relation.RelatedElements = (*relation.RelatedElements, column)
    elif action == "delete":
        if "demo-added" not in tagged:
            raise ValueError("Create the demonstration column before deleting it.")
        model.remove(tagged["demo-added"])


def save_atomic(model, destination: Path):
    """Expose a complete revision with one replacement of the destination."""
    destination = destination.resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix=".tessifc-", suffix=".ifc", dir=destination.parent)
    os.close(fd)
    temporary = Path(name)
    try:
        model.write(str(temporary))
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ifc", type=Path)
    parser.add_argument("action", choices=["init", "raise", "move", "opening", "rename", "create", "delete"])
    args = parser.parse_args()
    if args.action == "init":
        if args.ifc.exists():
            parser.error("Initialization requires a new path; the existing file was not overwritten.")
        model = create_model()
    else:
        model = ifcopenshell.open(str(args.ifc))
        edit_model(model, args.action)
    save_atomic(model, args.ifc)
    print(f"Saved {args.action}: {args.ifc}")


if __name__ == "__main__":
    main()
