// SPDX-License-Identifier: Apache-2.0
//! Orchestration: which products to evaluate, in what order, and what to do when
//! one fails (a diagnostic and, where possible, a degraded mesh). [`Engine::evaluate`]
//! does the whole model; [`Engine::session`] streams it in batches the caller sizes.
//!
//! ```no_run
//! use tessifc_engine::Engine;
//! use tessifc_model::Model;
//! use tessifc_step::{ParseOptions, parse};
//!
//! let bytes = std::fs::read("model.ifc")?;
//! let model = Model::new(parse(&bytes, &ParseOptions::default()));
//! let engine = Engine::new();
//! let result = engine.evaluate(&model);
//! println!("{} products, {} triangles", result.shapes.len(), result.triangles());
//! # Ok::<(), std::io::Error>(())
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod pack;
pub mod revision;

/// Diagnostic codes this crate raises itself.
pub mod codes {
    use tessifc_step::DiagCode;

    /// A relationship record with too many links to index; its targets are lost.
    pub const RELATIONSHIP_TOO_LARGE: DiagCode = DiagCode("W_RELATIONSHIP_TOO_LARGE");
    /// A coordinate did not survive the narrowing to f32; the part was dropped.
    pub const NON_FINITE_GEOMETRY: DiagCode = DiagCode("W_NON_FINITE_GEOMETRY");
}

/// Narrowest length precision a file may drive the tolerances with, in metres.
const MIN_PRECISION_M: f64 = 1e-9;
/// Coarsest length precision a file may drive the tolerances with, in metres.
const MAX_PRECISION_M: f64 = 1e-3;

use glam::DVec3;
use std::collections::HashSet;
use tessifc_geom::{
    DiagnosticSink, EvalCaches, EvalCtx, PhaseTimings, Provenance, Registry, Settings, Tolerances,
    Units, product_category, product_parts, product_transform, should_include,
};
use tessifc_mesh::Mesh64;
use tessifc_model::Model;
use tessifc_step::{DiagCode, Diagnostic};

pub use tessifc_geom::{PartGeometry, ProductCategory, SharedKey};

/// The geometry of one product that shares a single colour.
pub struct ShapePart {
    /// The triangles: a world-space mesh of this product's own, or a family
    /// mesh shared with every other product that places it.
    pub geometry: PartGeometry,
    /// Its colour, from the file's styles or from the class palette.
    pub color: [u8; 4],
    /// Which representation, item and evaluator produced it.
    pub provenance: Provenance,
}

impl ShapePart {
    /// The mesh in world coordinates and metres. A copy, for a shared part.
    pub fn mesh(&self) -> Mesh64 {
        self.geometry.world_mesh()
    }

    /// Triangles in the part.
    pub fn triangle_count(&self) -> usize {
        self.geometry.triangle_count()
    }

    /// Axis-aligned world bounds.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        self.geometry.bounds()
    }
}

/// One product's geometry, in world space.
///
/// A product is one shape but may be several parts: a window is a frame and a
/// pane, and the file gives them different materials. Merging them would mean
/// picking one colour for both, which is how a viewer ends up drawing glass as
/// an opaque white panel.
pub struct Shape {
    /// The product's express id.
    pub express_id: u32,
    /// Its IFC class name.
    pub class: String,
    /// Whether the product is physical geometry or a separately controllable helper.
    pub category: ProductCategory,
    /// Its geometry, one entry per colour, in the order the colours appear in
    /// the file. Never empty.
    pub parts: Vec<ShapePart>,
    /// The colour of its first part, for a caller that wants one colour for
    /// the whole product.
    pub color: [u8; 4],
}

impl Shape {
    /// Axis-aligned bounds.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        self.parts
            .iter()
            .filter_map(ShapePart::bounds)
            .reduce(|(low, high), (lo, hi)| (low.min(lo), high.max(hi)))
    }

    /// Every part merged into one world-space mesh, for a caller that wants
    /// one mesh per product and does not care that the colours differ.
    pub fn mesh(&self) -> Mesh64 {
        if let [only] = self.parts.as_slice() {
            return only.mesh();
        }
        let mut mesh = Mesh64::new();
        for part in &self.parts {
            mesh.append(&part.mesh());
        }
        mesh
    }

    /// Triangles across every part.
    pub fn triangle_count(&self) -> usize {
        self.parts.iter().map(ShapePart::triangle_count).sum()
    }
}

