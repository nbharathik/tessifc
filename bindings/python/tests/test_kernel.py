# SPDX-License-Identifier: Apache-2.0
"""The Python kernel over an embedded IFC4 fragment: reports, geometry as one
pack and as a stream, the IGP reader, and the errors the API promises."""

import unittest

import tessifc
from tessifc import igp

# A wall as an extruded rectangle, the smallest file that produces a solid.
FRAGMENT = "\n".join(
    [
        "ISO-10303-21;",
        "HEADER;",
        "FILE_DESCRIPTION(('ViewDefinition [CoordinationView]'),'2;1');",
        "FILE_NAME('wall.ifc','2026-01-01T00:00:00',(''),(''),'','','');",
        "FILE_SCHEMA(('IFC4'));",
        "ENDSEC;",
        "DATA;",
        "#1=IFCPROJECT('0YvctVUKr0kugbFTf53O9L',$,'Project',$,$,$,$,(#7),#11);",
        "#2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,0.3);",
        "#3=IFCDIRECTION((0.,0.,1.));",
        "#4=IFCEXTRUDEDAREASOLID(#2,$,#3,2.5);",
        "#5=IFCSHAPEREPRESENTATION(#7,'Body','SweptSolid',(#4));",
        "#6=IFCPRODUCTDEFINITIONSHAPE($,$,(#5));",
        "#7=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#8,$);",
        "#8=IFCAXIS2PLACEMENT3D(#9,$,$);",
        "#9=IFCCARTESIANPOINT((0.,0.,0.));",
        "#10=IFCWALL('1wall0000000000000000A',$,'Wall A',$,$,$,#6,$,$);",
        "#11=IFCUNITASSIGNMENT((#12));",
        "#12=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);",
        "#13=IFCBUILDINGSTOREY('2stor0000000000000000A',$,'Ground floor',$,$,$,$,$,.ELEMENT.,0.);",
        "#14=IFCRELCONTAINEDINSPATIALSTRUCTURE('3rel00000000000000000A',$,$,$,(#10),#13);",
        "ENDSEC;",
        "END-ISO-10303-21;",
    ]
).encode()


