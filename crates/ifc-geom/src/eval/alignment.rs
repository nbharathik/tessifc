// SPDX-License-Identifier: Apache-2.0
//! IFC4X3 alignment curves: segments stationed on parent curves, gradient and
//! cant profiles, and the frame at a distance along them.

use crate::context::EvalCtx;
use crate::error::{GeomError, codes};
use crate::placement::{cartesian_point, direction, orthonormal_frame};
use crate::registry::{CurveEvaluator, Polyline3, Registry};
use crate::units::Units;
use glam::{DMat4, DVec2, DVec3, DVec4};
use std::sync::Arc;
use tessifc_model::{Entity, Value};

/// Upper bound on the segments one alignment curve may list.
pub const MAX_ALIGNMENT_SEGMENTS: usize = 65_536;
/// Upper bound on the stations one curve may sample to.
const MAX_STATIONS: usize = 1_000_000;
/// Simpson intervals for a spiral position, per call; the floor keeps a short
/// gentle spiral accurate to well under a tenth of a millimetre.
const MIN_SPIRAL_STEPS: usize = 64;
const MAX_SPIRAL_STEPS: usize = 4096;
/// A spiral is integrated with at most this much turn per interval.
const SPIRAL_STEP_TURN: f64 = 0.05;
/// Intervals of a polynomial curve's arc-length table.
const POLYNOMIAL_TABLE: usize = 512;
/// Iterations for inverting a monotone function by bisection.
const BISECTIONS: u32 = 60;
/// A computed segment end further than this many tolerances from the next placement is a gap.
const GAP_TOLERANCES: f64 = 10.0;
/// A cached position further than this many tolerances from the computed one is a mismatch.
const MISMATCH_TOLERANCES: f64 = 10.0;
/// A cached axis further than this from the computed one is a mismatch, in radians.
const MISMATCH_ANGLE: f64 = 1e-3;

const ALIGNMENT_CLASSES: [&str; 3] = [
    "IfcGradientCurve",
    "IfcSegmentedReferenceCurve",
    "IfcOffsetCurveByDistances",
];

/// The frame of a curve at some distance along it.
#[derive(Clone, Copy, Debug)]
pub struct CurveFrame {
    /// The point on the curve, in metres.
    pub point: DVec3,
    /// The unit tangent in the direction of increasing distance.
    pub tangent: DVec3,
    /// The unit lateral, to the left when facing the tangent.
    pub lateral: DVec3,
    /// The unit up: normal to the tangent in its vertical plane, tilted by the cant.
    pub up: DVec3,
    /// The cant angle applied, in radians.
    pub cant: f64,
}

impl CurveFrame {
    /// The frame as a transform: x along the tangent, y lateral, z up.
    pub fn matrix(&self) -> DMat4 {
        DMat4::from_cols(
            self.tangent.extend(0.0),
            self.lateral.extend(0.0),
            self.up.extend(0.0),
            self.point.extend(1.0),
        )
    }
}

/// A position along a curve: arc length in metres, or the curve's own parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveMeasure {
    /// Metres along the curve.
    Length(f64),
    /// The curve's parameter: an angle for a circle, a segment index for a polyline.
    Parameter(f64),
}

/// Read an `IfcCurveMeasureSelect`; a plain number is taken as a length.
pub fn curve_measure(value: Value<'_>, units: &Units) -> Option<CurveMeasure> {
    match value {
        Value::Typed(typed) if typed.is("IFCPARAMETERVALUE") => {
            typed.value().as_f64().map(CurveMeasure::Parameter)
        }
        Value::Typed(typed) => typed
            .value()
            .as_f64()
            .map(|raw| CurveMeasure::Length(units.length(raw))),
        other => other
            .as_f64()
            .map(|raw| CurveMeasure::Length(units.length(raw))),
    }
    .filter(|measure| match measure {
        CurveMeasure::Length(value) | CurveMeasure::Parameter(value) => value.is_finite(),
    })
}

// ------------------------------------------------------------- parents

/// A parent curve in its own frame, in metres.
#[derive(Clone, Debug)]
enum Parent {
    /// `IfcLine`: parameter `t`, point `pnt + t * magnitude * dir`.
    Line {
        pnt: DVec2,
        dir: DVec2,
        magnitude: f64,
    },
    /// `IfcCircle`: parameter is the angle from local x, counter-clockwise.
    Circle {
        centre: DVec2,
        x: DVec2,
        y: DVec2,
        radius: f64,
    },
    /// A spiral: curvature is a polynomial in the arc length from the origin.
    Spiral {
        origin: DVec2,
        x: DVec2,
        y: DVec2,
        /// `(power, factor)`: curvature is the sum of `factor * s^power`.
        terms: Vec<(u32, f64)>,
    },
    /// `IfcPolynomialCurve`: `x(t)`, `y(t)`; arc length by a table over the segment's range.
    Polynomial {
        origin: DVec2,
        x: DVec2,
        y: DVec2,
        cx: Vec<f64>,
        cy: Vec<f64>,
        /// `(t, arc length from t = 0)`, increasing in both, covering the segment.
        table: Vec<(f64, f64)>,
    },
}

/// The 2D frame of a `Position`: origin and the unit x and y axes.
fn planar_position(value: Value<'_>, units: &Units) -> (DVec2, DVec2, DVec2) {
    let Some(entity) = value.as_entity() else {
        return (DVec2::ZERO, DVec2::X, DVec2::Y);
    };
    let origin = entity
        .attr("Location")
        .as_entity()
        .and_then(|point| cartesian_point(point, units))
        .map(|point| point.truncate())
        .unwrap_or(DVec2::ZERO);
    let x = entity
        .attr("RefDirection")
        .as_entity()
        .and_then(direction)
        .map(|axis| axis.truncate())
        .filter(|axis| axis.length_squared() > 0.0)
        .map(|axis| axis.normalize())
        .unwrap_or(DVec2::X);
    (origin, x, DVec2::new(-x.y, x.x))
}

/// One curvature term of a polynomial spiral: `sign(A) s^n / |A|^(n + 1)`.
fn spiral_term(power: u32, constant: Option<f64>) -> Option<(u32, f64)> {
    let constant = constant.filter(|value| value.is_finite() && *value != 0.0)?;
    let factor = constant.signum() / constant.abs().powi(power as i32 + 1);
    factor.is_finite().then_some((power, factor))
}

impl Parent {
    fn read(ctx: &EvalCtx<'_>, curve: Entity<'_>) -> Result<Parent, GeomError> {
        let units = &ctx.units;
        let length = |value: Value<'_>| value.as_f64().map(|raw| units.length(raw));
        if curve.is_a("IfcLine") {
            let pnt = curve
                .attr("Pnt")
                .as_entity()
                .and_then(|point| cartesian_point(point, units))
                .ok_or_else(|| GeomError::missing("Pnt"))?;
            let vector = curve
                .attr("Dir")
                .as_entity()
                .ok_or_else(|| GeomError::missing("Dir"))?;
            let dir = vector
                .attr("Orientation")
                .as_entity()
                .and_then(direction)
                .ok_or_else(|| GeomError::missing("Dir.Orientation"))?;
            let magnitude = length(vector.attr("Magnitude")).unwrap_or(1.0);
            if !(magnitude.is_finite() && magnitude > 0.0) {
                return Err(GeomError::Degenerate("a line with no magnitude".into()));
            }
            let dir = dir.truncate();
            if dir.length_squared() == 0.0 {
                return Err(GeomError::Degenerate(
                    "a line direction with no x or y".into(),
                ));
            }
            return Ok(Parent::Line {
                pnt: pnt.truncate(),
                dir: dir.normalize(),
                magnitude,
            });
        }
        if curve.is_a("IfcCircle") {
            let radius =
                length(curve.attr("Radius")).ok_or_else(|| GeomError::missing("Radius"))?;
            if !(radius.is_finite() && radius > ctx.tol.len) {
                return Err(GeomError::Degenerate("a circle with no radius".into()));
            }
            let (centre, x, y) = planar_position(curve.attr("Position"), units);
            return Ok(Parent::Circle {
                centre,
                x,
                y,
                radius,
            });
        }
        let spiral_terms: Option<Vec<(u32, &str)>> = if curve.is_a("IfcClothoid") {
            Some(vec![(1, "ClothoidConstant")])
        } else if curve.is_a("IfcSecondOrderPolynomialSpiral") {
            Some(vec![
                (2, "QuadraticTerm"),
                (1, "LinearTerm"),
                (0, "ConstantTerm"),
            ])
        } else if curve.is_a("IfcThirdOrderPolynomialSpiral") {
            Some(vec![
                (3, "CubicTerm"),
                (2, "QuadraticTerm"),
                (1, "LinearTerm"),
                (0, "ConstantTerm"),
            ])
        } else if curve.is_a("IfcSeventhOrderPolynomialSpiral") {
            Some(vec![
                (7, "SepticTerm"),
                (6, "SexticTerm"),
                (5, "QuinticTerm"),
                (4, "QuarticTerm"),
                (3, "CubicTerm"),
                (2, "QuadraticTerm"),
                (1, "LinearTerm"),
                (0, "ConstantTerm"),
            ])
        } else {
            None
        };
        if let Some(names) = spiral_terms {
            let (origin, x, y) = planar_position(curve.attr("Position"), units);
            let terms: Vec<(u32, f64)> = names
                .iter()
                .filter_map(|(power, name)| spiral_term(*power, length(curve.attr(name))))
                .collect();
            if terms.is_empty() {
                return Err(GeomError::Degenerate(format!(
                    "{} with no usable curvature term",
                    curve.class_name()
                )));
            }
            return Ok(Parent::Spiral {
                origin,
                x,
                y,
                terms,
            });
        }
        if curve.is_a("IfcPolynomialCurve") {
            let coefficients = |name: &str| -> Vec<f64> {
                curve
                    .attr(name)
                    .as_list()
                    .map(|list| list.floats().map(|raw| units.length(raw)).collect())
                    .unwrap_or_default()
            };
            let cx = coefficients("CoefficientsX");
            let cy = coefficients("CoefficientsY");
            if cx.len() > 64 || cy.len() > 64 {
                return Err(GeomError::LimitReached("polynomial coefficients".into()));
            }
            if cx.iter().chain(&cy).any(|value| !value.is_finite()) {
                return Err(GeomError::Degenerate(
                    "a non-finite polynomial coefficient".into(),
                ));
            }
            if cx.len() < 2 && cy.len() < 2 {
                return Err(GeomError::Degenerate(
                    "a polynomial curve with no term".into(),
                ));
            }
            let (origin, x, y) = planar_position(curve.attr("Position"), units);
            return Ok(Parent::Polynomial {
                origin,
                x,
                y,
                cx,
                cy,
                table: Vec::new(),
            });
        }
        if curve.is_a("IfcSineSpiral") || curve.is_a("IfcCosineSpiral") {
            return Err(GeomError::Unsupported(format!(
                "{} (its curvature formula is not settled)",
                curve.class_name()
            )));
        }
        Err(GeomError::Unsupported(format!(
            "{} as an alignment segment's parent curve",
            curve.class_name()
        )))
    }

