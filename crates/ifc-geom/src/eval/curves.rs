// SPDX-License-Identifier: Apache-2.0
//! Curves, flattened to polylines.
//!
//! Segment counts come from the chord tolerance in [`crate::context::Settings`].

use crate::context::EvalCtx;
use crate::error::{GeomError, codes};
use crate::placement::{axis2_placement_3d, cartesian_point, direction};
use crate::registry::{CurveEvaluator, Polyline3, Registry};
use glam::{DVec3, DVec4};
use tessifc_model::Entity;

/// Upper bound on the points one curve may flatten to; the file drives the count.
const MAX_CURVE_POINTS: usize = 1_000_000;

/// `IfcPolyline`: the common case, and the one most profiles use.
pub struct Polyline;

impl CurveEvaluator for Polyline {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcPolyline"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let list = item
            .attr("Points")
            .as_list()
            .ok_or_else(|| GeomError::missing("Points"))?;
        let mut points = Vec::new();
        for value in list {
            let Some(entity) = value.as_entity() else {
                continue;
            };
            if let Some(point) = cartesian_point(entity, &ctx.units) {
                if points.len() >= MAX_CURVE_POINTS {
                    return Err(GeomError::LimitReached("polyline points".into()));
                }
                if !point.is_finite() {
                    return Err(GeomError::Degenerate("non-finite polyline point".into()));
                }
                points.push(point);
            }
        }
        if points.len() < 2 {
            return Err(GeomError::Degenerate(format!(
                "a polyline of {} points is not a curve",
                points.len()
            )));
        }
        let closed =
            points.len() > 2 && (points[points.len() - 1] - points[0]).length() <= ctx.tol.len;
        let parameters = (0..points.len()).map(|index| index as f64).collect();
        Ok(Polyline3 {
            points,
            closed,
            parameters,
        })
    }
}

/// `IfcIndexedPolyCurve`: points in a shared list, segments by index.
///
/// Absent `Segments` means the whole point list is one polyline.
pub struct IndexedPolyCurve;

impl CurveEvaluator for IndexedPolyCurve {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcIndexedPolyCurve"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let list = item
            .attr("Points")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Points"))?;
        let coordinates = point_list(ctx, list)?;
        if coordinates.is_empty() {
            return Err(GeomError::Degenerate("an empty point list".into()));
        }

        let mut points: Vec<DVec3> = Vec::new();
        let push = |point: DVec3, points: &mut Vec<DVec3>| {
            if points
                .last()
                .map(|last| (*last - point).length() > ctx.tol.len)
                .unwrap_or(true)
            {
                points.push(point);
            }
        };

        match item.attr("Segments").as_list() {
            None => points = coordinates.clone(),
            Some(segments) => {
                for value in segments {
                    // The type name is all that tells IFCARCINDEX from IFCLINEINDEX.
                    let is_arc = match value {
                        tessifc_model::Value::Typed(typed) => typed.is("IFCARCINDEX"),
                        _ => false,
                    };
                    let Some(indices) = value.as_list() else {
                        continue;
                    };
                    let resolved: Vec<DVec3> = indices
                        .filter_map(|index| index.as_i64())
                        // One-based; checked so a hostile i64::MIN cannot overflow.
                        .filter_map(|index| index.checked_sub(1))
                        .filter_map(|zero_based| usize::try_from(zero_based).ok())
                        .filter_map(|index| coordinates.get(index).copied())
                        .collect();
                    let segment_points = if is_arc && resolved.len() == 3 {
                        arc_through(resolved[0], resolved[1], resolved[2], ctx)
                    } else {
                        resolved
                    };
                    for point in segment_points {
                        // Segments may repeat, so the cap is on the total, not the list.
                        if points.len() >= MAX_CURVE_POINTS {
                            return Err(GeomError::LimitReached("indexed curve points".into()));
                        }
                        push(point, &mut points);
                    }
                }
            }
        }

        if points.len() < 2 {
            return Err(GeomError::Degenerate(
                "an indexed curve with fewer than two points".into(),
            ));
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

/// Read `IfcCartesianPointList2D` or `3D`.
fn point_list(ctx: &EvalCtx<'_>, entity: Entity<'_>) -> Result<Vec<DVec3>, GeomError> {
    let list = entity
        .attr("CoordList")
        .as_list()
        .ok_or_else(|| GeomError::missing("CoordList"))?;
    let mut points = Vec::new();
    for row in list {
        let Some(mut values) = row.as_list().map(|list| list.floats()) else {
            continue;
        };
        let x = values.next().unwrap_or(0.0);
        let y = values.next().unwrap_or(0.0);
        let z = values.next().unwrap_or(0.0);
        points.push(DVec3::new(
            ctx.units.length(x),
            ctx.units.length(y),
            ctx.units.length(z),
        ));
    }
    Ok(points)
}

/// Points along the circular arc through three points, start and end included.
///
/// Collinear points give the straight line through them.
fn arc_through(start: DVec3, middle: DVec3, end: DVec3, ctx: &EvalCtx<'_>) -> Vec<DVec3> {
    let straight = vec![start, middle, end];
    let (u, v) = (middle - start, end - start);
    let normal = u.cross(v);
    if normal.length() <= ctx.tol.area {
        return straight;
    }

    // Circumcentre of the triangle, by the standard vector formula.
    let (uu, vv, uv) = (u.dot(u), v.dot(v), u.dot(v));
    let denominator = 2.0 * (uu * vv - uv * uv);
    if denominator.abs() <= f64::EPSILON {
        return straight;
    }
    let s = (uu * vv - uv * vv) / denominator;
    let t = (vv * uu - uv * uu) / denominator;
    let centre = start + u * s + v * t;
    let radius = (start - centre).length();
    if radius <= ctx.tol.len || !radius.is_finite() {
        return straight;
    }

    let axis = normal.normalize();
    let x = (start - centre).normalize();
    let y = axis.cross(x);

    let angle_of = |point: DVec3| {
        let offset = point - centre;
        offset.dot(y).atan2(offset.dot(x))
    };
    let mut sweep = angle_of(end);
    let middle_angle = angle_of(middle);
    // Go the way round that passes through the middle point.
    if sweep < 0.0 {
        sweep += std::f64::consts::TAU;
    }
    let mut normalised_middle = middle_angle;
    if normalised_middle < 0.0 {
        normalised_middle += std::f64::consts::TAU;
    }
    if normalised_middle > sweep {
        sweep -= std::f64::consts::TAU;
    }

    let segments = ctx.segments_for_radius(radius);
    let steps = ((segments as f64) * (sweep.abs() / std::f64::consts::TAU))
        .ceil()
        .max(1.0) as u32;
    (0..=steps)
        .map(|step| {
            let angle = sweep * step as f64 / steps as f64;
            centre + x * (radius * angle.cos()) + y * (radius * angle.sin())
        })
        .collect()
}

/// `IfcCompositeCurve`: segments, each a curve in its own right.
///
/// A segment that cannot be evaluated fails the whole curve: a sweep along a
/// directrix with a piece missing would be a different solid, not a coarser one.
/// A composite of `IfcCurveSegment` is an alignment and is stationed instead.
pub struct CompositeCurve;

impl CurveEvaluator for CompositeCurve {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCompositeCurve"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        if crate::eval::alignment::is_alignment_curve(item) {
            return crate::eval::alignment::sample(ctx, item);
        }
        let segments = item
            .attr("Segments")
            .as_list()
            .ok_or_else(|| GeomError::missing("Segments"))?;
        let registry = ctx.registry();
        let mut points: Vec<DVec3> = Vec::new();
        for value in segments {
            let Some(segment) = value.as_entity() else {
                continue;
            };
            if segment.is_a("IfcCurveSegment") {
                return Err(GeomError::Unsupported(
                    "IfcCurveSegment among IfcCompositeCurveSegment segments".into(),
                ));
            }
            let Some(curve) = segment.attr("ParentCurve").as_entity() else {
                continue;
            };
            let same_sense = segment.attr("SameSense").as_bool().unwrap_or(true);
            let part = registry.curve(ctx, curve)?;
            let mut part_points = part.points;
            if !same_sense {
                part_points.reverse();
            }
            for point in part_points {
                // Segments may reference one curve many times, so the cap is on the total.
                if points.len() >= MAX_CURVE_POINTS {
                    return Err(GeomError::LimitReached("composite curve points".into()));
                }
                if points
                    .last()
                    .map(|last| (*last - point).length() > ctx.tol.len)
                    .unwrap_or(true)
                {
                    points.push(point);
                }
            }
        }
        if points.len() < 2 {
            return Err(GeomError::Degenerate(
                "a composite curve with no readable segments".into(),
            ));
        }
        let closed = (points[points.len() - 1] - points[0]).length() <= ctx.tol.len;
        Ok(Polyline3 {
            points,
            closed,
            parameters: Vec::new(),
        })
    }
}

