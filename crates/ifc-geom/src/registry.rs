// SPDX-License-Identifier: Apache-2.0
//! The extension point: evaluators keyed by IFC class.
//!
//! Dispatch is exact class first, then up the inheritance chain.

use crate::context::EvalCtx;
use crate::error::GeomError;
use glam::{DMat4, DVec2, DVec3, DVec4};
use rustc_hash::FxHashMap;
use std::f64::consts::FRAC_PI_2;
use std::sync::OnceLock;
use tessifc_mesh::Mesh64;
use tessifc_model::Entity;
use tessifc_schema::{ClassId, Schema, SchemaId};

/// A 2D outline with holes, in the profile's own coordinates, in metres.
///
/// An open profile is a polyline with no area; sweeping it gives a surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile2D {
    /// The outer boundary, or the polyline of an open profile.
    pub outer: Vec<DVec2>,
    /// Holes, each wound against `outer`.
    pub holes: Vec<Vec<DVec2>>,
    /// True when `outer` does not close: `IfcArbitraryOpenProfileDef`.
    pub open: bool,
}

impl Profile2D {
    /// A closed profile with no holes.
    pub fn new(outer: Vec<DVec2>) -> Self {
        Profile2D {
            outer,
            holes: Vec::new(),
            open: false,
        }
    }

    /// An open profile: a polyline that encloses nothing.
    pub fn open(outer: Vec<DVec2>) -> Self {
        Profile2D {
            outer,
            holes: Vec::new(),
            open: true,
        }
    }

    /// Convert to the mesh crate's polygon type.
    pub fn to_polygon(&self) -> tessifc_mesh::Polygon2 {
        tessifc_mesh::Polygon2 {
            outer: self.outer.clone(),
            holes: self.holes.clone(),
        }
    }
}

/// A polyline in 3D, in metres.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Polyline3 {
    /// The points, in order.
    pub points: Vec<DVec3>,
    /// Whether the last point joins the first.
    pub closed: bool,
    /// The curve's own parameter at each point, when the evaluator knows it;
    /// empty otherwise.
    pub parameters: Vec<f64>,
}

/// Produces a solid, or a surface, from a representation item.
pub trait SolidEvaluator: Send + Sync {
    /// Exact IFC class names this handles.
    fn classes(&self) -> &'static [&'static str];
    /// Turn one item into a mesh in the item's own coordinates.
    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError>;
}

/// Produces a 2D outline from a profile definition.
pub trait ProfileEvaluator: Send + Sync {
    /// Exact IFC class names this handles.
    fn classes(&self) -> &'static [&'static str];
    /// Turn one profile into an outline with holes.
    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError>;
}

/// Produces a polyline from a curve.
pub trait CurveEvaluator: Send + Sync {
    /// Exact IFC class names this handles.
    fn classes(&self) -> &'static [&'static str];
    /// Turn one curve into a polyline.
    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError>;
}

/// A B-spline surface's control net and knots, in the surface's own frame.
#[derive(Clone, Debug)]
pub struct BSplineSurface {
    /// Degree along u, the control net's row direction.
    pub u_degree: usize,
    /// Degree along v, the control net's column direction.
    pub v_degree: usize,
    /// Control points, `net[u][v]`.
    pub net: Vec<Vec<DVec3>>,
    /// Weights in the same shape; all one for a non-rational surface.
    pub weights: Vec<Vec<f64>>,
    /// The full u knot vector, multiplicities already expanded.
    pub u_knots: Vec<f64>,
    /// The full v knot vector.
    pub v_knots: Vec<f64>,
}

