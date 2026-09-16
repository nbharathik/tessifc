<!-- SPDX-License-Identifier: Apache-2.0 -->
# Coverage

Which IFC representation items TessIFC can turn into geometry, and which it
cannot yet in the v0.2 developer preview. See the [preview contract](preview.md)
for the distinction between schema recognition and complete geometry support.

Legend: **covered** the kernel handles it; **partial** it handles the common
case and diagnoses the rest; **not supported** it is refused with a diagnostic
and the product is reported without that representation.

The registry table at the end of this page is generated from the code itself:

    tessifc coverage --markdown

A class listed there has an evaluator. The tables above it say what that
evaluator does with the cases real files contain, which is the narrower and
more useful claim.

## Interpreting capabilities

All generated schema entities can be inspected with `tessifc coverage --inventory`.
That includes classes with no geometry, direct dispatch routes, and inherited routes.
A product class is not a representation evaluator, and a registered ancestor does
not establish complete subtype semantics. Runtime product outcomes and diagnostics
are the evidence for a particular conversion.

Advanced faces inside shell-based surface models use the advanced-face path.
`IfcSurfaceCurve` and `IfcSeamCurve` can supply edge geometry. Explicit pcurves on
planes, cylinders, spheres, tori and B-spline surfaces preserve parameter-space
boundaries. Rectangular full-period trims can also use inverted 3D edges. A
single circular boundary on a sphere uses a cap tessellation in the boundary's
own frame, including when it crosses the sphere's parameter poles.
Swept-surface pcurve parameterisations, general nonrectangular seam-side selection,
vertex-loop singularities and spherical regions with holes remain partial or
unsupported. Surface patches have an adjustable vertex budget.
Recovered parameter scales require agreement with the separate 3D curve and are
diagnosed. Additional surface-reference and orientation repairs are opt-in;
see the [settings contract](sdk.md#the-kernel-api). Strict conversion refuses
recovered or incomplete output.

## Coordinate pipeline

| Feature | Status |
| --- | --- |
| Units: `IfcSIUnit` with prefix | covered |
| Units: `IfcConversionBasedUnit` (foot, inch) | covered |
| Plane angle unit: DEGREE against RADIAN | covered |
| `IfcGeometricRepresentationContext.Precision` to tolerances | covered |
| `IfcLocalPlacement` chain with caching | covered |
| `IfcAxis2Placement3D` with non-perpendicular `RefDirection` | covered |
| `IfcAxis2Placement2D` | covered |
| Model offset for georeferenced coordinates | covered |
| `IfcGridPlacement` | covered: the crossing of two offset axis curves, with an explicit direction, a second crossing, or the first axis's tangent as x |
| `IfcMapConversion` as metadata | covered: written to the pack's `georef` member with its target CRS; no position is moved |

## Curves

| Class | Status |
| --- | --- |
| `IfcPolyline` | covered |
| `IfcIndexedPolyCurve` (line and arc segments) | covered |
| `IfcCompositeCurve` | covered |
| `IfcTrimmedCurve` on line, circle, ellipse | covered |
| `IfcCircle`, `IfcLine`, `IfcEllipse` | covered |
| `IfcBezierCurve` / `IfcRationalBezierCurve` (IFC2X3) | covered: clamped polynomial and rational curves with validated weights |
| `IfcBSplineCurveWithKnots` | covered: non-rational, explicit knots and multiplicities, adaptive de Boor tessellation |
| `IfcRationalBSplineCurveWithKnots` | covered: de Boor in homogeneous coordinates |
| `IfcOffsetCurve2D` / `3D` | covered: mitred offsets, to the left in 2D and along the reference cross the tangent in 3D |

## Profiles

| Class | Status |
| --- | --- |
| `IfcRectangleProfileDef` | covered |
| `IfcRectangleHollowProfileDef` | covered |
| `IfcRoundedRectangleProfileDef` | covered |
| `IfcCircleProfileDef`, `IfcCircleHollowProfileDef` | covered |
| `IfcEllipseProfileDef` | covered |
| `IfcArbitraryClosedProfileDef` | covered |
| `IfcArbitraryProfileDefWithVoids` | covered |
| `IfcCompositeProfileDef` | covered |
| `IfcDerivedProfileDef` | covered |
| `IfcMirroredProfileDef` | covered: the parent mirrored about its y axis |
| `IfcIShapeProfileDef` (with fillets) | covered: symmetric profile and root fillets; flange slope and edge radius squared off and reported |
| `IfcAsymmetricIShapeProfileDef` | covered: both flanges and their root fillets, IFC2X3 and IFC4 spellings; edge radii and slopes squared off and reported |
| `IfcLShapeProfileDef` | covered: outline, root fillet; edge radius and leg slope diagnosed |
| `IfcUShapeProfileDef` | covered: outline, both root fillets; edge radius and flange slope diagnosed |
| `IfcCShapeProfileDef` | covered: outline with lips; internal fillet radius diagnosed |
| `IfcTShapeProfileDef` | covered: outline, both root fillets; edge radii and slopes diagnosed |
| `IfcZShapeProfileDef` | covered: outline, both root fillets; edge radius diagnosed |
| `IfcTrapeziumProfileDef` | covered |
| `IfcArbitraryOpenProfileDef` | covered: an open polyline; a solid asked for from it is built as a surface and reported |
| `IfcCenterLineProfileDef` | covered: mitred offset band |

## Solids and surfaces

| Class | Status |
| --- | --- |
| `IfcExtrudedAreaSolid` (oblique direction) | covered |
| `IfcFacetedBrep` | covered |
| `IfcFacetedBrepWithVoids` | covered |
| `IfcTriangulatedFaceSet` | covered |
| `IfcPolygonalFaceSet` (+ voids) | covered |
| `IfcMappedItem` (+ non-uniform scale) | covered |
| `IfcBooleanClippingResult`, `IfcBooleanResult` | covered: DIFFERENCE, UNION and INTERSECTION of closed solids, exact on convex cells; an operand whose cells cannot be proved to tile it is diagnosed and the body emitted un-cut |
| `IfcHalfSpaceSolid` | covered |
| `IfcPolygonalBoundedHalfSpace` (convex case) | partial: built as a bounded prism and clipped |
| `IfcBoxedHalfSpace` | covered: the enclosure clipped by the base surface |
| Openings via `IfcRelVoidsElement` | covered: any closed body, layer by layer when it is several shells; a body without a usable inside is cut through its faces with `W_OPENING_CUT_ON_SURFACE` |
| `IfcBoundingBox` (as a last-resort body) | covered: drawn with `W_BOUNDING_BOX_SUBSTITUTED` |
| `IfcRevolvedAreaSolid` | covered: full and partial turns, capped, any axis; the tapered variant blends to its end profile ring by ring |
| `IfcSweptDiskSolid` / `IfcSweptDiskSolidPolygonal` | covered: mitred corners at the half angle, polygonal fillets, and `StartParam`/`EndParam` on polyline directrices; trims on other directrix classes are diagnosed |
| `IfcSurfaceCurveSweptAreaSolid` | partial: over an `IfcPlane`, whose normal is the profile's x axis; other reference surfaces are diagnosed |
| `IfcCylindricalSurface`, `IfcSphericalSurface`, `IfcToroidalSurface`, `IfcSurfaceOfRevolution`, `IfcSurfaceOfLinearExtrusion`, `IfcPlane` | covered as face surfaces, with a point at any parameter pair and a closed-form inversion back to it |
| `IfcCurveBoundedPlane`, `IfcCurveBoundedSurface`, `IfcRectangularTrimmedSurface` | covered as face surfaces: they resolve to their basis surface, and the face's own edges do the trimming |
| `IfcFixedReferenceSweptAreaSolid` | covered: the reference projected normal to the tangent is the profile's x axis; corners mitred |
| `IfcCsgSolid` with block, pyramid, cone, cylinder, sphere | covered |
| `IfcShellBasedSurfaceModel` | covered |
| `IfcFaceBasedSurfaceModel` | covered |
| `IfcExtrudedAreaSolidTapered` | covered: lofted; outlines with different corner counts are resampled by arc length, exactly for two circles and with a diagnostic otherwise |
| `IfcSectionedSpine` | covered: sections lofted in order at their placements, resampled when their corners differ |
| `IfcSurfaceOfLinearExtrusion`, `IfcSurfaceOfRevolution` | covered as surfaces: the swept curve's sides, never capped |
| `IfcAdvancedBrep` / `IfcAdvancedBrepWithVoids` | partial: planar and bounded analytic/B-spline faces, polyline edges, and explicit pcurve boundaries. Rectangular parameter regions can cross a full period. Unsupported trims and singularities are diagnosed; refinement limits report unmet tolerance. Non-manifold shells carry `W_NON_MANIFOLD_INPUT` |
| `IfcBSplineSurfaceWithKnots` and its rational subtype | covered as a face surface: tensor-product de Boor with the weights folded in, knots validated against the control net |
| `IfcSectionedSolidHorizontal` | not supported |
| `IfcAlignment` (curve output) | not supported |

## Styles

| Feature | Status |
| --- | --- |
| `IfcStyledItem` to `IfcSurfaceStyle` (IFC4) | covered |
| `IfcPresentationStyleAssignment` indirection (IFC2X3) | covered |
| `IfcSurfaceStyleRendering` / `Shading` colour and transparency | covered |
| Material styles via `IfcRelAssociatesMaterial` | covered |
| Type-level styles via `IfcTypeProduct.RepresentationMaps` | partial: mapped-item style inheritance |
| Class-based default palette | covered |
| Per-item colours within one product | covered |
| `IfcIndexedColourMap` per-face colours on tessellated sets | covered: one mesh per colour, unmapped faces keep the item's style |
| Textures and image maps | not supported |

## Parsing and schema

The parser has no notion of geometry, so it does not appear in the tables
above. For completeness, what it delivers:

| Feature | Status |
| --- | --- |
| STEP-21 tokenizer with recovery diagnostics and local input limits | covered |
| Complex instances | covered |
| String escapes `''`, `\X\`, `\X2\`, `\X4\`, `\S\`, `\N\`, `\F\`, `\T\` | covered |
| `\P\`, accepted and consumed, its ISO 8859 page approximated by Latin-1 | partial |
| `$`, `*`, typed values, nested lists, binary literals | covered |
| Comments anywhere, CRLF, BOM, non-ASCII bytes | covered |
| Unknown classes preserved | covered |
| Truncated files, duplicate ids, out-of-range ids | covered |
| IFC2X3 TC1, IFC4 ADD2 TC1, IFC4X3 ADD2 tables | covered |
| Attribute access by name, in STEP argument order | covered |
| Derived-override slots (`IfcSIUnit.Dimensions`) | covered |
| Inverse index: voids, fills, materials, styles, aggregates, map users | covered |

## Registry, generated

Everything with an evaluator, read out of the registry itself:

    tessifc coverage --markdown

| Item | Kind | Schemas |
| --- | --- | --- |
| `IfcAdvancedBrep` | solid | IFC4, IFC4X3 |
| `IfcAdvancedBrepWithVoids` | solid | IFC4, IFC4X3 |
| `IfcArbitraryClosedProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcArbitraryOpenProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcArbitraryProfileDefWithVoids` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcAsymmetricIShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcBSplineCurveWithKnots` | curve | IFC4, IFC4X3 |
| `IfcBSplineSurfaceWithKnots` | surface | IFC4, IFC4X3 |
| `IfcBezierCurve` | curve | IFC2X3 |
| `IfcBlock` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcBooleanClippingResult` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcBooleanResult` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcBoundingBox` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcBoxedHalfSpace` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcCShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcCenterLineProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcCircle` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcCircleHollowProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcCircleProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcCompositeCurve` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcCompositeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcConnectedFaceSet` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcCsgSolid` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcCurveBoundedPlane` | surface | IFC2X3, IFC4, IFC4X3 |
| `IfcCurveBoundedSurface` | surface | IFC4, IFC4X3 |
| `IfcCylindricalSurface` | surface | IFC4, IFC4X3 |
| `IfcDerivedProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcEllipse` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcEllipseProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcExtrudedAreaSolid` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcExtrudedAreaSolidTapered` | solid | IFC4, IFC4X3 |
| `IfcFaceBasedSurfaceModel` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcFacetedBrep` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcFacetedBrepWithVoids` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcFixedReferenceSweptAreaSolid` | solid | IFC4, IFC4X3 |
| `IfcIShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcIndexedPolyCurve` | curve | IFC4, IFC4X3 |
| `IfcLShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcLine` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcManifoldSolidBrep` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcMappedItem` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcMirroredProfileDef` | profile | IFC4, IFC4X3 |
| `IfcOffsetCurve2D` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcOffsetCurve3D` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcPlane` | surface | IFC2X3, IFC4, IFC4X3 |
| `IfcPolygonalFaceSet` | solid | IFC4, IFC4X3 |
| `IfcPolyline` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcRationalBSplineCurveWithKnots` | curve | IFC4, IFC4X3 |
| `IfcRationalBSplineSurfaceWithKnots` | surface | IFC4, IFC4X3 |
| `IfcRationalBezierCurve` | curve | IFC2X3 |
| `IfcRectangleHollowProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcRectangleProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcRectangularPyramid` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcRectangularTrimmedSurface` | surface | IFC2X3, IFC4, IFC4X3 |
| `IfcRevolvedAreaSolid` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcRightCircularCone` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcRightCircularCylinder` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcRoundedRectangleProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcSectionedSpine` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcShellBasedSurfaceModel` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcSphere` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcSphericalSurface` | surface | IFC4, IFC4X3 |
| `IfcSurfaceCurveSweptAreaSolid` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcSurfaceOfLinearExtrusion` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcSurfaceOfLinearExtrusion` | surface | IFC2X3, IFC4, IFC4X3 |
| `IfcSurfaceOfRevolution` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcSurfaceOfRevolution` | surface | IFC2X3, IFC4, IFC4X3 |
| `IfcSweptDiskSolid` | solid | IFC2X3, IFC4, IFC4X3 |
| `IfcSweptDiskSolidPolygonal` | solid | IFC4, IFC4X3 |
| `IfcTShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcToroidalSurface` | surface | IFC4, IFC4X3 |
| `IfcTrapeziumProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcTriangulatedFaceSet` | solid | IFC4, IFC4X3 |
| `IfcTrimmedCurve` | curve | IFC2X3, IFC4, IFC4X3 |
| `IfcUShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
| `IfcZShapeProfileDef` | profile | IFC2X3, IFC4, IFC4X3 |
