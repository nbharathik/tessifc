# SPDX-License-Identifier: Apache-2.0
"""Build the demo house with IfcOpenShell, saving after every step so a viewer that follows
the file shows it grow. With --serve the session server is started beside it.

    python examples/agent-building/build_house.py [house.ifc] [--serve] [--port 8000] [--schema IFC4] [--pause 1.5]
"""

from __future__ import annotations

import argparse
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "adapters" / "ifcopenshell"))

from tessifc_session.model import create_model, owner_scope, write_model  # noqa: E402

EXPECTED = {"IfcWall": 10, "IfcSlab": 3, "IfcDoor": 2, "IfcWindow": 6, "IfcOpeningElement": 8, "IfcColumn": 2, "IfcBeam": 1}
FOOTPRINT = [(-0.15, -0.15), (10.15, -0.15), (10.15, 8.15), (-0.15, 8.15)]


class House:
    """The steps, each a method that returns a short description of what it added."""

    def __init__(self, model):
        import ifcopenshell.api
        import ifcopenshell.util.element

        self.model = model
        self.api = ifcopenshell.api
        self.element = ifcopenshell.util.element
        self.body = next(c for c in model.by_type("IfcGeometricRepresentationSubContext") if c.ContextIdentifier == "Body")
        self.storeys = {s.Name: s for s in model.by_type("IfcBuildingStorey")}

    # ------------------------------------------------------------ helpers

    def placement(self, product, x=0.0, y=0.0, z=0.0, angle=0.0, relative_to=None):
        import math

        c, s = math.cos(angle), math.sin(angle)
        matrix = [[c, -s, 0.0, x], [s, c, 0.0, y], [0.0, 0.0, 1.0, z], [0.0, 0.0, 0.0, 1.0]]
        self.api.run("geometry.edit_object_placement", self.model, product=product, matrix=matrix,
                     is_si=True, should_transform_children=False)
        if relative_to is not None:
            product.ObjectPlacement.PlacementRelTo = relative_to.ObjectPlacement

    def box(self, ifc_class, name, storey, *, at, size, angle=0.0, relative_to=None):
        """A rectangular extrusion centred on x/y at `at`, rising from z, like the JavaScript helper."""
        import ifcopenshell.util.shape_builder

        product = self.api.run("root.create_entity", self.model, ifc_class=ifc_class, name=name)
        self.api.run("spatial.assign_container", self.model, products=[product], relating_structure=self.storeys[storey])
        self.placement(product, *at, angle=angle, relative_to=relative_to)
        builder = ifcopenshell.util.shape_builder.ShapeBuilder(self.model)
        width, depth, height = size
        profile = builder.rectangle(size=(width, depth), position=(-width / 2, -depth / 2))
        solid = builder.extrude(profile, magnitude=height)
        shape = builder.get_representation(self.body, [solid])
        self.api.run("geometry.assign_representation", self.model, product=product, representation=shape)
        return product

    def wall(self, name, storey, start, end, height=2.8, thickness=0.3):
        import math

        (x1, y1), (x2, y2) = start, end
        length = math.hypot(x2 - x1, y2 - y1)
        angle = math.atan2(y2 - y1, x2 - x1)
        return self.box("IfcWall", name, storey, at=((x1 + x2) / 2, (y1 + y2) / 2, 0.0), size=(length, thickness, height), angle=angle)

    def slab(self, name, storey, polygon, thickness, z, predefined):
        import ifcopenshell.util.shape_builder

        product = self.api.run("root.create_entity", self.model, ifc_class="IfcSlab", name=name, predefined_type=predefined)
        self.api.run("spatial.assign_container", self.model, products=[product], relating_structure=self.storeys[storey])
        self.placement(product, 0.0, 0.0, z)
        builder = ifcopenshell.util.shape_builder.ShapeBuilder(self.model)
        outline = builder.polyline([(float(x), float(y)) for x, y in polygon], closed=True)
        solid = builder.extrude(builder.profile(outline), magnitude=thickness)
        self.api.run("geometry.assign_representation", self.model, product=product,
                     representation=builder.get_representation(self.body, [solid]))
        return product

    def filling(self, ifc_class, name, host, *, along, sill, size):
        width, height = size
        thickness = 0.3
        opening = self.box("IfcOpeningElement", f"{name} opening", self.element.get_container(host).Name,
                           at=(along, 0.0, sill), size=(width, thickness + 0.1, height), relative_to=host)
        self.api.run("feature.add_feature", self.model, feature=opening, element=host)
        product = self.box(ifc_class, name, self.element.get_container(host).Name, at=(along, 0.0, sill), size=(width, 0.05, height), relative_to=host)
        rel = self.api.run("feature.add_filling", self.model, opening=opening, element=product)
        if self.model.schema == "IFC2X3" and rel.OwnerHistory is None:
            # add_filling leaves the history empty; IFC2X3 requires one.
            rel.OwnerHistory = self.api.run("owner.create_owner_history", self.model)
        return product

    def colour(self, product, rgb, alpha=1.0):
        style = self.api.run("style.add_style", self.model, name=f"{product.Name} style")
        attributes = {"SurfaceColour": {"Name": None, "Red": rgb[0], "Green": rgb[1], "Blue": rgb[2]}, "Transparency": 1.0 - alpha}
        # IFC2X3 keeps Transparency on the rendering subtype only.
        ifc_class = "IfcSurfaceStyleRendering" if self.model.schema == "IFC2X3" else "IfcSurfaceStyleShading"
        if ifc_class == "IfcSurfaceStyleRendering":
            attributes["ReflectanceMethod"] = "NOTDEFINED"
        self.api.run("style.add_surface_style", self.model, style=style, ifc_class=ifc_class, attributes=attributes)
        for representation in product.Representation.Representations:
            self.api.run("style.assign_representation_styles", self.model, shape_representation=representation, styles=[style],
                         should_use_presentation_style_assignment=self.model.schema == "IFC2X3")

    # -------------------------------------------------------------- steps

    def exterior(self, storey):
        corners = [(0, 0), (10, 0), (10, 8), (0, 8)]
        names = ["south", "east", "north", "west"]
        suffix = "" if storey == "Ground floor" else " (upper)"
        return [self.wall(f"Exterior wall {names[i]}{suffix}", storey, corners[i], corners[(i + 1) % 4]) for i in range(4)]

    def step_1(self):
        self.exterior("Ground floor")
        return "4 exterior walls"

    def step_2(self):
        self.slab("Base slab", "Ground floor", FOOTPRINT, 0.2, -0.2, "BASESLAB")
        return "the base slab"

    def step_3(self):
        self.wall("Interior wall 1", "Ground floor", (5, 0.15), (5, 7.85), thickness=0.12)
        self.wall("Interior wall 2", "Ground floor", (5.06, 4), (9.85, 4), thickness=0.12)
        return "2 interior walls"

    def step_4(self):
        south = self.by_name("IfcWall", "Exterior wall south")
        interior = self.by_name("IfcWall", "Interior wall 1")
        self.filling("IfcDoor", "Entrance door", south, along=-2.0, sill=0.0, size=(1.0, 2.1))
        self.filling("IfcDoor", "Interior door", interior, along=1.5, sill=0.0, size=(0.9, 2.1))
        return "2 doors"

    def step_5(self):
        self.filling("IfcWindow", "South window", self.by_name("IfcWall", "Exterior wall south"), along=2.5, sill=0.9, size=(1.5, 1.2))
        self.filling("IfcWindow", "East window", self.by_name("IfcWall", "Exterior wall east"), along=-1.5, sill=0.9, size=(1.2, 1.2))
        self.filling("IfcWindow", "North window", self.by_name("IfcWall", "Exterior wall north"), along=2.0, sill=0.9, size=(1.8, 1.2))
        return "3 windows"

    def step_6(self):
        self.slab("Upper floor slab", "Upper floor", FOOTPRINT, 0.2, -0.2, "FLOOR")
        return "the upper floor slab"

    def step_7(self):
        walls = self.exterior("Upper floor")
        self.filling("IfcWindow", "South window (upper)", walls[0], along=-2.5, sill=0.9, size=(1.5, 1.2))
        self.filling("IfcWindow", "South window 2 (upper)", walls[0], along=2.5, sill=0.9, size=(1.5, 1.2))
        self.filling("IfcWindow", "North window (upper)", walls[2], along=0.0, sill=0.9, size=(1.8, 1.2))
        return "4 walls and 3 windows upstairs"

    def step_8(self):
        self.slab("Roof", "Upper floor", FOOTPRINT, 0.25, 2.8, "ROOF")
        return "the roof"

    def step_9(self):
        self.box("IfcColumn", "Column A", "Ground floor", at=(2.5, 4, 0), size=(0.3, 0.3, 2.8))
        self.box("IfcColumn", "Column B", "Ground floor", at=(7.5, 4, 0), size=(0.3, 0.3, 2.8))
        self.box("IfcBeam", "Beam AB", "Ground floor", at=(5.0, 4, 2.5), size=(5.0, 0.2, 0.3))
        return "2 columns and a beam"

    def step_10(self):
        count = 0
        for wall in self.model.by_type("IfcWall"):
            external = wall.Name.startswith("Exterior")
            pset = self.api.run("pset.add_pset", self.model, product=wall, name="Pset_WallCommon")
            self.api.run("pset.edit_pset", self.model, pset=pset, properties={"IsExternal": external, "LoadBearing": external, "FireRating": "REI60" if external else "REI30"})
            count += 1
        for slab in self.model.by_type("IfcSlab"):
            pset = self.api.run("pset.add_pset", self.model, product=slab, name="Pset_SlabCommon")
            self.api.run("pset.edit_pset", self.model, pset=pset, properties={"IsExternal": slab.Name in ("Roof", "Base slab"), "LoadBearing": True})
            count += 1
        for door in self.model.by_type("IfcDoor"):
            pset = self.api.run("pset.add_pset", self.model, product=door, name="Pset_DoorCommon")
            self.api.run("pset.edit_pset", self.model, pset=pset, properties={"IsExternal": door.Name == "Entrance door", "FireRating": "none"})
            count += 1
        return f"{count} property sets"

    def step_11(self):
        count = 0
        for wall in self.model.by_type("IfcWall"):
            self.colour(wall, (0.85, 0.8, 0.7) if wall.Name.startswith("Exterior") else (0.95, 0.95, 0.9))
            count += 1
        for slab in self.model.by_type("IfcSlab"):
            self.colour(slab, (0.55, 0.25, 0.2) if slab.Name == "Roof" else (0.6, 0.6, 0.62))
            count += 1
        for window in self.model.by_type("IfcWindow"):
            self.colour(window, (0.4, 0.6, 0.9), alpha=0.5)
            count += 1
        for door in self.model.by_type("IfcDoor"):
            self.colour(door, (0.5, 0.3, 0.15))
            count += 1
        for item in list(self.model.by_type("IfcColumn")) + list(self.model.by_type("IfcBeam")):
            self.colour(item, (0.35, 0.35, 0.38))
            count += 1
        return f"{count} coloured products"

    def by_name(self, ifc_class, name):
        return next(item for item in self.model.by_type(ifc_class) if item.Name == name)

    STEPS = [
        ("Ground floor exterior walls", "step_1"), ("Base slab", "step_2"), ("Interior walls", "step_3"), ("Doors", "step_4"),
        ("Ground floor windows", "step_5"), ("Upper floor slab", "step_6"), ("Upper floor walls and windows", "step_7"),
        ("Roof slab", "step_8"), ("Columns and a beam", "step_9"), ("Property sets", "step_10"), ("Colours", "step_11"),
    ]


