// SPDX-License-Identifier: Apache-2.0
//! What an evaluator is given, and what it may complain about.

use crate::error::GeomError;
use crate::eval::solids::MappedPart;
use crate::placement::PlacementCache;
use crate::units::Units;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use tessifc_mesh::Mesh64;
use tessifc_model::Model;
use tessifc_step::{DiagCode, Diagnostic};

/// Distances below which two things are the same thing.
///
/// Every comparison in this crate uses one of these, never a literal.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tolerances {
    /// Two points closer than this are the same point, in metres.
    pub len: f64,
    /// A triangle smaller than this has no area, in square metres.
    pub area: f64,
    /// Two directions closer than this are parallel, in radians.
    pub angle: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        // A tenth of a millimetre.
        Tolerances::from_precision(1e-4)
    }
}

impl Tolerances {
    /// Derive the family from one length precision, in metres.
    pub fn from_precision(length: f64) -> Self {
        let len = if length.is_finite() && length > 0.0 {
            length
        } else {
            1e-4
        };
        Tolerances {
            len,
            area: len * len,
            angle: 1e-6,
        }
    }
}

/// Everything a caller can turn up or down.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Segments per full circle, or `None` to choose from the chord tolerance.
    pub circle_segments: Option<u32>,
    /// How far a chord may sag from the true arc, in metres.
    pub chord_tolerance_m: f64,
    /// Maximum turn between consecutive circle segments, in radians.
    pub angular_tolerance_rad: f64,
    /// Maximum segments per circle, including fixed segment requests.
    pub max_circle_segments: u32,
    /// Maximum vertices in one trimmed surface patch, including its boundary.
    pub max_surface_vertices: u32,
    /// Recover an inconsistent pcurve scale only when its 3D curve verifies the repair.
    pub repair_pcurve_domains: bool,
    /// Permit diagnosed recovery from inconsistent surface-curve representations.
    pub repair_surface_curves: bool,
    /// Include space and spatial-zone volumes, flagged so a viewer can hide them.
    pub include_spaces: bool,
    /// Include opening and voiding-feature products. Drawing a hole fills it in.
    pub include_openings: bool,
    /// Include `IfcAnnotation` and other curve-only products.
    pub include_annotations: bool,
    /// Include non-physical reference products such as ports and analysis items.
    pub include_references: bool,
    /// Weld vertices before deciding whether a mesh is closed.
    pub weld: bool,
    /// Cut `IfcRelVoidsElement` openings out of the elements they void.
    pub cut_openings: bool,
    /// How deep a representation graph may nest before it is abandoned.
    pub max_depth: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            circle_segments: None,
            chord_tolerance_m: 0.002,
            angular_tolerance_rad: std::f64::consts::PI / 18.0,
            max_circle_segments: 512,
            max_surface_vertices: 16_384,
            repair_pcurve_domains: true,
            repair_surface_curves: false,
            include_spaces: true,
            include_openings: false,
            include_annotations: false,
            include_references: false,
            weld: true,
            cut_openings: true,
            max_depth: 24,
        }
    }
}

impl Settings {
    /// Reject invalid or unbounded settings before evaluation starts.
    pub fn validate(&self) -> Result<(), GeomError> {
        let invalid = |message: &str| GeomError::InvalidSettings(message.into());
        if !self.chord_tolerance_m.is_finite() || self.chord_tolerance_m <= 0.0 {
            return Err(invalid("chordToleranceM must be finite and positive"));
        }
        if !self.angular_tolerance_rad.is_finite()
            || self.angular_tolerance_rad <= 0.0
            || self.angular_tolerance_rad > std::f64::consts::PI
        {
            return Err(invalid("angularToleranceRad must be in (0, pi]"));
        }
        if !(8..=4096).contains(&self.max_circle_segments) {
            return Err(invalid("maxCircleSegments must be in [8, 4096]"));
        }
        if !(64..=1_048_576).contains(&self.max_surface_vertices) {
            return Err(invalid("maxSurfaceVertices must be in [64, 1048576]"));
        }
        if self
            .circle_segments
            .is_some_and(|count| count < 3 || count > self.max_circle_segments)
        {
            return Err(invalid("circleSegments must be in [3, maxCircleSegments]"));
        }
        if !(1..=128).contains(&self.max_depth) {
            return Err(invalid("maxDepth must be in [1, 128]"));
        }
        Ok(())
    }