/// What kind of surface, in its own canonical frame.
#[derive(Clone, Debug)]
pub enum SurfaceKind {
    /// The z = 0 plane; (u, v) are x and y.
    Plane,
    /// About the z axis; u is the angle, v is the height.
    Cylinder {
        /// Distance from the axis.
        radius: f64,
    },
    /// Centred on the origin; u is longitude, v is latitude.
    Sphere {
        /// Distance from the centre.
        radius: f64,
    },
    /// About the z axis; u goes round the axis, v round the tube.
    Torus {
        /// Distance from the axis to the tube's centre circle.
        major: f64,
        /// Tube radius.
        minor: f64,
    },
    /// A profile in the z = 0 plane swept along `along` for `depth`.
    Extrusion {
        /// The profile outline, in the z = 0 plane.
        profile: Vec<DVec2>,
        /// Whether the outline's last point joins its first.
        closed: bool,
        /// Sweep direction, which must leave the profile plane.
        along: DVec3,
        /// How far it is swept.
        depth: f64,
        /// The swept curve's own parameter at each outline point, or empty.
        knots: Vec<f64>,
    },
    /// A profile turned about an axis; u is the angle, v the profile parameter.
    Revolution {
        /// The profile as distance from the axis and height along it.
        section: Vec<DVec2>,
        /// Whether the profile's last point joins its first.
        closed: bool,
        /// A point on the axis.
        origin: DVec3,
        /// The axis direction, unit length.
        axis: DVec3,
        /// The radial direction at u = 0, unit length and across the axis.
        reference: DVec3,
        /// The swept curve's own parameter at each section point, or empty.
        knots: Vec<f64>,
    },
    /// A tensor-product B-spline, rational when any weight is not one.
    BSpline(Box<BSplineSurface>),
}

/// A surface a face can be trimmed on.
///
/// Its two jobs are [`Surface::point`], which puts a parameter pair on the
/// surface, and [`Surface::invert`], which puts a point back into parameters.
/// A face's edges arrive as 3D points and have to be inverted before the
/// region they bound can be triangulated.
#[derive(Clone, Debug)]
pub struct Surface {
    /// Which surface, in its own frame.
    pub kind: SurfaceKind,
    /// Where that frame sits.
    pub frame: DMat4,
    inverse: DMat4,
}

impl Surface {
    /// A surface of one kind, placed by one frame.
    pub fn new(kind: SurfaceKind, frame: DMat4) -> Surface {
        let inverse = frame.inverse();
        Surface {
            kind,
            frame,
            inverse,
        }
    }

    /// The point at one parameter pair.
    pub fn point(&self, uv: DVec2) -> DVec3 {
        self.frame.transform_point3(self.local_point(uv))
    }

    /// The parameters of a point, or `None` when it is not on the surface.
    ///
    /// Every branch is closed form or a bounded search: nothing here can fail
    /// to terminate on a value a file supplied.
    pub fn invert(&self, point: DVec3, tolerance: f64) -> Option<DVec2> {
        let tol = if tolerance.is_finite() && tolerance > 0.0 {
            tolerance
        } else {
            1e-9
        };
        let local = self.inverse.transform_point3(point);
        let uv = self.local_invert(local, tol)?;
        if !uv.is_finite() {
            return None;
        }
        Some(uv)
    }

    /// The period of each parameter, where it repeats.
    pub fn periods(&self) -> (Option<f64>, Option<f64>) {
        let turn = std::f64::consts::TAU;
        match &self.kind {
            SurfaceKind::Cylinder { .. } | SurfaceKind::Sphere { .. } => (Some(turn), None),
            SurfaceKind::Torus { .. } => (Some(turn), Some(turn)),
            SurfaceKind::Revolution { closed, .. } => {
                (Some(turn), closed.then_some(section_period(&self.kind)))
            }
            SurfaceKind::Extrusion { closed, .. } => {
                (closed.then_some(section_period(&self.kind)), None)
            }
            SurfaceKind::Plane | SurfaceKind::BSpline(_) => (None, None),
        }
    }