    /// Curvature of a spiral at arc length `s`.
    fn curvature(terms: &[(u32, f64)], s: f64) -> f64 {
        terms
            .iter()
            .map(|(power, factor)| factor * s.powi(*power as i32))
            .sum()
    }

    /// The turned angle of a spiral from the origin to arc length `s`.
    fn spiral_angle(terms: &[(u32, f64)], s: f64) -> f64 {
        terms
            .iter()
            .map(|(power, factor)| factor * s.powi(*power as i32 + 1) / f64::from(power + 1))
            .sum()
    }

    /// Position of a spiral at arc length `s`, by composite Simpson from the origin.
    fn spiral_position(terms: &[(u32, f64)], s: f64) -> DVec2 {
        if s == 0.0 {
            return DVec2::ZERO;
        }
        let turn = Parent::spiral_angle(terms, s).abs();
        let intervals = ((turn / SPIRAL_STEP_TURN).ceil() as usize)
            .clamp(MIN_SPIRAL_STEPS, MAX_SPIRAL_STEPS)
            .div_ceil(2)
            * 2;
        let h = s / intervals as f64;
        let mut sum = DVec2::ZERO;
        for step in 0..=intervals {
            let angle = Parent::spiral_angle(terms, h * step as f64);
            let weight = if step == 0 || step == intervals {
                1.0
            } else if step % 2 == 1 {
                4.0
            } else {
                2.0
            };
            sum += weight * DVec2::new(angle.cos(), angle.sin());
        }
        sum * (h / 3.0)
    }

    /// The polynomial's value and derivative at `t`.
    fn polynomial_at(cx: &[f64], cy: &[f64], t: f64) -> (DVec2, DVec2) {
        let eval = |coefficients: &[f64]| {
            let mut value = 0.0;
            let mut slope = 0.0;
            for (index, coefficient) in coefficients.iter().enumerate().rev() {
                value = value * t + coefficient;
                if index > 0 {
                    slope = slope * t + coefficient * index as f64;
                }
            }
            (value, slope)
        };
        let (x, dx) = eval(cx);
        let (y, dy) = eval(cy);
        (DVec2::new(x, y), DVec2::new(dx, dy))
    }

    /// Fill a polynomial's arc-length table over the parameter range `[a, b]`.
    fn tabulate(&mut self, a: f64, b: f64) {
        let Parent::Polynomial { cx, cy, table, .. } = self else {
            return;
        };
        let (low, high) = (a.min(b).min(0.0), a.max(b).max(0.0));
        let step = (high - low) / POLYNOMIAL_TABLE as f64;
        let speed = |t: f64| Parent::polynomial_at(cx, cy, t).1.length();
        // The table runs from `low`; the value at t = 0 is subtracted afterwards.
        let mut rows = Vec::with_capacity(POLYNOMIAL_TABLE + 1);
        let mut running = 0.0;
        rows.push((low, 0.0));
        for index in 0..POLYNOMIAL_TABLE {
            let t0 = low + step * index as f64;
            let t1 = t0 + step;
            running += step / 6.0 * (speed(t0) + 4.0 * speed(0.5 * (t0 + t1)) + speed(t1));
            rows.push((t1, running));
        }
        let at_zero = Parent::interpolate(&rows, 0.0, |row| row.0, |row| row.1);
        for row in &mut rows {
            row.1 -= at_zero;
        }
        *table = rows;
    }

    /// Linear interpolation in a table sorted by `key`.
    fn interpolate(
        table: &[(f64, f64)],
        at: f64,
        key: impl Fn(&(f64, f64)) -> f64,
        value: impl Fn(&(f64, f64)) -> f64,
    ) -> f64 {
        if table.len() < 2 {
            return 0.0;
        }
        let index = table
            .partition_point(|row| key(row) < at)
            .clamp(1, table.len() - 1);
        let (before, after) = (&table[index - 1], &table[index]);
        let span = key(after) - key(before);
        let fraction = if span.abs() > 0.0 {
            ((at - key(before)) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        value(before) + fraction * (value(after) - value(before))
    }

    /// Arc length from the parent's origin at native parameter `p`.
    fn length_of_param(&self, p: f64) -> f64 {
        match self {
            Parent::Line { magnitude, .. } => p * magnitude,
            Parent::Circle { radius, .. } => p * radius,
            Parent::Spiral { .. } => p,
            Parent::Polynomial { table, .. } => {
                Parent::interpolate(table, p, |row| row.0, |row| row.1)
            }
        }
    }

    /// Native parameter at arc length `s` from the parent's origin.
    fn param_of_length(&self, s: f64) -> f64 {
        match self {
            Parent::Line { magnitude, .. } => s / magnitude,
            Parent::Circle { radius, .. } => s / radius,
            Parent::Spiral { .. } => s,
            Parent::Polynomial { table, .. } => {
                Parent::interpolate(table, s, |row| row.1, |row| row.0)
            }
        }
    }

    /// Point and unit tangent at native parameter `p`.
    fn at(&self, p: f64) -> (DVec2, DVec2) {
        match self {
            Parent::Line {
                pnt,
                dir,
                magnitude,
            } => (*pnt + *dir * (p * magnitude), *dir),
            Parent::Circle {
                centre,
                x,
                y,
                radius,
            } => {
                let (sin, cos) = p.sin_cos();
                (
                    *centre + (*x * cos + *y * sin) * *radius,
                    -*x * sin + *y * cos,
                )
            }
            Parent::Spiral {
                origin,
                x,
                y,
                terms,
            } => {
                let local = Parent::spiral_position(terms, p);
                let angle = Parent::spiral_angle(terms, p);
                (
                    *origin + *x * local.x + *y * local.y,
                    *x * angle.cos() + *y * angle.sin(),
                )
            }
            Parent::Polynomial {
                origin,
                x,
                y,
                cx,
                cy,
                ..
            } => {
                let (local, slope) = Parent::polynomial_at(cx, cy, p);
                let tangent = if slope.length_squared() > 0.0 {
                    slope.normalize()
                } else {
                    DVec2::X
                };
                (
                    *origin + *x * local.x + *y * local.y,
                    *x * tangent.x + *y * tangent.y,
                )
            }
        }
    }

    /// The smallest radius of curvature over an arc-length range, or `None` for a straight parent.
    fn min_radius(&self, s0: f64, s1: f64) -> Option<f64> {
        match self {
            Parent::Line { .. } => None,
            Parent::Circle { radius, .. } => Some(*radius),
            Parent::Spiral { terms, .. } => {
                let curvature = Parent::curvature(terms, s0)
                    .abs()
                    .max(Parent::curvature(terms, s1).abs());
                (curvature > 0.0).then(|| 1.0 / curvature)
            }
            Parent::Polynomial { cx, cy, .. } => {
                // Sampled: a polynomial's curvature has no simple extremum.
                let mut sharpest = 0.0f64;
                for step in 0..=16 {
                    let s = s0 + (s1 - s0) * f64::from(step) / 16.0;
                    let t = self.param_of_length(s);
                    let (_, first) = Parent::polynomial_at(cx, cy, t);
                    let second = {
                        let h = 1e-3 * (1.0 + t.abs());
                        let (_, ahead) = Parent::polynomial_at(cx, cy, t + h);
                        let (_, behind) = Parent::polynomial_at(cx, cy, t - h);
                        (ahead - behind) / (2.0 * h)
                    };
                    let speed = first.length();
                    if speed > 0.0 {
                        sharpest = sharpest.max((first.perp_dot(second)).abs() / speed.powi(3));
                    }
                }
                (sharpest > 0.0).then(|| 1.0 / sharpest)
            }
        }
    }

    /// The angle turned over an arc-length range.
    fn turned(&self, s0: f64, s1: f64) -> f64 {
        match self {
            Parent::Line { .. } => 0.0,
            Parent::Circle { radius, .. } => ((s1 - s0) / radius).abs(),
            Parent::Spiral { terms, .. } => {
                (Parent::spiral_angle(terms, s1) - Parent::spiral_angle(terms, s0)).abs()
            }
            Parent::Polynomial { .. } => {
                let (_, t0) = self.at(self.param_of_length(s0));
                let (_, t1) = self.at(self.param_of_length(s1));
                t0.dot(t1).clamp(-1.0, 1.0).acos()
            }
        }
    }

    /// The parent's curvature normalised over the segment, for cant blending.
    fn curvature_blend(&self, s0: f64, s1: f64) -> Option<Blend> {
        match self {
            Parent::Line { .. } => Some(Blend::Linear),
            Parent::Spiral { terms, .. } => {
                let (k0, k1) = (Parent::curvature(terms, s0), Parent::curvature(terms, s1));
                if (k1 - k0).abs() > 0.0 {
                    Some(Blend::Curvature {
                        terms: terms.clone(),
                        s0,
                        s1,
                    })
                } else {
                    Some(Blend::Linear)
                }
            }
            _ => None,
        }
    }
}

/// A list attribute, refused before anything is read when it is over the segment cap.
fn bounded_list<'a>(
    entity: Entity<'a>,
    name: &str,
    what: &str,
) -> Result<tessifc_model::ListIter<'a>, GeomError> {
    let count = entity
        .attr(name)
        .as_list()
        .ok_or_else(|| GeomError::missing(name))?
        .count();
    if count > MAX_ALIGNMENT_SEGMENTS {
        return Err(GeomError::LimitReached(what.into()));
    }
    entity
        .attr(name)
        .as_list()
        .ok_or_else(|| GeomError::missing(name))
}

// ------------------------------------------------------------ segments

/// One `IfcCurveSegment` placed in its composite's plane.
#[derive(Clone, Debug)]
struct PlanarSegment {
    /// Maps the parent's frame onto the composite's plane.
    to_composite: DMat4,
    parent: Parent,
    /// Arc length on the parent at the segment start.
    start: f64,
    /// Signed arc length; negative runs the parent backwards.
    length: f64,
    /// Distance along the composite at the segment start.
    s0: f64,
}

impl PlanarSegment {
    /// Point and unit tangent at distance `d` from the segment start, in the composite's plane.
    fn at(&self, d: f64) -> (DVec2, DVec2) {
        let sign = self.length.signum();
        let parameter = self.parent.param_of_length(self.start + sign * d);
        let (point, tangent) = self.parent.at(parameter);
        let point = self.to_composite.transform_point3(point.extend(0.0));
        let tangent = self
            .to_composite
            .transform_vector3((tangent * sign).extend(0.0));
        (point.truncate(), tangent.truncate())
    }