def build(path: Path, *, schema: str = "IFC4", on_step=None):
    """Build the house into `path`, saving after every step; returns the product counts by class."""
    model = create_model(schema, name="Demo house", site="Demo site", building="Demo house",
                         storeys=[{"name": "Ground floor", "elevation": 0.0}, {"name": "Upper floor", "elevation": 3.0}])
    write_model(model, path)
    house = House(model)
    with owner_scope(model):
        for index, (title, method) in enumerate(House.STEPS, start=1):
            description = getattr(house, method)()
            write_model(model, path)
            if on_step:
                on_step(index, title, description)
    counts = {}
    for product in model.by_type("IfcProduct"):
        counts[product.is_a()] = counts.get(product.is_a(), 0) + 1
    return counts


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ifc", nargs="?", default="house.ifc", type=Path)
    parser.add_argument("--serve", action="store_true", help="Serve the viewer beside the build so it follows the file")
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--schema", default="IFC4")
    parser.add_argument("--pause", type=float, default=1.5, help="Seconds between steps while serving")
    args = parser.parse_args(argv)
    path = args.ifc.resolve()
    server = None
    if args.serve:
        from tessifc_session import EditSession, create_server

        write_model(create_model(args.schema, name="Demo house"), path)
        session = EditSession(path)
        server = create_server(session, args.port, root=ROOT)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        print(f"Open {server.viewer_url} and watch; the build starts in 5 seconds", flush=True)
        time.sleep(5)

    def on_step(index, title, description):
        print(f"{index}. {title}: {description}", flush=True)
        if server is not None:
            time.sleep(args.pause)

    counts = build(path, schema=args.schema, on_step=on_step)
    complete = all(counts.get(name, 0) == count for name, count in EXPECTED.items())
    print(f"Saved {path}: {'all counts as expected' if complete else counts}", flush=True)
    if server is not None:
        print("The viewer keeps following; press Ctrl+C to stop.", flush=True)
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
        server.shutdown()
    return 0 if complete else 1


if __name__ == "__main__":
    sys.exit(main())