/// Everything one evaluation run produced.
pub struct EvaluationResult {
    /// Effective geometry settings used for this run.
    pub settings: Settings,
    /// One entry per product that produced geometry, in ascending express id.
    pub shapes: Vec<Shape>,
    /// Everything that went wrong along the way.
    pub diagnostics: Vec<Diagnostic>,
    /// How many products were considered.
    pub products_considered: usize,
    /// How many were skipped by the settings rather than by failure.
    pub products_filtered: usize,
    /// Units the file declared.
    pub units: Units,
    /// The offset that would keep this model precise in f32, if a caller
    /// wants one. See [`Session::model_offset`] for how it is chosen.
    pub model_offset: DVec3,
    /// The file's map conversion as a JSON object, for the pack's `georef`.
    pub georef: Option<String>,
    /// Processor time spent in each evaluation phase.
    pub timings: PhaseTimings,
}

impl EvaluationResult {
    /// Total triangles across every shape.
    pub fn triangles(&self) -> usize {
        self.shapes.iter().map(Shape::triangle_count).sum()
    }

    /// Bounds of everything, in world space.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        let mut result: Option<(DVec3, DVec3)> = None;
        for shape in &self.shapes {
            let Some((lo, hi)) = shape.bounds() else {
                continue;
            };
            result = Some(match result {
                Some((low, high)) => (low.min(lo), high.max(hi)),
                None => (lo, hi),
            });
        }
        result
    }
}

/// The observed outcome for a product, separate from the schema inventory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProductOutcome {
    /// IFC express id.
    pub express_id: u32,
    /// Exact IFC entity class.
    pub class: String,
    /// Filtered, no representation, no usable representation, pending, emitted, or empty/failed.
    pub state: ProductState,
}

/// Evaluation state; emitted geometry may still carry diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductState {
    /// Excluded by the settings or selected product list.
    Filtered,
    /// The IFC product declares no representation.
    NoRepresentation,
    /// It declares representations, but none selected by the body policy.
    NoUsableRepresentation,
    /// Selected but not evaluated yet.
    Pending,
    /// At least one mesh part was emitted; inspect diagnostics for completeness.
    Emitted,
    /// Evaluation finished without triangles; inspect diagnostics for the cause.
    EmptyOrFailed,
}

fn product_outcomes(
    model: &Model,
    selected: &HashSet<u32>,
    done: &HashSet<u32>,
    emitted: &HashSet<u32>,
) -> Vec<ProductOutcome> {
    let mut outcomes: Vec<_> = model
        .entities_of_type("IfcProduct")
        .map(|product| {
            let id = product.id();
            let state = if !selected.contains(&id) {
                ProductState::Filtered
            } else if emitted.contains(&id) {
                ProductState::Emitted
            } else if product.attr("Representation").as_entity().is_none() {
                ProductState::NoRepresentation
            } else if tessifc_geom::representation_of(product).is_none() {
                ProductState::NoUsableRepresentation
            } else if !done.contains(&id) {
                ProductState::Pending
            } else {
                ProductState::EmptyOrFailed
            };
            ProductOutcome {
                express_id: id,
                class: product.class_name(),
                state,
            }
        })
        .collect();
    outcomes.sort_by_key(|outcome| outcome.express_id);
    outcomes
}

/// Turns models into geometry.
pub struct Engine {
    /// Caller settings.
    pub settings: Settings,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

impl Engine {
    /// An engine with default settings.
    pub fn new() -> Self {
        Engine {
            settings: Settings::default(),
        }
    }

    /// An engine with the given settings.
    pub fn with_settings(settings: Settings) -> Self {
        Engine { settings }
    }

    /// The products this engine would evaluate, in ascending express id.
    pub fn products(&self, model: &Model) -> Vec<u32> {
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            model,
            Units::default(),
            Tolerances::default(),
            &self.settings,
            &sink,
        );
        let mut ids: Vec<u32> = model
            .entities_of_type("IfcProduct")
            .filter(|product| should_include(&ctx, *product))
            .map(|product| product.id())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Evaluate every product.
    ///
    /// With the `parallel` feature the products are split across the rayon
    /// pool. The output is identical either way: shapes in ascending express
    /// id, diagnostics in the same order, and every family evaluated from the
    /// same source.
    pub fn evaluate(&self, model: &Model) -> EvaluationResult {
        let mut session = self.session(model);
        let ids = std::mem::take(&mut session.products);
        let (shapes, diagnostics, timings) = evaluate_all(&session, model, &ids);
        let mut leading = std::mem::take(&mut session.leading);
        leading.extend(diagnostics);
        EvaluationResult {
            settings: self.settings.clone(),
            shapes,
            diagnostics: dedup_diagnostics(leading, &mut HashSet::new()),
            products_considered: session.products_considered,
            products_filtered: session.products_filtered,
            units: session.units,
            model_offset: session.model_offset,
            georef: session.georef,
            timings,
        }
    }

    /// Classify every product after evaluating this model with these settings.
    pub fn outcomes(&self, model: &Model, result: &EvaluationResult) -> Vec<ProductOutcome> {
        let selected: HashSet<_> = self.products(model).into_iter().collect();
        let emitted: HashSet<_> = result.shapes.iter().map(|shape| shape.express_id).collect();
        product_outcomes(model, &selected, &selected, &emitted)
    }

