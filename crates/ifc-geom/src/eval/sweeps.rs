// SPDX-License-Identifier: Apache-2.0
//! Solids swept along three-dimensional curves.

use crate::context::EvalCtx;
use crate::error::{GeomError, codes};
use crate::placement::{axis2_placement_3d, direction};
use crate::registry::{Polyline3, Profile2D, Registry, SolidEvaluator};
use glam::{DMat4, DVec2, DVec3};
use tessifc_mesh::{Mesh64, triangulate_polygon};
use tessifc_model::Entity;

/// Upper bound on the vertices one swept disk may plan.
pub(crate) const MAX_SWEEP_VERTICES: usize = 4_000_000;

/// `IfcSweptDiskSolid`: a circular or annular section swept along a directrix.
///
/// The frame is parallel transported, so vertical runs do not collapse or flip,
/// and corners are mitred at the half angle as the specification requires.
pub struct SweptDiskSolid;

/// Sharpest mitre accepted; past it the joint would cut back into its own legs.
const MAX_MITRE_STRETCH: f64 = 4.0;

/// One cross-section station along the directrix.
struct Station {
    point: DVec3,
    normal: DVec3,
    binormal: DVec3,
    /// In-plane direction along which the mitre stretches the disk into the joint's ellipse.
    stretch_axis: DVec3,
    /// `1 / cos(half the turn)`, so 1.0 where the directrix is straight.
    stretch: f64,
}

impl SolidEvaluator for SweptDiskSolid {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSweptDiskSolid", "IfcSweptDiskSolidPolygonal"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let directrix = item
            .attr("Directrix")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Directrix"))?;
        let registry = ctx.registry();
        let curve = registry.curve(ctx, directrix)?;

        let radius = ctx.units.length(
            item.attr("Radius")
                .as_f64()
                .ok_or_else(|| GeomError::missing("Radius"))?,
        );
        let inner_radius = item
            .attr("InnerRadius")
            .as_f64()
            .map(|value| ctx.units.length(value));
        if !radius.is_finite() || radius <= ctx.tol.len {
            return Err(GeomError::Degenerate(
                "a swept disk with a non-positive radius".into(),
            ));
        }
        if inner_radius.is_some_and(|inner| {
            !inner.is_finite() || inner <= ctx.tol.len || inner >= radius - ctx.tol.len
        }) {
            return Err(GeomError::Degenerate(
                "a swept disk whose inner radius is not between zero and its outer radius".into(),
            ));
        }

        let mut curve = curve;
        apply_trim(ctx, item, directrix, &mut curve);

        // A polygonal sweep rounds its corners; a fillet that does not fit is reported
        // and the corners stay mitred.
        let fillet = item
            .attr("FilletRadius")
            .as_f64()
            .map(|value| ctx.units.length(value))
            .filter(|value| value.is_finite() && *value > ctx.tol.len);
        if let Some(fillet) = fillet {
            let outcome = if fillet < radius - ctx.tol.len {
                Err("the fillet radius is smaller than the disk radius".to_string())
            } else {
                match directrix_points(&curve, ctx.tol.len) {
                    Ok((_, true)) => Err("the directrix is closed".to_string()),
                    Ok((points, false)) => fillet_corners(
                        &points,
                        fillet,
                        ctx.segments_for_radius(fillet).max(8) as usize,
                    ),
                    Err(error) => Err(error.to_string()),
                }
            };
            match outcome {
                Ok(points) => {
                    curve = Polyline3 {
                        points,
                        closed: false,
                    }
                }
                Err(reason) => ctx.diag.warn(
                    codes::SWEEP_FILLET_IGNORED,
                    item.id(),
                    format!("FilletRadius not applied ({reason}); corners are mitred"),
                ),
            }
        }

        let segments = ctx.segments_for_radius(radius).max(8) as usize;
        sweep_disk(
            curve,
            radius,
            inner_radius,
            segments,
            ctx.tol.len,
            ctx.tol.area,
        )
    }
}

/// Cut the directrix to `StartParam`..`EndParam` where that is exact, else say so.
///
/// A polyline is parameterised one unit per segment (ISO 10303-42); other
/// directrix classes keep their whole length with a diagnostic.
fn apply_trim(ctx: &EvalCtx<'_>, item: Entity<'_>, directrix: Entity<'_>, curve: &mut Polyline3) {
    let start = item.attr("StartParam").as_f64();
    let end = item.attr("EndParam").as_f64();
    if start.is_none() && end.is_none() {
        return;
    }
    let trimmed = if directrix.is_a("IfcPolyline") {
        let last = curve.points.len().saturating_sub(1) as f64;
        trim_polyline(&curve.points, start.unwrap_or(0.0), end.unwrap_or(last))
    } else {
        None
    };
    match trimmed {
        Some(points) => {
            curve.points = points;
            curve.closed = false;
        }
        None => ctx.diag.warn(
            codes::SWEEP_PARAMETERS_APPROXIMATED,
            item.id(),
            format!(
                "StartParam/EndParam on an {} directrix not applied; swept over the full directrix",
                directrix.class_name()
            ),
        ),
    }
}