    /// Is this point where the surface's own parameters stop being usable?
    ///
    /// A sphere's poles are one point for every longitude, and the tip of a
    /// revolved profile that touches its axis is the same. A face whose
    /// boundary runs through one cannot be trimmed in (u, v) at all: the
    /// boundary comes back as a jump between two unrelated parameters.
    pub fn is_singular(&self, point: DVec3, tolerance: f64) -> bool {
        let local = self.inverse.transform_point3(point);
        let tol = if tolerance.is_finite() && tolerance > 0.0 {
            tolerance
        } else {
            1e-9
        };
        match &self.kind {
            SurfaceKind::Sphere { radius } => local.truncate().length() <= tol.max(radius * 1e-6),
            SurfaceKind::Cylinder { .. } | SurfaceKind::Torus { .. } => {
                local.truncate().length() <= tol
            }
            SurfaceKind::Revolution { origin, axis, .. } => {
                let offset = local - *origin;
                (offset - *axis * offset.dot(*axis)).length() <= tol
            }
            SurfaceKind::Plane | SurfaceKind::Extrusion { .. } | SurfaceKind::BSpline(_) => false,
        }
    }

    /// True when the surface bends in both parameters, so a triangle over it
    /// needs points inside as well as on its edges.
    pub fn is_doubly_curved(&self) -> bool {
        match &self.kind {
            SurfaceKind::Plane | SurfaceKind::Cylinder { .. } | SurfaceKind::Extrusion { .. } => {
                false
            }
            SurfaceKind::Sphere { .. } | SurfaceKind::Torus { .. } => true,
            SurfaceKind::Revolution { section, .. } => section.len() > 2,
            SurfaceKind::BSpline(surface) => surface.u_degree > 1 || surface.v_degree > 1,
        }
    }

    fn local_point(&self, uv: DVec2) -> DVec3 {
        match &self.kind {
            SurfaceKind::Plane => DVec3::new(uv.x, uv.y, 0.0),
            SurfaceKind::Cylinder { radius } => {
                DVec3::new(radius * uv.x.cos(), radius * uv.x.sin(), uv.y)
            }
            SurfaceKind::Sphere { radius } => {
                let (u, v) = (uv.x, uv.y.clamp(-FRAC_PI_2, FRAC_PI_2));
                DVec3::new(
                    radius * v.cos() * u.cos(),
                    radius * v.cos() * u.sin(),
                    radius * v.sin(),
                )
            }
            SurfaceKind::Torus { major, minor } => {
                let reach = major + minor * uv.y.cos();
                DVec3::new(reach * uv.x.cos(), reach * uv.x.sin(), minor * uv.y.sin())
            }
            SurfaceKind::Extrusion {
                profile,
                closed,
                along,
                ..
            } => {
                let at = point_on_outline(profile, *closed, uv.x);
                DVec3::new(at.x, at.y, 0.0) + *along * uv.y
            }
            SurfaceKind::Revolution {
                section,
                closed,
                origin,
                axis,
                reference,
                ..
            } => {
                let at = point_on_outline(section, *closed, uv.y);
                let side = axis.cross(*reference);
                *origin
                    + *reference * (at.x * uv.x.cos())
                    + side * (at.x * uv.x.sin())
                    + *axis * at.y
            }
            SurfaceKind::BSpline(surface) => surface.point(uv),
        }
    }