    /// Start evaluating a model in batches.
    ///
    /// The session owns no reference to the model, so a host that keeps
    /// models in a table can keep sessions next to them. Pass the same model
    /// to every [`Session::next`].
    pub fn session(&self, model: &Model) -> Session {
        Session::new(self, model)
    }
}

/// A model being evaluated in batches, for streaming.
///
/// Each call to [`Session::next`] evaluates the next few products and returns
/// them. What a batch does not carry is anything already sent: a family
/// evaluated for batch one is remembered, and a diagnostic reported once is
/// not reported again.
pub struct Session {
    settings: Settings,
    units: Units,
    tolerances: Tolerances,
    products: Vec<u32>,
    cursor: usize,
    caches: EvalCaches,
    model_offset: DVec3,
    georef: Option<String>,
    products_considered: usize,
    products_filtered: usize,
    emitted: Vec<u32>,
    triangles: usize,
    leading: Vec<Diagnostic>,
    seen: HashSet<(DiagCode, Option<u32>, String)>,
}

/// What one call to [`Session::next`] produced.
pub struct Batch {
    /// The products evaluated in this batch, in ascending express id.
    pub shapes: Vec<Shape>,
    /// Diagnostics raised by them, none of which was returned before.
    pub diagnostics: Vec<Diagnostic>,
    /// True when nothing is left to evaluate.
    pub is_final: bool,
    /// Processor time this batch spent in each evaluation phase.
    pub timings: PhaseTimings,
}

/// How far a batch has come, for a caller deciding when to stop it.
#[derive(Copy, Clone, Debug)]
pub struct BatchProgress {
    /// Products evaluated in this batch so far.
    pub products: usize,
    /// Triangles produced in this batch so far.
    pub triangles: usize,
    /// Products evaluated across the whole session so far.
    pub done: usize,
    /// Products the session will evaluate in total.
    pub total: usize,
}

impl Session {
    fn new(engine: &Engine, model: &Model) -> Session {
        let units = Units::from_model(model);
        let mut leading = model.image().diagnostics.items().to_vec();
        if units.assumed {
            leading.push(
                Diagnostic::warning(
                    tessifc_geom::codes::UNITS_ASSUMED,
                    0,
                    "the file declares no length unit; metres assumed",
                )
                .with_id(0),
            );
        }
        let settings_error = engine.settings.validate().err();
        if let Some(error) = &settings_error {
            leading.push(Diagnostic::error(error.code(), 0, error.to_string()));
        }
        let tolerances = tolerances_of(model, &units);
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(model, units, tolerances, &engine.settings, &sink);

        let mut products = Vec::new();
        let mut considered = 0;
        let mut filtered = 0;
        // The lowest placement corner, chosen before any geometry exists so every
        // streamed batch agrees on it; f32 needs the offset near the geometry, not exact.
        let mut lowest: Option<DVec3> = None;
        for product in model.entities_of_type("IfcProduct") {
            considered += 1;
            if !should_include(&ctx, product) {
                filtered += 1;
                continue;
            }
            if settings_error.is_some() {
                continue;
            }
            products.push(product.id());
            if product.attr("ObjectPlacement").as_entity().is_some() {
                let origin = product_transform(&ctx, model, product).w_axis.truncate();
                if origin.is_finite() {
                    lowest = Some(lowest.map_or(origin, |low| low.min(origin)));
                }
            }
        }
        products.sort_unstable();

        for &id in model.inverse().dropped_relationships() {
            let line = model.entity(id).map_or(0, |entity| entity.line());
            leading.push(
                Diagnostic::warning(
                    codes::RELATIONSHIP_TOO_LARGE,
                    line,
                    "this relationship has too many links to index; the objects \
                     it relates are not connected",
                )
                .with_id(id),
            );
        }

        Session {
            settings: engine.settings.clone(),
            units,
            tolerances,
            products,
            cursor: 0,
            georef: tessifc_geom::georef::georef_json(model, &units),
            caches: ctx.into_caches(),
            model_offset: lowest.unwrap_or(DVec3::ZERO),
            products_considered: considered,
            products_filtered: filtered,
            emitted: Vec::new(),
            triangles: 0,
            leading,
            seen: HashSet::new(),
        }
    }

    /// Products the session will evaluate.
    pub fn total(&self) -> usize {
        self.products.len()
    }

    /// Products evaluated so far.
    pub fn done(&self) -> usize {
        self.cursor
    }

    /// True once every product has been evaluated.
    pub fn is_finished(&self) -> bool {
        self.cursor >= self.products.len()
    }

