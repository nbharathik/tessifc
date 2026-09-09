<!-- SPDX-License-Identifier: Apache-2.0 -->
# v0.1 developer preview

TessIFC 0.1.0 provides a Rust geometry kernel, CLI, browser and Node WASM
bindings, a three.js adapter and a local browser viewer. This release is
intended for integration feedback and geometry evaluation. It is not a claim
of complete IFC conformance, universal model recovery or validated engineering accuracy.

## Supported workflow

1. Open an IFC-SPF file using the native CLI or `Kernel.openModel`.
2. Check the declared/resolved schema and parse diagnostics.
3. Evaluate geometry using explicit settings where tolerances matter.
4. Inspect the summary, product outcomes and diagnostic codes.
5. Consume the IGP output or shape arrays, retaining element IDs and model offset.
6. Release geometry when it is no longer needed and close the model after inspection or editing.

The schema tables cover IFC2X3 TC1, IFC4 ADD2 TC1 and IFC4X3 ADD2. A product
class can use several geometry representations. Recognising its name and
attributes does not mean every representation of that product is supported.
[Coverage](coverage.md) describes the evaluator conditions.

## Limits to account for

* Some advanced BReps, periodic trims and inconsistent surface/edge references
  cannot produce a usable closed mesh. Missing topology is reported.
* Booleans can refuse an operation outside their supported or bounded cases.
  A retained operand is degraded geometry, not a successful subtraction.
* Tessellation tolerances are constrained by segment and surface budgets.
  An unmet tolerance produces a warning; a requested tolerance is not a certificate.
* Domain repair is enabled only where the independent 3D curve supports it.
  Broader surface-curve recovery is opt-in and remains diagnosed.
* Spatial/linear infrastructure entities may be readable without having a
  supported drawable representation. Textures and IFCZIP are not supported.
* Measurement reads the displayed tessellation in metres. Verify accuracy
  independently before using it for fabrication, structural or safety decisions.
* Memory and time are not globally bounded for every possible input. Browser
  hosts should use a worker; services should impose file-size, memory and time limits.

Use `tessifc convert ... --strict` to reject missing, degraded, repaired or
uncertain results. In a custom host, apply the equivalent policy to outcome
and diagnostic fields before accepting a pack. See [SDK quality handling](sdk.md).

## Compatibility

Pin package versions during the preview. Keep all TessIFC packages on the same
version. The IGP v0 binary layout is versioned; optional JSON fields may be
added. Diagnostic text can change, so branch on codes. Rust APIs and preview
host APIs may evolve in a later minor release.

Geometry is evaluated deterministically for the same input and settings on
a given build. Runtime timings and chunk boundaries can differ. The native and
WASM builds agree on indices, coordinates and product outcomes; bitwise
equivalence on every platform and every IFC file is not guaranteed.

## Reporting an issue

For a geometry issue, include the TessIFC version, schema, effective settings,
element ID, diagnostic codes and expected shape. A minimal generator or
embedded IFC fragment is preferable to a large model. Share only data you
are authorised to disclose; report crashes privately through the security policy.