    fn end(&self) -> f64 {
        self.s0 + self.length.abs()
    }
}

/// The 2D placement of a segment: location and reference direction in the plane.
fn segment_placement(value: Value<'_>, units: &Units) -> Result<(DVec2, DVec2), GeomError> {
    let entity = value
        .as_entity()
        .ok_or_else(|| GeomError::missing("Placement"))?;
    let location = entity
        .attr("Location")
        .as_entity()
        .and_then(|point| cartesian_point(point, units))
        .ok_or_else(|| GeomError::missing("Placement.Location"))?;
    let reference = entity
        .attr("RefDirection")
        .as_entity()
        .and_then(direction)
        .map(|axis| axis.truncate())
        .filter(|axis| axis.length_squared() > 0.0)
        .map(|axis| axis.normalize())
        .unwrap_or(DVec2::X);
    Ok((location.truncate(), reference))
}

/// A rigid motion of the plane taking `from` (point, direction) to `to`.
fn plane_motion(from: (DVec2, DVec2), to: (DVec2, DVec2)) -> DMat4 {
    let angle = to.1.y.atan2(to.1.x) - from.1.y.atan2(from.1.x);
    let rotation = DMat4::from_rotation_z(angle);
    DMat4::from_translation(to.0.extend(0.0))
        * rotation
        * DMat4::from_translation(-from.0.extend(0.0))
}

/// A segment as read, before the composite has chosen how its placement applies.
struct RawSegment {
    parent: Parent,
    start: f64,
    length: f64,
    /// The placement as a frame: the parent drawn in the placement's coordinates.
    placed: DMat4,
    /// The motion taking the parent's start point and travel direction onto the placement.
    stationed: DMat4,
}

impl RawSegment {
    fn into_segment(self, to_composite: DMat4, s0: f64) -> PlanarSegment {
        PlanarSegment {
            to_composite,
            parent: self.parent,
            start: self.start,
            length: self.length,
            s0,
        }
    }

    /// Start and end points under one of the two conventions.
    fn ends(&self, to_composite: DMat4) -> (DVec2, DVec2) {
        let probe = PlanarSegment {
            to_composite,
            parent: self.parent.clone(),
            start: self.start,
            length: self.length,
            s0: 0.0,
        };
        (probe.at(0.0).0, probe.at(self.length.abs()).0)
    }
}

/// Read one segment; `Ok(None)` is a zero-length marker such as the end of an alignment.
fn read_segment(ctx: &EvalCtx<'_>, segment: Entity<'_>) -> Result<Option<RawSegment>, GeomError> {
    if !segment.is_a("IfcCurveSegment") {
        return Err(GeomError::Unsupported(format!(
            "{} among alignment segments",
            segment.class_name()
        )));
    }
    let parent_curve = segment
        .attr("ParentCurve")
        .as_entity()
        .ok_or_else(|| GeomError::missing("ParentCurve"))?;
    let mut parent = Parent::read(ctx, parent_curve)?;
    let start_measure = curve_measure(segment.attr("SegmentStart"), &ctx.units)
        .ok_or_else(|| GeomError::missing("SegmentStart"))?;
    let length_measure = curve_measure(segment.attr("SegmentLength"), &ctx.units)
        .ok_or_else(|| GeomError::missing("SegmentLength"))?;
    let (start, length) = segment_span(ctx, &mut parent, start_measure, length_measure)?;
    if length.abs() <= ctx.tol.len {
        return Ok(None);
    }
    let (location, reference) = segment_placement(segment.attr("Placement"), &ctx.units)?;
    let (p0, t0) = parent.at(parent.param_of_length(start));
    let travel = t0 * length.signum();
    let stationed = plane_motion((p0, travel), (location, reference));
    let placed = plane_motion((DVec2::ZERO, DVec2::X), (location, reference));
    Ok(Some(RawSegment {
        parent,
        start,
        length,
        placed,
        stationed,
    }))
}

/// Which way a composite's placements apply, decided by how well its segments chain.
///
/// The standard puts the parent's start point on the placement; some files
/// draw the parent in the placement's coordinates instead, with identity
/// placements and absolute parent curves. The convention whose segment ends
/// meet their successors' starts is the one the author used.
fn chained_matrices(raw: &[RawSegment], tolerance: f64) -> Vec<DMat4> {
    let gap_total = |pick: &dyn Fn(&RawSegment) -> DMat4| -> f64 {
        raw.windows(2)
            .map(|pair| {
                let (_, end) = pair[0].ends(pick(&pair[0]));
                let (start, _) = pair[1].ends(pick(&pair[1]));
                (end - start).length()
            })
            .sum()
    };
    let stationed = gap_total(&|segment| segment.stationed);
    let placed = gap_total(&|segment| segment.placed);
    // One segment gives no evidence, and the standard's reading applies.
    let use_placed = raw.len() > 1 && placed + tolerance < stationed;
    raw.iter()
        .map(|segment| {
            if use_placed {
                segment.placed
            } else {
                segment.stationed
            }
        })
        .collect()
}

/// The parent arc length at the segment start and the signed arc length of the segment.
fn segment_span(
    ctx: &EvalCtx<'_>,
    parent: &mut Parent,
    start: CurveMeasure,
    length: CurveMeasure,
) -> Result<(f64, f64), GeomError> {
    // A polynomial's arc length needs its table over the parameters the segment
    // spans; a length measure has to be found inside it, so the range grows
    // until the table reaches that far.
    if let Parent::Polynomial { .. } = parent {
        let (mut a, mut b) = match (start, length) {
            (CurveMeasure::Parameter(p), CurveMeasure::Parameter(dp)) => {
                (p.min(p + dp), p.max(p + dp))
            }
            (CurveMeasure::Parameter(p), _) => (p - 1.0, p + 1.0),
            _ => (-1.0, 1.0),
        };
        let wanted = |measure: CurveMeasure| match measure {
            CurveMeasure::Length(value) => Some(value),
            CurveMeasure::Parameter(_) => None,
        };
        let reach = match (wanted(start), wanted(length)) {
            (Some(s), Some(l)) => Some((s.min(s + l), s.max(s + l))),
            (Some(s), None) => Some((s, s)),
            (None, Some(l)) => Some((l.min(0.0), l.max(0.0))),
            (None, None) => None,
        };
        for _ in 0..24 {
            parent.tabulate(a, b);
            let Parent::Polynomial { table, .. } = &*parent else {
                unreachable!()
            };
            let covered = match (reach, table.first(), table.last()) {
                (Some((low, high)), Some(first), Some(last)) => first.1 <= low && last.1 >= high,
                _ => true,
            };
            if covered {
                break;
            }
            let span = b - a;
            a -= span;
            b += span;
        }
    }
    // A spiral's parameter is its arc length: that is how files write it, and
    // the standard's own parameter would put the segments kilometres away.
    let to_parameter = |measure: CurveMeasure| -> f64 {
        match measure {
            CurveMeasure::Parameter(value) => match parent {
                Parent::Circle { .. } => ctx.units.angle(value),
                _ => value,
            },
            CurveMeasure::Length(value) => parent.param_of_length(value),
        }
    };
    let p0 = to_parameter(start);
    let start_length = parent.length_of_param(p0);
    let signed_length = match length {
        CurveMeasure::Length(value) => value,
        CurveMeasure::Parameter(delta) => {
            let p1 = match parent {
                Parent::Circle { .. } => p0 + ctx.units.angle(delta),
                _ => p0 + delta,
            };
            parent.length_of_param(p1) - start_length
        }
    };
    if !(start_length.is_finite() && signed_length.is_finite()) {
        return Err(GeomError::Degenerate("a non-finite segment span".into()));
    }
    Ok((start_length, signed_length))
}

/// A composite of placed segments in one plane, parameterised by distance along.
#[derive(Clone, Debug)]
struct PlanarCurve {
    segments: Vec<PlanarSegment>,
    length: f64,
    /// Metres per unit of the file's length, for a parameter value on the composite.
    unit: f64,
}

impl PlanarCurve {
    /// Read a composite whose segments are `IfcCurveSegment`; reports gaps once.
    fn read(ctx: &EvalCtx<'_>, composite: Entity<'_>) -> Result<PlanarCurve, GeomError> {
        let list = bounded_list(composite, "Segments", "alignment segments")?;
        let mut raw: Vec<RawSegment> = Vec::new();
        for (index, value) in list.enumerate() {
            if index >= MAX_ALIGNMENT_SEGMENTS {
                return Err(GeomError::LimitReached("alignment segments".into()));
            }
            let Some(entity) = value.as_entity() else {
                continue;
            };
            if let Some(segment) = read_segment(ctx, entity)? {
                raw.push(segment);
            }
        }
        if raw.is_empty() {
            return Err(GeomError::Degenerate(
                "an alignment with no segment of any length".into(),
            ));
        }
        let matrices = chained_matrices(&raw, GAP_TOLERANCES * ctx.tol.len);
        let mut segments: Vec<PlanarSegment> = Vec::with_capacity(raw.len());
        let mut s0 = 0.0;
        let mut gap_reported = false;
        for (index, (segment, matrix)) in raw.into_iter().zip(matrices).enumerate() {
            let segment = segment.into_segment(matrix, s0);
            if let Some(previous) = segments.last() {
                let (end, _) = previous.at(previous.length.abs());
                let (start, _) = segment.at(0.0);
                let gap = (end - start).length();
                if gap > GAP_TOLERANCES * ctx.tol.len && !gap_reported {
                    gap_reported = true;
                    ctx.diag.warn(
                        codes::ALIGNMENT_SEGMENT_GAP,
                        composite.id(),
                        format!(
                            "segment {index} starts {gap:.4} m from where the previous one ends; each segment keeps its own placement"
                        ),
                    );
                }
            }
            s0 = segment.end();
            segments.push(segment);
        }
        Ok(PlanarCurve {
            segments,
            length: s0,
            unit: ctx.units.length(1.0),
        })
    }

    fn segment_at(&self, s: f64) -> &PlanarSegment {
        let index = self
            .segments
            .partition_point(|segment| segment.end() < s)
            .min(self.segments.len() - 1);
        &self.segments[index]
    }

    /// Point and unit tangent at distance `s`; beyond the ends the end segments extend.
    fn at(&self, s: f64) -> (DVec2, DVec2) {
        let segment = self.segment_at(s);
        segment.at(s - segment.s0)
    }

    /// Distance along for a composite parameter: the segments' lengths
    /// accumulate, in the file's length unit, since a segment cut by length
    /// measures has no other parameter.
    fn length_of_parameter(&self, parameter: f64) -> f64 {
        parameter * self.unit
    }
}

// ------------------------------------------------------------- profiles

/// A vertical segment: a planar segment in the (distance, elevation) plane and the distances it covers.
#[derive(Clone, Debug)]
struct VerticalSegment {
    segment: PlanarSegment,
    s_start: f64,
    s_end: f64,
}