/// The part of a polyline between two parameters, one unit per segment.
fn trim_polyline(points: &[DVec3], start: f64, end: f64) -> Option<Vec<DVec3>> {
    if points.len() < 2 || !start.is_finite() || !end.is_finite() {
        return None;
    }
    let last = (points.len() - 1) as f64;
    let (low, high) = (start.min(end), start.max(end));
    if low < -1e-9 || high > last + 1e-9 || high - low <= 1e-12 {
        return None;
    }
    let at = |parameter: f64| {
        let parameter = parameter.clamp(0.0, last);
        let index = (parameter.floor() as usize).min(points.len() - 2);
        let fraction = parameter - index as f64;
        points[index] + (points[index + 1] - points[index]) * fraction
    };
    let mut out = vec![at(low)];
    for (index, point) in points.iter().enumerate() {
        let parameter = index as f64;
        if parameter > low + 1e-9 && parameter < high - 1e-9 {
            out.push(*point);
        }
    }
    out.push(at(high));
    if start > end {
        out.reverse();
    }
    Some(out)
}

/// Round every corner of an open polyline with an arc of `radius`.
///
/// Fails, with the reason, where an arc would not fit its legs.
fn fillet_corners(points: &[DVec3], radius: f64, segments: usize) -> Result<Vec<DVec3>, String> {
    if points.len() < 3 {
        return Ok(points.to_vec());
    }
    let mut out = vec![points[0]];
    for index in 1..points.len() - 1 {
        let corner = points[index];
        let incoming = corner - points[index - 1];
        let outgoing = points[index + 1] - corner;
        let (length_in, length_out) = (incoming.length(), outgoing.length());
        let (direction_in, direction_out) = (incoming / length_in, outgoing / length_out);
        let turn = direction_in.dot(direction_out).clamp(-1.0, 1.0).acos();
        if turn < 1e-9 {
            out.push(corner);
            continue;
        }
        if turn > std::f64::consts::PI - 1e-6 {
            return Err(format!("corner {index} reverses direction"));
        }
        let tangent_distance = radius * (turn / 2.0).tan();
        // An end leg is free up to its length; an inner leg is shared by two corners.
        let room_in = if index == 1 {
            length_in
        } else {
            length_in / 2.0
        };
        let room_out = if index + 2 == points.len() {
            length_out
        } else {
            length_out / 2.0
        };
        if tangent_distance > room_in + 1e-9 || tangent_distance > room_out + 1e-9 {
            return Err(format!(
                "a fillet of radius {radius} does not fit at corner {index}"
            ));
        }
        let first = corner - direction_in * tangent_distance;
        let last = corner + direction_out * tangent_distance;
        let bisector = (direction_out - direction_in).normalize_or_zero();
        let centre = corner + bisector * (radius / (turn / 2.0).cos());
        let from = first - centre;
        let axis = from.cross(last - centre).normalize_or_zero();
        let steps = ((segments as f64 * turn / std::f64::consts::TAU).ceil() as usize).max(2);
        for step in 0..=steps {
            let rotation = DMat4::from_axis_angle(axis, turn * step as f64 / steps as f64);
            out.push(centre + rotation.transform_vector3(from));
        }
    }
    out.push(points[points.len() - 1]);
    Ok(out)
}

/// A directrix's distinct points, and whether it closes on itself.
fn directrix_points(curve: &Polyline3, tolerance: f64) -> Result<(Vec<DVec3>, bool), GeomError> {
    let mut points: Vec<DVec3> = Vec::with_capacity(curve.points.len());
    for &point in &curve.points {
        if point.is_finite()
            && points
                .last()
                .is_none_or(|previous: &DVec3| (*previous - point).length() > tolerance)
        {
            points.push(point);
        }
    }
    let closed = curve.closed
        && points.len() > 2
        && (points[0] - points[points.len() - 1]).length() <= tolerance;
    if closed {
        points.pop();
    }
    if points.len() < 2 || (closed && points.len() < 3) {
        return Err(GeomError::Degenerate(
            "a sweep directrix with fewer than two distinct points".into(),
        ));
    }
    Ok((points, closed))
}

/// How a profile's x axis is carried along the directrix.
enum FrameRule {
    /// Parallel transport from the first station: no twist, right for a disk.
    Transported,
    /// The x axis is a fixed direction projected normal to the tangent.
    FixedReference(DVec3),
}

fn frames_for(tangents: &[DVec3], rule: &FrameRule) -> Result<Vec<(DVec3, DVec3)>, GeomError> {
    match rule {
        FrameRule::Transported => Ok(transported_frames(tangents)),
        FrameRule::FixedReference(reference) => tangents
            .iter()
            .map(|&tangent| {
                let projected = *reference - tangent * reference.dot(tangent);
                if projected.length_squared() <= 1e-20 {
                    return Err(GeomError::Degenerate(
                        "a sweep whose fixed reference is parallel to its directrix".into(),
                    ));
                }
                let x = projected.normalize();
                Ok((x, tangent.cross(x).normalize()))
            })
            .collect(),
    }
}