class KernelTests(unittest.TestCase):
    def setUp(self):
        self.kernel = tessifc.Kernel()
        self.model = self.kernel.open_model(FRAGMENT)

    def tearDown(self):
        self.kernel.close_all()

    def test_version_and_module_facts(self):
        self.assertRegex(tessifc.version(), r"^\d+\.\d+\.\d+")
        self.assertEqual(tessifc.__version__, tessifc.version())
        self.assertTrue(issubclass(tessifc.KernelError, Exception))

    def test_model_info_and_reports(self):
        info = self.kernel.get_model_info(self.model)
        self.assertEqual(info["schema"], "IFC4")
        self.assertGreater(info["entities"], 0)
        self.assertEqual(info["products"]["IfcWall"], 1)
        self.assertEqual(self.kernel.get_class_name(self.model, 10), "IfcWall")
        self.assertEqual(self.kernel.get_product_category(self.model, 10), "physical")
        self.assertIsNone(self.kernel.get_product_category(self.model, 2))
        self.assertEqual(self.kernel.get_ids_of_type(self.model, "IfcWall"), [10])
        self.assertEqual(self.kernel.get_ids_of_type(self.model, "IfcNoSuchClass"), [])
        entity = self.kernel.get_entity_info(self.model, 10)
        self.assertEqual(entity["class"], "IfcWall")
        name = next(field for field in entity["fields"] if field["name"] == "Name")
        self.assertEqual(name["value"], "Wall A")
        self.assertEqual(name["raw"], "'Wall A'")
        self.assertIsNone(self.kernel.get_entity_info(self.model, 999))
        hierarchy = self.kernel.get_spatial_hierarchy(self.model)
        by_id = {node["expressId"]: node for node in hierarchy["nodes"]}
        self.assertEqual(by_id[10]["parentExpressId"], 13)
        self.assertEqual(by_id[10]["globalId"], "1wall0000000000000000A")
        attributes = self.kernel.get_class_attributes(self.model, "IfcWall")
        self.assertEqual(attributes["attributes"][0]["name"], "GlobalId")
        self.assertEqual(self.kernel.get_class_supertypes(self.model, "IfcWall")[-1], "IfcRoot")
        self.assertIsNone(self.kernel.get_class_supertypes(self.model, "IfcNoSuchClass"))
        self.assertEqual(self.kernel.get_diagnostics(self.model), [])
        self.assertIn("IfcWall", str(self.kernel.get_geometry_capabilities(self.model)))
        self.assertIsNone(self.kernel.get_model_info(999))

    def test_whole_evaluation_and_pack(self):
        summary = self.kernel.evaluate_geometry(self.model, {"includeOpenings": False})
        self.assertEqual(summary["products"], 1)
        self.assertEqual(summary["triangles"], 12)
        self.assertEqual(summary["effectiveSettings"]["includeOpenings"], False)
        outcomes = self.kernel.get_product_outcomes(self.model)
        # The storey is a product too; it has no representation to draw.
        self.assertEqual([(o["express_id"], o["state"]) for o in outcomes], [(10, "emitted"), (13, "no_representation")])
        kept = self.kernel.get_pack(self.model)
        taken = self.kernel.take_pack(self.model)
        self.assertTrue(taken.startswith(b"IGP\0"))
        self.assertEqual(kept, taken, "get_pack and take_pack write the same bytes")
        self.assertIsNone(self.kernel.take_pack(self.model), "the geometry is released with the pack")
        pack = igp.read_igp(taken)
        self.assertEqual(pack.index["schema"], "IFC4")
        self.assertEqual(pack.instances.count, 1)
        self.assertEqual(pack.instances.express_ids[0], 10)
        self.assertEqual(pack.class_of(0), "IfcWall")
        self.assertEqual(len(pack.geometry), 1)
        mesh = pack.geometry[0]
        self.assertEqual(mesh.vertex_count, pack.index["geometries"][0]["positions"]["count"])
        self.assertEqual(mesh.triangle_count, 12)
        self.assertTrue(mesh.closed)
        xs = mesh.positions[0::3]
        self.assertAlmostEqual(max(xs) - min(xs), 4.0, places=5)
        self.assertEqual(pack.instances.color(0)[3], 255)
        self.assertEqual(pack.instances.transform(0)[15], 1.0)
        self.assertTrue(pack.stream["final"])
        self.assertIsNone(pack.instances.material, "no textures, no material column")
        # The hierarchy after the evaluation marks the wall as rendered.
        node = next(n for n in self.kernel.get_spatial_hierarchy(self.model)["nodes"] if n["expressId"] == 10)
        self.assertTrue(node["rendered"])

    def test_streamed_chunks_match_the_whole_pack(self):
        summary = self.kernel.begin_geometry_stream(self.model)
        self.assertEqual(summary["products"], 2, "the wall and the storey are considered")
        chunks = []
        while (chunk := self.kernel.next_geometry_chunk(self.model, 0, 1, 0)) is not None:
            chunks.append(igp.read_igp(chunk))
        self.assertEqual(len(chunks), 2, "one product per chunk was asked for")
        self.assertIsNone(self.kernel.next_geometry_chunk(self.model), "nothing after the final chunk")
        progress = self.kernel.stream_progress(self.model)
        self.assertTrue(progress["finished"])
        self.assertEqual(progress["emitted"], 1)
        self.assertEqual(sum(chunk.instances.count for chunk in chunks), 1)
        self.assertTrue(chunks[-1].stream["final"])
        self.assertFalse(chunks[0].stream["final"])
        self.assertEqual(chunks[0].geometry[0].triangle_count, 12)
        self.assertTrue(self.kernel.cancel_geometry_stream(self.model))
        self.assertFalse(self.kernel.cancel_geometry_stream(self.model))
        outcomes = self.kernel.get_product_outcomes(self.model)
        self.assertEqual(outcomes[0]["state"], "emitted", "outcomes survive the stream's release")

    def test_errors_and_lifecycle(self):
        with self.assertRaises(tessifc.KernelError):
            self.kernel.evaluate_geometry(self.model, {"chordToleranceM": -1})
        with self.assertRaises(tessifc.KernelError):
            self.kernel.evaluate_geometry(self.model, "not json")
        with self.assertRaises(tessifc.KernelError):
            self.kernel.open_model(FRAGMENT, {"schemaOverride": "IFC9"})
        self.assertIsNone(self.kernel.evaluate_geometry(999))
        self.assertEqual(self.kernel.model_count(), 1)
        self.assertTrue(self.kernel.close_model(self.model))
        self.assertFalse(self.kernel.close_model(self.model))
        self.assertEqual(self.kernel.model_count(), 0)
        with tessifc.Kernel() as kernel:
            kernel.open_model(FRAGMENT)
            self.assertEqual(kernel.model_count(), 1)
        self.assertEqual(kernel.model_count(), 0, "the context manager closes every model")

    def test_a_damaged_file_opens_with_diagnostics(self):
        truncated = self.kernel.open_model(FRAGMENT[: len(FRAGMENT) // 2])
        info = self.kernel.get_model_info(truncated)
        self.assertGreaterEqual(info["diagnostics"]["total"], 1)
        self.assertIsInstance(self.kernel.get_diagnostics(truncated), list)
        self.assertIsNotNone(self.kernel.evaluate_geometry(truncated))
        junk = self.kernel.open_model(b"\x00\x01\x02\xff")
        self.assertEqual(self.kernel.get_model_info(junk)["entities"], 0)
        self.assertGreaterEqual(len(self.kernel.get_diagnostics(junk)), 1)

    def test_the_reader_refuses_bad_bytes(self):
        with self.assertRaises(igp.PackError):
            igp.read_igp(b"\0" * 4)
        with self.assertRaises(igp.PackError):
            igp.read_igp(b"\0" * 24)
        self.kernel.evaluate_geometry(self.model)
        pack = self.kernel.take_pack(self.model)
        with self.assertRaises(igp.PackError):
            igp.read_igp(pack[:-40])


class NumpyTests(unittest.TestCase):
    def test_views_become_arrays(self):
        try:
            import numpy
        except ImportError:
            self.skipTest("numpy is not installed")
        kernel = tessifc.Kernel()
        model = kernel.open_model(FRAGMENT)
        kernel.evaluate_geometry(model)
        pack = igp.read_igp(kernel.take_pack(model))
        positions = igp.to_numpy(pack.geometry[0].positions, 3)
        indices = igp.to_numpy(pack.geometry[0].indices, 3)
        self.assertEqual(positions.shape[1], 3)
        self.assertEqual(indices.shape, (12, 3))
        self.assertEqual(positions.dtype, numpy.float32)
        self.assertLess(int(indices.max()), positions.shape[0])
        kernel.close_all()


if __name__ == "__main__":
    unittest.main()