/// Elevation along a gradient curve, from its vertical segments.
#[derive(Clone, Debug)]
struct GradientProfile {
    segments: Vec<VerticalSegment>,
}

impl GradientProfile {
    fn read(ctx: &EvalCtx<'_>, curve: Entity<'_>) -> Result<GradientProfile, GeomError> {
        let planar = PlanarCurve::read(ctx, curve)?;
        let mut segments: Vec<VerticalSegment> = planar
            .segments
            .into_iter()
            .map(|segment| {
                let (start, _) = segment.at(0.0);
                let (end, _) = segment.at(segment.length.abs());
                VerticalSegment {
                    segment,
                    s_start: start.x.min(end.x),
                    s_end: start.x.max(end.x),
                }
            })
            .collect();
        segments.sort_by(|a, b| a.s_start.total_cmp(&b.s_start));
        Ok(GradientProfile { segments })
    }

    /// Elevation and slope `dz/ds` at distance `s`; the ends are held.
    fn at(&self, s: f64) -> (f64, f64) {
        let index = self
            .segments
            .partition_point(|segment| segment.s_end < s)
            .min(self.segments.len() - 1);
        let vertical = &self.segments[index];
        let d = vertical.distance_for(s.clamp(vertical.s_start, vertical.s_end));
        let (point, tangent) = vertical.segment.at(d);
        let slope = if tangent.x.abs() > 1e-12 {
            tangent.y / tangent.x
        } else {
            0.0
        };
        (point.y, slope)
    }

    /// Distances where the profile changes direction or curvature.
    fn breakpoints(&self) -> impl Iterator<Item = f64> + '_ {
        self.segments
            .iter()
            .flat_map(|segment| [segment.s_start, segment.s_end])
    }
}

impl VerticalSegment {
    /// The distance along the segment whose horizontal position is `s`.
    fn distance_for(&self, s: f64) -> f64 {
        let length = self.segment.length.abs();
        if let Parent::Line { .. } = self.segment.parent {
            let (start, tangent) = self.segment.at(0.0);
            return if tangent.x.abs() > 1e-12 {
                ((s - start.x) / tangent.x).clamp(0.0, length)
            } else {
                0.0
            };
        }
        // Horizontal position increases along a valid vertical segment.
        let forward = self.segment.at(length).0.x >= self.segment.at(0.0).0.x;
        let (mut low, mut high) = (0.0, length);
        for _ in 0..BISECTIONS {
            let mid = 0.5 * (low + high);
            let x = self.segment.at(mid).0.x;
            if (x < s) == forward {
                low = mid;
            } else {
                high = mid;
            }
        }
        0.5 * (low + high)
    }
}

/// How a cant entry blends into the next one.
#[derive(Clone, Debug)]
enum Blend {
    Linear,
    /// Follows the parent spiral's curvature, normalised over the segment.
    Curvature {
        terms: Vec<(u32, f64)>,
        s0: f64,
        s1: f64,
    },
}

impl Blend {
    fn at(&self, u: f64) -> f64 {
        match self {
            Blend::Linear => u,
            Blend::Curvature { terms, s0, s1 } => {
                let (k0, k1) = (Parent::curvature(terms, *s0), Parent::curvature(terms, *s1));
                ((Parent::curvature(terms, s0 + u * (s1 - s0)) - k0) / (k1 - k0)).clamp(0.0, 1.0)
            }
        }
    }
}

/// Offsets and cant at one distance, blended towards the next entry.
#[derive(Clone, Debug)]
struct CantEntry {
    s: f64,
    lateral: f64,
    vertical: f64,
    angle: f64,
    blend: Blend,
}

/// Lateral and vertical offsets and the cant angle along a segmented reference curve.
#[derive(Clone, Debug)]
struct CantProfile {
    entries: Vec<CantEntry>,
}

impl CantProfile {
    fn read(ctx: &EvalCtx<'_>, curve: Entity<'_>) -> Result<CantProfile, GeomError> {
        let list = bounded_list(curve, "Segments", "cant segments")?;
        let mut entries = Vec::new();
        let mut approximated = false;
        for (index, value) in list.enumerate() {
            if index >= MAX_ALIGNMENT_SEGMENTS {
                return Err(GeomError::LimitReached("alignment segments".into()));
            }
            let Some(segment) = value.as_entity() else {
                continue;
            };
            if !segment.is_a("IfcCurveSegment") {
                return Err(GeomError::Unsupported(format!(
                    "{} among cant segments",
                    segment.class_name()
                )));
            }
            let placement = segment
                .attr("Placement")
                .as_entity()
                .ok_or_else(|| GeomError::missing("Placement"))?;
            let location = placement
                .attr("Location")
                .as_entity()
                .and_then(|point| cartesian_point(point, &ctx.units))
                .ok_or_else(|| GeomError::missing("Placement.Location"))?;
            // The axis, in the curve's frame of tangent, lateral and up, is the tilted up.
            let axis = placement
                .attr("Axis")
                .as_entity()
                .and_then(direction)
                .unwrap_or(DVec3::Z);
            let angle = (-axis.y).atan2(axis.z);
            let parent_curve = segment
                .attr("ParentCurve")
                .as_entity()
                .ok_or_else(|| GeomError::missing("ParentCurve"))?;
            let blend = match Parent::read(ctx, parent_curve) {
                Ok(mut parent) => {
                    let start = curve_measure(segment.attr("SegmentStart"), &ctx.units)
                        .unwrap_or(CurveMeasure::Length(0.0));
                    let length = curve_measure(segment.attr("SegmentLength"), &ctx.units)
                        .unwrap_or(CurveMeasure::Length(0.0));
                    match segment_span(ctx, &mut parent, start, length) {
                        Ok((s0, span)) => parent.curvature_blend(s0, s0 + span),
                        Err(_) => None,
                    }
                }
                Err(_) => None,
            };
            let blend = blend.unwrap_or_else(|| {
                if !approximated {
                    approximated = true;
                    ctx.diag.warn(
                        codes::CANT_APPROXIMATED,
                        curve.id(),
                        format!(
                            "cant along a {} segment is blended linearly",
                            parent_curve.class_name()
                        ),
                    );
                }
                Blend::Linear
            });
            entries.push(CantEntry {
                s: location.x,
                lateral: location.y,
                vertical: location.z,
                angle,
                blend,
            });
        }
        // An explicit end placement closes the last blend.
        let end = curve.attr("EndPoint").as_entity();
        let end_location = end.and_then(|end| {
            end.attr("Location")
                .as_entity()
                .and_then(|point| cartesian_point(point, &ctx.units))
        });
        if let (Some(end), Some(location)) = (end, end_location) {
            let axis = end
                .attr("Axis")
                .as_entity()
                .and_then(direction)
                .unwrap_or(DVec3::Z);
            entries.push(CantEntry {
                s: location.x,
                lateral: location.y,
                vertical: location.z,
                angle: (-axis.y).atan2(axis.z),
                blend: Blend::Linear,
            });
        }
        if entries.is_empty() {
            return Err(GeomError::Degenerate("a cant curve with no segment".into()));
        }
        entries.sort_by(|a, b| a.s.total_cmp(&b.s));
        Ok(CantProfile { entries })
    }

    /// `(lateral, vertical, angle)` at distance `s`; the ends are held.
    fn at(&self, s: f64) -> (f64, f64, f64) {
        let index = self.entries.partition_point(|entry| entry.s <= s);
        if index == 0 {
            let first = &self.entries[0];
            return (first.lateral, first.vertical, first.angle);
        }
        let from = &self.entries[index - 1];
        let Some(to) = self.entries.get(index) else {
            return (from.lateral, from.vertical, from.angle);
        };
        let span = to.s - from.s;
        let u = if span > 0.0 {
            from.blend.at(((s - from.s) / span).clamp(0.0, 1.0))
        } else {
            1.0
        };
        (
            from.lateral + u * (to.lateral - from.lateral),
            from.vertical + u * (to.vertical - from.vertical),
            from.angle + u * (to.angle - from.angle),
        )
    }
}

/// One point of an offset curve by distances.
#[derive(Clone, Debug)]
struct OffsetEntry {
    s: f64,
    lateral: f64,
    vertical: f64,
}

// ---------------------------------------------------------- the curve

/// The horizontal basis a spatial curve is stationed along.
#[derive(Clone, Debug)]
enum Basis {
    Planar(PlanarCurve),
    /// An unbounded `IfcLine`, stationed from its point.
    Line {
        origin: DVec3,
        dir: DVec3,
        magnitude: f64,
    },
    /// Any polyline, with the cumulative length at each point.
    Sampled {
        points: Vec<DVec3>,
        cumulative: Vec<f64>,
    },
}

impl Basis {
    fn length(&self) -> f64 {
        match self {
            Basis::Planar(curve) => curve.length,
            Basis::Line { .. } => f64::INFINITY,
            Basis::Sampled { cumulative, .. } => cumulative.last().copied().unwrap_or(0.0),
        }
    }