/// `IfcCircle` and `IfcEllipse`: closed conics, tessellated adaptively.
///
/// A full turn from local +X counter-clockwise, matching `IfcTrimmedCurve`'s parameter.
pub struct Conic;

impl CurveEvaluator for Conic {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCircle", "IfcEllipse"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let (major, minor) = radii(ctx, item)?;
        let frame = axis2_placement_3d(item.attr("Position"), &ctx.units);
        let segments = ctx.segments_for_radius(major.max(minor)).max(3);
        let mut points = Vec::with_capacity(segments as usize + 1);
        let mut parameters = Vec::with_capacity(segments as usize + 1);
        for step in 0..=segments {
            let angle = std::f64::consts::TAU * f64::from(step) / f64::from(segments);
            points.push(frame.transform_point3(DVec3::new(
                major * angle.cos(),
                minor * angle.sin(),
                0.0,
            )));
            parameters.push(angle);
        }
        Ok(Polyline3 {
            points,
            closed: true,
            parameters,
        })
    }
}

/// Semi-axes of a conic, in metres. A circle has two equal ones.
fn radii(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<(f64, f64), GeomError> {
    let value = |name: &str| item.attr(name).as_f64().map(|raw| ctx.units.length(raw));
    let (major, minor) = if item.is_a("IfcCircle") {
        let radius = value("Radius").ok_or_else(|| GeomError::missing("Radius"))?;
        (radius, radius)
    } else {
        (
            value("SemiAxis1").ok_or_else(|| GeomError::missing("SemiAxis1"))?,
            value("SemiAxis2").ok_or_else(|| GeomError::missing("SemiAxis2"))?,
        )
    };
    if !(major.is_finite() && minor.is_finite()) || major <= 0.0 || minor <= 0.0 {
        return Err(GeomError::Degenerate(
            "a conic with a non-positive radius".into(),
        ));
    }
    Ok((major, minor))
}

/// The arc of a conic between two vertices; `forward` follows increasing parameter.
///
/// The endpoints returned are the original vertices, to keep topology continuous.
pub(crate) fn conic_arc_between(
    ctx: &EvalCtx<'_>,
    conic: Entity<'_>,
    start: DVec3,
    end: DVec3,
    forward: bool,
) -> Result<Vec<DVec3>, GeomError> {
    if !(conic.is_a("IfcCircle") || conic.is_a("IfcEllipse")) {
        return Err(GeomError::Unsupported(format!(
            "{} is not a conic",
            conic.class_name()
        )));
    }
    let (major, minor) = radii(ctx, conic)?;
    let frame = axis2_placement_3d(conic.attr("Position"), &ctx.units);
    let inverse = frame.inverse();
    let local_start = inverse.transform_point3(start);
    let local_end = inverse.transform_point3(end);

    // Refuse endpoints that are not on this conic, beyond file precision.
    let endpoint_error = |point: DVec3| {
        let radial = ((point.x / major).powi(2) + (point.y / minor).powi(2)).sqrt();
        let radial_error = (radial - 1.0).abs() * major.max(minor);
        radial_error.max(point.z.abs())
    };
    let endpoint_tolerance = ctx.tol.len.max(major.max(minor) * 1e-6);
    if endpoint_error(local_start) > endpoint_tolerance
        || endpoint_error(local_end) > endpoint_tolerance
    {
        return Err(GeomError::Degenerate(
            "a conic edge whose vertices are not on its edge geometry".into(),
        ));
    }

    let from = (local_start.y / minor).atan2(local_start.x / major);
    let to = (local_end.y / minor).atan2(local_end.x / major);
    let full = std::f64::consts::TAU;
    let mut sweep = to - from;
    if forward {
        while sweep <= 0.0 {
            sweep += full;
        }
    } else {
        while sweep >= 0.0 {
            sweep -= full;
        }
    }

    let whole = ctx.segments_for_radius(major.max(minor)).max(3);
    let steps = ((f64::from(whole) * sweep.abs() / full).ceil() as u32).max(1);
    let mut points = Vec::with_capacity(steps as usize + 1);
    points.push(start);
    for step in 1..steps {
        let angle = from + sweep * f64::from(step) / f64::from(steps);
        points.push(frame.transform_point3(DVec3::new(
            major * angle.cos(),
            minor * angle.sin(),
            0.0,
        )));
    }
    points.push(end);
    Ok(points)
}

/// `IfcBSplineCurveWithKnots` and its rational subtype, by de Boor in homogeneous coordinates.
///
/// Subdivided to the same chord tolerance as analytic curves.
pub struct BSplineWithKnots;

impl CurveEvaluator for BSplineWithKnots {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcBSplineCurveWithKnots",
            "IfcRationalBSplineCurveWithKnots",
            "IfcBezierCurve",
            "IfcRationalBezierCurve",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let (points, parameters) = bspline_points_over(ctx, item, None, false)?;
        let closed = item.attr("ClosedCurve").as_bool().unwrap_or(false)
            || (points[0] - points[points.len() - 1]).length() <= ctx.tol.len;
        Ok(Polyline3 {
            points,
            closed,
            parameters,
        })
    }
}