/// Sweep any profile along a directrix, mitring the corners.
///
/// The profile's x and y follow the station frame and its z the tangent, so
/// this is an extrusion bent along the curve; a closed directrix gets no caps.
fn sweep_profile(
    profile: &Profile2D,
    curve: &Polyline3,
    rule: &FrameRule,
    length_tolerance: f64,
    area_tolerance: f64,
) -> Result<Mesh64, GeomError> {
    let (points, closed) = directrix_points(curve, length_tolerance)?;
    let stations = stations(&points, closed, length_tolerance, rule)?;

    // Loops in the order the cap triangulation indexes them.
    let mut flat: Vec<DVec2> = profile.outer.clone();
    let mut loops: Vec<(usize, bool)> = vec![(profile.outer.len(), !profile.open)];
    if !profile.open {
        for hole in &profile.holes {
            if hole.len() >= 3 {
                flat.extend_from_slice(hole);
                loops.push((hole.len(), true));
            }
        }
    }
    let count = flat.len();
    if count < 2 || (!profile.open && count < 3) {
        return Err(GeomError::Degenerate(
            "a swept profile with too few points".into(),
        ));
    }
    if stations
        .len()
        .checked_mul(count)
        .is_none_or(|n| n > MAX_SWEEP_VERTICES)
    {
        return Err(GeomError::Degenerate(
            "a sweep larger than the vertex budget".into(),
        ));
    }
    let cap = if profile.open || closed {
        Vec::new()
    } else {
        triangulate_polygon(&profile.to_polygon())?
    };

    let mut mesh = Mesh64::with_capacity(stations.len() * count, count * stations.len() * 6);
    for station in &stations {
        for point in &flat {
            let offset = station.normal * point.x + station.binormal * point.y;
            let along = offset.dot(station.stretch_axis) * (station.stretch - 1.0);
            mesh.positions
                .push(station.point + offset + station.stretch_axis * along);
        }
    }

    // The profile's z is the tangent, so the sweep is an upward extrusion.
    let flip_sides = profile.open || tessifc_mesh::signed_area(&profile.outer) >= 0.0;
    let links = if closed {
        stations.len()
    } else {
        stations.len() - 1
    };
    let mut start = 0usize;
    for (length, closed_loop) in loops {
        let edges = if closed_loop { length } else { length - 1 };
        for index in 0..edges {
            let a_local = start + index;
            let b_local = start + (index + 1) % length;
            for station in 0..links {
                let next = (station + 1) % stations.len();
                let a = (station * count + a_local) as u32;
                let b = (station * count + b_local) as u32;
                let c = (next * count + b_local) as u32;
                let d = (next * count + a_local) as u32;
                if flip_sides {
                    mesh.push_triangle(a, b, c);
                    mesh.push_triangle(a, c, d);
                } else {
                    mesh.push_triangle(a, c, b);
                    mesh.push_triangle(a, d, c);
                }
            }
        }
        start += length;
    }
    if !cap.is_empty() {
        let last = ((stations.len() - 1) * count) as u32;
        for triangle in cap.chunks_exact(3) {
            let (a, b, c) = (triangle[0], triangle[1], triangle[2]);
            mesh.push_triangle(a, c, b);
            mesh.push_triangle(a + last, b + last, c + last);
        }
    }

    mesh.remove_degenerate_triangles(area_tolerance);
    mesh.closed = Some(mesh.is_edge_manifold());
    if mesh.closed == Some(true) {
        mesh.fix_orientation();
    }
    Ok(mesh)
}

/// `IfcFixedReferenceSweptAreaSolid` and `IfcSurfaceCurveSweptAreaSolid`.
///
/// Both carry the profile's x axis along a fixed direction: the file's
/// reference, or the normal of a plane the directrix lies on.
pub struct SweptAreaAlongCurve;

impl SolidEvaluator for SweptAreaAlongCurve {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcFixedReferenceSweptAreaSolid",
            "IfcSurfaceCurveSweptAreaSolid",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let registry = ctx.registry();
        let swept = item
            .attr("SweptArea")
            .as_entity()
            .ok_or_else(|| GeomError::missing("SweptArea"))?;
        let profile = registry.profile(ctx, swept)?;
        if profile.open {
            ctx.diag.warn(
                codes::OPEN_PROFILE_SURFACE,
                item.id(),
                format!(
                    "{} of an open profile; a surface was built",
                    item.class_name()
                ),
            );
        }
        let directrix = item
            .attr("Directrix")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Directrix"))?;
        let mut curve = registry.curve(ctx, directrix)?;
        apply_trim(ctx, item, directrix, &mut curve);

        let reference = if item.is_a("IfcFixedReferenceSweptAreaSolid") {
            item.attr("FixedReference")
                .as_entity()
                .and_then(direction)
                .ok_or_else(|| GeomError::missing("FixedReference"))?
        } else {
            let surface = item
                .attr("ReferenceSurface")
                .as_entity()
                .ok_or_else(|| GeomError::missing("ReferenceSurface"))?;
            if !surface.is_a("IfcPlane") {
                return Err(GeomError::Unsupported(format!(
                    "IfcSurfaceCurveSweptAreaSolid over {}",
                    surface.class_name()
                )));
            }
            axis2_placement_3d(surface.attr("Position"), &ctx.units)
                .transform_vector3(DVec3::Z)
                .normalize_or(DVec3::Z)
        };