    /// Point and unit tangent at distance `s`.
    fn at(&self, s: f64) -> (DVec3, DVec3) {
        match self {
            Basis::Planar(curve) => {
                let (point, tangent) = curve.at(s);
                (point.extend(0.0), tangent.extend(0.0))
            }
            Basis::Line { origin, dir, .. } => (*origin + *dir * s, *dir),
            Basis::Sampled { points, cumulative } => {
                let index = cumulative
                    .partition_point(|length| *length < s)
                    .clamp(1, points.len() - 1);
                let (a, b) = (points[index - 1], points[index]);
                let span = cumulative[index] - cumulative[index - 1];
                let fraction = if span > 0.0 {
                    ((s - cumulative[index - 1]) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let chord = b - a;
                let tangent = if chord.length_squared() > 0.0 {
                    chord.normalize()
                } else {
                    DVec3::X
                };
                (a + chord * fraction, tangent)
            }
        }
    }

    fn length_of_parameter(&self, parameter: f64) -> f64 {
        match self {
            Basis::Planar(curve) => curve.length_of_parameter(parameter),
            Basis::Line { magnitude, .. } => parameter * magnitude,
            Basis::Sampled { cumulative, .. } => {
                let index = parameter.floor().clamp(0.0, (cumulative.len() - 2) as f64) as usize;
                cumulative[index]
                    + (parameter - index as f64).clamp(0.0, 1.0)
                        * (cumulative[index + 1] - cumulative[index])
            }
        }
    }

    /// Distances where the basis bends or changes segment.
    fn breakpoints(&self) -> Vec<f64> {
        match self {
            Basis::Planar(curve) => {
                let mut stations = vec![0.0];
                stations.extend(curve.segments.iter().map(PlanarSegment::end));
                stations
            }
            Basis::Line { .. } => Vec::new(),
            Basis::Sampled { cumulative, .. } => cumulative.clone(),
        }
    }
}

/// A curve in space with a horizontal basis, an optional elevation profile,
/// optional cant and optional offsets, parameterised by distance along the basis.
#[derive(Clone, Debug)]
pub struct SpatialCurve {
    basis: Basis,
    vertical: Option<GradientProfile>,
    cant: Option<CantProfile>,
    offsets: Vec<OffsetEntry>,
}

impl SpatialCurve {
    /// The length of the basis, in metres.
    pub fn length(&self) -> f64 {
        self.basis.length()
    }

    /// Distance along for a measure on this curve.
    pub fn distance_of(&self, measure: CurveMeasure) -> f64 {
        match measure {
            CurveMeasure::Length(s) => s,
            CurveMeasure::Parameter(p) => self.basis.length_of_parameter(p),
        }
    }

    /// The frame at distance `s`; beyond the ends the end segments extend.
    pub fn frame_at(&self, s: f64) -> CurveFrame {
        let (base, horizontal) = self.basis.at(s);
        let flat = DVec3::new(horizontal.x, horizontal.y, 0.0);
        let flat = if flat.length_squared() > 0.0 {
            flat.normalize()
        } else {
            DVec3::X
        };
        let mut point = base;
        let mut tangent = horizontal;
        if let Some(profile) = &self.vertical {
            let (z, slope) = profile.at(s);
            point.z = z;
            tangent = DVec3::new(flat.x, flat.y, slope).normalize();
        }
        let mut lateral = DVec3::Z.cross(flat).normalize();
        let mut up = tangent.cross(lateral).normalize();
        let mut cant = 0.0;
        if let Some(profile) = &self.cant {
            let (lateral_offset, vertical_offset, angle) = profile.at(s);
            point += lateral * lateral_offset + up * vertical_offset;
            cant = angle;
            let (sin, cos) = angle.sin_cos();
            let tilted_lateral = lateral * cos + up * sin;
            let tilted_up = up * cos - lateral * sin;
            lateral = tilted_lateral;
            up = tilted_up;
        }
        if !self.offsets.is_empty() {
            let (lateral_offset, vertical_offset) = self.offset_at(s);
            point += lateral * lateral_offset + up * vertical_offset;
        }
        CurveFrame {
            point,
            tangent,
            lateral,
            up,
            cant,
        }
    }

    /// The frame at a measure.
    pub fn frame_at_measure(&self, measure: CurveMeasure) -> CurveFrame {
        self.frame_at(self.distance_of(measure))
    }

    /// Linear interpolation of the offset list; the ends are held.
    fn offset_at(&self, s: f64) -> (f64, f64) {
        let index = self.offsets.partition_point(|entry| entry.s <= s);
        if index == 0 {
            return (self.offsets[0].lateral, self.offsets[0].vertical);
        }
        let from = &self.offsets[index - 1];
        let Some(to) = self.offsets.get(index) else {
            return (from.lateral, from.vertical);
        };
        let span = to.s - from.s;
        let u = if span > 0.0 {
            ((s - from.s) / span).clamp(0.0, 1.0)
        } else {
            1.0
        };
        (
            from.lateral + u * (to.lateral - from.lateral),
            from.vertical + u * (to.vertical - from.vertical),
        )
    }

    /// Distances to sample between `from` and `to`, curvature adaptive and
    /// bounded; the ends and every breakpoint inside are included.
    pub fn stations(&self, ctx: &EvalCtx<'_>, from: f64, to: f64) -> Result<Vec<f64>, GeomError> {
        let (from, to) = (from.min(to), from.max(to));
        let mut stations = vec![from, to];
        let push = |value: f64, stations: &mut Vec<f64>| -> Result<(), GeomError> {
            if stations.len() >= MAX_STATIONS {
                return Err(GeomError::LimitReached("alignment stations".into()));
            }
            if value > from && value < to {
                stations.push(value);
            }
            Ok(())
        };
        for breakpoint in self.basis.breakpoints() {
            push(breakpoint, &mut stations)?;
        }
        if let Basis::Planar(curve) = &self.basis {
            for segment in &curve.segments {
                let (a, b) = (segment.s0.max(from), segment.end().min(to));
                if b <= a {
                    continue;
                }
                let sign = segment.length.signum();
                let (pa, pb) = (
                    segment.start + sign * (a - segment.s0),
                    segment.start + sign * (b - segment.s0),
                );
                let Some(radius) = segment.parent.min_radius(pa.min(pb), pa.max(pb)) else {
                    continue;
                };
                let turned = segment.parent.turned(pa.min(pb), pa.max(pb));
                let steps = steps_for_turn(ctx, radius, turned);
                for step in 1..steps {
                    push(a + (b - a) * step as f64 / steps as f64, &mut stations)?;
                }
            }
        }
        if let Some(profile) = &self.vertical {
            for breakpoint in profile.breakpoints() {
                push(breakpoint, &mut stations)?;
            }
            for vertical in &profile.segments {
                let (a, b) = (vertical.s_start.max(from), vertical.s_end.min(to));
                if b <= a {
                    continue;
                }
                let length = vertical.segment.length.abs();
                let Some(radius) = vertical.segment.parent.min_radius(
                    vertical
                        .segment
                        .start
                        .min(vertical.segment.start + vertical.segment.length),
                    vertical
                        .segment
                        .start
                        .max(vertical.segment.start + vertical.segment.length),
                ) else {
                    continue;
                };
                let turned = vertical.segment.parent.turned(
                    vertical
                        .segment
                        .start
                        .min(vertical.segment.start + vertical.segment.length),
                    vertical
                        .segment
                        .start
                        .max(vertical.segment.start + vertical.segment.length),
                ) * ((b - a) / length.max(1e-12)).min(1.0);
                let steps = steps_for_turn(ctx, radius, turned);
                for step in 1..steps {
                    push(a + (b - a) * step as f64 / steps as f64, &mut stations)?;
                }
            }
        }
        if let Some(profile) = &self.cant {
            for entry in &profile.entries {
                push(entry.s, &mut stations)?;
            }
            // A twist is sampled under the angular tolerance like a turn.
            for pair in profile.entries.windows(2) {
                let (a, b) = (pair[0].s.max(from), pair[1].s.min(to));
                let span = pair[1].s - pair[0].s;
                if b <= a || span <= 0.0 {
                    continue;
                }
                let turned = (pair[1].angle - pair[0].angle).abs() * ((b - a) / span).min(1.0);
                let steps = steps_for_angle(ctx, turned);
                for step in 1..steps {
                    push(a + (b - a) * step as f64 / steps as f64, &mut stations)?;
                }
            }
        }
        for entry in &self.offsets {
            push(entry.s, &mut stations)?;
        }
        stations.sort_by(f64::total_cmp);
        stations.dedup_by(|a, b| (*a - *b).abs() <= ctx.tol.len);
        Ok(stations)
    }

    /// The curve between two distances as a polyline.
    pub fn polyline(&self, ctx: &EvalCtx<'_>, from: f64, to: f64) -> Result<Polyline3, GeomError> {
        if !(from.is_finite() && to.is_finite()) {
            return Err(GeomError::Degenerate("an unbounded alignment".into()));
        }
        let stations = self.stations(ctx, from, to)?;
        let mut points: Vec<DVec3> = Vec::with_capacity(stations.len());
        for s in stations {
            let frame = self.frame_at(s);
            if !frame.point.is_finite() {
                return Err(GeomError::Degenerate("a non-finite alignment point".into()));
            }
            if points
                .last()
                .map(|last| (*last - frame.point).length() > ctx.tol.len)
                .unwrap_or(true)
            {
                points.push(frame.point);
            }
        }
        if points.len() < 2 {
            return Err(GeomError::Degenerate("an alignment of no length".into()));
        }
        let closed =
            points.len() > 2 && (points[points.len() - 1] - points[0]).length() <= ctx.tol.len;
        Ok(Polyline3 {
            points,
            closed,
            parameters: Vec::new(),
        })
    }
}

/// Stations for an arc of `radius` turning through `turned` radians, from the
/// settings' chord and angular tolerances; long radii turn little and need few.
fn steps_for_turn(ctx: &EvalCtx<'_>, radius: f64, turned: f64) -> usize {
    let settings = ctx.settings;
    let cap = f64::from(settings.max_circle_segments.clamp(8, 4096));
    if let Some(fixed) = settings.circle_segments {
        let per_turn = f64::from(fixed.clamp(3, cap as u32));
        return ((turned / std::f64::consts::TAU) * per_turn)
            .ceil()
            .clamp(1.0, cap) as usize;
    }
    if !(radius.is_finite() && radius > 0.0) || turned <= 0.0 {
        return 1;
    }
    // The angle per station whose chord sits within the tolerance, as in
    // `Settings::segments_for_radius`, then the angular limit.
    let angle = 4.0
        * (0.5 * (settings.chord_tolerance_m / radius).min(2.0))
            .sqrt()
            .asin();
    let per_step = angle.min(settings.angular_tolerance_rad).max(1e-9);
    (turned / per_step).ceil().clamp(1.0, cap) as usize
}

/// Stations for a twist of `turned` radians, under the angular tolerance alone.
fn steps_for_angle(ctx: &EvalCtx<'_>, turned: f64) -> usize {
    let settings = ctx.settings;
    let cap = f64::from(settings.max_circle_segments.clamp(8, 4096));
    if !(turned.is_finite() && turned > 0.0) {
        return 1;
    }
    (turned / settings.angular_tolerance_rad.max(1e-9))
        .ceil()
        .clamp(1.0, cap) as usize
}

/// Whether a curve is stationed along a basis: an alignment class, or a
/// composite made of `IfcCurveSegment`.
pub fn is_alignment_curve(curve: Entity<'_>) -> bool {
    if ALIGNMENT_CLASSES.iter().any(|class| curve.is_a(class)) {
        return true;
    }
    curve.is_a("IfcCompositeCurve")
        && curve
            .attr("Segments")
            .as_list()
            .and_then(|mut list| list.next())
            .and_then(|value| value.as_entity())
            .is_some_and(|segment| segment.is_a("IfcCurveSegment"))
}

/// The spatial curve of any curve entity, built once per context.
///
/// Alignment classes are read from their segments; any other curve is sampled
/// through the registry and stationed by arc length.
pub fn spatial_curve(ctx: &EvalCtx<'_>, curve: Entity<'_>) -> Result<Arc<SpatialCurve>, GeomError> {
    ctx.alignment_curve(curve.id(), || {
        ctx.nested(|| build_spatial_curve(ctx, curve))
    })
}

fn build_spatial_curve(ctx: &EvalCtx<'_>, curve: Entity<'_>) -> Result<SpatialCurve, GeomError> {
    if curve.is_a("IfcGradientCurve") {
        let base = curve
            .attr("BaseCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BaseCurve"))?;
        let basis = spatial_curve(ctx, base)?;
        return Ok(SpatialCurve {
            basis: basis.basis.clone(),
            vertical: Some(GradientProfile::read(ctx, curve)?),
            cant: None,
            offsets: Vec::new(),
        });
    }
    if curve.is_a("IfcSegmentedReferenceCurve") {
        let base = curve
            .attr("BaseCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BaseCurve"))?;
        let basis = spatial_curve(ctx, base)?;
        return Ok(SpatialCurve {
            basis: basis.basis.clone(),
            vertical: basis.vertical.clone(),
            cant: Some(CantProfile::read(ctx, curve)?),
            offsets: Vec::new(),
        });
    }
    if curve.is_a("IfcOffsetCurveByDistances") {
        let base = curve
            .attr("BasisCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BasisCurve"))?;
        let basis = spatial_curve(ctx, base)?;
        let list = bounded_list(curve, "OffsetValues", "offset values")?;
        let mut offsets = Vec::new();
        for (index, value) in list.enumerate() {
            if index >= MAX_ALIGNMENT_SEGMENTS {
                return Err(GeomError::LimitReached("offset values".into()));
            }
            let Some(entry) = value.as_entity() else {
                continue;
            };
            let distance = curve_measure(entry.attr("DistanceAlong"), &ctx.units)
                .ok_or_else(|| GeomError::missing("DistanceAlong"))?;
            let read = |name: &str| {
                entry
                    .attr(name)
                    .as_f64()
                    .map(|raw| ctx.units.length(raw))
                    .filter(|value| value.is_finite())
                    .unwrap_or(0.0)
            };
            offsets.push(OffsetEntry {
                s: basis.distance_of(distance),
                lateral: read("OffsetLateral"),
                vertical: read("OffsetVertical"),
            });
        }
        if offsets.is_empty() {
            return Err(GeomError::Degenerate(
                "an offset curve with no offsets".into(),
            ));
        }
        offsets.sort_by(|a, b| a.s.total_cmp(&b.s));
        let mut merged: Vec<OffsetEntry> = basis.offsets.clone();
        merged.extend(offsets);
        merged.sort_by(|a, b| a.s.total_cmp(&b.s));
        return Ok(SpatialCurve {
            basis: basis.basis.clone(),
            vertical: basis.vertical.clone(),
            cant: basis.cant.clone(),
            offsets: merged,
        });
    }
    if is_alignment_curve(curve) {
        return Ok(SpatialCurve {
            basis: Basis::Planar(PlanarCurve::read(ctx, curve)?),
            vertical: None,
            cant: None,
            offsets: Vec::new(),
        });
    }
    if curve.is_a("IfcLine") {
        let origin = curve
            .attr("Pnt")
            .as_entity()
            .and_then(|point| cartesian_point(point, &ctx.units))
            .ok_or_else(|| GeomError::missing("Pnt"))?;
        let vector = curve
            .attr("Dir")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Dir"))?;
        let dir = vector
            .attr("Orientation")
            .as_entity()
            .and_then(direction)
            .ok_or_else(|| GeomError::missing("Dir.Orientation"))?;
        let magnitude = vector
            .attr("Magnitude")
            .as_f64()
            .map(|raw| ctx.units.length(raw))
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0);
        return Ok(SpatialCurve {
            basis: Basis::Line {
                origin,
                dir,
                magnitude,
            },
            vertical: None,
            cant: None,
            offsets: Vec::new(),
        });
    }
    let polyline = ctx.registry().curve(ctx, curve)?;
    let mut cumulative = Vec::with_capacity(polyline.points.len());
    let mut running = 0.0;
    for (index, point) in polyline.points.iter().enumerate() {
        if index > 0 {
            running += (*point - polyline.points[index - 1]).length();
        }
        cumulative.push(running);
    }
    if polyline.points.len() < 2 || running <= ctx.tol.len {
        return Err(GeomError::Degenerate("a basis curve of no length".into()));
    }
    Ok(SpatialCurve {
        basis: Basis::Sampled {
            points: polyline.points,
            cumulative,
        },
        vertical: None,
        cant: None,
        offsets: Vec::new(),
    })
}