    fn local_invert(&self, local: DVec3, tol: f64) -> Option<DVec2> {
        match &self.kind {
            SurfaceKind::Plane => Some(DVec2::new(local.x, local.y)),
            SurfaceKind::Cylinder { .. } => {
                let reach = local.truncate().length();
                if reach <= tol {
                    return None;
                }
                Some(DVec2::new(local.y.atan2(local.x), local.z))
            }
            SurfaceKind::Sphere { radius } => {
                let length = local.length();
                if length <= tol {
                    return None;
                }
                Some(DVec2::new(
                    local.y.atan2(local.x),
                    (local.z / radius).clamp(-1.0, 1.0).asin(),
                ))
            }
            SurfaceKind::Torus { major, .. } => {
                let reach = local.truncate().length();
                if reach <= tol {
                    return None;
                }
                Some(DVec2::new(
                    local.y.atan2(local.x),
                    local.z.atan2(reach - major),
                ))
            }
            SurfaceKind::Extrusion {
                profile,
                closed,
                along,
                ..
            } => {
                // The profile is in the z = 0 plane, so the sweep parameter is
                // whatever it took to get to this height.
                let v = local.z / along.z;
                let flat = local - *along * v;
                let u = outline_parameter(profile, *closed, DVec2::new(flat.x, flat.y))?;
                Some(DVec2::new(u, v))
            }
            SurfaceKind::Revolution {
                section,
                closed,
                origin,
                axis,
                reference,
                ..
            } => {
                let offset = local - *origin;
                let height = offset.dot(*axis);
                let radial = offset - *axis * height;
                let side = axis.cross(*reference);
                let u = radial.dot(side).atan2(radial.dot(*reference));
                let v = outline_parameter(section, *closed, DVec2::new(radial.length(), height))?;
                Some(DVec2::new(u, v))
            }
            SurfaceKind::BSpline(surface) => surface.invert(local, tol),
        }
    }
}

/// The outline parameter (one unit per segment) for a swept curve's own
/// parameter, read through the knots recorded at each outline point.
pub fn outline_parameter_at_knot(knots: &[f64], value: f64) -> Option<f64> {
    if knots.len() < 2 || !value.is_finite() {
        return None;
    }
    for (index, pair) in knots.windows(2).enumerate() {
        let (low, high) = (pair[0].min(pair[1]), pair[0].max(pair[1]));
        if value >= low && value <= high {
            let span = pair[1] - pair[0];
            let fraction = if span.abs() > 0.0 {
                (value - pair[0]) / span
            } else {
                0.0
            };
            return Some(index as f64 + fraction);
        }
    }
    None
}

/// How long an outline's parameter runs: one unit per segment.
fn section_period(kind: &SurfaceKind) -> f64 {
    match kind {
        SurfaceKind::Revolution { section, .. } => section.len() as f64,
        SurfaceKind::Extrusion { profile, .. } => profile.len() as f64,
        _ => 0.0,
    }
}

/// A point at one unit per segment along an outline, clamped to its ends.
fn point_on_outline(points: &[DVec2], closed: bool, at: f64) -> DVec2 {
    if points.is_empty() {
        return DVec2::ZERO;
    }
    let segments = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    if segments == 0 {
        return points[0];
    }
    let at = if closed {
        at.rem_euclid(segments as f64)
    } else {
        at.clamp(0.0, segments as f64)
    };
    let index = (at.floor() as usize).min(segments - 1);
    let fraction = at - index as f64;
    let a = points[index];
    let b = points[(index + 1) % points.len()];
    a + (b - a) * fraction
}

/// The parameter of the point on an outline nearest a given point.
///
/// A linear scan over the segments: bounded by the outline, and exact for the
/// polylines every profile evaluator produces.
fn outline_parameter(points: &[DVec2], closed: bool, target: DVec2) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let segments = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    let mut best: Option<(f64, f64)> = None;
    for index in 0..segments {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        let along = b - a;
        let length = along.length_squared();
        let fraction = if length <= f64::EPSILON {
            0.0
        } else {
            ((target - a).dot(along) / length).clamp(0.0, 1.0)
        };
        let distance = (a + along * fraction - target).length_squared();
        if best.is_none_or(|(known, _)| distance < known) {
            best = Some((distance, index as f64 + fraction));
        }
    }
    best.map(|(_, at)| at)
}

impl BSplineSurface {
    /// The point at one parameter pair, in the surface's own frame.
    pub fn point(&self, uv: DVec2) -> DVec3 {
        // Only the non-zero basis support is needed in either parameter direction.
        let at = de_boor(self.net.len(), self.u_degree, &self.u_knots, uv.x, |u| {
            de_boor(self.net[u].len(), self.v_degree, &self.v_knots, uv.y, |v| {
                self.net[u][v].extend(1.0) * self.weights[u][v]
            })
        });
        if at.w <= 0.0 || !at.is_finite() {
            return DVec3::splat(f64::NAN);
        }
        at.truncate() / at.w
    }