        let mut mesh = sweep_profile(
            &profile,
            &curve,
            &FrameRule::FixedReference(reference),
            ctx.tol.len,
            ctx.tol.area,
        )?;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// `IfcSectionedSpine`: cross sections at placements along a spine, lofted in turn.
pub struct SectionedSpine;

impl SolidEvaluator for SectionedSpine {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSectionedSpine"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let registry = ctx.registry();
        let mut sections: Vec<Profile2D> = Vec::new();
        for value in item
            .attr("CrossSections")
            .as_list()
            .ok_or_else(|| GeomError::missing("CrossSections"))?
        {
            let profile = value
                .as_entity()
                .ok_or_else(|| GeomError::missing("a cross section"))?;
            sections.push(registry.profile(ctx, profile)?);
        }
        let placements: Vec<DMat4> = item
            .attr("CrossSectionPositions")
            .as_list()
            .ok_or_else(|| GeomError::missing("CrossSectionPositions"))?
            .map(|value| axis2_placement_3d(value, &ctx.units))
            .collect();
        if sections.len() < 2 || sections.len() != placements.len() {
            return Err(GeomError::Degenerate(
                "a sectioned spine needs one placement per section, and two sections at least"
                    .into(),
            ));
        }
        if sections.iter().any(|section| section.open) {
            return Err(GeomError::Unsupported(
                "IfcSectionedSpine with open cross sections".into(),
            ));
        }