    /// How many segments to spend on a full circle of this radius.
    ///
    /// Adaptive from the chord tolerance, clamped at both ends.
    pub fn segments_for_radius(&self, radius: f64) -> u32 {
        let cap = self.max_circle_segments.clamp(8, 4096);
        if let Some(fixed) = self.circle_segments {
            return fixed.clamp(3, cap);
        }
        if radius <= 0.0 || !radius.is_finite() {
            return 12;
        }
        // sin(theta / 4)^2 = sagitta / (2r), avoiding cancellation for small tolerances.
        let angle = 4.0
            * (0.5 * (self.chord_tolerance_m / radius).min(2.0))
                .sqrt()
                .asin();
        let count = (std::f64::consts::TAU / angle.min(self.angular_tolerance_rad)).ceil();
        (count as u32)
            .clamp(8, cap)
            .div_ceil(4)
            .saturating_mul(4)
            .min(cap)
    }

    /// Whether a chosen circle discretisation satisfies both accuracy limits.
    pub fn circle_tolerance_met(&self, radius: f64, segments: u32) -> bool {
        if !radius.is_finite() || radius <= 0.0 || segments < 3 {
            return false;
        }
        let turn = std::f64::consts::TAU / segments as f64;
        let sagitta = radius * (2.0 * (turn / 4.0).sin().powi(2));
        sagitta <= self.chord_tolerance_m && turn <= self.angular_tolerance_rad
    }
}

/// Diagnostics collected while evaluating, in evaluation order.
#[derive(Default)]
pub struct DiagnosticSink {
    items: RefCell<Vec<Diagnostic>>,
}

impl DiagnosticSink {
    /// Record one.
    pub fn push(&self, diagnostic: Diagnostic) {
        self.items.borrow_mut().push(diagnostic);
    }

    /// Record a warning against an instance.
    pub fn warn(&self, code: DiagCode, express_id: u32, message: impl Into<String>) {
        self.push(Diagnostic::warning(code, 0, message).with_id(express_id));
    }

    /// Record an error against an instance.
    pub fn error(&self, code: DiagCode, express_id: u32, message: impl Into<String>) {
        self.push(Diagnostic::error(code, 0, message).with_id(express_id));
    }

    /// Take everything recorded so far.
    pub fn take(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.items.borrow_mut())
    }

    /// How many are held.
    pub fn len(&self) -> usize {
        self.items.borrow().len()
    }

    /// True when nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }
}

/// A monotonic stopwatch, and a no-op where the target has no clock.
///
/// `Instant::now` panics on `wasm32-unknown-unknown`, and the kernel runs there.
pub struct Stopwatch {
    #[cfg(not(target_arch = "wasm32"))]
    started: std::time::Instant,
}

impl Stopwatch {
    /// Start one.
    pub fn start() -> Self {
        Stopwatch {
            #[cfg(not(target_arch = "wasm32"))]
            started: std::time::Instant::now(),
        }
    }

    /// Milliseconds since it started; always zero without a clock.
    pub fn ms(&self) -> f64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.started.elapsed().as_secs_f64() * 1e3
        }
        #[cfg(target_arch = "wasm32")]
        {
            0.0
        }
    }
}

impl Default for Stopwatch {
    fn default() -> Self {
        Stopwatch::start()
    }
}

/// What one evaluation spent in each phase, summed.
///
/// Times are processor time, not wall time: a parallel run sums what every
/// thread spent, so a total here can exceed the run's own duration.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PhaseTimings {
    /// Processor time inside `IfcBooleanResult`, in milliseconds.
    pub boolean_ms: f64,
    /// Processor time subtracting a product's openings, in milliseconds.
    pub openings_ms: f64,
    /// How many boolean operators were performed.
    pub boolean_calls: u32,
    /// How many products had openings subtracted.
    pub openings_calls: u32,
}

impl PhaseTimings {
    /// Add another evaluation's totals to these.
    pub fn merge(&mut self, other: PhaseTimings) {
        self.boolean_ms += other.boolean_ms;
        self.openings_ms += other.openings_ms;
        self.boolean_calls += other.boolean_calls;
        self.openings_calls += other.openings_calls;
    }
}

/// Phase times gathered while evaluating, one per context.
///
/// Held by value on the context rather than shared, for the same reason the
/// diagnostics are: one product per thread, merged serially afterwards.
#[derive(Default)]
pub struct Timings {
    boolean_ms: std::cell::Cell<f64>,
    openings_ms: std::cell::Cell<f64>,
    boolean_calls: std::cell::Cell<u32>,
    openings_calls: std::cell::Cell<u32>,
}

impl Timings {
    /// Charge a boolean operator.
    pub fn add_boolean(&self, ms: f64) {
        self.boolean_ms.set(self.boolean_ms.get() + ms);
        self.boolean_calls.set(self.boolean_calls.get() + 1);
    }