    /// The domain each parameter runs over.
    pub fn domain(&self) -> (DVec2, DVec2) {
        let u = (
            self.u_knots[self.u_degree],
            self.u_knots[self.u_knots.len() - 1 - self.u_degree],
        );
        let v = (
            self.v_knots[self.v_degree],
            self.v_knots[self.v_knots.len() - 1 - self.v_degree],
        );
        (DVec2::new(u.0, v.0), DVec2::new(u.1, v.1))
    }

    /// The parameters of a point, from a sampled seed and a bounded descent.
    fn invert(&self, target: DVec3, tol: f64) -> Option<DVec2> {
        const SEEDS: usize = 24;
        const STEPS: usize = 40;
        let (low, high) = self.domain();
        let span = high - low;
        if span.x <= 0.0 || span.y <= 0.0 {
            return None;
        }
        let mut best = (f64::INFINITY, DVec2::ZERO);
        for i in 0..=SEEDS {
            for j in 0..=SEEDS {
                let uv = low
                    + DVec2::new(
                        span.x * i as f64 / SEEDS as f64,
                        span.y * j as f64 / SEEDS as f64,
                    );
                let distance = (self.point(uv) - target).length_squared();
                if distance < best.0 {
                    best = (distance, uv);
                }
            }
        }
        // Coordinate descent on a shrinking step: no derivative, no division,
        // and it cannot run away on a degenerate patch.
        let mut step = span / SEEDS as f64;
        let mut at = best.1;
        for _ in 0..STEPS {
            let mut moved = false;
            for delta in [
                DVec2::new(step.x, 0.0),
                DVec2::new(-step.x, 0.0),
                DVec2::new(0.0, step.y),
                DVec2::new(0.0, -step.y),
            ] {
                let candidate = (at + delta).clamp(low, high);
                let distance = (self.point(candidate) - target).length_squared();
                if distance < best.0 {
                    best = (distance, candidate);
                    at = candidate;
                    moved = true;
                }
            }
            if !moved {
                step *= 0.5;
                if step.x <= tol * 1e-3 && step.y <= tol * 1e-3 {
                    break;
                }
            }
        }
        Some(at)
    }
}

/// One de Boor evaluation over homogeneous control points.
fn de_boor(
    count: usize,
    degree: usize,
    knots: &[f64],
    at: f64,
    mut control: impl FnMut(usize) -> DVec4,
) -> DVec4 {
    if count == 0 || degree > 16 {
        return DVec4::ZERO;
    }
    let last = count - 1;
    let low = knots[degree];
    let high = knots[knots.len() - 1 - degree];
    let at = at.clamp(low, high);
    // The span holding `at`, found by bisection over the knot vector.
    let mut span = degree;
    let mut lower = degree;
    let mut upper = last + 1;
    while lower < upper {
        let middle = (lower + upper) / 2;
        if at < knots[middle] {
            upper = middle;
        } else {
            lower = middle + 1;
        }
    }
    if lower > degree {
        span = (lower - 1).min(last);
    }
    let mut work = [DVec4::ZERO; 17];
    for (index, value) in work.iter_mut().enumerate().take(degree + 1) {
        *value = control((span + index).saturating_sub(degree).min(last));
    }
    for round in 1..=degree {
        for index in (round..=degree).rev() {
            let knot = span + index - degree;
            let left = knots.get(knot).copied().unwrap_or(low);
            let right = knots
                .get(knot + degree + 1 - round)
                .copied()
                .unwrap_or(high);
            let width = right - left;
            let alpha = if width <= 0.0 {
                0.0
            } else {
                ((at - left) / width).clamp(0.0, 1.0)
            };
            work[index] = work[index - 1] * (1.0 - alpha) + work[index] * alpha;
        }
    }
    work[degree]
}