// ----------------------------------------------------------- placements

/// The frame of an `IfcPointByDistanceExpression` on its basis curve.
fn point_frame(ctx: &EvalCtx<'_>, point: Entity<'_>) -> Result<CurveFrame, GeomError> {
    if !point.is_a("IfcPointByDistanceExpression") {
        return Err(GeomError::Unsupported(format!(
            "{} as a linear placement location",
            point.class_name()
        )));
    }
    let basis = point
        .attr("BasisCurve")
        .as_entity()
        .ok_or_else(|| GeomError::missing("BasisCurve"))?;
    let curve = spatial_curve(ctx, basis)?;
    let distance = curve_measure(point.attr("DistanceAlong"), &ctx.units)
        .ok_or_else(|| GeomError::missing("DistanceAlong"))?;
    let offset = |name: &str| {
        point
            .attr(name)
            .as_f64()
            .map(|raw| ctx.units.length(raw))
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
    };
    let mut frame = curve.frame_at_measure(distance);
    frame.point += frame.lateral * offset("OffsetLateral")
        + frame.up * offset("OffsetVertical")
        + frame.tangent * offset("OffsetLongitudinal");
    Ok(frame)
}

/// The transform of an `IfcAxis2PlacementLinear`: its point on the curve, with
/// `Axis` and `RefDirection` read in the curve's frame of tangent, lateral and up.
pub fn linear_frame(ctx: &EvalCtx<'_>, placement: Entity<'_>) -> Result<DMat4, GeomError> {
    linear_frame_against(ctx, placement, None)
}

/// The angle between two frames' x or z axes, whichever is larger.
fn frame_gap(a: &DMat4, b: &DMat4) -> f64 {
    let angle = |p: DVec4, q: DVec4| {
        p.truncate()
            .normalize_or_zero()
            .dot(q.truncate().normalize_or_zero())
            .clamp(-1.0, 1.0)
            .acos()
    };
    angle(a.x_axis, b.x_axis).max(angle(a.z_axis, b.z_axis))
}

/// `linear_frame`, with a cached `CartesianPosition` as evidence of the
/// author's convention: some files write `Axis` and `RefDirection` as world
/// directions, and the cache says so when it agrees with that reading alone.
fn linear_frame_against(
    ctx: &EvalCtx<'_>,
    placement: Entity<'_>,
    cached: Option<&DMat4>,
) -> Result<DMat4, GeomError> {
    let point = placement
        .attr("Location")
        .as_entity()
        .ok_or_else(|| GeomError::missing("Location"))?;
    let frame = point_frame(ctx, point)?;
    let written = |name: &str| placement.attr(name).as_entity().and_then(direction);
    let (axis, reference) = (written("Axis"), written("RefDirection"));
    let build = |axis: Option<DVec3>, reference: Option<DVec3>| {
        let (x, y, z) = orthonormal_frame(
            Some(axis.unwrap_or(frame.up)),
            Some(reference.unwrap_or(frame.tangent)),
        );
        DMat4::from_cols(
            x.extend(0.0),
            y.extend(0.0),
            z.extend(0.0),
            frame.point.extend(1.0),
        )
    };
    let in_frame =
        |local: DVec3| frame.tangent * local.x + frame.lateral * local.y + frame.up * local.z;
    let relative = build(axis.map(in_frame), reference.map(in_frame));
    if let Some(cached) = cached
        && (axis.is_some() || reference.is_some())
    {
        let world = build(axis, reference);
        if frame_gap(&world, cached) <= MISMATCH_ANGLE
            && frame_gap(&relative, cached) > MISMATCH_ANGLE
        {
            return Ok(world);
        }
    }
    Ok(relative)
}

/// The local transform of an `IfcLinearPlacement` and the placement it is
/// relative to. A cached `CartesianPosition` that disagrees is reported on
/// `product`; the computed frame wins.
pub fn linear_placement(
    ctx: &EvalCtx<'_>,
    placement: Entity<'_>,
    product: u32,
) -> Result<(DMat4, Option<u32>), GeomError> {
    let relative = placement
        .attr("RelativePlacement")
        .as_entity()
        .ok_or_else(|| GeomError::missing("RelativePlacement"))?;
    let stored = placement
        .attr("CartesianPosition")
        .as_entity()
        .map(|cached| crate::placement::axis2_placement_3d(Value::Ref(cached), &ctx.units));
    let local = linear_frame_against(ctx, relative, stored.as_ref())?;
    if let Some(stored) = stored {
        let origin_gap = (stored.w_axis.truncate() - local.w_axis.truncate()).length();
        let axis_gap = frame_gap(&stored, &local);
        if origin_gap > MISMATCH_TOLERANCES * ctx.tol.len || axis_gap > MISMATCH_ANGLE {
            ctx.diag.warn(
                codes::LINEAR_PLACEMENT_MISMATCH,
                product,
                format!(
                    "the cached CartesianPosition sits {origin_gap:.4} m and {axis_gap:.4} rad from the placement computed along the curve; the curve wins"
                ),
            );
        }
    }
    let parent = placement
        .attr("PlacementRelTo")
        .as_entity()
        .map(|parent| parent.id());
    Ok((local, parent))
}

// ------------------------------------------------------------ evaluators

/// `IfcGradientCurve`, `IfcSegmentedReferenceCurve` and `IfcOffsetCurveByDistances`, sampled to a polyline.
pub struct AlignmentCurve;

impl CurveEvaluator for AlignmentCurve {
    fn classes(&self) -> &'static [&'static str] {
        &ALIGNMENT_CLASSES
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        sample(ctx, item)
    }
}

/// The whole curve as a polyline, through the shared spatial curve.
pub fn sample(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
    let curve = spatial_curve(ctx, item)?;
    curve.polyline(ctx, 0.0, curve.length())
}

/// Spirals and polynomial curves are unbounded: they are drawn only inside a segment.
pub struct SpiralOrPolynomial;

impl CurveEvaluator for SpiralOrPolynomial {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcClothoid",
            "IfcSecondOrderPolynomialSpiral",
            "IfcThirdOrderPolynomialSpiral",
            "IfcSeventhOrderPolynomialSpiral",
            "IfcSineSpiral",
            "IfcCosineSpiral",
            "IfcPolynomialCurve",
        ]
    }

    fn evaluate(&self, _ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        Err(GeomError::Unsupported(format!(
            "{} on its own; it is unbounded outside an IfcCurveSegment",
            item.class_name()
        )))
    }
}