    /// Charge one product's opening subtraction.
    pub fn add_openings(&self, ms: f64) {
        self.openings_ms.set(self.openings_ms.get() + ms);
        self.openings_calls.set(self.openings_calls.get() + 1);
    }

    /// Take everything charged so far.
    pub fn take(&self) -> PhaseTimings {
        PhaseTimings {
            boolean_ms: self.boolean_ms.replace(0.0),
            openings_ms: self.openings_ms.replace(0.0),
            boolean_calls: self.boolean_calls.replace(0),
            openings_calls: self.openings_calls.replace(0),
        }
    }
}

/// What an evaluator is handed.
///
/// Evaluators hold no state of their own, so one product per thread is safe.
pub struct EvalCtx<'a> {
    /// The model being read.
    pub model: &'a Model,
    /// File units, already resolved.
    pub units: Units,
    /// Tolerances derived from the file's own precision.
    pub tol: Tolerances,
    /// Caller settings.
    pub settings: &'a Settings,
    /// Where complaints go.
    pub diag: &'a DiagnosticSink,
    /// Where phase times go.
    pub time: Timings,
    /// Shared placement resolution.
    pub placements: RefCell<PlacementCache>,
    /// The evaluator set for nested lookups; `None` means the shared defaults.
    registry: Option<&'a crate::registry::Registry>,
    /// Material colours, built on first use and then reused.
    materials: RefCell<Option<MaterialTable>>,
    /// Geometry of each `IfcRepresentationMap`, before per-instance targets.
    mapped_geometry: RefCell<HashMap<u32, MappedResult>>,
    /// Same-colour items of a mapped representation merged, keyed by (representation, items).
    merged_mapped: RefCell<HashMap<(u32, u64), Arc<Mesh64>>>,
    /// `IfcIndexedColourMap` by the face set it colours, built on first use.
    colour_maps: RefCell<Option<HashMap<u32, u32>>>,
    /// Boolean outcomes retained across nested evaluation and cached families.
    boolean_outcomes: RefCell<HashMap<u32, crate::product::BooleanStatus>>,
    /// Boolean results of the product being evaluated, by item id.
    boolean_meshes: RefCell<HashMap<u32, Arc<Mesh64>>>,
    /// Current nesting depth in the representation graph.
    depth: std::cell::Cell<u32>,
}

/// A mapped representation's items, or the error evaluating them gave.
///
/// Shared rather than cloned, since a family is placed thousands of times.
pub(crate) type MappedResult = Result<Arc<Vec<MappedPart>>, GeomError>;

/// Material colours by object id, built once per model.
pub(crate) type MaterialTable = Arc<HashMap<u32, crate::style::Rgba>>;

/// Everything a context remembers between products.
///
/// A streaming caller carries these across contexts with [`EvalCtx::into_caches`].
#[derive(Default)]
pub struct EvalCaches {
    boolean_outcomes: HashMap<u32, crate::product::BooleanStatus>,
    /// Resolved placement chains.
    pub placements: PlacementCache,
    materials: Option<MaterialTable>,
    mapped_geometry: HashMap<u32, MappedResult>,
    merged_mapped: HashMap<(u32, u64), Arc<Mesh64>>,
}

impl EvalCaches {
    /// How many mapped representations have been evaluated.
    pub fn mapped_sources(&self) -> usize {
        self.mapped_geometry.len()
    }
}

impl<'a> EvalCtx<'a> {
    /// A context for one model.
    pub fn new(
        model: &'a Model,
        units: Units,
        tol: Tolerances,
        settings: &'a Settings,
        diag: &'a DiagnosticSink,
    ) -> Self {
        EvalCtx::with_caches(model, units, tol, settings, diag, EvalCaches::default())
    }

    /// A context that starts from caches an earlier context filled.
    pub fn with_caches(
        model: &'a Model,
        units: Units,
        tol: Tolerances,
        settings: &'a Settings,
        diag: &'a DiagnosticSink,
        caches: EvalCaches,
    ) -> Self {
        EvalCtx {
            model,
            units,
            tol,
            settings,
            diag,
            time: Timings::default(),
            placements: RefCell::new(caches.placements),
            registry: None,
            materials: RefCell::new(caches.materials),
            mapped_geometry: RefCell::new(caches.mapped_geometry),
            merged_mapped: RefCell::new(caches.merged_mapped),
            colour_maps: RefCell::new(None),
            boolean_outcomes: RefCell::new(caches.boolean_outcomes),
            boolean_meshes: RefCell::new(HashMap::new()),
            depth: std::cell::Cell::new(0),
        }
    }