/// Produces a parametric surface from a surface item.
pub trait SurfaceEvaluator: Send + Sync {
    /// Exact IFC class names this handles.
    fn classes(&self) -> &'static [&'static str];
    /// Turn one item into a surface.
    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError>;
}

/// Evaluators for one schema, indexed by class id.
///
/// Bound to a schema because class ids differ between schemas.
pub struct Registry {
    schema: &'static Schema,
    solids: Vec<Box<dyn SolidEvaluator>>,
    profiles: Vec<Box<dyn ProfileEvaluator>>,
    curves: Vec<Box<dyn CurveEvaluator>>,
    surfaces: Vec<Box<dyn SurfaceEvaluator>>,
    solid_by_class: FxHashMap<ClassId, usize>,
    profile_by_class: FxHashMap<ClassId, usize>,
    curve_by_class: FxHashMap<ClassId, usize>,
    surface_by_class: FxHashMap<ClassId, usize>,
}

impl Registry {
    /// An empty registry for a schema.
    pub fn empty(schema: SchemaId) -> Self {
        Registry {
            schema: Schema::get(schema),
            solids: Vec::new(),
            profiles: Vec::new(),
            curves: Vec::new(),
            surfaces: Vec::new(),
            solid_by_class: FxHashMap::default(),
            profile_by_class: FxHashMap::default(),
            curve_by_class: FxHashMap::default(),
            surface_by_class: FxHashMap::default(),
        }
    }

    /// Everything TessIFC implements, for a schema.
    pub fn defaults(schema: SchemaId) -> Self {
        let mut registry = Registry::empty(schema);
        crate::eval::register_defaults(&mut registry);
        registry
    }

    /// The default registry for a schema, built once for the process.
    ///
    /// A caller with its own evaluators uses [`EvalCtx::with_registry`] instead.
    pub fn shared(schema: SchemaId) -> &'static Registry {
        static CACHE: [OnceLock<Registry>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
        let slot = match schema {
            SchemaId::Ifc2x3 => 0,
            SchemaId::Ifc4 => 1,
            SchemaId::Ifc4x3 => 2,
        };
        CACHE[slot].get_or_init(|| Registry::defaults(schema))
    }

