# SPDX-License-Identifier: Apache-2.0
"""Ready-made Python scripts for the session panel and the MCP server (kept in step with the viewer's script-examples.js)."""

PYTHON_EXAMPLES = [
    {
        "title": "Add a door to a wall",
        "source": "# Cuts an opening into the selected wall (or the first wall) and fills it with a door.\nwall = selected if selected is not None and selected.is_a(\"IfcWall\") else model.by_type(\"IfcWall\")[0]\nstorey = element.get_container(wall) or model.by_type(\"IfcBuildingStorey\")[0]\ncontext = [c for c in model.by_type(\"IfcGeometricRepresentationContext\") if c.is_a() == \"IfcGeometricRepresentationContext\"][0]\nsolid = wall.Representation.Representations[0].Items[0]\nthickness = solid.SweptArea.YDim if solid.is_a(\"IfcExtrudedAreaSolid\") and solid.SweptArea.is_a(\"IfcRectangleProfileDef\") else 0.3\n\ndef box(ifc_class, name, x, width, depth, height):\n    point = model.create_entity(\"IfcCartesianPoint\", Coordinates=(float(x), 0.0, 0.0))\n    axes = model.create_entity(\"IfcAxis2Placement3D\", Location=point)\n    placement = model.create_entity(\"IfcLocalPlacement\", PlacementRelTo=wall.ObjectPlacement, RelativePlacement=axes)\n    profile = model.create_entity(\"IfcRectangleProfileDef\", ProfileType=\"AREA\", XDim=float(width), YDim=float(depth))\n    direction = model.create_entity(\"IfcDirection\", DirectionRatios=(0.0, 0.0, 1.0))\n    shape_solid = model.create_entity(\"IfcExtrudedAreaSolid\", SweptArea=profile, ExtrudedDirection=direction, Depth=float(height))\n    shape = model.create_entity(\"IfcShapeRepresentation\", ContextOfItems=context, RepresentationIdentifier=\"Body\",\n                                RepresentationType=\"SweptSolid\", Items=[shape_solid])\n    representation = model.create_entity(\"IfcProductDefinitionShape\", Representations=[shape])\n    return model.create_entity(ifc_class, GlobalId=guid.new(), Name=name, ObjectPlacement=placement, Representation=representation)\n\nopening = box(\"IfcOpeningElement\", \"Door opening\", 1.0, 0.9, thickness + 0.1, 2.1)\ndoor = box(\"IfcDoor\", \"New door\", 1.0, 0.9, 0.05, 2.1)\nmodel.create_entity(\"IfcRelVoidsElement\", GlobalId=guid.new(), RelatingBuildingElement=wall, RelatedOpeningElement=opening)\nmodel.create_entity(\"IfcRelFillsElement\", GlobalId=guid.new(), RelatingOpeningElement=opening, RelatedBuildingElement=door)\nrel = next((r for r in model.by_type(\"IfcRelContainedInSpatialStructure\") if r.RelatingStructure == storey), None)\nif rel is not None:\n    rel.RelatedElements = list(rel.RelatedElements) + [door]\nelse:\n    model.create_entity(\"IfcRelContainedInSpatialStructure\", GlobalId=guid.new(), RelatedElements=[door], RelatingStructure=storey)\nprint(\"added\", door, \"into\", wall.Name)\n",
    },
    {
        "title": "Raise the selected wall",
        "source": "# Makes the selected extrusion (or the first wall) half a unit taller.\ntarget = selected or model.by_type(\"IfcWall\")[0]\nsolid = target.Representation.Representations[0].Items[0]\nsolid.Depth = solid.Depth + 0.5\nprint(\"raised\", target.Name, \"to\", solid.Depth)\n",
    },
    {
        "title": "Move the selection",
        "source": "# Shifts the selected product by one unit along x through its placement point.\ntarget = selected or model.by_type(\"IfcWall\")[0]\npoint = target.ObjectPlacement.RelativePlacement.Location\nx, y, z = point.Coordinates\npoint.Coordinates = (x + 1.0, y, z)\nprint(\"moved\", target.Name, \"to\", point.Coordinates)\n",
    },
    {
        "title": "Rename the selection",
        "source": "# Names are metadata: the viewer updates its tree without tessellating anything.\ntarget = selected or model.by_type(\"IfcWall\")[0]\ntarget.Name = (target.Name or \"Element\") + \" (renamed)\"\nprint(\"renamed\", target.GlobalId, \"to\", target.Name)\n",
    },
    {
        "title": "Delete the selection",
        "source": "# Removes the selected product; IfcOpenShell detaches its relationships.\nif selected is None:\n    raise ValueError(\"Select an element first\")\nprint(\"removing\", selected.Name)\nmodel.remove(selected)\n",
    },
    {
        "title": "List walls (read only)",
        "source": "# Prints without changing anything; no revision is published.\nfor wall in model.by_type(\"IfcWall\"):\n    solid = wall.Representation.Representations[0].Items[0] if wall.Representation else None\n    container = element.get_container(wall)\n    print(wall.id(), wall.Name, \"height\", getattr(solid, \"Depth\", \"?\"), \"in\", container.Name if container else \"no storey\")\n",
    },
]