    /// A boolean result already built for this product, copied out.
    pub(crate) fn cached_boolean(&self, id: u32) -> Option<Mesh64> {
        self.boolean_meshes
            .borrow()
            .get(&id)
            .map(|mesh| (**mesh).clone())
    }

    /// Keep a boolean result for the rest of this product.
    pub(crate) fn cache_boolean(&self, id: u32, mesh: &Mesh64) {
        self.boolean_meshes
            .borrow_mut()
            .insert(id, Arc::new(mesh.clone()));
    }

    /// Forget the boolean results of the previous product.
    pub(crate) fn clear_boolean_cache(&self) {
        self.boolean_meshes.borrow_mut().clear();
    }

    /// The `IfcIndexedColourMap` that colours a tessellated face set, if any.
    pub fn colour_map_of(&self, face_set: u32) -> Option<u32> {
        let mut maps = self.colour_maps.borrow_mut();
        let index = maps.get_or_insert_with(|| {
            self.model
                .entities_of_type("IfcIndexedColourMap")
                .filter_map(|map| {
                    map.attr("MappedTo")
                        .as_entity()
                        .map(|target| (target.id(), map.id()))
                })
                .collect()
        });
        index.get(&face_set).copied()
    }

    /// Take the caches back out, to seed the next context for the same model.
    pub fn into_caches(self) -> EvalCaches {
        EvalCaches {
            boolean_outcomes: self.boolean_outcomes.into_inner(),
            placements: self.placements.into_inner(),
            materials: self.materials.into_inner(),
            mapped_geometry: self.mapped_geometry.into_inner(),
            merged_mapped: self.merged_mapped.into_inner(),
        }
    }