    /// The schema these evaluators are keyed against.
    pub fn schema(&self) -> &'static Schema {
        self.schema
    }

    /// Register a solid evaluator. A later registration wins.
    pub fn register_solid(&mut self, evaluator: Box<dyn SolidEvaluator>) {
        let index = self.solids.len();
        for name in evaluator.classes() {
            if let Some(class) = self.schema.class_by_name(name) {
                self.solid_by_class.insert(class, index);
            }
        }
        self.solids.push(evaluator);
    }

    /// Register a profile evaluator.
    pub fn register_profile(&mut self, evaluator: Box<dyn ProfileEvaluator>) {
        let index = self.profiles.len();
        for name in evaluator.classes() {
            if let Some(class) = self.schema.class_by_name(name) {
                self.profile_by_class.insert(class, index);
            }
        }
        self.profiles.push(evaluator);
    }

    /// Register a curve evaluator.
    pub fn register_curve(&mut self, evaluator: Box<dyn CurveEvaluator>) {
        let index = self.curves.len();
        for name in evaluator.classes() {
            if let Some(class) = self.schema.class_by_name(name) {
                self.curve_by_class.insert(class, index);
            }
        }
        self.curves.push(evaluator);
    }

    /// Register a surface evaluator.
    pub fn register_surface(&mut self, evaluator: Box<dyn SurfaceEvaluator>) {
        let index = self.surfaces.len();
        for name in evaluator.classes() {
            if let Some(class) = self.schema.class_by_name(name) {
                self.surface_by_class.insert(class, index);
            }
        }
        self.surfaces.push(evaluator);
    }

    /// Find the handler for a class, walking up the inheritance chain.
    fn lookup(&self, table: &FxHashMap<ClassId, usize>, class: ClassId) -> Option<usize> {
        if let Some(&index) = table.get(&class) {
            return Some(index);
        }
        let mut current = self.schema.class(class).parent;
        // Bounded by the class count so a malformed table cannot loop.
        for _ in 0..self.schema.class_count() {
            let parent = current?;
            if let Some(&index) = table.get(&parent) {
                return Some(index);
            }
            current = self.schema.class(parent).parent;
        }
        None
    }

    /// Evaluate a solid item.
    pub fn solid(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        ctx.settings.validate()?;
        let index = self
            .lookup(&self.solid_by_class, item.class())
            .ok_or_else(|| GeomError::Unsupported(item.class_name()))?;
        ctx.nested(|| self.solids[index].evaluate(ctx, item))
    }

    /// Evaluate a profile.
    pub fn profile(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        ctx.settings.validate()?;
        let index = self
            .lookup(&self.profile_by_class, item.class())
            .ok_or_else(|| GeomError::Unsupported(item.class_name()))?;
        ctx.nested(|| self.profiles[index].evaluate(ctx, item))
    }

    /// Evaluate a curve.
    pub fn curve(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        ctx.settings.validate()?;
        let index = self
            .lookup(&self.curve_by_class, item.class())
            .ok_or_else(|| GeomError::Unsupported(item.class_name()))?;
        ctx.nested(|| self.curves[index].evaluate(ctx, item))
    }

    /// Evaluate a surface.
    pub fn surface(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError> {
        ctx.settings.validate()?;
        let index = self
            .lookup(&self.surface_by_class, item.class())
            .ok_or_else(|| GeomError::Unsupported(item.class_name()))?;
        ctx.nested(|| self.surfaces[index].evaluate(ctx, item))
    }

    /// Is there a solid evaluator for this class?
    pub fn handles_solid(&self, class: ClassId) -> bool {
        self.lookup(&self.solid_by_class, class).is_some()
    }

    /// Every schema entity and its direct or inherited dispatch routes.
    ///
    /// A route describes dispatch, not verified geometric conformance.
    pub fn inventory(&self) -> Vec<CoverageEntity> {
        self.schema
            .class_ids()
            .map(|class| {
                let definition = self.schema.class(class);
                let mut routes = Vec::new();
                for (table, kind) in [
                    (&self.solid_by_class, "solid"),
                    (&self.profile_by_class, "profile"),
                    (&self.curve_by_class, "curve"),
                    (&self.surface_by_class, "surface"),
                ] {
                    let mut current = Some(class);
                    for _ in 0..self.schema.class_count() {
                        let Some(candidate) = current else {
                            break;
                        };
                        if table.contains_key(&candidate) {
                            routes.push(CoverageRoute {
                                kind,
                                registered_class: self.schema.class(candidate).name,
                                inherited: candidate != class,
                            });
                            break;
                        }
                        current = self.schema.class(candidate).parent;
                    }
                }
                CoverageEntity {
                    class: definition.name,
                    is_abstract: definition.is_abstract,
                    routes,
                }
            })
            .collect()
    }

    /// Every class name with a registered evaluator, sorted.
    ///
    /// Generates the published coverage table, so it cannot drift from the code.
    pub fn coverage(&self) -> Vec<(&'static str, &'static str)> {
        let mut rows = Vec::new();
        for (table, kind) in [
            (&self.solid_by_class, "solid"),
            (&self.profile_by_class, "profile"),
            (&self.curve_by_class, "curve"),
            (&self.surface_by_class, "surface"),
        ] {
            for &class in table.keys() {
                rows.push((self.schema.class(class).name, kind));
            }
        }
        rows.sort_unstable();
        rows.dedup();
        rows
    }
}

/// One schema entity in the capability inventory.
#[derive(Debug, serde::Serialize)]
pub struct CoverageEntity {
    /// EXPRESS entity name.
    pub class: &'static str,
    /// Whether instances of the exact class are forbidden by EXPRESS.
    pub is_abstract: bool,
    /// Registered evaluator routes; empty does not imply a data class needs tessellation.
    pub routes: Vec<CoverageRoute>,
}

