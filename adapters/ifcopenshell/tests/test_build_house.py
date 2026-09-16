# SPDX-License-Identifier: Apache-2.0
"""The Python house example: every step saves a file that re-opens, the counts match the
JavaScript variant, and IFC2X3 reaches the same counts."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

try:
    import ifcopenshell
except ImportError:  # pragma: no cover - optional runtime
    ifcopenshell = None

ROOT = Path(__file__).resolve().parents[3]
EXAMPLE = ROOT / "examples" / "agent-building" / "build_house.py"


def load_example():
    spec = importlib.util.spec_from_file_location("build_house", EXAMPLE)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


@unittest.skipIf(ifcopenshell is None, "IfcOpenShell is not installed")
class BuildHouseTests(unittest.TestCase):
    def test_the_house_reaches_the_expected_counts(self):
        example = load_example()
        directory = Path(tempfile.mkdtemp(prefix="tessifc-house-"))
        path = directory / "house.ifc"
        steps = []
        counts = example.build(path, on_step=lambda index, title, description: steps.append((index, title, description)))
        self.assertEqual(len(steps), len(example.House.STEPS))
        self.assertTrue(all(description for _, _, description in steps), steps)
        for name, count in example.EXPECTED.items():
            self.assertEqual(counts.get(name, 0), count, name)

        reopened = ifcopenshell.open(str(path))
        self.assertEqual(reopened.schema, "IFC4")
        for name, count in example.EXPECTED.items():
            self.assertEqual(len(reopened.by_type(name, include_subtypes=False)), count, name)
        self.assertEqual(len(reopened.by_type("IfcBuildingStorey")), 2)
        self.assertEqual(len(reopened.by_type("IfcRelVoidsElement")), 8)
        self.assertEqual(len(reopened.by_type("IfcRelFillsElement")), 8)
        self.assertTrue(reopened.by_type("IfcPropertySet"))
        self.assertTrue(reopened.by_type("IfcStyledItem"))
        for product in reopened.by_type("IfcElement"):
            self.assertIsNotNone(product.Representation, product)
            self.assertIsNotNone(product.ObjectPlacement, product)

    def test_the_ifc2x3_variant_reaches_the_same_counts(self):
        example = load_example()
        directory = Path(tempfile.mkdtemp(prefix="tessifc-house-"))
        path = directory / "house2x3.ifc"
        counts = example.build(path, schema="IFC2X3")
        for name, count in example.EXPECTED.items():
            self.assertEqual(counts.get(name, 0), count, name)
        reopened = ifcopenshell.open(str(path))
        self.assertEqual(reopened.schema, "IFC2X3")
        self.assertTrue(reopened.by_type("IfcPresentationStyleAssignment"))
        for entity in reopened.by_type("IfcRoot"):
            self.assertIsNotNone(entity.OwnerHistory, entity)


if __name__ == "__main__":
    unittest.main()