    /// Use `registry` for sub-item lookups instead of the shared defaults.
    ///
    /// This makes a caller's own evaluators reachable from nested items.
    pub fn with_registry(mut self, registry: &'a crate::registry::Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// The evaluator set to use for a sub-item.
    pub fn registry(&self) -> &crate::registry::Registry {
        match self.registry {
            Some(registry) => registry,
            None => crate::registry::Registry::shared(self.model.image().schema),
        }
    }

    /// Record the most severe boolean result for an item.
    pub fn record_boolean(&self, item: u32, outcome: crate::product::BooleanStatus) {
        self.boolean_outcomes
            .borrow_mut()
            .entry(item)
            .and_modify(|old| *old = old.merge(outcome))
            .or_insert(outcome);
    }

    /// Boolean result for an item evaluated in this context or its caches.
    pub fn boolean_outcome(&self, item: u32) -> crate::product::BooleanStatus {
        self.boolean_outcomes
            .borrow()
            .get(&item)
            .copied()
            .unwrap_or_default()
    }

    /// Choose circle segments and report when the configured budget limits accuracy.
    pub fn segments_for_radius(&self, radius: f64) -> u32 {
        let segments = self.settings.segments_for_radius(radius);
        if radius.is_finite()
            && radius > 0.0
            && !self.settings.circle_tolerance_met(radius, segments)
        {
            self.diag.warn(crate::error::codes::TESSELLATION_TOLERANCE_UNMET, 0,
                format!("{segments} circle segments at radius {radius} m exceed the requested chord or angular tolerance"));
        }
        segments
    }

    /// Run `body` one level deeper, refusing past the configured limit.
    ///
    /// A self-referencing representation would otherwise overflow the stack.
    pub fn nested<T>(&self, body: impl FnOnce() -> Result<T, GeomError>) -> Result<T, GeomError> {
        let depth = self.depth.get();
        if depth >= self.settings.max_depth {
            return Err(GeomError::TooDeep(depth));
        }
        self.depth.set(depth + 1);
        let result = body();
        self.depth.set(depth);
        result
    }

    /// The material colour table, built once with `build` and then reused.
    pub(crate) fn material_colours(
        &self,
        build: impl FnOnce(&'a Model) -> HashMap<u32, crate::style::Rgba>,
    ) -> MaterialTable {
        let mut slot = self.materials.borrow_mut();
        slot.get_or_insert_with(|| Arc::new(build(self.model)))
            .clone()
    }

    /// Evaluate a mapped representation once and share it across mapping targets.
    ///
    /// Caching the source also emits its diagnostics once rather than per placement.
    pub(crate) fn mapped_parts(
        &self,
        representation_id: u32,
        build: impl FnOnce() -> Result<Vec<MappedPart>, GeomError>,
    ) -> MappedResult {
        if let Some(parts) = self.mapped_geometry.borrow().get(&representation_id) {
            return parts.clone();
        }
        let parts = build().map(Arc::new);
        self.mapped_geometry
            .borrow_mut()
            .insert(representation_id, parts.clone());
        parts
    }

    /// The items `items` (a bit per index) of a mapped representation, merged into one mesh.
    pub(crate) fn merged_mapped(
        &self,
        representation_id: u32,
        items: u64,
        build: impl FnOnce() -> Mesh64,
    ) -> Arc<Mesh64> {
        let key = (representation_id, items);
        if let Some(mesh) = self.merged_mapped.borrow().get(&key) {
            return mesh.clone();
        }
        let mesh = Arc::new(build());
        self.merged_mapped.borrow_mut().insert(key, mesh.clone());
        mesh
    }

    /// Current nesting depth, for diagnostics.
    pub fn depth(&self) -> u32 {
        self.depth.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec3;
    use std::cell::Cell;

    #[test]
    fn tolerances_derive_from_precision() {
        let tol = Tolerances::from_precision(1e-5);
        assert_eq!(tol.len, 1e-5);
        // The square of 1e-5 is not exactly 1e-10 in binary floating point.
        assert!((tol.area - 1e-10).abs() < 1e-22, "got {}", tol.area);
    }

    #[test]
    fn a_nonsense_precision_falls_back() {
        assert_eq!(Tolerances::from_precision(0.0).len, 1e-4);
        assert_eq!(Tolerances::from_precision(-1.0).len, 1e-4);
        assert_eq!(Tolerances::from_precision(f64::NAN).len, 1e-4);
    }

    #[test]
    fn segment_counts_scale_with_radius() {
        let settings = Settings::default();
        let small = settings.segments_for_radius(0.01);
        let large = settings.segments_for_radius(10.0);
        assert!(
            small < large,
            "a bigger circle needs more segments: {small} then {large}"
        );
        assert!((8..=128).contains(&small));
        assert!((8..=512).contains(&large));
    }

    #[test]
    fn a_fixed_segment_count_overrides_the_radius() {
        let settings = Settings {
            circle_segments: Some(24),
            ..Settings::default()
        };
        assert_eq!(settings.segments_for_radius(0.01), 24);
        assert_eq!(settings.segments_for_radius(100.0), 24);
    }

    #[test]
    fn adaptive_circles_meet_the_requested_sagitta() {
        let settings = Settings::default();
        for radius in [0.01, 1.0, 10.0, 100.0] {
            let count = settings.segments_for_radius(radius);
            let sagitta =
                2.0 * radius * (std::f64::consts::PI / (2.0 * count as f64)).sin().powi(2);
            assert!(
                sagitta <= settings.chord_tolerance_m,
                "radius {radius}: {count} segments miss by {sagitta} m"
            );
        }
        assert!((50..=52).contains(&settings.segments_for_radius(1.0)));
    }

    #[test]
    fn absurd_radii_do_not_produce_absurd_counts() {
        let settings = Settings::default();
        assert!(
            (8..=128).contains(&settings.segments_for_radius(0.0))
                || settings.segments_for_radius(0.0) == 12
        );
        assert!(settings.segments_for_radius(f64::INFINITY) <= 128);
        assert!(settings.segments_for_radius(-5.0) > 0);
    }

    #[test]
    fn diagnostics_accumulate_and_drain() {
        let sink = DiagnosticSink::default();
        assert!(sink.is_empty());
        sink.warn(DiagCode::UNKNOWN_CLASS, 7, "something");
        sink.error(DiagCode::BAD_NUMBER, 8, "something else");
        assert_eq!(sink.len(), 2);
        let taken = sink.take();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].express_id, Some(7));
        assert!(sink.is_empty(), "taking must drain");
    }

    #[test]
    fn mapped_geometry_is_built_once_per_representation() {
        let model = Model::new(tessifc_step::parse(
            b"ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;ENDSEC;",
            &tessifc_step::ParseOptions::default(),
        ));
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );
        let builds = Cell::new(0);
        let first = ctx
            .mapped_parts(42, || {
                builds.set(builds.get() + 1);
                let mut mesh = Mesh64::new();
                mesh.positions.extend([DVec3::ZERO, DVec3::X, DVec3::Y]);
                mesh.indices.extend([0, 1, 2]);
                Ok(vec![MappedPart {
                    mesh: Arc::new(mesh),
                    colour: None,
                }])
            })
            .unwrap();
        let second = ctx
            .mapped_parts(42, || {
                builds.set(builds.get() + 1);
                Err(GeomError::Degenerate("must not run".into()))
            })
            .unwrap();
        assert_eq!(builds.get(), 1);
        assert_eq!(first, second);
    }
}