fn bspline_points(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Vec<DVec3>, GeomError> {
    Ok(bspline_points_over(ctx, item, None, false)?.0)
}

pub(crate) fn reweighted_reference_points(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
) -> Result<Vec<DVec3>, GeomError> {
    Ok(bspline_points_over(ctx, item, None, true)?.0)
}

/// Flatten a B-spline, optionally over a sub-range of its parameter domain,
/// with the parameter at every point.
fn bspline_points_over(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
    range: Option<(f64, f64)>,
    reweight_reference: bool,
) -> Result<(Vec<DVec3>, Vec<f64>), GeomError> {
    let degree = item
        .attr("Degree")
        .as_i64()
        .and_then(|degree| usize::try_from(degree).ok())
        .ok_or_else(|| GeomError::missing("Degree"))?;
    if degree == 0 || degree > 16 {
        return Err(GeomError::Unsupported(format!(
            "a B-spline of degree {degree}"
        )));
    }
    let mut positions = Vec::new();
    for value in item
        .attr("ControlPointsList")
        .as_list()
        .ok_or_else(|| GeomError::missing("ControlPointsList"))?
    {
        if positions.len() >= 65_536 {
            return Err(GeomError::LimitReached("curve control points".into()));
        }
        let point = value
            .as_entity()
            .and_then(|point| cartesian_point(point, &ctx.units))
            .filter(|point| point.is_finite())
            .ok_or_else(|| GeomError::missing("B-spline control point"))?;
        positions.push(point);
    }

    // Weights, when rational; the homogeneous division happens once at the end.
    let rational =
        item.is_a("IfcRationalBSplineCurveWithKnots") || item.is_a("IfcRationalBezierCurve");
    if rational && item.attr("WeightsData").as_list().is_none() {
        return Err(GeomError::missing("WeightsData"));
    }
    let weights: Vec<f64> = match item.attr("WeightsData").as_list() {
        Some(list) => list
            .take(positions.len() + 1)
            .map(|value| {
                value
                    .as_f64()
                    .filter(|weight| weight.is_finite() && *weight > 0.0)
                    .ok_or_else(|| GeomError::Degenerate("a non-positive B-spline weight".into()))
            })
            .collect::<Result<_, _>>()?,
        None => vec![1.0; positions.len()],
    };
    if weights.len() != positions.len() {
        return Err(GeomError::Degenerate(format!(
            "a B-spline with {} control points and {} weights",
            positions.len(),
            weights.len()
        )));
    }
    let weight_scale = weights.iter().copied().fold(0.0_f64, f64::max);
    if reweight_reference {
        if !rational
            || (weight_scale - 1.0).abs() > 1e-12
            || weights.first().is_none_or(|w| (*w - 1.0).abs() > 1e-12)
            || weights.last().is_none_or(|w| (*w - 1.0).abs() > 1e-12)
            || weights.iter().all(|w| (*w - 1.0).abs() < 1e-12)
        {
            return Err(GeomError::Unsupported(
                "reference curve weight recovery".into(),
            ));
        }
        for (position, weight) in positions.iter_mut().zip(&weights) {
            *position *= *weight;
        }
    }
    let weights: Vec<_> = weights
        .iter()
        .map(|weight| *weight / weight_scale)
        .collect();
    if weights
        .iter()
        .any(|weight| !weight.is_finite() || *weight <= 0.0)
    {
        return Err(GeomError::Degenerate(
            "B-spline weight range exceeds numeric precision".into(),
        ));
    }
    let control: Vec<DVec4> = positions
        .iter()
        .zip(&weights)
        .map(|(point, weight)| DVec4::new(point.x, point.y, point.z, 1.0) * *weight)
        .collect();
    if control.len() <= degree {
        return Err(GeomError::Degenerate(format!(
            "a degree {degree} B-spline with only {} control points",
            control.len()
        )));
    }

    let knots = if item.is_a("IfcBezierCurve") {
        if control.len() != degree + 1 {
            return Err(GeomError::Degenerate(
                "Bezier control point count must equal degree plus one".into(),
            ));
        }
        let mut knots = vec![0.0; degree + 1];
        knots.extend(std::iter::repeat_n(1.0, degree + 1));
        knots
    } else {
        let multiplicities: Vec<usize> = item
            .attr("KnotMultiplicities")
            .as_list()
            .ok_or_else(|| GeomError::missing("KnotMultiplicities"))?
            .take(control.len() + degree + 2)
            .map(|value| {
                value
                    .as_i64()
                    .and_then(|count| usize::try_from(count).ok())
                    .filter(|count| *count > 0)
                    .ok_or_else(|| GeomError::Degenerate("a non-positive knot multiplicity".into()))
            })
            .collect::<Result<_, _>>()?;
        let unique_knots: Vec<f64> = item
            .attr("Knots")
            .as_list()
            .ok_or_else(|| GeomError::missing("Knots"))?
            .take(control.len() + degree + 2)
            .map(|value| {
                value
                    .as_f64()
                    .filter(|knot| knot.is_finite())
                    .ok_or_else(|| GeomError::Degenerate("a non-finite B-spline knot".into()))
            })
            .collect::<Result<_, _>>()?;
        if multiplicities.len() != unique_knots.len()
            || unique_knots.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(GeomError::Degenerate(
                "a B-spline with inconsistent or unordered knots".into(),
            ));
        }
        let expected = control.len() + degree + 1;
        let total = multiplicities
            .iter()
            .try_fold(0usize, |sum, count| sum.checked_add(*count))
            .filter(|sum| *sum == expected)
            .ok_or_else(|| {
                GeomError::Degenerate(
                    "B-spline knot multiplicities that do not sum to the knot vector".into(),
                )
            })?;
        let mut knots = Vec::with_capacity(total);
        for (knot, count) in unique_knots.into_iter().zip(multiplicities) {
            knots.extend(std::iter::repeat_n(knot, count));
        }
        if knots.len() != control.len() + degree + 1 {
            return Err(GeomError::Degenerate(format!(
                "a B-spline knot vector of length {} for {} control points and degree {degree}",
                knots.len(),
                control.len()
            )));
        }

        knots
    };

    let last_control = control.len() - 1;
    let domain_start = knots[degree];
    let domain_end = knots[last_control + 1];
    if domain_end <= domain_start {
        return Err(GeomError::Degenerate(
            "a B-spline with an empty parameter domain".into(),
        ));
    }
    let (domain_start, domain_end) = match range {
        Some((from, to)) => {
            let (low, high) = if from <= to { (from, to) } else { (to, from) };
            let (low, high) = (low.max(domain_start), high.min(domain_end));
            if !low.is_finite() || !high.is_finite() || high <= low {
                return Err(GeomError::Degenerate(
                    "a B-spline trimmed to an empty parameter range".into(),
                ));
            }
            (low, high)
        }
        None => (domain_start, domain_end),
    };
    let tolerance = ctx.settings.chord_tolerance_m.max(ctx.tol.len);
    let first = project(de_boor(&control, degree, &knots, domain_start));
    let mut points = vec![first];
    let mut parameters = vec![domain_start];
    for span in degree..=last_control {
        let from = knots[span].max(domain_start);
        let to = knots[span + 1].min(domain_end);
        if to <= from {
            continue;
        }
        let start = *points.last().expect("the first B-spline point exists");
        let end = project(de_boor(&control, degree, &knots, to));
        subdivide_bspline(
            &control,
            degree,
            &knots,
            from,
            start,
            to,
            end,
            tolerance,
            0,
            &mut points,
            &mut parameters,
        )?;
    }
    if points.len() < 2 || points.iter().any(|point| !point.is_finite()) {
        return Err(GeomError::Degenerate(
            "a B-spline that produced fewer than two finite points".into(),
        ));
    }
    Ok((points, parameters))
}

/// Divide out the weight; the reader refuses zero weights.
fn project(point: DVec4) -> DVec3 {
    if point.w > 0.0 && point.is_finite() {
        DVec3::new(point.x, point.y, point.z) / point.w
    } else {
        DVec3::splat(f64::NAN)
    }
}