        // Every section gets the same corner counts, outline and holes alike.
        let hole_count = sections[0].holes.len();
        if sections
            .iter()
            .any(|section| section.holes.len() != hole_count)
        {
            ctx.diag.warn(
                codes::PROFILE_DETAIL_APPROXIMATED,
                item.id(),
                "sections with different numbers of holes; lofted without holes",
            );
            for section in &mut sections {
                section.holes.clear();
            }
        }
        let outer_count = sections.iter().map(|s| s.outer.len()).max().unwrap_or(0);
        let hole_counts: Vec<usize> = (0..sections[0].holes.len())
            .map(|hole| {
                sections
                    .iter()
                    .map(|s| s.holes[hole].len())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let mut resampled = false;
        for section in &mut sections {
            if section.outer.len() != outer_count {
                section.outer = crate::eval::solids::resample_loop(&section.outer, outer_count);
                resampled = true;
            }
            for (hole, &count) in section.holes.iter_mut().zip(&hole_counts) {
                if hole.len() != count {
                    *hole = crate::eval::solids::resample_loop(hole, count);
                    resampled = true;
                }
            }
        }
        if resampled {
            ctx.diag.warn(
                codes::PROFILE_DETAIL_APPROXIMATED,
                item.id(),
                "sections whose corners do not correspond; lofted between resampled outlines",
            );
        }

        let flat = |section: &Profile2D| -> Vec<DVec2> {
            let mut points = section.outer.clone();
            for hole in &section.holes {
                if hole.len() >= 3 {
                    points.extend_from_slice(hole);
                }
            }
            points
        };
        let mut loops: Vec<usize> = vec![outer_count];
        loops.extend(hole_counts.iter().copied().filter(|&n| n >= 3));
        let count: usize = loops.iter().sum();
        if count < 3
            || sections
                .len()
                .checked_mul(count)
                .is_none_or(|n| n > MAX_SWEEP_VERTICES)
        {
            return Err(GeomError::Degenerate(
                "a sectioned spine with too few or too many corners".into(),
            ));
        }
        let cap_start = triangulate_polygon(&sections[0].to_polygon())?;
        let cap_end = triangulate_polygon(&sections[sections.len() - 1].to_polygon())?;

        let mut mesh = Mesh64::with_capacity(sections.len() * count, sections.len() * count * 6);
        for (section, placement) in sections.iter().zip(&placements) {
            for point in flat(section) {
                mesh.positions
                    .push(placement.transform_point3(DVec3::new(point.x, point.y, 0.0)));
            }
        }

        // The sections advance along the first placement's z, or against it.
        let advance = placements[1].transform_point3(DVec3::ZERO)
            - placements[0].transform_point3(DVec3::ZERO);
        let upwards = advance.dot(placements[0].transform_vector3(DVec3::Z)) >= 0.0;
        let outward = tessifc_mesh::signed_area(&sections[0].outer) >= 0.0;
        let flip_sides = upwards == outward;
        let mut start = 0usize;
        for length in loops {
            for index in 0..length {
                let a_local = start + index;
                let b_local = start + (index + 1) % length;
                for station in 0..sections.len() - 1 {
                    let a = (station * count + a_local) as u32;
                    let b = (station * count + b_local) as u32;
                    let c = ((station + 1) * count + b_local) as u32;
                    let d = ((station + 1) * count + a_local) as u32;
                    if flip_sides {
                        mesh.push_triangle(a, b, c);
                        mesh.push_triangle(a, c, d);
                    } else {
                        mesh.push_triangle(a, c, b);
                        mesh.push_triangle(a, d, c);
                    }
                }
            }
            start += length;
        }
        let last = ((sections.len() - 1) * count) as u32;
        for triangle in cap_start.chunks_exact(3) {
            let (a, b, c) = (triangle[0], triangle[1], triangle[2]);
            if upwards {
                mesh.push_triangle(a, c, b);
            } else {
                mesh.push_triangle(a, b, c);
            }
        }
        for triangle in cap_end.chunks_exact(3) {
            let (a, b, c) = (triangle[0] + last, triangle[1] + last, triangle[2] + last);
            if upwards {
                mesh.push_triangle(a, b, c);
            } else {
                mesh.push_triangle(a, c, b);
            }
        }

        mesh.remove_degenerate_triangles(ctx.tol.area);
        tessifc_mesh::weld_and_close(&mut mesh, ctx.tol.len);
        mesh.closed = Some(mesh.is_edge_manifold());
        if mesh.closed == Some(true) {
            mesh.fix_orientation();
        }
        Ok(mesh)
    }
}

fn sweep_disk(
    curve: Polyline3,
    radius: f64,
    inner_radius: Option<f64>,
    segments: usize,
    length_tolerance: f64,
    area_tolerance: f64,
) -> Result<Mesh64, GeomError> {
    let (points, closed) = directrix_points(&curve, length_tolerance)?;

    // Point count times segment count is file-driven, so bound it before allocating.
    let planned = points
        .len()
        .checked_mul(segments)
        .and_then(|n| n.checked_mul(if inner_radius.is_some() { 2 } else { 1 }));
    if planned.is_none_or(|n| n > MAX_SWEEP_VERTICES) {
        return Err(GeomError::Degenerate(
            "a swept disk larger than the vertex budget".into(),
        ));
    }

    let stations = stations(&points, closed, length_tolerance, &FrameRule::Transported)?;
    let ring_count = points.len();
    let shell_count = if inner_radius.is_some() { 2 } else { 1 };
    let cap_vertices = if closed || inner_radius.is_some() {
        0
    } else {
        2
    };
    let link_count = if closed { ring_count } else { ring_count - 1 };
    let side_triangles = link_count * segments * 2 * shell_count;
    let cap_triangles = if closed { 0 } else { segments * 2 };
    let mut mesh = Mesh64::with_capacity(
        ring_count * segments * shell_count + cap_vertices,
        (side_triangles + cap_triangles) * 3,
    );

    append_rings(&mut mesh, &stations, radius, segments);
    let inner_base = inner_radius.map(|inner| {
        let base = mesh.positions.len() as u32;
        append_rings(&mut mesh, &stations, inner, segments);
        base
    });

    append_tube_sides(&mut mesh, 0, ring_count, segments, closed, true);
    if let Some(base) = inner_base {
        append_tube_sides(&mut mesh, base, ring_count, segments, closed, false);
    }

    if !closed {
        if let Some(inner_base) = inner_base {
            append_annular_cap(&mut mesh, 0, inner_base, segments, true);
            let outer_end = ((ring_count - 1) * segments) as u32;
            let inner_end = inner_base + outer_end;
            append_annular_cap(&mut mesh, outer_end, inner_end, segments, false);
        } else {
            let start_center = mesh.positions.len() as u32;
            mesh.positions.push(points[0]);
            append_solid_cap(&mut mesh, 0, start_center, segments, true);
            let end_center = mesh.positions.len() as u32;
            mesh.positions.push(points[ring_count - 1]);
            append_solid_cap(
                &mut mesh,
                ((ring_count - 1) * segments) as u32,
                end_center,
                segments,
                false,
            );
        }
    }

    mesh.remove_degenerate_triangles(area_tolerance);
    mesh.closed = Some(mesh.is_edge_manifold());
    if mesh.closed == Some(true) {
        mesh.fix_orientation();
    }
    Ok(mesh)
}

fn stations(
    points: &[DVec3],
    closed: bool,
    tolerance: f64,
    rule: &FrameRule,
) -> Result<Vec<Station>, GeomError> {
    let count = points.len();
    let mut tangents = Vec::with_capacity(count);
    let mut stretches = Vec::with_capacity(count);
    for index in 0..count {
        let before = if index == 0 {
            if closed { count - 1 } else { 0 }
        } else {
            index - 1
        };
        let after = if index + 1 == count {
            if closed { 0 } else { index }
        } else {
            index + 1
        };
        let incoming = (points[index] - points[before]).normalize_or_zero();
        let outgoing = (points[after] - points[index]).normalize_or_zero();
        let at_start = index == 0 && !closed;
        let at_end = index + 1 == count && !closed;
        let tangent = if at_start {
            outgoing
        } else if at_end {
            incoming
        } else {
            (incoming + outgoing).normalize_or_zero()
        };
        let tangent = if tangent.length_squared() <= tolerance * tolerance {
            if outgoing.length_squared() > 0.0 {
                outgoing
            } else {
                incoming
            }
        } else {
            tangent
        };
        if tangent.length_squared() <= f64::EPSILON {
            return Err(GeomError::Degenerate(
                "a swept disk directrix with no usable tangent".into(),
            ));
        }
        // The mitre plane bisects the turn, so a disk perpendicular to either leg meets
        // it in an ellipse stretched by 1 / cos(half turn) within the turn's own plane.
        let stretch = if at_start || at_end {
            (DVec3::ZERO, 1.0)
        } else {
            let cos_half = tangent.dot(incoming).max(1.0 / MAX_MITRE_STRETCH);
            let axis = (incoming - tangent * tangent.dot(incoming)).normalize_or_zero();
            if axis.length_squared() < 0.5 {
                (DVec3::ZERO, 1.0)
            } else {
                (axis, 1.0 / cos_half)
            }
        };
        tangents.push(tangent);
        stretches.push(stretch);
    }
    let frames = frames_for(&tangents, rule)?;
    Ok(points
        .iter()
        .zip(frames)
        .zip(stretches)
        .map(
            |((point, (normal, binormal)), (stretch_axis, stretch))| Station {
                point: *point,
                normal,
                binormal,
                stretch_axis,
                stretch,
            },
        )
        .collect())
}

fn seed_normal(tangent: DVec3) -> DVec3 {
    let reference = if tangent.z.abs() < 0.9 {
        DVec3::Z
    } else {
        DVec3::X
    };
    tangent.cross(reference).normalize()
}

fn transported_frames(tangents: &[DVec3]) -> Vec<(DVec3, DVec3)> {
    let mut normal = seed_normal(tangents[0]);
    let mut previous = tangents[0];
    let mut frames = Vec::with_capacity(tangents.len());
    for &tangent in tangents {
        // Carry the normal by the rotation that takes the previous tangent to this one,
        // so a corner ring is the rotated ring and the mitre stays exact.
        let axis = previous.cross(tangent);
        let sine = axis.length();
        let cosine = previous.dot(tangent);
        if sine > 1e-12 {
            let k = axis / sine;
            normal =
                normal * cosine + k.cross(normal) * sine + k * (k.dot(normal) * (1.0 - cosine));
        } else if cosine < 0.0 {
            normal = seed_normal(tangent);
        }
        let projected = normal - tangent * normal.dot(tangent);
        normal = if projected.length_squared() > 1e-20 {
            projected.normalize()
        } else {
            seed_normal(tangent)
        };
        let binormal = tangent.cross(normal).normalize();
        frames.push((normal, binormal));
        previous = tangent;
    }
    frames
}

fn append_rings(mesh: &mut Mesh64, stations: &[Station], radius: f64, segments: usize) {
    for station in stations {
        for step in 0..segments {
            let angle = std::f64::consts::TAU * step as f64 / segments as f64;
            let offset = (station.normal * angle.cos() + station.binormal * angle.sin()) * radius;
            let along = offset.dot(station.stretch_axis) * (station.stretch - 1.0);
            mesh.positions
                .push(station.point + offset + station.stretch_axis * along);
        }
    }
}

fn append_tube_sides(
    mesh: &mut Mesh64,
    base: u32,
    ring_count: usize,
    segments: usize,
    closed: bool,
    reverse: bool,
) {
    let links = if closed { ring_count } else { ring_count - 1 };
    for ring in 0..links {
        let next_ring = (ring + 1) % ring_count;
        for step in 0..segments {
            let next = (step + 1) % segments;
            let a = base + (ring * segments + step) as u32;
            let b = base + (next_ring * segments + step) as u32;
            let c = base + (next_ring * segments + next) as u32;
            let d = base + (ring * segments + next) as u32;
            if reverse {
                mesh.push_triangle(a, c, b);
                mesh.push_triangle(a, d, c);
            } else {
                mesh.push_triangle(a, b, c);
                mesh.push_triangle(a, c, d);
            }
        }
    }
}

fn append_solid_cap(mesh: &mut Mesh64, ring: u32, center: u32, segments: usize, reverse: bool) {
    for step in 0..segments {
        let a = ring + step as u32;
        let b = ring + ((step + 1) % segments) as u32;
        if reverse {
            mesh.push_triangle(center, b, a);
        } else {
            mesh.push_triangle(center, a, b);
        }
    }
}

fn append_annular_cap(mesh: &mut Mesh64, outer: u32, inner: u32, segments: usize, reverse: bool) {
    for step in 0..segments {
        let next = (step + 1) % segments;
        let a = outer + step as u32;
        let b = outer + next as u32;
        let c = inner + next as u32;
        let d = inner + step as u32;
        if reverse {
            mesh.push_triangle(a, c, b);
            mesh.push_triangle(a, d, c);
        } else {
            mesh.push_triangle(a, b, c);
            mesh.push_triangle(a, c, d);
        }
    }
}

/// Register swept solids.
pub fn register(registry: &mut Registry) {
    registry.register_solid(Box::new(SweptDiskSolid));
    registry.register_solid(Box::new(SweptAreaAlongCurve));
    registry.register_solid(Box::new(SectionedSpine));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polyline(points: &[[f64; 3]]) -> Polyline3 {
        Polyline3 {
            points: points
                .iter()
                .map(|point| DVec3::from_array(*point))
                .collect(),
            closed: false,
        }
    }

    fn sweep(points: &[[f64; 3]]) -> Mesh64 {
        sweep_disk(polyline(points), 0.1, None, 16, 1e-9, 1e-18).unwrap()
    }

    #[test]
    fn a_mitred_bend_keeps_the_volume_and_area_of_its_straight_legs() {
        // The mitre plane passes through the corner, so what it cuts from one leg it
        // adds to the other; an even ring gives the prism the same symmetry.
        let bend = sweep(&[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0]]);
        let straight = sweep(&[[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]]);
        assert_eq!(bend.closed, Some(true));
        let (volume, reference) = (bend.signed_volume().abs(), straight.signed_volume().abs());
        assert!(
            (volume - reference).abs() < 1e-9 * reference,
            "bend {volume}, straight {reference}"
        );
        let (area, reference) = (bend.surface_area(), straight.surface_area());
        assert!(
            (area - reference).abs() < 1e-9 * reference,
            "bend {area}, straight {reference}"
        );
    }

