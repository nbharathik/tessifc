<!-- SPDX-License-Identifier: Apache-2.0 -->
# tessifc-geom

Part of [TessIFC](../../README.md).

IFC geometry evaluation: units, placements, curves, profiles, solids, styles
and provenance. A `Registry` maps an IFC class to its evaluator; an `EvalCtx`
carries the model, the units, the tolerances, the settings, the caches and a
diagnostic sink through every evaluation.

```rust
use tessifc_geom::{DiagnosticSink, EvalCtx, Registry, Settings, Tolerances, Units, product_parts};
use tessifc_model::Model;
use tessifc_step::{ParseOptions, parse};

let bytes = std::fs::read("model.ifc")?;
let image = parse(&bytes, &ParseOptions::default());
let registry = Registry::shared(image.schema);
let model = Model::new(image);
let settings = Settings::default();
let sink = DiagnosticSink::default();
let ctx = EvalCtx::new(&model, Units::from_model(&model), Tolerances::default(), &settings, &sink);

for wall in model.entities_of_type("IfcWall") {
    match product_parts(&ctx, registry, &model, wall) {
        Some(parts) => println!("#{}: {} coloured parts", wall.id(), parts.len()),
        None => println!("#{}: nothing to draw", wall.id()),
    }
}
for diagnostic in sink.take() {
    println!("{diagnostic}");
}
# Ok::<(), std::io::Error>(())
```

## Extension point

Evaluators are trait objects keyed by IFC class. To support another class,
implement `SolidEvaluator`, `CurveEvaluator` or `ProfileEvaluator` in one
file under `src/eval/`, register it with `register_solid`, `register_curve`
or `register_profile` (later registrations override earlier ones), add a
fixture and a test, and add a row to `docs/coverage.md`. An evaluator is
pure: it reads the context, may call back into the registry for its
sub-items, and returns a mesh or a `GeomError` with a stable code from
`codes`. Invalid input should be refused through that error path.

## What it owns

* `units`: `IfcSIUnit` with prefixes, conversion-based units, degrees
  against radians; metres and radians inside.
* `placement`: local placement chains with a cache, non-perpendicular
  reference directions projected, 2D and 3D axes.
* `eval`: curves (polylines, indexed polycurves, composites, trims, circles,
  ellipses, B-splines), profiles (rectangles, circles, arbitrary with voids,
  composite, derived, the parametric steel family), solids (extrusions,
  revolutions, swept disks, faceted and advanced B-reps, tessellated sets,
  CSG primitives, mapped items, clipping and openings).
* `style`: file colours through `IfcStyledItem`,
  `IfcPresentationStyleAssignment` and material associations, then a class
  palette.
* `product`: representation selection, per-colour parts, shared family
  geometry by `SharedKey`, provenance.

Unsupported cases are diagnosed. See the [preview contract](../../docs/preview.md)
and [coverage table](../../docs/coverage.md); `tessifc coverage --markdown`
prints the registry.

Licensed under Apache-2.0.