    /// The offset a packer subtracts from every position, in metres.
    ///
    /// The lowest corner of the product placements. It is reported rather
    /// than applied to the meshes: baking it in would make two models of the
    /// same site fail to line up.
    pub fn model_offset(&self) -> DVec3 {
        self.model_offset
    }

    /// The file's map conversion as JSON, if it declares one.
    pub fn georef(&self) -> Option<&str> {
        self.georef.as_deref()
    }

    /// Units the file declared.
    pub fn units(&self) -> Units {
        self.units
    }

    /// How many products were considered.
    pub fn products_considered(&self) -> usize {
        self.products_considered
    }

    /// How many were skipped by the settings rather than by failure.
    pub fn products_filtered(&self) -> usize {
        self.products_filtered
    }

    /// Express ids of every product that produced geometry so far.
    pub fn emitted(&self) -> &[u32] {
        &self.emitted
    }

    /// Keep only these products, for re-evaluating a few after an edit.
    ///
    /// Ids that are not included products are ignored. The model offset is
    /// unchanged, so the result lines up with the pack it patches; set it
    /// explicitly with [`Session::set_model_offset`] if that pack used
    /// another.
    pub fn restrict(&mut self, ids: &[u32]) {
        let keep: HashSet<u32> = ids.iter().copied().collect();
        self.products.retain(|id| keep.contains(id));
        // Meant to precede the first batch; after one, the cursor must not run past the end.
        self.cursor = self.cursor.min(self.products.len());
    }

    /// Use this offset instead of the one derived from the placements.
    pub fn set_model_offset(&mut self, offset: DVec3) {
        self.model_offset = offset;
    }

    /// Triangles produced so far.
    pub fn triangles(&self) -> usize {
        self.triangles
    }

    /// Per-product progress, including products with no drawable representation.
    pub fn outcomes(&self, model: &Model) -> Vec<ProductOutcome> {
        product_outcomes(
            model,
            &self.products.iter().copied().collect(),
            &self.products[..self.cursor].iter().copied().collect(),
            &self.emitted.iter().copied().collect(),
        )
    }

    /// Evaluate the next batch.
    ///
    /// `stop` is asked after every product whether the batch is big enough;
    /// a batch always holds at least one product. `model` must be the model
    /// the session was started on.
    pub fn next(&mut self, model: &Model, mut stop: impl FnMut(BatchProgress) -> bool) -> Batch {
        let sink = DiagnosticSink::default();
        let caches = std::mem::take(&mut self.caches);
        let registry = Registry::shared(model.image().schema);
        let ctx = EvalCtx::with_caches(
            model,
            self.units,
            self.tolerances,
            &self.settings,
            &sink,
            caches,
        )
        .with_registry(registry);

        let mut shapes = Vec::new();
        let mut diagnostics = std::mem::take(&mut self.leading);
        let mut progress = BatchProgress {
            products: 0,
            triangles: 0,
            done: self.cursor,
            total: self.products.len(),
        };
        while self.cursor < self.products.len() {
            let id = self.products[self.cursor];
            self.cursor += 1;
            if let Some(shape) = evaluate_one(&ctx, registry, model, id) {
                progress.triangles += shape.triangle_count();
                self.emitted.push(shape.express_id);
                shapes.push(shape);
            }
            diagnostics.extend(sink.take());
            progress.products += 1;
            progress.done = self.cursor;
            if self.cursor < self.products.len() && stop(progress) {
                break;
            }
        }
        self.triangles += progress.triangles;
        let timings = ctx.time.take();
        self.caches = ctx.into_caches();
        // Nothing after the last product reads the family meshes, so a caller
        // keeping the session for its outcomes does not keep those too.
        if self.is_finished() {
            self.caches = EvalCaches::default();
        }
        Batch {
            shapes,
            diagnostics: dedup_diagnostics(diagnostics, &mut self.seen),
            is_final: self.is_finished(),
            timings,
        }
    }
}

/// One product, through the registry, into a shape.
fn evaluate_one(ctx: &EvalCtx<'_>, registry: &Registry, model: &Model, id: u32) -> Option<Shape> {
    let product = model.entity(id)?;
    let parts = product_parts(ctx, registry, model, product)?;
    let parts: Vec<ShapePart> = parts
        .into_iter()
        .filter(|part| !part.geometry.is_empty())
        .map(|part| {
            let mut geometry = part.geometry;
            // Shared meshes are reordered once each, when they are packed.
            if let PartGeometry::Unique(mesh) = &mut geometry {
                tessifc_mesh::optimize_vertex_locality(mesh);
            }
            ShapePart {
                geometry,
                color: part.color.0,
                provenance: part.provenance,
            }
        })
        .collect();
    let first = parts.first()?;
    Some(Shape {
        express_id: product.id(),
        class: product.class_name(),
        category: product_category(product),
        color: first.color,
        parts,
    })
}

/// Evaluate `ids` with a fresh context, in order.
fn evaluate_chunk(
    session: &Session,
    model: &Model,
    ids: &[u32],
) -> (Vec<Shape>, Vec<Diagnostic>, PhaseTimings) {
    let sink = DiagnosticSink::default();
    let registry = Registry::shared(model.image().schema);
    let ctx = EvalCtx::new(
        model,
        session.units,
        session.tolerances,
        &session.settings,
        &sink,
    )
    .with_registry(registry);
    let mut shapes = Vec::new();
    let mut diagnostics = Vec::new();
    for &id in ids {
        if let Some(shape) = evaluate_one(&ctx, registry, model, id) {
            shapes.push(shape);
        }
        diagnostics.extend(sink.take());
    }
    let timings = ctx.time.take();
    (shapes, diagnostics, timings)
}

/// Evaluate every id, across the thread pool where there is one.
fn evaluate_all(
    session: &Session,
    model: &Model,
    ids: &[u32],
) -> (Vec<Shape>, Vec<Diagnostic>, PhaseTimings) {
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        if rayon::current_num_threads() > 1 && ids.len() >= MIN_PARALLEL_PRODUCTS {
            // One product is one unit of work, so boolean-heavy walls cannot pin the
            // run to one thread; each rayon split carries its own caches.
            let registry = Registry::shared(model.image().schema);
            let results: Vec<(Option<Shape>, Vec<Diagnostic>, PhaseTimings)> = ids
                .par_iter()
                .map_init(EvalCaches::default, |caches, &id| {
                    let sink = DiagnosticSink::default();
                    let ctx = EvalCtx::with_caches(
                        model,
                        session.units,
                        session.tolerances,
                        &session.settings,
                        &sink,
                        std::mem::take(caches),
                    )
                    .with_registry(registry);
                    let shape = evaluate_one(&ctx, registry, model, id);
                    let timings = ctx.time.take();
                    *caches = ctx.into_caches();
                    (shape, sink.take(), timings)
                })
                .collect();
            let mut shapes = Vec::with_capacity(results.len());
            let mut diagnostics = Vec::new();
            let mut timings = PhaseTimings::default();
            for (shape, product_diagnostics, product_timings) in results {
                shapes.extend(shape);
                diagnostics.extend(product_diagnostics);
                timings.merge(product_timings);
            }
            return (shapes, diagnostics, timings);
        }
    }
    evaluate_chunk(session, model, ids)
}