    #[test]
    fn a_mitred_corner_reaches_the_outer_edge_of_the_joint() {
        // A right angle at (2, 0) with radius 0.1 has its outer corner at (2.1, -0.1);
        // an unstretched ring in the bisector plane stops short of it.
        let bend = sweep(&[[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0]]);
        let (low, high) = bend.bounds().unwrap();
        assert!((high.x - 2.1).abs() < 1e-9, "high x {}", high.x);
        assert!((low.y + 0.1).abs() < 1e-9, "low y {}", low.y);
    }

    #[test]
    fn a_reversal_is_swept_without_a_mitre_rather_than_refused() {
        let mesh = sweep(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
        assert!(mesh.positions.iter().all(|point| point.is_finite()));
    }

    #[test]
    fn a_straight_swept_disk_is_a_closed_cylinder() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((0.,0.,2.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCSWEPTDISKSOLID(#3,0.5,$,$,$);\n",
        ));
        let mesh = crate::eval::tests::eval_solid(&model, 4).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let (low, high) = mesh.bounds().unwrap();
        assert!((low.z - 0.0).abs() < 1e-9 && (high.z - 2.0).abs() < 1e-9);
        assert!((low.x + 0.5).abs() < 0.01 && (high.x - 0.5).abs() < 0.01);
        let measured = mesh.signed_volume().abs();
        assert!(
            (measured - std::f64::consts::PI * 0.5).abs() < 0.03,
            "measured {measured}"
        );
    }

    #[test]
    fn an_inner_radius_makes_a_closed_pipe() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((0.,0.,2.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCSWEPTDISKSOLID(#3,0.5,0.3,$,$);\n",
        ));
        let mesh = crate::eval::tests::eval_solid(&model, 4).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let expected = std::f64::consts::PI * (0.5f64.powi(2) - 0.3f64.powi(2)) * 2.0;
        let measured = mesh.signed_volume().abs();
        assert!(
            (measured - expected).abs() < 0.03,
            "measured {measured}, expected {expected}"
        );
    }

    #[test]
    fn a_bent_directrix_keeps_finite_manifold_geometry() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((0.,0.,1.));\n",
            "#3=IFCCARTESIANPOINT((1.,0.,1.));\n",
            "#4=IFCPOLYLINE((#1,#2,#2,#3));\n",
            "#5=IFCSWEPTDISKSOLID(#4,0.1,$,$,$);\n",
        ));
        let mesh = crate::eval::tests::eval_solid(&model, 5).unwrap();
        assert_eq!(mesh.closed, Some(true));
        assert!(mesh.positions.iter().all(|point| point.is_finite()));
    }

    #[test]
    fn a_closed_circular_directrix_makes_a_torus_without_caps() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCCIRCLE(#2,1.);\n",
            "#4=IFCSWEPTDISKSOLID(#3,0.1,$,$,$);\n",
        ));
        let mesh = crate::eval::tests::eval_solid(&model, 4).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let measured = mesh.signed_volume().abs();
        let expected = 2.0 * std::f64::consts::PI.powi(2) * 0.1f64.powi(2);
        assert!(
            (measured - expected).abs() < 0.02,
            "measured {measured}, expected {expected}"
        );
    }

    #[test]
    fn invalid_radii_are_refused_per_element() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((0.,0.,1.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCSWEPTDISKSOLID(#3,0.1,0.2,$,$);\n",
        ));
        assert!(matches!(
            crate::eval::tests::eval_solid(&model, 4),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn a_polyline_trim_keeps_the_middle_segments() {
        // Four unit segments, trimmed 1..3: the middle two, one unit per segment.
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#4=IFCCARTESIANPOINT((3.,0.,0.));\n",
            "#5=IFCCARTESIANPOINT((4.,0.,0.));\n",
            "#6=IFCPOLYLINE((#1,#2,#3,#4,#5));\n",
            "#7=IFCSWEPTDISKSOLID(#6,0.1,$,1.,3.);\n",
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 7);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let (low, high) = mesh.bounds().unwrap();
        assert!((low.x - 1.0).abs() < 1e-9 && (high.x - 3.0).abs() < 1e-9);
        let straight = sweep(&[[1.0, 0.0, 0.0], [3.0, 0.0, 0.0]]);
        let reference = straight.signed_volume().abs();
        assert!((mesh.signed_volume().abs() - reference).abs() < 0.03 * reference);
    }

    #[test]
    fn a_trim_on_a_curve_that_is_not_a_polyline_is_still_reported() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCCIRCLE(#2,1.);\n",
            "#4=IFCSWEPTDISKSOLID(#3,0.1,$,0.,1.);\n",
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 4);
        assert!(mesh.is_ok());
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == codes::SWEEP_PARAMETERS_APPROXIMATED)
        );
    }

    #[test]
    fn a_filleted_corner_shortens_the_centre_line() {
        // Pappus: pi r^2 times the centre line, which loses 2 R tan(a/2) and gains R a.
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,2.,0.));\n",
            "#4=IFCPOLYLINE((#1,#2,#3));\n",
            "#5=IFCSWEPTDISKSOLIDPOLYGONAL(#4,0.1,$,$,$,0.3);\n",
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 5);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        let centre_line = 4.0 - 2.0 * 0.3 + 0.3 * std::f64::consts::FRAC_PI_2;
        let expected = std::f64::consts::PI * 0.01 * centre_line;
        let volume = mesh.signed_volume().abs();
        assert!(
            (volume - expected).abs() < 0.02 * expected,
            "got {volume}, want {expected}"
        );
    }

    #[test]
    fn a_fillet_smaller_than_the_disk_is_reported_and_the_corner_mitred() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,2.,0.));\n",
            "#4=IFCPOLYLINE((#1,#2,#3));\n",
            "#5=IFCSWEPTDISKSOLIDPOLYGONAL(#4,0.1,$,$,$,0.05);\n",
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 5);
        assert!(mesh.is_ok());
        assert!(
            diagnostics
                .iter()
                .any(|item| item.code == codes::SWEEP_FILLET_IGNORED),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_rectangle_swept_with_a_fixed_reference_is_a_box() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.3,0.2);\n",
            "#2=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#4=IFCPOLYLINE((#2,#3));\n",
            "#5=IFCDIRECTION((0.,0.,1.));\n",
            "#6=IFCFIXEDREFERENCESWEPTAREASOLID(#1,$,#4,$,$,#5);\n",
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 6);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        assert!((mesh.signed_volume().abs() - 0.12).abs() < 1e-9);
        assert!((mesh.surface_area() - 2.12).abs() < 1e-9);
        // The profile's x axis follows the reference: XDim 0.3 stands vertical.
        let (low, high) = mesh.bounds().unwrap();
        assert!((high.z - low.z - 0.3).abs() < 1e-9 && (high.y - low.y - 0.2).abs() < 1e-9);
    }

    #[test]
    fn a_fixed_reference_sweep_round_a_corner_is_mitred() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.3,0.2);\n",
            "#2=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#4=IFCCARTESIANPOINT((2.,2.,0.));\n",
            "#5=IFCPOLYLINE((#2,#3,#4));\n",
            "#6=IFCDIRECTION((0.,0.,1.));\n",
            "#7=IFCFIXEDREFERENCESWEPTAREASOLID(#1,$,#5,$,$,#6);\n",
        ));
        let mesh = crate::eval::tests::eval_solid(&model, 7).unwrap();
        assert_eq!(mesh.closed, Some(true));
        // A rectangle is centrally symmetric, so the mitre keeps the straight volume.
        assert!((mesh.signed_volume().abs() - 0.24).abs() < 1e-9);
    }

    #[test]
    fn a_surface_curve_sweep_over_a_plane_takes_the_plane_normal_as_x() {
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.05);\n",
            "#2=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#3=IFCCARTESIANPOINT((2.,0.,0.));\n",
            "#4=IFCPOLYLINE((#2,#3));\n",
            "#5=IFCAXIS2PLACEMENT3D(#2,$,$);\n",
            "#6=IFCPLANE(#5);\n",
            "#7=IFCSURFACECURVESWEPTAREASOLID(#1,$,#4,$,$,#6);\n",
        ));
        let mesh = crate::eval::tests::eval_solid(&model, 7).unwrap();
        assert_eq!(mesh.closed, Some(true));
        assert!((mesh.signed_volume().abs() - 0.04).abs() < 1e-9);
        let (low, high) = mesh.bounds().unwrap();
        assert!((high.z - low.z - 0.4).abs() < 1e-9);
    }

    #[test]
    fn a_sectioned_spine_lofts_its_sections_in_order() {
        // A square that grows to twice its size two metres up: a frustum.
        let model = crate::eval::tests::model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);\n",
            "#2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#4=IFCCARTESIANPOINT((0.,0.,2.));\n",
            "#5=IFCAXIS2PLACEMENT3D(#3,$,$);\n",
            "#6=IFCAXIS2PLACEMENT3D(#4,$,$);\n",
            "#7=IFCPOLYLINE((#3,#4));\n",
            "#8=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#7);\n",
            "#9=IFCCOMPOSITECURVE((#8),.F.);\n",
            "#10=IFCSECTIONEDSPINE(#9,(#1,#2),(#5,#6));\n",
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 10);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        let frustum = 2.0 / 3.0 * (1.0 + 4.0 + 2.0);
        assert!((mesh.signed_volume().abs() - frustum).abs() < 1e-9);
    }
}