/// Register the alignment evaluators.
pub fn register(registry: &mut Registry) {
    registry.register_curve(Box::new(AlignmentCurve));
    registry.register_curve(Box::new(SpiralOrPolynomial));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DiagnosticSink, Settings, Tolerances};
    use crate::eval::tests::model_of_schema;
    use tessifc_model::Model;
    use tessifc_step::Diagnostic;

    const SCHEMA: &str = "IFC4X3_ADD2";

    fn with_curve<T>(
        model: &Model,
        id: u32,
        body: impl FnOnce(&EvalCtx<'_>, &SpatialCurve) -> T,
    ) -> (T, Vec<Diagnostic>) {
        let units = Units::from_model(model);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(model, units, Tolerances::default(), &settings, &sink);
        let curve = spatial_curve(&ctx, model.entity(id).unwrap()).expect("the curve reads");
        let value = body(&ctx, &curve);
        (value, sink.take())
    }

    fn close(a: DVec3, b: DVec3, tolerance: f64) -> bool {
        (a - b).length() <= tolerance
    }

    /// A straight segment of `length` from the origin along x, as `#1..#6`.
    fn line_segment(length: f64) -> String {
        [
            "#1=IFCCARTESIANPOINT((0.,0.));",
            "#2=IFCDIRECTION((1.,0.));",
            "#3=IFCAXIS2PLACEMENT2D(#1,#2);",
            "#4=IFCVECTOR(#2,1.);",
            "#5=IFCLINE(#1,#4);",
            &format!("#6=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({length}),#5);"),
            "",
        ]
        .join("\n")
    }

    fn lines(items: &[&str]) -> String {
        let mut text = items.join("\n");
        text.push('\n');
        text
    }

    #[test]
    fn a_line_segment_frames_along_x() {
        let source = line_segment(10.0) + "#7=IFCCOMPOSITECURVE((#6),.F.);\n";
        let model = model_of_schema(SCHEMA, &source);
        let ((length, frame), diagnostics) =
            with_curve(&model, 7, |_, curve| (curve.length(), curve.frame_at(4.0)));
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!((length - 10.0).abs() < 1e-9);
        assert!(close(frame.point, DVec3::new(4.0, 0.0, 0.0), 1e-9));
        assert!(close(frame.tangent, DVec3::X, 1e-9));
        assert!(close(frame.lateral, DVec3::Y, 1e-9));
        assert!(close(frame.up, DVec3::Z, 1e-9));
    }

    #[test]
    fn a_circle_segment_turning_right_ends_where_the_arc_does() {
        // The parent circle is centred at its own origin; the segment runs it
        // backwards from the point on +x, so the composite turns right.
        let quarter = 10.0 * std::f64::consts::FRAC_PI_2;
        let model = model_of_schema(
            SCHEMA,
            &lines(&[
                "#1=IFCCARTESIANPOINT((0.,0.));",
                "#2=IFCDIRECTION((1.,0.));",
                "#3=IFCAXIS2PLACEMENT2D(#1,#2);",
                "#4=IFCCIRCLE(#3,10.);",
                &format!(
                    "#5=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(-{quarter}),#4);"
                ),
                "#6=IFCCOMPOSITECURVE((#5),.F.);",
            ]),
        );
        let ((end, middle, length), _) = with_curve(&model, 6, |_, curve| {
            (
                curve.frame_at(quarter),
                curve.frame_at(quarter / 2.0),
                curve.length(),
            )
        });
        assert!((length - quarter).abs() < 1e-9);
        assert!(
            close(end.point, DVec3::new(10.0, -10.0, 0.0), 1e-9),
            "got {}",
            end.point
        );
        assert!(close(end.tangent, -DVec3::Y, 1e-9), "got {}", end.tangent);
        let half = std::f64::consts::FRAC_1_SQRT_2 * 10.0;
        assert!(
            close(middle.point, DVec3::new(half, half - 10.0, 0.0), 1e-9),
            "got {}",
            middle.point
        );
    }

    #[test]
    fn the_clothoid_matches_the_reference_numbers() {
        let model = model_of_schema(
            SCHEMA,
            &lines(&[
                "#1=IFCCARTESIANPOINT((0.,0.));",
                "#2=IFCDIRECTION((1.,0.));",
                "#3=IFCAXIS2PLACEMENT2D(#1,#2);",
                "#4=IFCCLOTHOID(#3,-273.861278752584);",
                "#5=IFCCARTESIANPOINT((400.,0.));",
                "#6=IFCAXIS2PLACEMENT2D(#5,#2);",
                "#7=IFCCURVESEGMENT(.CONTINUOUS.,#6,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(150.),#4);",
                "#8=IFCCOMPOSITECURVE((#7),.F.);",
            ]),
        );
        let (end, _) = with_curve(&model, 8, |_, curve| curve.frame_at(150.0));
        assert!(
            close(end.point, DVec3::new(549.66285, -7.48796, 0.0), 1e-5),
            "got {}",
            end.point
        );
        assert!(
            close(
                end.tangent,
                DVec3::new(0.988771077936042, -0.149438132473604, 0.0),
                1e-6
            ),
            "got {}",
            end.tangent
        );
    }

    #[test]
    fn a_parameter_value_on_a_circle_is_an_angle_in_the_file_unit() {
        let model = model_of_schema(
            SCHEMA,
            &lines(&[
                "#90=IFCDIMENSIONALEXPONENTS(0,0,0,0,0,0,0);",
                "#91=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);",
                "#92=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);",
                "#93=IFCMEASUREWITHUNIT(IFCPLANEANGLEMEASURE(0.017453292519943295),#92);",
                "#94=IFCCONVERSIONBASEDUNIT(#90,.PLANEANGLEUNIT.,'DEGREE',#93);",
                "#95=IFCUNITASSIGNMENT((#91,#94));",
                "#96=IFCPROJECT('p',$,'P',$,$,$,$,$,#95);",
                "#1=IFCCARTESIANPOINT((0.,0.));",
                "#2=IFCDIRECTION((1.,0.));",
                "#3=IFCAXIS2PLACEMENT2D(#1,#2);",
                "#4=IFCCIRCLE(#3,2.);",
                "#5=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(90.),#4);",
                "#6=IFCCOMPOSITECURVE((#5),.F.);",
            ]),
        );
        let ((length, end), _) = with_curve(&model, 6, |_, curve| {
            (curve.length(), curve.frame_at(curve.length()))
        });
        assert!(
            (length - std::f64::consts::PI).abs() < 1e-9,
            "a quarter of a circle of radius two, got {length}"
        );
        assert!(
            close(end.point, DVec3::new(2.0, 2.0, 0.0), 1e-9),
            "a left turn from the origin, got {}",
            end.point
        );
    }

    /// A hundred metres straight with a vertical profile: ten metres up at a
    /// gradient of a tenth for fifty metres, then a sag circle of radius a thousand.
    fn gradient_model() -> Model {
        let slope: f64 = 0.1;
        let along = 50.0 * (1.0 + slope * slope).sqrt();
        let source = line_segment(100.0)
            + &lines(&[
                "#7=IFCCOMPOSITECURVE((#6),.F.);",
                "#10=IFCCARTESIANPOINT((0.,10.));",
                &format!("#11=IFCDIRECTION((1.,{slope}));"),
                "#12=IFCAXIS2PLACEMENT2D(#10,#11);",
                &format!(
                    "#13=IFCCURVESEGMENT(.CONTINUOUS.,#12,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({along}),#5);"
                ),
                "#14=IFCCARTESIANPOINT((50.,15.));",
                "#15=IFCAXIS2PLACEMENT2D(#14,#11);",
                "#16=IFCCIRCLE(#3,1000.);",
                "#17=IFCCURVESEGMENT(.CONTINUOUS.,#15,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(60.),#16);",
                "#18=IFCGRADIENTCURVE((#13,#17),.F.,#7,$);",
            ]);
        model_of_schema(SCHEMA, &source)
    }

    #[test]
    fn gradient_elevations_follow_their_segments() {
        let model = gradient_model();
        let ((on_line, on_arc), diagnostics) = with_curve(&model, 18, |_, curve| {
            (curve.frame_at(20.0), curve.frame_at(60.0))
        });
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(
            close(on_line.point, DVec3::new(20.0, 0.0, 12.0), 1e-9),
            "got {}",
            on_line.point
        );
        let slope = on_line.tangent.z / on_line.tangent.x;
        assert!((slope - 0.1).abs() < 1e-9, "got {slope}");
        // The sag circle: centre to the left of the start direction, a thousand away.
        let direction = DVec2::new(1.0, 0.1).normalize();
        let centre = DVec2::new(50.0, 15.0) + DVec2::new(-direction.y, direction.x) * 1000.0;
        let radius: f64 = 1000.0;
        let expected_z = centre.y - (radius.powi(2) - (60.0 - centre.x).powi(2)).sqrt();
        assert!(
            (on_arc.point.z - expected_z).abs() < 1e-6,
            "got {} expected {expected_z}",
            on_arc.point.z
        );
        let expected_slope =
            (60.0 - centre.x) / (radius.powi(2) - (60.0 - centre.x).powi(2)).sqrt();
        let slope = on_arc.tangent.z / on_arc.tangent.x;
        assert!(
            (slope - expected_slope).abs() < 1e-6,
            "got {slope} expected {expected_slope}"
        );
    }

    #[test]
    fn frames_are_orthonormal_along_a_gradient_curve() {
        let model = gradient_model();
        let (frames, _) = with_curve(&model, 18, |ctx, curve| {
            let stations = curve.stations(ctx, 0.0, curve.length()).unwrap();
            stations
                .iter()
                .map(|s| curve.frame_at(*s))
                .collect::<Vec<_>>()
        });
        assert!(frames.len() >= 3);
        for frame in frames {
            for axis in [frame.tangent, frame.lateral, frame.up] {
                assert!((axis.length() - 1.0).abs() < 1e-9);
            }
            assert!(frame.tangent.dot(frame.lateral).abs() < 1e-9);
            assert!(frame.tangent.dot(frame.up).abs() < 1e-9);
            assert!(frame.lateral.dot(frame.up).abs() < 1e-9);
            assert!(frame.up.z > 0.9 && close(frame.lateral, DVec3::Y, 1e-9));
        }
    }

    #[test]
    fn the_curve_samples_to_a_polyline_through_the_registry() {
        let model = gradient_model();
        let curve = crate::eval::tests::eval_curve(&model, 18).unwrap();
        let first = curve.points[0];
        let last = *curve.points.last().unwrap();
        assert!(
            close(first, DVec3::new(0.0, 0.0, 10.0), 1e-9),
            "got {first}"
        );
        assert!((last.x - 100.0).abs() < 1e-9 && !curve.closed, "got {last}");
        assert!(
            curve.points.len() > 4,
            "the sag circle is sampled, got {}",
            curve.points.len()
        );
    }

    #[test]
    fn a_linear_frame_with_a_lateral_offset_lands_inside_a_left_turn() {
        let quarter = 10.0 * std::f64::consts::FRAC_PI_2;
        let model = model_of_schema(
            SCHEMA,
            &lines(&[
                "#1=IFCCARTESIANPOINT((0.,0.));",
                "#2=IFCDIRECTION((1.,0.));",
                "#3=IFCAXIS2PLACEMENT2D(#1,#2);",
                "#4=IFCCIRCLE(#3,10.);",
                &format!(
                    "#5=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({quarter}),#4);"
                ),
                "#6=IFCCOMPOSITECURVE((#5),.F.);",
                &format!("#7=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE({quarter}),1.,$,$,#6);"),
                "#8=IFCAXIS2PLACEMENTLINEAR(#7,$,$);",
                "#9=IFCDIRECTION((0.,0.,1.));",
                "#10=IFCDIRECTION((0.,1.,0.));",
                "#11=IFCAXIS2PLACEMENTLINEAR(#7,#9,#10);",
            ]),
        );
        let units = Units::from_model(&model);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(&model, units, Tolerances::default(), &settings, &sink);
        let frame = linear_frame(&ctx, model.entity(8).unwrap()).unwrap();
        let origin = frame.w_axis.truncate();
        // A left turn from the origin heading +x has its centre at (0, 10); the
        // quarter point is (10, 10) and one metre to the left is (9, 10).
        assert!(
            close(origin, DVec3::new(9.0, 10.0, 0.0), 1e-9),
            "got {origin}"
        );
        assert!(
            close(frame.x_axis.truncate(), DVec3::Y, 1e-9),
            "x follows the tangent, got {}",
            frame.x_axis
        );
        // Axis and RefDirection are read in the curve's frame: y there is the lateral.
        let turned = linear_frame(&ctx, model.entity(11).unwrap()).unwrap();
        assert!(
            close(turned.x_axis.truncate(), -DVec3::X, 1e-9),
            "got {}",
            turned.x_axis
        );
        assert!(close(turned.z_axis.truncate(), DVec3::Z, 1e-9));
        assert!(sink.take().is_empty());
    }

    #[test]
    fn a_gap_between_segments_is_reported_once() {
        // Three straight segments, each placed a metre past where the previous one ends.
        let source = line_segment(10.0)
            + &lines(&[
                "#10=IFCCARTESIANPOINT((11.,0.));",
                "#11=IFCAXIS2PLACEMENT2D(#10,#2);",
                "#12=IFCCURVESEGMENT(.CONTINUOUS.,#11,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#5);",
                "#13=IFCCARTESIANPOINT((22.,0.));",
                "#14=IFCAXIS2PLACEMENT2D(#13,#2);",
                "#15=IFCCURVESEGMENT(.CONTINUOUS.,#14,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#5);",
                "#16=IFCCOMPOSITECURVE((#6,#12,#15),.F.);",
            ]);
        let model = model_of_schema(SCHEMA, &source);
        let ((length, end), diagnostics) = with_curve(&model, 16, |_, curve| {
            (curve.length(), curve.frame_at(25.0))
        });
        let gaps: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.code == codes::ALIGNMENT_SEGMENT_GAP)
            .collect();
        assert_eq!(gaps.len(), 1, "{diagnostics:?}");
        assert_eq!(gaps[0].express_id, Some(16));
        assert!((length - 30.0).abs() < 1e-9);
        // Each segment keeps its own placement: five into the third is at 27.
        assert!(
            close(end.point, DVec3::new(27.0, 0.0, 0.0), 1e-9),
            "got {}",
            end.point
        );
    }

    #[test]
    fn absolute_parents_under_identity_placements_chain_the_other_way() {
        // A file that draws each parent in its placement's coordinates: identity
        // placements, a line from (5, 0) and a circle whose start sits at the
        // line's end. Under the standard's reading both would start at the origin.
        let quarter = 10.0 * std::f64::consts::FRAC_PI_2;
        let model = model_of_schema(
            SCHEMA,
            &lines(&[
                "#1=IFCCARTESIANPOINT((0.,0.));",
                "#2=IFCDIRECTION((1.,0.));",
                "#3=IFCAXIS2PLACEMENT2D(#1,#2);",
                "#4=IFCCARTESIANPOINT((5.,0.));",
                "#5=IFCVECTOR(#2,1.);",
                "#6=IFCLINE(#4,#5);",
                "#7=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#6);",
                "#8=IFCCARTESIANPOINT((15.,10.));",
                "#9=IFCDIRECTION((0.,-1.));",
                "#10=IFCAXIS2PLACEMENT2D(#8,#9);",
                "#11=IFCCIRCLE(#10,10.);",
                &format!(
                    "#12=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({quarter}),#11);"
                ),
                "#13=IFCCOMPOSITECURVE((#7,#12),.F.);",
            ]),
        );
        let ((start, corner, end), diagnostics) = with_curve(&model, 13, |_, curve| {
            (
                curve.frame_at(0.0),
                curve.frame_at(10.0),
                curve.frame_at(10.0 + quarter),
            )
        });
        assert!(
            diagnostics.is_empty(),
            "the segments chain, so no gap: {diagnostics:?}"
        );
        assert!(
            close(start.point, DVec3::new(5.0, 0.0, 0.0), 1e-9),
            "got {}",
            start.point
        );
        assert!(
            close(corner.point, DVec3::new(15.0, 0.0, 0.0), 1e-9),
            "got {}",
            corner.point
        );
        assert!(
            close(end.point, DVec3::new(25.0, 10.0, 0.0), 1e-9),
            "a left turn to (25, 10), got {}",
            end.point
        );
    }

    #[test]
    fn a_composite_over_the_segment_cap_is_refused_before_it_is_read() {
        let mut source = line_segment(10.0);
        source.push_str("#7=IFCCOMPOSITECURVE((");
        source.push_str(&vec!["#6"; 100_000].join(","));
        source.push_str("),.F.);\n");
        let model = model_of_schema(SCHEMA, &source);
        let units = Units::from_model(&model);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(&model, units, Tolerances::default(), &settings, &sink);
        assert!(matches!(
            spatial_curve(&ctx, model.entity(7).unwrap()),
            Err(GeomError::LimitReached(_))
        ));
    }

    #[test]
    fn cant_tilts_the_frame_and_blends_linearly_along_a_clothoid() {
        // Level base; a cant ramp from nothing to a tenth of a radian over the first ten metres.
        let (sin, cos) = (0.1f64).sin_cos();
        let source = line_segment(20.0)
            + &lines(&[
                "#7=IFCCOMPOSITECURVE((#6),.F.);",
                "#8=IFCGRADIENTCURVE((#6),.F.,#7,$);",
                "#10=IFCCARTESIANPOINT((0.,0.,0.));",
                "#11=IFCDIRECTION((0.,0.,1.));",
                "#12=IFCDIRECTION((1.,0.,0.));",
                "#13=IFCAXIS2PLACEMENT3D(#10,#11,#12);",
                "#14=IFCCLOTHOID(#3,100.);",
                "#15=IFCCURVESEGMENT(.CONTINUOUS.,#13,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#14);",
                "#16=IFCCARTESIANPOINT((10.,0.,0.));",
                &format!("#17=IFCDIRECTION((0.,{},{cos}));", -sin),
                "#18=IFCAXIS2PLACEMENT3D(#16,#17,#12);",
                "#19=IFCCURVESEGMENT(.CONTINUOUS.,#18,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#5);",
                "#20=IFCSEGMENTEDREFERENCECURVE((#15,#19),.F.,#8,$);",
            ]);
        let model = model_of_schema(SCHEMA, &source);
        let ((start, half, end), diagnostics) = with_curve(&model, 20, |_, curve| {
            (
                curve.frame_at(0.0),
                curve.frame_at(5.0),
                curve.frame_at(15.0),
            )
        });
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(start.cant.abs() < 1e-9 && close(start.up, DVec3::Z, 1e-9));
        assert!(
            (half.cant - 0.05).abs() < 1e-9,
            "a clothoid ramps its cant linearly, got {}",
            half.cant
        );
        assert!(
            (end.cant - 0.1).abs() < 1e-9,
            "the second segment holds the cant, got {}",
            end.cant
        );
        assert!(
            close(end.up, DVec3::new(0.0, -sin, cos), 1e-9),
            "got {}",
            end.up
        );
        assert!(end.lateral.dot(end.up).abs() < 1e-9 && end.tangent.dot(end.up).abs() < 1e-9);
    }

    #[test]
    fn an_offset_curve_runs_beside_its_basis() {
        let source = line_segment(10.0)
            + &lines(&[
                "#7=IFCCOMPOSITECURVE((#6),.F.);",
                "#8=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(0.),2.,1.,$,#7);",
                "#9=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(10.),4.,1.,$,#7);",
                "#10=IFCOFFSETCURVEBYDISTANCES(#7,(#8,#9),$);",
            ]);
        let model = model_of_schema(SCHEMA, &source);
        let (middle, _) = with_curve(&model, 10, |_, curve| curve.frame_at(5.0));
        assert!(
            close(middle.point, DVec3::new(5.0, 3.0, 1.0), 1e-9),
            "got {}",
            middle.point
        );
        let polyline = crate::eval::tests::eval_curve(&model, 10).unwrap();
        assert!(close(polyline.points[0], DVec3::new(0.0, 2.0, 1.0), 1e-9));
        assert!(close(
            *polyline.points.last().unwrap(),
            DVec3::new(10.0, 4.0, 1.0),
            1e-9
        ));
    }

    #[test]
    fn a_vertical_parabola_is_inverted_by_its_table() {
        // A parabolic vertical curve, y = x^2 / 2000 for a hundred metres, on a level base.
        let source = line_segment(100.0)
            + &lines(&[
                "#7=IFCCOMPOSITECURVE((#6),.F.);",
                "#10=IFCCARTESIANPOINT((0.,5.));",
                "#11=IFCAXIS2PLACEMENT2D(#10,#2);",
                "#12=IFCPOLYNOMIALCURVE(#3,(0.,1.),(0.,0.,0.0005),$);",
                "#13=IFCCURVESEGMENT(.CONTINUOUS.,#11,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(100.),#12);",
                "#14=IFCGRADIENTCURVE((#13),.F.,#7,$);",
            ]);
        let model = model_of_schema(SCHEMA, &source);
        let (frames, diagnostics) = with_curve(&model, 14, |_, curve| {
            [curve.frame_at(40.0), curve.frame_at(100.0)]
        });
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(
            (frames[0].point.z - (5.0 + 40.0 * 40.0 * 0.0005)).abs() < 1e-6,
            "got {}",
            frames[0].point.z
        );
        assert!(
            (frames[1].point.z - 10.0).abs() < 1e-6,
            "got {}",
            frames[1].point.z
        );
        let slope = frames[0].tangent.z / frames[0].tangent.x;
        assert!((slope - 0.04).abs() < 1e-6, "got {slope}");
    }
}