/// Below this many products the pool is not worth waking.
#[cfg(feature = "parallel")]
const MIN_PARALLEL_PRODUCTS: usize = 64;

/// Drop repeats of a diagnostic already in `seen`, keeping the first.
///
/// A family's own problems are reported when its source is evaluated, and a
/// parallel or batched run evaluates the source once per context. The first
/// report is kept, at the first product that used it, so the output does not
/// depend on how the work was split.
fn dedup_diagnostics(
    diagnostics: Vec<Diagnostic>,
    seen: &mut HashSet<(DiagCode, Option<u32>, String)>,
) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .filter(|diagnostic| {
            seen.insert((
                diagnostic.code,
                diagnostic.express_id,
                diagnostic.message.clone(),
            ))
        })
        .collect()
}

/// Tolerances from the file's own `Precision`, scaled into metres.
fn tolerances_of(model: &Model, units: &Units) -> Tolerances {
    for context in model.entities_of_type("IfcGeometricRepresentationContext") {
        // A subcontext inherits Precision, and writes it as a derived slot, so
        // the value lives on the parent context.
        if let Some(precision) = context.attr("Precision").as_f64()
            && precision > 0.0
        {
            // The file's own value is the weld quantum, so it is clamped: a
            // coarse one erases small elements, a tiny one saturates the grid.
            let length = units
                .length(precision)
                .clamp(MIN_PRECISION_M, MAX_PRECISION_M);
            return Tolerances::from_precision(length);
        }
    }
    Tolerances::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_step::{ParseOptions, parse};

    fn model_of(data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    /// Two walls, one at the origin and one ten metres along.
    const TWO_WALLS: &str = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
         #2=IFCDIRECTION((0.,0.,1.));\n\
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n\
         #4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
         #5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));\n\
         #6=IFCCARTESIANPOINT((0.,0.,0.));\n\
         #7=IFCAXIS2PLACEMENT3D(#6,$,$);\n\
         #8=IFCLOCALPLACEMENT($,#7);\n\
         #9=IFCWALL('a',$,'W1',$,$,#8,#5,$,$);\n\
         #10=IFCCARTESIANPOINT((10.,0.,0.));\n\
         #11=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
         #12=IFCLOCALPLACEMENT($,#11);\n\
         #13=IFCWALL('b',$,'W2',$,$,#12,#5,$,$);\n";

    /// A family placed three times: a mapped box used by three proxies at
    /// different positions, plus one unmapped wall.
    const THREE_CHAIRS: &str = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);\n\
         #2=IFCDIRECTION((0.,0.,1.));\n\
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,1.);\n\
         #4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
         #5=IFCCARTESIANPOINT((0.,0.,0.));\n\
         #6=IFCAXIS2PLACEMENT3D(#5,$,$);\n\
         #7=IFCREPRESENTATIONMAP(#6,#4);\n\
         #20=IFCMAPPEDITEM(#7,$);\n\
         #21=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#20));\n\
         #22=IFCPRODUCTDEFINITIONSHAPE($,$,(#21));\n\
         #30=IFCCARTESIANPOINT((2.,0.,0.));\n\
         #31=IFCAXIS2PLACEMENT3D(#30,$,$);\n\
         #32=IFCLOCALPLACEMENT($,#31);\n\
         #33=IFCFURNISHINGELEMENT('c1',$,$,$,$,#32,#22,$);\n\
         #40=IFCCARTESIANPOINT((4.,0.,0.));\n\
         #41=IFCAXIS2PLACEMENT3D(#40,$,$);\n\
         #42=IFCLOCALPLACEMENT($,#41);\n\
         #43=IFCFURNISHINGELEMENT('c2',$,$,$,$,#42,#22,$);\n\
         #50=IFCCARTESIANPOINT((6.,0.,0.));\n\
         #51=IFCAXIS2PLACEMENT3D(#50,$,$);\n\
         #52=IFCLOCALPLACEMENT($,#51);\n\
         #53=IFCFURNISHINGELEMENT('c3',$,$,$,$,#52,#22,$);\n\
         #60=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n\
         #61=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#60));\n\
         #62=IFCPRODUCTDEFINITIONSHAPE($,$,(#61));\n\
         #63=IFCWALL('w',$,$,$,$,$,#62,$,$);\n";

    #[test]
    fn two_walls_become_two_shapes() {
        let model = model_of(TWO_WALLS);
        let result = Engine::new().evaluate(&model);
        assert_eq!(result.shapes.len(), 2);
        assert_eq!(result.shapes[0].express_id, 9);
        assert_eq!(result.shapes[1].express_id, 13);
        assert_eq!(result.shapes[0].class, "IfcWall");
        assert_eq!(result.triangles(), 24, "two boxes of twelve triangles");
        for shape in &result.shapes {
            assert_eq!(shape.parts.len(), 1);
            assert!((shape.parts[0].mesh().signed_volume() - 12.0).abs() < 1e-9);
        }
    }

    #[test]
    fn shapes_come_out_in_express_id_order() {
        // Determinism is a stated principle: the same file must always produce
        // the same order, whatever the model happened to iterate in.
        let model = model_of(TWO_WALLS);
        let first = Engine::new().evaluate(&model);
        let second = Engine::new().evaluate(&model);
        let ids: Vec<u32> = first.shapes.iter().map(|shape| shape.express_id).collect();
        let again: Vec<u32> = second.shapes.iter().map(|shape| shape.express_id).collect();
        assert_eq!(ids, again);
        assert!(
            ids.windows(2).all(|pair| pair[0] < pair[1]),
            "ascending: {ids:?}"
        );
    }

    #[test]
    fn the_model_offset_is_the_lowest_placement() {
        let model = model_of(TWO_WALLS);
        let result = Engine::new().evaluate(&model);
        // The first wall is placed at the origin and the second ten metres
        // along, so the lowest placement corner is the origin.
        assert!(
            result.model_offset.length() < 1e-9,
            "got {}",
            result.model_offset
        );
        // And a session decides it before evaluating anything, so a streamed
        // pack and a whole one agree.
        let session = Engine::new().session(&model);
        assert_eq!(session.model_offset(), result.model_offset);
    }

    #[test]
    fn bounds_cover_everything() {
        let model = model_of(TWO_WALLS);
        let result = Engine::new().evaluate(&model);
        let (lo, hi) = result.bounds().unwrap();
        assert!((lo - DVec3::new(-1.0, -1.0, 0.0)).length() < 1e-9);
        assert!(
            (hi - DVec3::new(11.0, 1.0, 3.0)).length() < 1e-9,
            "got {hi}"
        );
    }

    #[test]
    fn openings_are_filtered_and_counted() {
        let source = format!("{TWO_WALLS}#20=IFCOPENINGELEMENT('c',$,'O',$,$,#8,#5,$,$);\n");
        let model = model_of(&source);
        let result = Engine::new().evaluate(&model);
        assert_eq!(result.shapes.len(), 2, "the opening must not be drawn");
        assert_eq!(result.products_considered, 3);
        assert_eq!(result.products_filtered, 1);
    }

    #[test]
    fn a_file_with_no_units_says_so() {
        let model = model_of(TWO_WALLS);
        let result = Engine::new().evaluate(&model);
        assert!(result.units.assumed);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == tessifc_geom::codes::UNITS_ASSUMED),
            "assuming units silently is how a model ends up a thousand times too big"
        );
    }

    #[test]
    fn precision_from_the_context_drives_the_tolerance() {
        let source =
            format!("#30=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,0.001,#7,$);\n{TWO_WALLS}");
        let model = model_of(&source);
        let units = Units::from_model(&model);
        let tolerances = tolerances_of(&model, &units);
        assert!(
            (tolerances.len - 0.001).abs() < 1e-12,
            "got {}",
            tolerances.len
        );
    }

    #[test]
    fn a_wild_precision_is_clamped_to_something_survivable() {
        // A decimetre weld quantum would erase every element under a decimetre.
        let coarse =
            format!("#30=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,0.1,#7,$);\n{TWO_WALLS}");
        let model = model_of(&coarse);
        let units = Units::from_model(&model);
        assert_eq!(tolerances_of(&model, &units).len, MAX_PRECISION_M);

        let fine =
            format!("#30=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-30,#7,$);\n{TWO_WALLS}");
        let model = model_of(&fine);
        let units = Units::from_model(&model);
        assert_eq!(tolerances_of(&model, &units).len, MIN_PRECISION_M);
    }

    #[test]
    fn an_oversized_relationship_is_reported_not_dropped_in_silence() {
        let side = 300u32;
        let mut source = String::new();
        for id in 1..=side * 2 {
            source.push_str(&format!("#{id}=IFCWALL('w',$,$,$,$,$,$,$,$);\n"));
        }
        let join = |range: std::ops::RangeInclusive<u32>| {
            range
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let rel = side * 2 + 1;
        source.push_str(&format!(
            "#{rel}=IFCRELAGGREGATES('r',$,$,$,({}),({}));\n",
            join(1..=side),
            join(side + 1..=side * 2)
        ));
        let model = model_of(&source);
        let result = Engine::new().evaluate(&model);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == codes::RELATIONSHIP_TOO_LARGE && d.express_id == Some(rel))
        );
    }

    #[test]
    fn an_empty_model_produces_an_empty_result() {
        let model = model_of("#1=IFCWALL('a',$,'W',$,$,$,$,$,$);\n");
        let result = Engine::new().evaluate(&model);
        assert!(result.shapes.is_empty());
        assert_eq!(result.triangles(), 0);
        assert!(result.bounds().is_none());
    }

    #[test]
    fn a_family_is_one_shared_mesh_with_three_placements() {
        let model = model_of(THREE_CHAIRS);
        let result = Engine::new().evaluate(&model);
        assert_eq!(result.shapes.len(), 4);
        let chairs: Vec<&Shape> = result
            .shapes
            .iter()
            .filter(|shape| shape.class == "IfcFurnishingElement")
            .collect();
        assert_eq!(chairs.len(), 3);
        let keys: Vec<SharedKey> = chairs
            .iter()
            .map(|chair| {
                chair.parts[0]
                    .geometry
                    .shared_key()
                    .expect("a placed family with no openings stays shared")
            })
            .collect();
        assert!(
            keys.iter().all(|key| *key == keys[0]),
            "every chair names the same source: {keys:?}"
        );
        // The placements differ and the world meshes land where the file says.
        for (chair, x) in chairs.iter().zip([2.0, 4.0, 6.0]) {
            let (lo, hi) = chair.bounds().unwrap();
            let centre = (lo + hi) * 0.5;
            assert!(
                (centre - DVec3::new(x, 0.0, 0.5)).length() < 1e-9,
                "chair centre {centre}, expected x {x}"
            );
            assert!((chair.parts[0].mesh().signed_volume() - 1.0).abs() < 1e-9);
        }
        let wall = result
            .shapes
            .iter()
            .find(|shape| shape.class == "IfcWall")
            .unwrap();
        assert!(
            wall.parts[0].geometry.shared_key().is_none(),
            "an ordinary solid is the product's own mesh"
        );
    }

    #[test]
    fn a_session_streams_the_same_shapes_as_one_call() {
        let model = model_of(THREE_CHAIRS);
        let whole = Engine::new().evaluate(&model);
        let engine = Engine::new();
        let mut session = engine.session(&model);
        assert_eq!(session.total(), 4);
        let mut streamed = Vec::new();
        let mut batches = 0;
        loop {
            // One product per batch, the smallest a caller can ask for.
            let batch = session.next(&model, |progress| progress.products >= 1);
            batches += 1;
            streamed.extend(batch.shapes);
            if batch.is_final {
                break;
            }
        }
        assert_eq!(batches, 4);
        assert!(session.is_finished());
        assert_eq!(session.done(), 4);
        assert_eq!(session.emitted(), &[33, 43, 53, 63]);
        assert_eq!(session.triangles(), whole.triangles());
        let ids: Vec<u32> = streamed.iter().map(|shape| shape.express_id).collect();
        let expected: Vec<u32> = whole.shapes.iter().map(|shape| shape.express_id).collect();
        assert_eq!(ids, expected);
        for (a, b) in streamed.iter().zip(&whole.shapes) {
            assert_eq!(a.parts.len(), b.parts.len());
            assert_eq!(
                a.parts[0].geometry.shared_key(),
                b.parts[0].geometry.shared_key()
            );
            assert_eq!(a.parts[0].mesh().positions, b.parts[0].mesh().positions);
        }
    }

    #[test]
    fn a_session_reports_each_diagnostic_once() {
        // No units: the model-level warning goes out with the first batch and
        // never again, even though every batch starts a fresh context.
        let model = model_of(TWO_WALLS);
        let engine = Engine::new();
        let mut session = engine.session(&model);
        let first = session.next(&model, |progress| progress.products >= 1);
        let second = session.next(&model, |progress| progress.products >= 1);
        let units = |batch: &Batch| {
            batch
                .diagnostics
                .iter()
                .filter(|d| d.code == tessifc_geom::codes::UNITS_ASSUMED)
                .count()
        };
        assert_eq!(units(&first), 1);
        assert_eq!(units(&second), 0);
        assert!(second.is_final);
    }

    #[test]
    fn a_restricted_session_evaluates_only_what_it_was_given() {
        let model = model_of(THREE_CHAIRS);
        let engine = Engine::new();
        let full = engine.session(&model);
        let mut session = engine.session(&model);
        session.restrict(&[43, 999, 63]);
        assert_eq!(session.total(), 2, "an id that is not a product is ignored");
        assert_eq!(session.model_offset(), full.model_offset());
        let batch = session.next(&model, |_| false);
        assert!(batch.is_final);
        let ids: Vec<u32> = batch.shapes.iter().map(|shape| shape.express_id).collect();
        assert_eq!(ids, vec![43, 63]);
    }

    #[test]
    fn a_batch_always_holds_at_least_one_product() {
        let model = model_of(TWO_WALLS);
        let engine = Engine::new();
        let mut session = engine.session(&model);
        let batch = session.next(&model, |_| true);
        assert_eq!(batch.shapes.len(), 1);
        assert!(!batch.is_final);
    }

    #[test]
    fn chunked_evaluation_matches_the_serial_result() {
        // The parallel path is chunks evaluated with separate contexts and
        // concatenated. Whatever the chunking, the answer must be the same.
        let model = model_of(THREE_CHAIRS);
        let engine = Engine::new();
        let session = engine.session(&model);
        let ids = session.products.clone();
        let (serial, serial_diagnostics, _) = evaluate_chunk(&session, &model, &ids);
        let mut chunked = Vec::new();
        let mut chunked_diagnostics = Vec::new();
        for chunk in ids.chunks(1) {
            let (shapes, diagnostics, _) = evaluate_chunk(&session, &model, chunk);
            chunked.extend(shapes);
            chunked_diagnostics.extend(diagnostics);
        }
        assert_eq!(serial.len(), chunked.len());
        for (a, b) in serial.iter().zip(&chunked) {
            assert_eq!(a.express_id, b.express_id);
            assert_eq!(
                a.parts[0].geometry.shared_key(),
                b.parts[0].geometry.shared_key()
            );
            assert_eq!(a.parts[0].mesh().positions, b.parts[0].mesh().positions);
        }
        let codes = |list: &[Diagnostic]| list.iter().map(|d| d.code).collect::<Vec<_>>();
        assert_eq!(
            codes(&dedup_diagnostics(serial_diagnostics, &mut HashSet::new())),
            codes(&dedup_diagnostics(chunked_diagnostics, &mut HashSet::new()))
        );
    }

    #[test]
    fn restricting_after_a_batch_keeps_the_cursor_in_range() {
        let model = model_of(THREE_CHAIRS);
        let engine = Engine::new();
        let mut session = engine.session(&model);
        let _ = session.next(&model, |_| true);
        session.restrict(&[]);
        assert_eq!(session.total(), 0);
        assert!(session.done() <= session.total());
        // Outcomes slice the products up to the cursor; this used to slice past the end.
        let outcomes = session.outcomes(&model);
        assert!(outcomes.iter().all(|outcome| outcome.express_id != 0));
    }

    #[test]
    fn a_finished_session_holds_no_family_meshes() {
        let model = model_of(THREE_CHAIRS);
        let engine = Engine::new();
        let mut session = engine.session(&model);
        let batch = session.next(&model, |_| false);
        assert!(batch.is_final);
        assert_eq!(session.caches.mapped_sources(), 0);
    }
}