/// A dispatch route, independent of fixture evidence or conformance claims.
#[derive(Debug, serde::Serialize)]
pub struct CoverageRoute {
    /// Solid, profile, curve or surface.
    pub kind: &'static str,
    /// The exact class whose evaluator is invoked.
    pub registered_class: &'static str,
    /// True when dispatch climbs to an ancestor.
    pub inherited: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DiagnosticSink, Settings, Tolerances};
    use crate::units::Units;
    use tessifc_model::Model;
    use tessifc_step::{ParseOptions, parse};

    struct AlwaysCube(&'static [&'static str]);

    impl SolidEvaluator for AlwaysCube {
        fn classes(&self) -> &'static [&'static str] {
            self.0
        }
        fn evaluate(&self, _ctx: &EvalCtx<'_>, _item: Entity<'_>) -> Result<Mesh64, GeomError> {
            let mut mesh = Mesh64::new();
            mesh.push_vertex(DVec3::ZERO);
            mesh.push_vertex(DVec3::X);
            mesh.push_vertex(DVec3::Y);
            mesh.push_triangle(0, 1, 2);
            Ok(mesh)
        }
    }

    fn model_of(data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    #[test]
    fn an_exact_class_dispatches() {
        let model = model_of("#1=IFCEXTRUDEDAREASOLID($,$,$,1.);\n");
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );

        let mut registry = Registry::empty(SchemaId::Ifc4);
        registry.register_solid(Box::new(AlwaysCube(&["IfcExtrudedAreaSolid"])));
        assert!(registry.solid(&ctx, model.entity(1).unwrap()).is_ok());
    }

    #[test]
    fn a_subclass_falls_back_to_its_parent() {
        // Registered for the supertype, asked for the subtype.
        let model = model_of("#1=IFCEXTRUDEDAREASOLIDTAPERED($,$,$,1.,$);\n");
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );

        let mut registry = Registry::empty(SchemaId::Ifc4);
        registry.register_solid(Box::new(AlwaysCube(&["IfcExtrudedAreaSolid"])));
        assert!(
            registry.solid(&ctx, model.entity(1).unwrap()).is_ok(),
            "a tapered extrusion should fall back to the extrusion evaluator"
        );
    }

    #[test]
    fn an_unregistered_class_says_so_by_name() {
        let model = model_of("#1=IFCSPHERE($,1.);\n");
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );

        let registry = Registry::empty(SchemaId::Ifc4);
        match registry.solid(&ctx, model.entity(1).unwrap()) {
            Err(GeomError::Unsupported(name)) => assert_eq!(name, "IfcSphere"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn a_later_registration_overrides_an_earlier_one() {
        let model = model_of("#1=IFCEXTRUDEDAREASOLID($,$,$,1.);\n");
        let mut registry = Registry::empty(SchemaId::Ifc4);
        registry.register_solid(Box::new(AlwaysCube(&["IfcExtrudedAreaSolid"])));
        registry.register_solid(Box::new(AlwaysCube(&["IfcExtrudedAreaSolid"])));
        let class = registry
            .schema()
            .class_by_name("IfcExtrudedAreaSolid")
            .unwrap();
        assert_eq!(registry.solid_by_class[&class], 1, "the second one wins");
        let _ = model;
    }

    #[test]
    fn coverage_lists_what_is_registered() {
        let mut registry = Registry::empty(SchemaId::Ifc4);
        registry.register_solid(Box::new(AlwaysCube(&[
            "IfcExtrudedAreaSolid",
            "IfcFacetedBrep",
        ])));
        let coverage = registry.coverage();
        assert!(coverage.contains(&("IfcExtrudedAreaSolid", "solid")));
        assert!(coverage.contains(&("IfcFacetedBrep", "solid")));
        assert_eq!(coverage.len(), 2);
    }
}