fn de_boor(control: &[DVec4], degree: usize, knots: &[f64], parameter: f64) -> DVec4 {
    let last_control = control.len() - 1;
    let domain_end = knots[last_control + 1];
    let span = if parameter >= domain_end {
        last_control
    } else {
        knots
            .partition_point(|knot| *knot <= parameter)
            .saturating_sub(1)
            .clamp(degree, last_control)
    };
    let mut work = [DVec4::ZERO; 17];
    work[..=degree].copy_from_slice(&control[span - degree..=span]);
    for level in 1..=degree {
        for offset in (level..=degree).rev() {
            let knot_index = span - degree + offset;
            let denominator = knots[knot_index + degree - level + 1] - knots[knot_index];
            let alpha = if denominator > 0.0 {
                ((parameter - knots[knot_index]) / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            work[offset] = work[offset - 1].lerp(work[offset], alpha);
        }
    }
    work[degree]
}

#[allow(clippy::too_many_arguments)]
fn subdivide_bspline(
    control: &[DVec4],
    degree: usize,
    knots: &[f64],
    from: f64,
    start: DVec3,
    to: f64,
    end: DVec3,
    tolerance: f64,
    depth: u32,
    points: &mut Vec<DVec3>,
    parameters: &mut Vec<f64>,
) -> Result<(), GeomError> {
    if points.len() >= MAX_CURVE_POINTS {
        return Err(GeomError::LimitReached(
            "B-spline subdivision points".into(),
        ));
    }
    let middle_parameter = from * 0.5 + to * 0.5;
    let middle = project(de_boor(control, degree, knots, middle_parameter));
    let chord = end - start;
    let mut deviation: f64 = 0.0;
    for sample in [
        project(de_boor(control, degree, knots, from * 0.75 + to * 0.25)),
        middle,
        project(de_boor(control, degree, knots, from * 0.25 + to * 0.75)),
    ] {
        if !sample.is_finite() || !start.is_finite() || !end.is_finite() {
            return Err(GeomError::Degenerate("non-finite B-spline sample".into()));
        }
        let fraction = if chord.length_squared() > 0.0 {
            ((sample - start).dot(chord) / chord.length_squared()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        deviation = deviation.max(sample.distance(start.lerp(end, fraction)));
    }
    if depth < 2 || deviation > tolerance {
        if depth >= 12 || middle_parameter <= from || middle_parameter >= to {
            return Err(GeomError::LimitReached(
                "B-spline subdivision depth before tolerance was met".into(),
            ));
        }
        subdivide_bspline(
            control,
            degree,
            knots,
            from,
            start,
            middle_parameter,
            middle,
            tolerance,
            depth + 1,
            points,
            parameters,
        )?;
        subdivide_bspline(
            control,
            degree,
            knots,
            middle_parameter,
            middle,
            to,
            end,
            tolerance,
            depth + 1,
            points,
            parameters,
        )?;
    } else {
        points.push(end);
        parameters.push(to);
    }
    Ok(())
}

/// Evaluate a B-spline edge and align it with topological edge direction.
pub(crate) fn bspline_edge_between(
    ctx: &EvalCtx<'_>,
    curve: Entity<'_>,
    start: DVec3,
    end: DVec3,
    same_sense: bool,
) -> Result<Vec<DVec3>, GeomError> {
    let mut points = bspline_points(ctx, curve)?;
    if !same_sense {
        points.reverse();
    }
    let tolerance = ctx.tol.len * 10.0;
    if (points[0] - start).length() > tolerance
        || (points[points.len() - 1] - end).length() > tolerance
    {
        return Err(GeomError::Degenerate(
            "a B-spline edge whose curve endpoints do not match its vertices".into(),
        ));
    }
    points[0] = start;
    let last = points.len() - 1;
    points[last] = end;
    Ok(points)
}

/// Trim a polyline edge without dropping corners or reversing its declared direction.
pub(crate) fn polyline_edge_between(
    ctx: &EvalCtx<'_>,
    curve: Entity<'_>,
    start: DVec3,
    end: DVec3,
    same_sense: bool,
) -> Result<Vec<DVec3>, GeomError> {
    let mut line = ctx.registry().curve(ctx, curve)?;
    if !same_sense {
        line.points.reverse();
    }
    trim_polyline_edge(&line.points, start, end, line.closed, ctx.tol.len)
}

fn trim_polyline_edge(
    points: &[DVec3],
    start: DVec3,
    end: DVec3,
    closed: bool,
    tolerance: f64,
) -> Result<Vec<DVec3>, GeomError> {
    if points.len() < 2 {
        return Err(GeomError::Degenerate("an empty polyline edge".into()));
    }
    let locate = |point: DVec3| -> Result<f64, GeomError> {
        let mut best: Option<(f64, f64)> = None;
        for (index, pair) in points.windows(2).enumerate() {
            let delta = pair[1] - pair[0];
            if delta.length_squared() == 0.0 {
                continue;
            }
            let fraction = ((point - pair[0]).dot(delta) / delta.length_squared()).clamp(0.0, 1.0);
            let distance = (point - pair[0].lerp(pair[1], fraction)).length();
            if best.is_none_or(|known| distance < known.0) {
                best = Some((distance, index as f64 + fraction));
            }
        }
        best.filter(|(distance, _)| *distance <= tolerance)
            .map(|(_, at)| at)
            .ok_or_else(|| {
                GeomError::Degenerate("a topological vertex is not on its polyline edge".into())
            })
    };
    let from = locate(start)?;
    let mut to = locate(end)?;
    let period = (points.len() - 1) as f64;
    if closed && to <= from {
        to += period;
    }
    if to <= from {
        return Err(GeomError::Degenerate(
            "polyline edge endpoints contradict SameSense".into(),
        ));
    }
    let mut out = vec![start];
    for index in (from.floor() as usize + 1)..(to.ceil() as usize) {
        out.push(points[index % (points.len() - 1)]);
    }
    out.push(end);
    Ok(out)
}

/// `IfcLine`: a point and a direction, as a segment one magnitude long.
///
/// Gives `IfcTrimmedCurve` a parameterisation and everything else something finite.
pub struct Line;

impl CurveEvaluator for Line {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcLine"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let (origin, orientation, magnitude) = line_parts(ctx, item)?;
        Ok(Polyline3 {
            points: vec![origin, origin + orientation * magnitude],
            closed: false,
            parameters: vec![0.0, 1.0],
        })
    }
}

/// An `IfcLine`'s origin, unit direction and the magnitude its parameter scales.
fn line_parts(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<(DVec3, DVec3, f64), GeomError> {
    let origin = item
        .attr("Pnt")
        .as_entity()
        .and_then(|point| cartesian_point(point, &ctx.units))
        .ok_or_else(|| GeomError::missing("Pnt"))?;
    let vector = item
        .attr("Dir")
        .as_entity()
        .ok_or_else(|| GeomError::missing("Dir"))?;
    // IfcVector carries the direction and its length separately.
    let orientation = vector
        .attr("Orientation")
        .as_entity()
        .and_then(direction)
        .ok_or_else(|| GeomError::missing("Orientation"))?;
    let magnitude = vector.attr("Magnitude").as_f64().unwrap_or(1.0);
    let magnitude = if magnitude.abs() > 0.0 {
        ctx.units.length(magnitude)
    } else {
        1.0
    };
    Ok((origin, orientation, magnitude))
}

/// An `IfcLine` between two parameters; the parameter scales the whole vector.
fn trimmed_line(
    ctx: &EvalCtx<'_>,
    basis: Entity<'_>,
    from: f64,
    to: f64,
) -> Result<Polyline3, GeomError> {
    let (origin, orientation, magnitude) = line_parts(ctx, basis)?;
    let start = origin + orientation * (magnitude * from);
    let end = origin + orientation * (magnitude * to);
    if !start.is_finite() || !end.is_finite() || (end - start).length() <= ctx.tol.len {
        return Err(GeomError::Degenerate("a line trimmed to no length".into()));
    }
    Ok(Polyline3 {
        points: vec![start, end],
        closed: false,
        parameters: vec![from, to],
    })
}

/// Where a point sits on a polyline, as a segment index and a fraction along it.
fn locate_on_polyline(points: &[DVec3], target: DVec3) -> (usize, f64) {
    let mut best = (0usize, 0.0f64, f64::INFINITY);
    for index in 0..points.len().saturating_sub(1) {
        let span = points[index + 1] - points[index];
        let length = span.length_squared();
        let along = if length > 0.0 {
            ((target - points[index]).dot(span) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let distance = (points[index] + span * along - target).length();
        if distance < best.2 {
            best = (index, along, distance);
        }
    }
    (best.0, best.1)
}

/// The part of a polyline between two points, keeping the corners in between.
fn slice_polyline(points: &[DVec3], start: DVec3, end: DVec3, tolerance: f64) -> Vec<DVec3> {
    if points.len() < 2 {
        return vec![start, end];
    }
    let (first, from) = locate_on_polyline(points, start);
    let (last, to) = locate_on_polyline(points, end);
    let forward = first < last || (first == last && from <= to);
    let mut sliced = vec![start];
    if forward {
        sliced.extend(points[first + 1..=last].iter().copied());
    } else {
        sliced.extend(points[last + 1..=first].iter().rev().copied());
    }
    sliced.push(end);
    sliced.dedup_by(|a, b| (*a - *b).length() <= tolerance);
    sliced
}

/// The point at a polyline parameter: a corner index plus a fraction of the next span.
fn point_at_polyline_parameter(points: &[DVec3], parameter: f64) -> Option<DVec3> {
    if points.len() < 2 || !parameter.is_finite() {
        return None;
    }
    let last = points.len() - 1;
    let clamped = parameter.clamp(0.0, last as f64);
    let index = (clamped.floor() as usize).min(last - 1);
    Some(points[index].lerp(points[index + 1], clamped - index as f64))
}

/// `IfcTrimmedCurve`: a piece of another curve.
///
/// A conic's trim parameter is an angle in the file's own plane-angle unit.
pub struct TrimmedCurve;

impl CurveEvaluator for TrimmedCurve {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcTrimmedCurve"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let basis = item
            .attr("BasisCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BasisCurve"))?;
        let sense = item.attr("SenseAgreement").as_bool().unwrap_or(true);
        let prefer_parameter = item
            .attr("MasterRepresentation")
            .as_text()
            .map(|text| !text.raw().eq_ignore_ascii_case(b"CARTESIAN"))
            .unwrap_or(true);

        if (basis.is_a("IfcCircle") || basis.is_a("IfcEllipse"))
            && let Some(arc) = trimmed_conic(ctx, item, basis, sense, prefer_parameter)?
        {
            return Ok(arc);
        }

        let parameters = (
            trim_parameter(item.attr("Trim1")).filter(|value| value.is_finite()),
            trim_parameter(item.attr("Trim2")).filter(|value| value.is_finite()),
        );
        let cartesian = (
            trim_point(ctx, item.attr("Trim1")),
            trim_point(ctx, item.attr("Trim2")),
        );

        // A line is exact, so it is cut before the basis curve is flattened.
        if basis.is_a("IfcLine")
            && let (Some(from), Some(to)) = parameters
            && (prefer_parameter || cartesian.0.is_none() || cartesian.1.is_none())
        {
            // The curve runs from Trim1 to Trim2; the sense flag only picks a
            // direction round a periodic basis, which a line is not.
            return trimmed_line(ctx, basis, from, to);
        }

        // A B-spline is cut in its own parameter, which is a knot value.
        if (basis.is_a("IfcBSplineCurveWithKnots") || basis.is_a("IfcBezierCurve"))
            && let (Some(from), Some(to)) = parameters
            && (prefer_parameter || cartesian.0.is_none() || cartesian.1.is_none())
        {
            let (mut points, mut parameters) =
                bspline_points_over(ctx, basis, Some((from, to)), false)?;
            // Sampled in parameter order; the curve itself runs from Trim1 to Trim2.
            if from > to {
                points.reverse();
                parameters.reverse();
            }
            return Ok(Polyline3 {
                points,
                closed: false,
                parameters,
            });
        }

        let registry = ctx.registry();
        let whole = registry.curve(ctx, basis)?;

        // A cartesian trim keeps the corners between the two points, not just the chord.
        if let (Some(start), Some(end)) = cartesian {
            let points = slice_polyline(&whole.points, start, end, ctx.tol.len);
            return Ok(Polyline3 {
                points,
                closed: false,
                parameters: Vec::new(),
            });
        }

        // A polyline's parameter is a corner index plus a fraction.
        if basis.is_a("IfcPolyline")
            && let (Some(from), Some(to)) = parameters
            && let (Some(start), Some(end)) = (
                point_at_polyline_parameter(&whole.points, from),
                point_at_polyline_parameter(&whole.points, to),
            )
        {
            let points = slice_polyline(&whole.points, start, end, ctx.tol.len);
            return Ok(Polyline3 {
                points,
                closed: false,
                parameters: Vec::new(),
            });
        }

        ctx.diag.warn(
            codes::TRIM_IGNORED,
            item.id(),
            "the trim could not be applied; the whole basis curve is used",
        );
        Ok(whole)
    }
}

/// A trimmed conic, as an arc. `None` when the trims are not usable.
fn trimmed_conic(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
    basis: Entity<'_>,
    sense: bool,
    prefer_parameter: bool,
) -> Result<Option<Polyline3>, GeomError> {
    let (major, minor) = radii(ctx, basis)?;
    let frame = axis2_placement_3d(basis.attr("Position"), &ctx.units);
    let inverse = frame.inverse();

    let angle_of = |value: tessifc_model::Value<'_>| -> Option<f64> {
        if prefer_parameter && let Some(raw) = trim_parameter(value) {
            return Some(ctx.units.angle(raw));
        }
        // A cartesian trim's angle is where the point sits in the conic's frame.
        let point = trim_point(ctx, value)?;
        let local = inverse.transform_point3(point);
        Some((local.y / minor).atan2(local.x / major))
    };

    let (Some(from), Some(to)) = (angle_of(item.attr("Trim1")), angle_of(item.attr("Trim2")))
    else {
        return Ok(None);
    };

    // The sense flag decides whether a quarter turn is a quarter or three quarters.
    let full = std::f64::consts::TAU;
    let mut sweep = to - from;
    // An absurd sweep would absorb the += below and never end, so reject it first.
    if !sweep.is_finite() || sweep.abs() > 64.0 * full {
        return Ok(None);
    }
    if sense {
        while sweep <= 0.0 {
            sweep += full;
        }
    } else {
        while sweep >= 0.0 {
            sweep -= full;
        }
    }
    if sweep.abs() < 1e-12 {
        return Ok(None);
    }

    let whole = ctx.segments_for_radius(major.max(minor)).max(3);
    let steps = ((f64::from(whole) * (sweep.abs() / full)).ceil() as u32).max(1);
    let mut points = Vec::with_capacity(steps as usize + 1);
    let mut parameters = Vec::with_capacity(steps as usize + 1);
    for step in 0..=steps {
        let angle = from + sweep * f64::from(step) / f64::from(steps);
        points.push(frame.transform_point3(DVec3::new(
            major * angle.cos(),
            minor * angle.sin(),
            0.0,
        )));
        parameters.push(angle);
    }
    Ok(Some(Polyline3 {
        points,
        closed: false,
        parameters,
    }))
}

/// The `IfcParameterValue` inside an `IfcTrimmingSelect` set, if there is one.
fn trim_parameter(value: tessifc_model::Value<'_>) -> Option<f64> {
    let list = value.as_list()?;
    for entry in list {
        if let Some(number) = entry.as_f64() {
            return Some(number);
        }
    }
    None
}

/// The `IfcCartesianPoint` inside an `IfcTrimmingSelect` set, if there is one.
fn trim_point(ctx: &EvalCtx<'_>, value: tessifc_model::Value<'_>) -> Option<DVec3> {
    let list = value.as_list()?;
    for entry in list {
        if let Some(entity) = entry.as_entity()
            && entity.is_a("IfcCartesianPoint")
        {
            return cartesian_point(entity, &ctx.units);
        }
    }
    None
}

/// `IfcOffsetCurve2D` and `IfcOffsetCurve3D`: a basis curve moved sideways.
///
/// In 2D a positive distance lies to the left of the curve's direction; in 3D
/// the offset follows `RefDirection x tangent`, as ISO 10303-42 defines it.
pub struct OffsetCurve;

impl CurveEvaluator for OffsetCurve {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcOffsetCurve2D", "IfcOffsetCurve3D"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Polyline3, GeomError> {
        let basis = item
            .attr("BasisCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BasisCurve"))?;
        let curve = ctx.registry().curve(ctx, basis)?;
        let distance = ctx.units.length(
            item.attr("Distance")
                .as_f64()
                .ok_or_else(|| GeomError::missing("Distance"))?,
        );
        if !distance.is_finite() {
            return Err(GeomError::Degenerate("an offset by a non-number".into()));
        }
        let mut points = curve.points.clone();
        points.dedup_by(|a, b| (*a - *b).length() <= ctx.tol.len);
        if points.len() < 2 {
            return Err(GeomError::Degenerate(
                "an offset of a curve with fewer than two points".into(),
            ));
        }
        let offset = if item.is_a("IfcOffsetCurve3D") {
            let reference = item
                .attr("RefDirection")
                .as_entity()
                .and_then(direction)
                .ok_or_else(|| GeomError::missing("RefDirection"))?;
            offset_polyline(&points, curve.closed, distance, |tangent| {
                reference.cross(tangent)
            })
        } else {
            offset_polyline(&points, curve.closed, distance, |tangent| {
                DVec3::new(-tangent.y, tangent.x, 0.0)
            })
        };
        Ok(Polyline3 {
            points: offset,
            closed: curve.closed,
            parameters: Vec::new(),
        })
    }
}

/// Offset a polyline by `distance` along `normal_of(tangent)`, corners mitred.
///
/// A corner whose two normals nearly cancel is capped rather than sent to
/// infinity, so a hostile file cannot drive a point off the map.
pub(crate) fn offset_polyline(
    points: &[DVec3],
    closed: bool,
    distance: f64,
    normal_of: impl Fn(DVec3) -> DVec3,
) -> Vec<DVec3> {
    let count = points.len();
    let segment_normal = |from: usize, to: usize| -> DVec3 {
        let tangent = (points[to] - points[from]).normalize_or_zero();
        normal_of(tangent).normalize_or_zero()
    };
    let mut out = Vec::with_capacity(count);
    for (index, point) in points.iter().enumerate() {
        let incoming = if index > 0 {
            Some(segment_normal(index - 1, index))
        } else if closed && count > 2 {
            Some(segment_normal(count - 1, 0))
        } else {
            None
        };
        let outgoing = if index + 1 < count {
            Some(segment_normal(index, index + 1))
        } else if closed && count > 2 {
            Some(segment_normal(index, 0))
        } else {
            None
        };
        let shift = match (incoming, outgoing) {
            (Some(a), Some(b)) => {
                let bisector = (a + b).normalize_or_zero();
                // The mitre length keeps both offset segments at the same distance.
                let cosine = bisector.dot(a).max(0.25);
                bisector * (distance / cosine)
            }
            (Some(a), None) | (None, Some(a)) => a * distance,
            (None, None) => DVec3::ZERO,
        };
        out.push(*point + shift);
    }
    out
}

/// Register every curve evaluator this module provides.
pub fn register(registry: &mut Registry) {
    registry.register_curve(Box::new(Polyline));
    registry.register_curve(Box::new(IndexedPolyCurve));
    registry.register_curve(Box::new(CompositeCurve));
    registry.register_curve(Box::new(Conic));
    registry.register_curve(Box::new(BSplineWithKnots));
    registry.register_curve(Box::new(Line));
    registry.register_curve(Box::new(TrimmedCurve));
    registry.register_curve(Box::new(OffsetCurve));
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "schema-ifc2x3")]
    #[test]
    fn ifc2x3_bezier_and_rational_bezier_follow_their_control_points() {
        let fixture = |curve: &str| {
            let source = format!(
                "ISO-10303-21;HEADER;FILE_SCHEMA(('IFC2X3'));ENDSEC;DATA;\n\
                #1=IFCCARTESIANPOINT((1.,0.,0.));#2=IFCCARTESIANPOINT((1.,1.,0.));#3=IFCCARTESIANPOINT((0.,1.,0.));{curve}ENDSEC;END-ISO-10303-21;"
            );
            tessifc_model::Model::new(tessifc_step::parse(source.as_bytes(), &Default::default()))
        };
        let model = fixture(
            "#4=IFCRATIONALBEZIERCURVE(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,(1.,0.7071067811865476,1.));",
        );
        let curve = eval_curve(&model, 4).unwrap();
        for point in curve.points {
            assert!((point.length() - 1.0).abs() < 1e-10);
        }
        let model = fixture("#4=IFCBEZIERCURVE(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.);");
        let curve = eval_curve(&model, 4).unwrap();
        assert!(
            curve
                .points
                .iter()
                .any(|point| (*point - DVec3::new(0.75, 0.75, 0.)).length() < 1e-10)
        );
        let model = fixture("#4=IFCRATIONALBEZIERCURVE(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,$);");
        assert!(eval_curve(&model, 4).is_err());
    }

    #[test]
    fn polyline_edge_trims_keep_corners_and_reject_off_curve_vertices() {
        let points = [
            DVec3::ZERO,
            DVec3::X,
            DVec3::new(1., 1., 0.),
            DVec3::Y,
            DVec3::ZERO,
        ];
        let sliced = trim_polyline_edge(
            &points,
            DVec3::new(0.5, 0., 0.),
            DVec3::new(1., 0.5, 0.),
            true,
            1e-6,
        )
        .unwrap();
        assert_eq!(
            sliced,
            vec![DVec3::new(0.5, 0., 0.), DVec3::X, DVec3::new(1., 0.5, 0.)]
        );
        let wrapped = trim_polyline_edge(
            &points,
            DVec3::new(0., 0.5, 0.),
            DVec3::new(0.5, 0., 0.),
            true,
            1e-6,
        )
        .unwrap();
        assert_eq!(wrapped[1], DVec3::ZERO);
        assert!(trim_polyline_edge(&points, DVec3::Z, DVec3::X, true, 1e-6).is_err());
        assert!(trim_polyline_edge(&points, DVec3::Y, DVec3::X, false, 1e-6).is_err());
    }

    /// A circle of radius 2 about the origin, in the XY plane, as #1..#3.
    const CIRCLE: &str = "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
         #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
         #3=IFCCIRCLE(#2,2.);\n";

    #[test]
    fn a_circle_closes_and_has_the_right_radius() {
        let model = model_of(CIRCLE);
        let curve = eval_curve(&model, 3).unwrap();
        assert!(curve.closed);
        assert!(
            curve.points.len() >= 9,
            "adaptive, but never coarser than eight"
        );
        for point in &curve.points {
            assert!(
                (point.length() - 2.0).abs() < 1e-9,
                "off the circle: {point}"
            );
            assert!(point.z.abs() < 1e-12);
        }
        // First and last are the same point, since the loop is closed.
        let first = curve.points[0];
        assert!((first - *curve.points.last().unwrap()).length() < 1e-12);
        assert!(
            (first - DVec3::new(2.0, 0.0, 0.0)).length() < 1e-12,
            "starts at +X"
        );
    }

    #[test]
    fn an_ellipse_has_two_different_radii() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
             #3=IFCELLIPSE(#2,3.,1.);\n",
        );
        let curve = eval_curve(&model, 3).unwrap();
        let xs = curve
            .points
            .iter()
            .map(|p| p.x.abs())
            .fold(0.0f64, f64::max);
        let ys = curve
            .points
            .iter()
            .map(|p| p.y.abs())
            .fold(0.0f64, f64::max);
        assert!((xs - 3.0).abs() < 1e-9, "got {xs}");
        assert!((ys - 1.0).abs() < 1e-9, "got {ys}");
    }

    #[test]
    fn an_absurd_trim_parameter_does_not_hang() {
        // A parameter this large absorbs the whole-turn step; it must give up, not loop.
        let model = model_of(&format!(
            "{CIRCLE}#4=IFCTRIMMEDCURVE(#3,(IFCPARAMETERVALUE(0.)),             (IFCPARAMETERVALUE(1.E300)),.T.,.PARAMETER.);
"
        ));
        // Reaching this line is the assertion; the untrimmed conic stands.
        let curve = eval_curve(&model, 4).expect("the untrimmed conic still evaluates");
        assert!(curve.points.iter().all(|point| point.is_finite()));
    }

    #[test]
    fn a_trimmed_circle_is_the_arc_between_the_parameters() {
        // No angle unit is declared, so the parameters are radians.
        let model = model_of(&format!(
            "{CIRCLE}#4=IFCTRIMMEDCURVE(#3,(IFCPARAMETERVALUE(0.)),\
             (IFCPARAMETERVALUE(1.5707963267948966)),.T.,.PARAMETER.);\n"
        ));
        let curve = eval_curve(&model, 4).unwrap();
        assert!(!curve.closed);
        let first = curve.points[0];
        let last = *curve.points.last().unwrap();
        assert!(
            (first - DVec3::new(2.0, 0.0, 0.0)).length() < 1e-9,
            "got {first}"
        );
        assert!(
            (last - DVec3::new(0.0, 2.0, 0.0)).length() < 1e-9,
            "got {last}"
        );
        for point in &curve.points {
            assert!(
                point.x >= -1e-9 && point.y >= -1e-9,
                "the arc must stay in the quadrant"
            );
            assert!((point.length() - 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn the_sense_flag_takes_the_long_way_round() {
        // SenseAgreement false is the other three quarters, not the same quarter backwards.
        let model = model_of(&format!(
            "{CIRCLE}#4=IFCTRIMMEDCURVE(#3,(IFCPARAMETERVALUE(0.)),\
             (IFCPARAMETERVALUE(1.5707963267948966)),.F.,.PARAMETER.);\n"
        ));
        let curve = eval_curve(&model, 4).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (last - DVec3::new(0.0, 2.0, 0.0)).length() < 1e-9,
            "same end, got {last}"
        );
        assert!(
            curve.points.iter().any(|point| point.x < -1.0),
            "the long way round passes through -X"
        );
    }

    #[test]
    fn degrees_are_honoured_when_the_file_declares_them() {
        let source = format!(
            "#90=IFCDIMENSIONALEXPONENTS(0,0,0,0,0,0,0);\n\
             #91=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n\
             #92=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);\n\
             #93=IFCMEASUREWITHUNIT(IFCPLANEANGLEMEASURE(0.017453292519943295),#92);\n\
             #94=IFCCONVERSIONBASEDUNIT(#90,.PLANEANGLEUNIT.,'DEGREE',#93);\n\
             #95=IFCUNITASSIGNMENT((#91,#94));\n\
             #96=IFCPROJECT('p',$,'P',$,$,$,$,$,#95);\n{CIRCLE}\
             #4=IFCTRIMMEDCURVE(#3,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(90.)),.T.,.PARAMETER.);\n"
        );
        let model = model_of(&source);
        let curve = eval_curve(&model, 4).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (last - DVec3::new(0.0, 2.0, 0.0)).length() < 1e-9,
            "90 degrees is a quarter turn, not fourteen turns: got {last}"
        );
    }

    #[test]
    fn a_cartesian_trim_is_read_as_a_point_on_the_curve() {
        let model = model_of(&format!(
            "{CIRCLE}#5=IFCCARTESIANPOINT((2.,0.,0.));\n\
             #6=IFCCARTESIANPOINT((0.,2.,0.));\n\
             #4=IFCTRIMMEDCURVE(#3,(#5),(#6),.T.,.CARTESIAN.);\n"
        ));
        let curve = eval_curve(&model, 4).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (last - DVec3::new(0.0, 2.0, 0.0)).length() < 1e-9,
            "got {last}"
        );
    }

    #[test]
    fn a_line_becomes_a_segment_one_magnitude_long() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((1.,2.,3.));\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCVECTOR(#2,5.);\n\
             #4=IFCLINE(#1,#3);\n",
        );
        let curve = eval_curve(&model, 4).unwrap();
        assert_eq!(curve.points.len(), 2);
        assert!((curve.points[0] - DVec3::new(1.0, 2.0, 3.0)).length() < 1e-12);
        assert!((curve.points[1] - DVec3::new(1.0, 2.0, 8.0)).length() < 1e-12);
    }

    #[test]
    fn a_conic_with_no_radius_is_refused() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
             #3=IFCCIRCLE(#2,0.);\n",
        );
        assert!(eval_curve(&model, 3).is_err());
    }

    #[test]
    fn a_clamped_bspline_uses_its_knots_and_control_points() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCCARTESIANPOINT((1.,1.,0.));\n\
             #3=IFCCARTESIANPOINT((2.,0.,0.));\n\
             #4=IFCBSPLINECURVEWITHKNOTS(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,\
                 (3,3),(0.,1.),.UNSPECIFIED.);\n",
        );
        let curve = eval_curve(&model, 4).unwrap();
        assert!((curve.points[0] - DVec3::ZERO).length() < 1e-12);
        assert!(
            (curve.points[curve.points.len() - 1] - DVec3::new(2.0, 0.0, 0.0)).length() < 1e-12
        );
        let highest = curve.points.iter().map(|point| point.y).fold(0.0, f64::max);
        assert!(
            (highest - 0.5).abs() < 1e-9,
            "quadratic midpoint was {highest}"
        );
        assert!(
            curve.points.len() >= 5,
            "adaptive subdivision must retain curvature"
        );
    }
    use super::*;
    #[cfg(feature = "schema-ifc4x3")]
    use crate::eval::tests::model_of_schema;
    use crate::eval::tests::{eval_curve, model_of};

    #[test]
    fn a_polyline_reads_its_points() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCCARTESIANPOINT((1.,0.,0.));\n\
             #3=IFCCARTESIANPOINT((1.,1.,0.));\n#4=IFCPOLYLINE((#1,#2,#3));\n",
        );
        let curve = eval_curve(&model, 4).unwrap();
        assert_eq!(curve.points.len(), 3);
        assert!(!curve.closed);
    }

    #[test]
    fn a_closed_polyline_says_so() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCCARTESIANPOINT((1.,0.,0.));\n\
             #3=IFCCARTESIANPOINT((1.,1.,0.));\n#4=IFCPOLYLINE((#1,#2,#3,#1));\n",
        );
        assert!(eval_curve(&model, 4).unwrap().closed);
    }

    #[test]
    fn a_single_point_is_not_a_curve() {
        let model = model_of("#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCPOLYLINE((#1));\n");
        assert!(matches!(
            eval_curve(&model, 2),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn an_indexed_curve_without_segments_is_the_whole_list() {
        let model = model_of(
            "#1=IFCCARTESIANPOINTLIST2D(((0.,0.),(1.,0.),(1.,1.)),$);\n\
             #2=IFCINDEXEDPOLYCURVE(#1,$,$);\n",
        );
        let curve = eval_curve(&model, 2).unwrap();
        assert_eq!(curve.points.len(), 3);
    }

    #[test]
    fn an_indexed_curve_follows_line_segments() {
        let model = model_of(
            "#1=IFCCARTESIANPOINTLIST2D(((0.,0.),(1.,0.),(1.,1.)),$);\n\
             #2=IFCINDEXEDPOLYCURVE(#1,(IFCLINEINDEX((1,2)),IFCLINEINDEX((2,3))),$);\n",
        );
        let curve = eval_curve(&model, 2).unwrap();
        assert_eq!(curve.points.len(), 3, "got {:?}", curve.points);
    }

    #[test]
    fn an_arc_index_becomes_a_curve_not_a_corner() {
        // Three points on the unit circle at 0, 45 and 90 degrees.
        let model = model_of(
            "#1=IFCCARTESIANPOINTLIST2D(((1.,0.),(0.7071067811865476,0.7071067811865476),(0.,1.)),$);\n\
             #2=IFCINDEXEDPOLYCURVE(#1,(IFCARCINDEX((1,2,3))),$);\n",
        );
        let curve = eval_curve(&model, 2).unwrap();
        assert!(
            curve.points.len() > 3,
            "an arc should be subdivided, got {}",
            curve.points.len()
        );
        for point in &curve.points {
            let radius = (point.x * point.x + point.y * point.y).sqrt();
            assert!(
                (radius - 1.0).abs() < 1e-6,
                "point {point} is not on the unit circle"
            );
        }
    }

    #[test]
    fn three_collinear_points_stay_a_straight_line() {
        let model = model_of(
            "#1=IFCCARTESIANPOINTLIST2D(((0.,0.),(1.,0.),(2.,0.)),$);\n\
             #2=IFCINDEXEDPOLYCURVE(#1,(IFCARCINDEX((1,2,3))),$);\n",
        );
        let curve = eval_curve(&model, 2).unwrap();
        assert_eq!(curve.points.len(), 3, "a degenerate arc is a line");
    }

    #[test]
    fn units_are_applied_to_a_point_list() {
        let model = model_of(
            "#1=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n\
             #2=IFCUNITASSIGNMENT((#1));\n\
             #3=IFCPROJECT('g',$,'P',$,$,$,$,$,#2);\n\
             #4=IFCCARTESIANPOINTLIST2D(((0.,0.),(1000.,0.)),$);\n\
             #5=IFCINDEXEDPOLYCURVE(#4,$,$);\n",
        );
        let curve = eval_curve(&model, 5).unwrap();
        assert!(
            (curve.points[1].x - 1.0).abs() < 1e-12,
            "got {}",
            curve.points[1].x
        );
    }

    #[test]
    fn a_rational_b_spline_draws_the_conic_its_weights_ask_for() {
        // The textbook NURBS quarter circle, middle weight cos(45 degrees).
        let source = concat!(
            "#1=IFCCARTESIANPOINT((1.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,1.,0.));\n",
            "#3=IFCCARTESIANPOINT((0.,1.,0.));\n",
            "#4=IFCRATIONALBSPLINECURVEWITHKNOTS(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,\n",
            "  (3,3),(0.,1.),.UNSPECIFIED.,(1.,0.70710678118654752,1.));\n"
        );
        let model = model_of(source);
        let curve = eval_curve(&model, 4).unwrap();
        assert!(curve.points.len() >= 4, "the arc has to be subdivided");
        for point in &curve.points {
            let radius = (point.x * point.x + point.y * point.y).sqrt();
            assert!(
                (radius - 1.0).abs() < 1e-6,
                "{point} sits {radius} from the centre, not on the unit circle"
            );
        }
        // Endpoints are interpolated by the clamped knot vector.
        assert!((curve.points[0] - DVec3::new(1.0, 0.0, 0.0)).length() < 1e-12);
        let last = curve.points[curve.points.len() - 1];
        assert!((last - DVec3::new(0.0, 1.0, 0.0)).length() < 1e-12);
    }

    #[test]
    fn unit_weights_give_the_same_curve_as_the_polynomial_form() {
        let rational = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,2.,0.));\n",
            "#3=IFCCARTESIANPOINT((3.,2.,0.));\n",
            "#4=IFCCARTESIANPOINT((4.,0.,0.));\n",
            "#5=IFCRATIONALBSPLINECURVEWITHKNOTS(3,(#1,#2,#3,#4),.UNSPECIFIED.,.F.,.F.,\n",
            "  (4,4),(0.,1.),.UNSPECIFIED.,(1.,1.,1.,1.));\n"
        ));
        let plain = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,2.,0.));\n",
            "#3=IFCCARTESIANPOINT((3.,2.,0.));\n",
            "#4=IFCCARTESIANPOINT((4.,0.,0.));\n",
            "#5=IFCBSPLINECURVEWITHKNOTS(3,(#1,#2,#3,#4),.UNSPECIFIED.,.F.,.F.,\n",
            "  (4,4),(0.,1.),.UNSPECIFIED.);\n"
        ));
        let with_weights = eval_curve(&rational, 5).unwrap();
        let without = eval_curve(&plain, 5).unwrap();
        assert_eq!(with_weights.points.len(), without.points.len());
        for (a, b) in with_weights.points.iter().zip(&without.points) {
            assert!((*a - *b).length() < 1e-12, "{a} against {b}");
        }
    }

    #[test]
    fn a_weight_of_zero_is_refused_rather_than_dividing_by_it() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((1.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,1.,0.));\n",
            "#3=IFCCARTESIANPOINT((0.,1.,0.));\n",
            "#4=IFCRATIONALBSPLINECURVEWITHKNOTS(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,\n",
            "  (3,3),(0.,1.),.UNSPECIFIED.,(1.,0.,1.));\n"
        ));
        assert!(eval_curve(&model, 4).is_err());
    }
    #[test]
    fn a_parameter_trimmed_line_is_as_long_as_the_parameters_say() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCDIRECTION((1.,0.,0.));\n",
            "#3=IFCVECTOR(#2,1.);\n",
            "#4=IFCLINE(#1,#3);\n",
            "#5=IFCTRIMMEDCURVE(#4,(IFCPARAMETERVALUE(0.)),",
            "(IFCPARAMETERVALUE(5.)),.T.,.PARAMETER.);\n"
        ));
        let curve = eval_curve(&model, 5).unwrap();
        assert_eq!(curve.points.len(), 2);
        assert!((curve.points[0] - DVec3::ZERO).length() < 1e-12);
        assert!(
            (curve.points[1] - DVec3::new(5.0, 0.0, 0.0)).length() < 1e-12,
            "the trim is five units along, not one magnitude, got {}",
            curve.points[1]
        );
    }

    #[test]
    fn a_line_parameter_scales_the_whole_vector() {
        // The parameter multiplies Magnitude, so a vector of 2 reaches 10 at t = 5.
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCDIRECTION((1.,0.,0.));\n",
            "#3=IFCVECTOR(#2,2.);\n",
            "#4=IFCLINE(#1,#3);\n",
            "#5=IFCTRIMMEDCURVE(#4,(IFCPARAMETERVALUE(0.)),",
            "(IFCPARAMETERVALUE(5.)),.T.,.PARAMETER.);\n"
        ));
        let curve = eval_curve(&model, 5).unwrap();
        let length = (curve.points[1] - curve.points[0]).length();
        assert!((length - 10.0).abs() < 1e-12, "got {length}");
    }

    #[test]
    fn a_composite_curve_keeps_a_trimmed_line_segment() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCDIRECTION((1.,0.,0.));\n",
            "#3=IFCVECTOR(#2,1.);\n",
            "#4=IFCLINE(#1,#3);\n",
            "#5=IFCTRIMMEDCURVE(#4,(IFCPARAMETERVALUE(0.)),",
            "(IFCPARAMETERVALUE(5.)),.T.,.PARAMETER.);\n",
            "#6=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#5);\n",
            "#7=IFCCOMPOSITECURVE((#6),.F.);\n"
        ));
        let curve = eval_curve(&model, 7).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (last - DVec3::new(5.0, 0.0, 0.0)).length() < 1e-12,
            "the profile boundary is five units long, got {last}"
        );
    }

    #[test]
    fn a_cartesian_trimmed_polyline_keeps_its_corners() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((5.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((5.,5.,0.));\n",
            "#4=IFCCARTESIANPOINT((10.,5.,0.));\n",
            "#5=IFCPOLYLINE((#1,#2,#3,#4));\n",
            "#6=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#7=IFCCARTESIANPOINT((8.,5.,0.));\n",
            "#8=IFCTRIMMEDCURVE(#5,(#6),(#7),.T.,.CARTESIAN.);\n"
        ));
        let curve = eval_curve(&model, 8).unwrap();
        assert_eq!(
            curve.points.len(),
            4,
            "the two corners between the trims must survive, got {:?}",
            curve.points
        );
        let length: f64 = curve
            .points
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).length())
            .sum();
        assert!((length - 11.0).abs() < 1e-12, "got {length}");
    }

    #[test]
    fn a_parameter_trimmed_bspline_stops_at_the_knot_value() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,1.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#4=IFCBSPLINECURVEWITHKNOTS(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,",
            "(3,3),(0.,1.),.UNSPECIFIED.);\n",
            "#5=IFCTRIMMEDCURVE(#4,(IFCPARAMETERVALUE(0.)),",
            "(IFCPARAMETERVALUE(0.5)),.T.,.PARAMETER.);\n"
        ));
        let curve = eval_curve(&model, 5).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (last - DVec3::new(1.0, 0.5, 0.0)).length() < 1e-9,
            "half way along a quadratic, got {last}"
        );
    }

    #[test]
    fn a_trim_that_cannot_be_applied_is_reported() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((5.,0.,0.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#3);\n",
            "#5=IFCCOMPOSITECURVE((#4),.F.);\n",
            "#6=IFCTRIMMEDCURVE(#5,(IFCPARAMETERVALUE(0.)),",
            "(IFCPARAMETERVALUE(1.)),.T.,.PARAMETER.);\n"
        ));
        let (curve, diagnostics) = crate::eval::tests::eval_curve_with_diagnostics(&model, 6);
        assert!(curve.is_ok(), "the basis curve still stands");
        assert!(
            diagnostics.iter().any(|d| d.code == codes::TRIM_IGNORED),
            "an unapplied trim must be announced, got {diagnostics:?}"
        );
    }

    #[test]
    fn tiny_knot_domains_preserve_the_curve() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCCARTESIANPOINT((2.,0.,0.));\n\
             #3=IFCBSPLINECURVEWITHKNOTS(1,(#1,#2),.UNSPECIFIED.,.F.,.F.,(2,2),(0.,1.E-20),.UNSPECIFIED.);",
        );
        let curve = crate::eval::tests::eval_curve(&model, 3).unwrap();
        assert_eq!(curve.points.first(), Some(&DVec3::ZERO));
        assert_eq!(curve.points.last(), Some(&(DVec3::X * 2.0)));
        assert!(
            curve
                .points
                .iter()
                .any(|point| (point.x - 1.0).abs() < 1e-12)
        );
    }

    #[test]
    fn unresolved_curvature_at_the_depth_limit_is_refused() {
        let control = [
            DVec4::new(0.0, 0.0, 0.0, 1.0),
            DVec4::new(1.0, 2.0, 0.0, 1.0),
            DVec4::new(2.0, 0.0, 0.0, 1.0),
        ];
        let mut points = vec![DVec3::ZERO];
        let result = subdivide_bspline(
            &control,
            2,
            &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            0.0,
            DVec3::ZERO,
            1.0,
            DVec3::X * 2.0,
            1e-6,
            12,
            &mut points,
            &mut Vec::new(),
        );
        assert!(matches!(result, Err(GeomError::LimitReached(_))));
        assert_eq!(points.len(), 1);
    }

    #[test]
    fn b_spline_subdivision_stops_at_the_point_budget() {
        // A control net far coarser than any tolerance, already at the budget.
        let control = vec![
            DVec4::new(0.0, 0.0, 0.0, 1.0),
            DVec4::new(1e5, 1e5, 0.0, 1.0),
            DVec4::new(2e5, -1e5, 0.0, 1.0),
            DVec4::new(3e5, 0.0, 0.0, 1.0),
        ];
        let knots = vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        let start = project(de_boor(&control, 3, &knots, 0.0));
        let end = project(de_boor(&control, 3, &knots, 1.0));
        let mut points = vec![DVec3::ZERO; MAX_CURVE_POINTS];
        let result = subdivide_bspline(
            &control,
            3,
            &knots,
            0.0,
            start,
            1.0,
            end,
            1e-9,
            0,
            &mut points,
            &mut Vec::new(),
        );
        assert!(matches!(result, Err(GeomError::LimitReached(_))));
        assert_eq!(points.len(), MAX_CURVE_POINTS);
    }

    #[test]
    fn a_trimmed_line_runs_from_the_first_trim_whatever_the_sense() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCDIRECTION((1.,0.,0.));\n\
             #3=IFCVECTOR(#2,1.);\n\
             #4=IFCLINE(#1,#3);\n\
             #5=IFCTRIMMEDCURVE(#4,(IFCPARAMETERVALUE(5.)),(IFCPARAMETERVALUE(0.)),.F.,.PARAMETER.);\n",
        );
        let curve = eval_curve(&model, 5).unwrap();
        assert!(
            (curve.points[0].x - 5.0).abs() < 1e-9,
            "starts at Trim1, got {:?}",
            curve.points
        );
        assert!(curve.points.last().unwrap().x.abs() < 1e-9);
    }

    #[test]
    fn a_repeated_arc_segment_cannot_grow_past_the_point_budget() {
        let mut source = String::from(
            "#1=IFCCARTESIANPOINTLIST2D(((0.,0.),(1000.,1000.),(2000.,0.)));\n#2=IFCINDEXEDPOLYCURVE(#1,(",
        );
        // A large arc samples hundreds of points; the repeats add up past the budget.
        for index in 0..4200 {
            if index > 0 {
                source.push(',');
            }
            source.push_str("IFCARCINDEX((1,2,3))");
        }
        source.push_str("),$);\n");
        let model = model_of(&source);
        assert!(matches!(
            eval_curve(&model, 2),
            Err(GeomError::LimitReached(_))
        ));
    }

    /// One straight IFC4X3 alignment segment, ten units along x.
    #[cfg(feature = "schema-ifc4x3")]
    const CURVE_SEGMENT_4X3: &str = concat!(
        "#1=IFCCARTESIANPOINT((0.,0.));\n",
        "#2=IFCDIRECTION((1.,0.));\n",
        "#3=IFCAXIS2PLACEMENT2D(#1,#2);\n",
        "#4=IFCVECTOR(#2,1.);\n",
        "#5=IFCLINE(#1,#4);\n",
        "#6=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#5);\n",
    );

    #[cfg(feature = "schema-ifc4x3")]
    #[test]
    fn a_4x3_curve_segment_composite_is_stationed_along_its_segments() {
        let model = model_of_schema(
            "IFC4X3_ADD2",
            &format!("{CURVE_SEGMENT_4X3}#7=IFCCOMPOSITECURVE((#6),.F.);\n"),
        );
        let curve = eval_curve(&model, 7).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (curve.points[0] - DVec3::ZERO).length() < 1e-9
                && (last - DVec3::new(10.0, 0.0, 0.0)).length() < 1e-9,
            "ten units of the parent line from the placement, got {:?}",
            curve.points
        );
    }

    #[cfg(feature = "schema-ifc4x3")]
    #[test]
    fn a_gradient_curve_over_a_straight_base_is_drawn_in_space() {
        // The same straight segment as the vertical profile: ten along, ten up.
        let model = model_of_schema(
            "IFC4X3_ADD2",
            &format!(
                "{CURVE_SEGMENT_4X3}#7=IFCCOMPOSITECURVE((#6),.F.);\n\
                 #8=IFCGRADIENTCURVE((#6),.F.,#7,$);\n"
            ),
        );
        let curve = eval_curve(&model, 8).unwrap();
        let last = *curve.points.last().unwrap();
        assert!(
            (last - DVec3::new(10.0, 0.0, 0.0)).length() < 1e-9,
            "a level profile keeps the base curve's points, got {last}"
        );
        // A spiral has no extent of its own.
        let model = model_of_schema(
            "IFC4X3_ADD2",
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCAXIS2PLACEMENT2D(#1,$);\n#3=IFCCLOTHOID(#2,100.);\n",
        );
        assert!(matches!(
            eval_curve(&model, 3),
            Err(GeomError::Unsupported(_))
        ));
    }

    #[test]
    fn a_composite_with_an_unreadable_segment_is_refused() {
        // The second segment's parent is a circle with no radius: a sweep along
        // the first segment alone would be a different solid.
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCDIRECTION((1.,0.,0.));\n",
            "#3=IFCVECTOR(#2,1.);\n",
            "#4=IFCLINE(#1,#3);\n",
            "#5=IFCTRIMMEDCURVE(#4,(IFCPARAMETERVALUE(0.)),",
            "(IFCPARAMETERVALUE(5.)),.T.,.PARAMETER.);\n",
            "#6=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#5);\n",
            "#7=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#8=IFCCIRCLE(#7,-1.);\n",
            "#9=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#8);\n",
            "#10=IFCCOMPOSITECURVE((#6,#9),.F.);\n"
        ));
        assert!(matches!(
            eval_curve(&model, 10),
            Err(GeomError::Degenerate(_))
        ));
    }
}
