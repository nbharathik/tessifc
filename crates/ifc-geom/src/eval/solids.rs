// SPDX-License-Identifier: Apache-2.0
//! Solids: the things that end up on screen.

use crate::context::EvalCtx;
use crate::error::{GeomError, codes};
use crate::eval::curves::{bspline_edge_between, conic_arc_between, polyline_edge_between};
use crate::placement::{axis2_placement_3d, cartesian_point, direction, transformation_operator};
use crate::registry::{Registry, SolidEvaluator, Surface, SurfaceKind};
use crate::style::Rgba;
use glam::{DMat4, DVec2, DVec3};
use std::sync::Arc;
use tessifc_mesh::{
    Mesh64, Plane, PlaneBasis, Polygon2, heal_t_junctions, triangulate_face, triangulate_polygon,
    weld_and_close,
};
use tessifc_model::Entity;

/// `IfcExtrudedAreaSolid`: a profile pushed along a direction.
///
/// `Depth` runs along `ExtrudedDirection`, not Z, so an oblique extrusion is a shear.
pub struct ExtrudedAreaSolid;

impl SolidEvaluator for ExtrudedAreaSolid {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcExtrudedAreaSolid"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let registry = ctx.registry();
        let swept = item
            .attr("SweptArea")
            .as_entity()
            .ok_or_else(|| GeomError::missing("SweptArea"))?;
        let profile = registry.profile(ctx, swept)?;

        let depth = ctx.units.length(
            item.attr("Depth")
                .as_f64()
                .ok_or_else(|| GeomError::missing("Depth"))?,
        );
        if depth.abs() <= ctx.tol.len {
            return Err(GeomError::Degenerate(format!(
                "an extrusion of depth {depth}"
            )));
        }
        let direction_vector = item
            .attr("ExtrudedDirection")
            .as_entity()
            .and_then(direction)
            .unwrap_or(DVec3::Z);
        let offset = direction_vector * depth;

        if profile.open {
            ctx.diag.warn(
                codes::OPEN_PROFILE_SURFACE,
                item.id(),
                "IfcExtrudedAreaSolid of an open profile; a surface was built",
            );
        }
        let mesh = extrude(&profile, offset, ctx)?;
        let mut mesh = mesh;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// `IfcSurfaceOfLinearExtrusion` and `IfcSurfaceOfRevolution`: swept surfaces.
///
/// The swept curve is a profile, open or closed; the result never has caps.
pub struct SweptSurface;

impl SolidEvaluator for SweptSurface {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSurfaceOfLinearExtrusion", "IfcSurfaceOfRevolution"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let swept = item
            .attr("SweptCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("SweptCurve"))?;
        let profile = ctx.registry().profile(ctx, swept)?;
        let mut mesh = if item.is_a("IfcSurfaceOfLinearExtrusion") {
            let depth = ctx.units.length(
                item.attr("Depth")
                    .as_f64()
                    .ok_or_else(|| GeomError::missing("Depth"))?,
            );
            if !depth.is_finite() || depth.abs() <= ctx.tol.len {
                return Err(GeomError::Degenerate(format!(
                    "a surface extruded through {depth}"
                )));
            }
            let offset = item
                .attr("ExtrudedDirection")
                .as_entity()
                .and_then(direction)
                .unwrap_or(DVec3::Z)
                * depth;
            extrude_sides(&profile, offset, ctx)
        } else {
            let placement = item
                .attr("AxisPosition")
                .as_entity()
                .ok_or_else(|| GeomError::missing("AxisPosition"))?;
            let origin = placement
                .attr("Location")
                .as_entity()
                .and_then(|point| cartesian_point(point, &ctx.units))
                .unwrap_or(DVec3::ZERO);
            let axis = placement
                .attr("Axis")
                .as_entity()
                .and_then(direction)
                .unwrap_or(DVec3::Z)
                .normalize_or_zero();
            if axis == DVec3::ZERO {
                return Err(GeomError::Degenerate(
                    "a revolution about an axis of no length".into(),
                ));
            }
            let reach = profile
                .outer
                .iter()
                .map(|point| {
                    let local = DVec3::new(point.x, point.y, 0.0) - origin;
                    (local - axis * local.dot(axis)).length()
                })
                .fold(0.0_f64, f64::max);
            let steps = ctx.segments_for_radius(reach).max(3);
            revolve_mesh(
                &profile,
                None,
                origin,
                axis,
                std::f64::consts::TAU,
                steps,
                true,
                false,
                ctx,
            )?
        };
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// Sweep a 2D profile along a displacement, capping both ends.
///
/// An open profile gives a ribbon: its sides only, and no caps.
fn extrude(
    profile: &crate::registry::Profile2D,
    offset: DVec3,
    ctx: &EvalCtx<'_>,
) -> Result<Mesh64, GeomError> {
    if profile.open {
        return Ok(extrude_sides(profile, offset, ctx));
    }
    let polygon = profile.to_polygon();
    let cap = triangulate_polygon(&polygon)?;

    // The vertex order earcutr indexes into: outer loop, then each hole.
    let mut flat = profile.outer.clone();
    for hole in &profile.holes {
        if hole.len() >= 3 {
            flat.extend_from_slice(hole);
        }
    }
    let count = flat.len();
    if count
        .checked_mul(2)
        .is_none_or(|n| n > super::sweeps::MAX_SWEEP_VERTICES)
    {
        return Err(GeomError::LimitReached("extrusion vertices".into()));
    }

    let mut mesh = Mesh64::with_capacity(count * 2, cap.len() * 2 + count * 6);
    for point in &flat {
        mesh.positions.push(DVec3::new(point.x, point.y, 0.0));
    }
    for point in &flat {
        mesh.positions
            .push(DVec3::new(point.x, point.y, 0.0) + offset);
    }

    // Side quads follow the loop order; ear clipping normalises the caps, so those
    // follow only the sweep.
    let upwards = offset.z >= 0.0;
    let outward = tessifc_mesh::signed_area(&profile.outer) >= 0.0;
    let flip_sides = upwards == outward;
    let flip_caps = upwards;

    for triangle in cap.chunks_exact(3) {
        let (a, b, c) = (triangle[0], triangle[1], triangle[2]);
        if flip_caps {
            mesh.push_triangle(a, c, b);
        } else {
            mesh.push_triangle(a, b, c);
        }
        let (a, b, c) = (a + count as u32, b + count as u32, c + count as u32);
        if flip_caps {
            mesh.push_triangle(a, b, c);
        } else {
            mesh.push_triangle(a, c, b);
        }
    }

    // Side walls, one quad per edge of every loop.
    let mut start = 0usize;
    let mut loops: Vec<usize> = vec![profile.outer.len()];
    loops.extend(profile.holes.iter().map(Vec::len).filter(|&len| len >= 3));
    for length in loops {
        for index in 0..length {
            let a = (start + index) as u32;
            let b = (start + (index + 1) % length) as u32;
            let c = b + count as u32;
            let d = a + count as u32;
            if flip_sides {
                mesh.push_triangle(a, b, c);
                mesh.push_triangle(a, c, d);
            } else {
                mesh.push_triangle(a, c, b);
                mesh.push_triangle(a, d, c);
            }
        }
        start += length;
    }

    mesh.closed = Some(true);
    mesh.remove_degenerate_triangles(ctx.tol.area);
    // Whether it really is closed is decided by counting edges, not by hope.
    mesh.closed = Some(mesh.is_edge_manifold());
    Ok(mesh)
}

/// The side walls of an extrusion alone: a surface, open or closed around.
fn extrude_sides(profile: &crate::registry::Profile2D, offset: DVec3, ctx: &EvalCtx<'_>) -> Mesh64 {
    let mut loops: Vec<(&[DVec2], bool)> = vec![(&profile.outer, !profile.open)];
    if !profile.open {
        loops.extend(
            profile
                .holes
                .iter()
                .filter(|hole| hole.len() >= 3)
                .map(|hole| (hole.as_slice(), true)),
        );
    }
    let count: usize = loops.iter().map(|(points, _)| points.len()).sum();
    let mut mesh = Mesh64::with_capacity(count * 2, count * 6);
    for (points, _) in &loops {
        for point in *points {
            mesh.positions.push(DVec3::new(point.x, point.y, 0.0));
        }
    }
    for (points, _) in &loops {
        for point in *points {
            mesh.positions
                .push(DVec3::new(point.x, point.y, 0.0) + offset);
        }
    }
    let upwards = offset.z >= 0.0;
    let outward = profile.open || tessifc_mesh::signed_area(&profile.outer) >= 0.0;
    let flip_sides = upwards == outward;
    let mut start = 0usize;
    for (points, closed_loop) in &loops {
        let length = points.len();
        let edges = if *closed_loop {
            length
        } else {
            length.saturating_sub(1)
        };
        for index in 0..edges {
            let a = (start + index) as u32;
            let b = (start + (index + 1) % length) as u32;
            let c = b + count as u32;
            let d = a + count as u32;
            if flip_sides {
                mesh.push_triangle(a, b, c);
                mesh.push_triangle(a, c, d);
            } else {
                mesh.push_triangle(a, c, b);
                mesh.push_triangle(a, d, c);
            }
        }
        start += length;
    }
    mesh.remove_degenerate_triangles(ctx.tol.area);
    mesh.closed = Some(false);
    mesh
}

/// `IfcRevolvedAreaSolid`: a profile spun about an axis.
///
/// A full turn needs no end caps; a partial turn needs both.
pub struct RevolvedAreaSolid;

impl SolidEvaluator for RevolvedAreaSolid {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcRevolvedAreaSolid"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let swept = item
            .attr("SweptArea")
            .as_entity()
            .ok_or_else(|| GeomError::missing("SweptArea"))?;
        let mut profile = ctx.registry().profile(ctx, swept)?;

        // A tapered revolution blends towards its end profile ring by ring.
        let mut end_profile = None;
        if item.is_a("IfcRevolvedAreaSolidTapered") {
            let end = item
                .attr("EndSweptArea")
                .as_entity()
                .ok_or_else(|| GeomError::missing("EndSweptArea"))?;
            let mut end = ctx.registry().profile(ctx, end)?;
            if end.open != profile.open {
                return Err(GeomError::Degenerate(
                    "a tapered revolution between an open and a closed profile".into(),
                ));
            }
            if profile.holes.len() != end.holes.len() {
                profile.holes.clear();
                end.holes.clear();
                ctx.diag.warn(
                    codes::PROFILE_DETAIL_APPROXIMATED,
                    item.id(),
                    "the two profiles have different numbers of holes; revolved without them",
                );
            }
            let mut resampled = false;
            if profile.outer.len() != end.outer.len() {
                let count = profile.outer.len().max(end.outer.len());
                profile.outer = resample_loop(&profile.outer, count);
                end.outer = resample_loop(&end.outer, count);
                resampled = true;
            }
            for (hole, other) in profile.holes.iter_mut().zip(end.holes.iter_mut()) {
                if hole.len() != other.len() {
                    let count = hole.len().max(other.len());
                    *hole = resample_loop(hole, count);
                    *other = resample_loop(other, count);
                    resampled = true;
                }
            }
            if resampled {
                ctx.diag.warn(
                    codes::PROFILE_DETAIL_APPROXIMATED,
                    item.id(),
                    "the two profiles' corners do not correspond; revolved between resampled outlines",
                );
            }
            end_profile = Some(end);
        }

        let angle = ctx.units.angle(
            item.attr("Angle")
                .as_f64()
                .ok_or_else(|| GeomError::missing("Angle"))?,
        );
        if !angle.is_finite() || angle.abs() <= ctx.tol.angle {
            return Err(GeomError::Degenerate(format!(
                "a revolution through {angle} radians"
            )));
        }
        let angle = angle.clamp(-std::f64::consts::TAU, std::f64::consts::TAU);

        let placement = item
            .attr("Axis")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Axis"))?;
        let origin = placement
            .attr("Location")
            .as_entity()
            .and_then(|point| cartesian_point(point, &ctx.units))
            .unwrap_or(DVec3::ZERO);
        let axis = placement
            .attr("Axis")
            .as_entity()
            .and_then(direction)
            .unwrap_or(DVec3::Z)
            .normalize_or_zero();
        if axis == DVec3::ZERO {
            return Err(GeomError::Degenerate(
                "a revolution about an axis of no length".into(),
            ));
        }

        // The profile's reach from the axis decides the step count.
        let reach = profile
            .outer
            .iter()
            .chain(profile.holes.iter().flatten())
            .map(|point| {
                let local = DVec3::new(point.x, point.y, 0.0) - origin;
                (local - axis * local.dot(axis)).length()
            })
            .fold(0.0_f64, f64::max);
        let full = ctx.segments_for_radius(reach).max(3);
        let fraction = angle.abs() / std::f64::consts::TAU;
        let steps = ((full as f64 * fraction).ceil() as u32).max(2);
        let closed = (angle.abs() - std::f64::consts::TAU).abs() <= ctx.tol.angle;

        if profile.open {
            ctx.diag.warn(
                codes::OPEN_PROFILE_SURFACE,
                item.id(),
                "IfcRevolvedAreaSolid of an open profile; a surface was built",
            );
        }
        let mesh = revolve(
            &profile,
            end_profile.as_ref(),
            origin,
            axis,
            angle,
            steps,
            closed,
            ctx,
        )?;
        let mut mesh = mesh;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// Sweep a profile through `angle` about a line, capping the ends unless closed.
#[allow(clippy::too_many_arguments)]
fn revolve(
    profile: &crate::registry::Profile2D,
    end: Option<&crate::registry::Profile2D>,
    origin: DVec3,
    axis: DVec3,
    angle: f64,
    steps: u32,
    closed: bool,
    ctx: &EvalCtx<'_>,
) -> Result<Mesh64, GeomError> {
    revolve_mesh(
        profile,
        end,
        origin,
        axis,
        angle,
        steps,
        closed,
        !profile.open,
        ctx,
    )
}

/// The loops of a profile flattened in triangulation order: outer, then holes.
fn flat_loops(profile: &crate::registry::Profile2D) -> (Vec<DVec2>, Vec<(usize, bool)>) {
    let mut flat = profile.outer.clone();
    let mut loops: Vec<(usize, bool)> = vec![(profile.outer.len(), !profile.open)];
    if !profile.open {
        for hole in &profile.holes {
            if hole.len() >= 3 {
                flat.extend_from_slice(hole);
                loops.push((hole.len(), true));
            }
        }
    }
    (flat, loops)
}

/// The revolution itself; `caps` closes the ends of a partial turn of a closed profile.
#[allow(clippy::too_many_arguments)]
fn revolve_mesh(
    profile: &crate::registry::Profile2D,
    end: Option<&crate::registry::Profile2D>,
    origin: DVec3,
    axis: DVec3,
    angle: f64,
    steps: u32,
    closed: bool,
    caps: bool,
    ctx: &EvalCtx<'_>,
) -> Result<Mesh64, GeomError> {
    // The loops in the order the triangulation indexes them; an open profile is
    // one polyline whose ends stay apart. A tapered end blends towards its own.
    let (flat, loops) = flat_loops(profile);
    let flat_end = end.map(flat_loops).map(|(points, _)| points);
    if flat_end
        .as_ref()
        .is_some_and(|points| points.len() != flat.len())
    {
        return Err(GeomError::Degenerate(
            "a tapered revolution whose profiles do not correspond".into(),
        ));
    }
    let cap = if caps && !closed {
        triangulate_polygon(&profile.to_polygon())?
    } else {
        Vec::new()
    };
    let cap_end = match (caps && !closed, end) {
        (true, Some(end)) => triangulate_polygon(&end.to_polygon())?,
        _ => cap.clone(),
    };
    let count = flat.len();
    let rings = if closed { steps } else { steps + 1 } as usize;
    // The profile count and the ring count both come from the file.
    if count
        .checked_mul(rings)
        .is_none_or(|n| n > super::sweeps::MAX_SWEEP_VERTICES)
    {
        return Err(GeomError::LimitReached("revolution vertices".into()));
    }

    let mut mesh = Mesh64::with_capacity(count * rings, count * rings * 6 + cap.len() * 2);
    for ring in 0..rings {
        let fraction = ring as f64 / steps as f64;
        let rotation = DMat4::from_axis_angle(axis, angle * fraction);
        for (index, point) in flat.iter().enumerate() {
            let blended = match &flat_end {
                Some(end) => *point + (end[index] - *point) * fraction,
                None => *point,
            };
            let local = DVec3::new(blended.x, blended.y, 0.0) - origin;
            mesh.push_vertex(origin + rotation.transform_vector3(local));
        }
    }

    // Which way the surface faces depends on the sweep's sign and on which side
    // of the axis the profile sits; the furthest corner decides the side.
    let radial = DVec3::Z.cross(axis);
    let side = profile
        .outer
        .iter()
        .map(|point| (DVec3::new(point.x, point.y, 0.0) - origin).dot(radial))
        .fold(0.0f64, |far, d| if d.abs() > far.abs() { d } else { far });
    let sweep_positive = (angle >= 0.0) == (side > 0.0);
    let outward = profile.open || tessifc_mesh::signed_area(&profile.outer) >= 0.0;
    let flip = sweep_positive == outward;
    // Ear clipping normalises the cap contour, so only the sweep decides there.
    let cap_flip = sweep_positive;

    let mut start = 0usize;
    for (length, closed_loop) in &loops {
        let edges = if *closed_loop {
            *length
        } else {
            length.saturating_sub(1)
        };
        for index in 0..edges {
            let a = start + index;
            let b = start + (index + 1) % length;
            for ring in 0..steps as usize {
                let next = (ring + 1) % rings;
                let (p, q) = (ring * count, next * count);
                let (v0, v1) = ((p + a) as u32, (p + b) as u32);
                let (v2, v3) = ((q + b) as u32, (q + a) as u32);
                if flip {
                    mesh.push_triangle(v0, v1, v2);
                    mesh.push_triangle(v0, v2, v3);
                } else {
                    mesh.push_triangle(v0, v2, v1);
                    mesh.push_triangle(v0, v3, v2);
                }
            }
        }
        start += length;
    }

    if !cap.is_empty() {
        let last = steps as usize * count;
        for triangle in cap.chunks_exact(3) {
            let (a, b, c) = (triangle[0], triangle[1], triangle[2]);
            if cap_flip {
                mesh.push_triangle(a, c, b);
            } else {
                mesh.push_triangle(a, b, c);
            }
        }
        for triangle in cap_end.chunks_exact(3) {
            let (a, b, c) = (
                triangle[0] + last as u32,
                triangle[1] + last as u32,
                triangle[2] + last as u32,
            );
            if cap_flip {
                mesh.push_triangle(a, b, c);
            } else {
                mesh.push_triangle(a, c, b);
            }
        }
    }

    mesh.remove_degenerate_triangles(ctx.tol.area);
    tessifc_mesh::weld_and_close(&mut mesh, ctx.tol.len);
    mesh.closed = Some(mesh.is_edge_manifold());
    Ok(mesh)
}

/// `IfcExtrudedAreaSolidTapered`: a straight sweep between two profiles.
///
/// Lofted directly when both outlines have the same point count.
pub struct ExtrudedAreaSolidTapered;

impl SolidEvaluator for ExtrudedAreaSolidTapered {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcExtrudedAreaSolidTapered"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let registry = ctx.registry();
        let start = registry.profile(
            ctx,
            item.attr("SweptArea")
                .as_entity()
                .ok_or_else(|| GeomError::missing("SweptArea"))?,
        )?;
        let end = registry.profile(
            ctx,
            item.attr("EndSweptArea")
                .as_entity()
                .ok_or_else(|| GeomError::missing("EndSweptArea"))?,
        )?;
        let depth = ctx.units.length(
            item.attr("Depth")
                .as_f64()
                .ok_or_else(|| GeomError::missing("Depth"))?,
        );
        if depth.abs() <= ctx.tol.len {
            return Err(GeomError::Degenerate(format!(
                "a tapered extrusion of depth {depth}"
            )));
        }
        let offset = item
            .attr("ExtrudedDirection")
            .as_entity()
            .and_then(direction)
            .unwrap_or(DVec3::Z)
            * depth;

        // Loops with different corner counts are resampled by arc length to a common
        // count; an arbitrary outline loses its corners, and the file is told.
        let (mut start, mut end) = (start, end);
        let mut resampled = false;
        if start.outer.len() != end.outer.len() {
            let count = start.outer.len().max(end.outer.len());
            start.outer = resample_loop(&start.outer, count);
            end.outer = resample_loop(&end.outer, count);
            resampled = true;
        }
        if start.holes.len() != end.holes.len() {
            start.holes.clear();
            end.holes.clear();
            ctx.diag.warn(
                codes::PROFILE_DETAIL_APPROXIMATED,
                item.id(),
                "the two profiles have different numbers of holes; lofted without them",
            );
        }
        for (hole, other) in start.holes.iter_mut().zip(end.holes.iter_mut()) {
            if hole.len() != other.len() {
                let count = hole.len().max(other.len());
                *hole = resample_loop(hole, count);
                *other = resample_loop(other, count);
                resampled = true;
            }
        }
        let both_round = ["SweptArea", "EndSweptArea"].iter().all(|name| {
            item.attr(name)
                .as_entity()
                .is_some_and(|profile| profile.is_a("IfcCircleProfileDef"))
        });
        if resampled && !both_round {
            ctx.diag.warn(
                codes::PROFILE_DETAIL_APPROXIMATED,
                item.id(),
                "the two profiles' corners do not correspond; lofted between resampled outlines",
            );
        }

        let mut mesh = loft(&start, &end, offset, ctx)?;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// Redistribute a closed loop's corners uniformly by arc length, `count` of them.
pub(crate) fn resample_loop(points: &[DVec2], count: usize) -> Vec<DVec2> {
    if points.len() < 2 || count < 3 {
        return points.to_vec();
    }
    let mut cumulative = Vec::with_capacity(points.len() + 1);
    let mut total = 0.0;
    cumulative.push(0.0);
    for index in 0..points.len() {
        let next = points[(index + 1) % points.len()];
        total += (next - points[index]).length();
        cumulative.push(total);
    }
    if total <= 0.0 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(count);
    let mut segment = 0;
    for step in 0..count {
        let target = total * step as f64 / count as f64;
        while segment + 1 < points.len() && cumulative[segment + 1] < target {
            segment += 1;
        }
        let from = points[segment];
        let to = points[(segment + 1) % points.len()];
        let span = cumulative[segment + 1] - cumulative[segment];
        let fraction = if span > 0.0 {
            ((target - cumulative[segment]) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.push(from + (to - from) * fraction);
    }
    out
}

/// Join two outlines with the same corner count into a solid.
fn loft(
    start: &crate::registry::Profile2D,
    end: &crate::registry::Profile2D,
    offset: DVec3,
    ctx: &EvalCtx<'_>,
) -> Result<Mesh64, GeomError> {
    let cap = triangulate_polygon(&start.to_polygon())?;
    let mut bottom = start.outer.clone();
    let mut top = end.outer.clone();
    let mut loops: Vec<usize> = vec![start.outer.len()];
    for (hole, other) in start.holes.iter().zip(&end.holes) {
        if hole.len() >= 3 && hole.len() == other.len() {
            bottom.extend_from_slice(hole);
            top.extend_from_slice(other);
            loops.push(hole.len());
        }
    }
    let count = bottom.len();
    if count != top.len() {
        return Err(GeomError::Degenerate(
            "a taper whose two outlines do not correspond".into(),
        ));
    }
    if count
        .checked_mul(2)
        .is_none_or(|n| n > super::sweeps::MAX_SWEEP_VERTICES)
    {
        return Err(GeomError::LimitReached("taper vertices".into()));
    }

    let mut mesh = Mesh64::with_capacity(count * 2, cap.len() * 2 + count * 6);
    for point in &bottom {
        mesh.push_vertex(DVec3::new(point.x, point.y, 0.0));
    }
    for point in &top {
        mesh.push_vertex(DVec3::new(point.x, point.y, 0.0) + offset);
    }

    // Side quads follow the loop order; ear clipping normalises the caps, so those
    // follow only the sweep.
    let upwards = offset.z >= 0.0;
    let outward = tessifc_mesh::signed_area(&start.outer) >= 0.0;
    let flip = upwards == outward;
    let flip_caps = upwards;

    for triangle in cap.chunks_exact(3) {
        let (a, b, c) = (triangle[0], triangle[1], triangle[2]);
        if flip_caps {
            mesh.push_triangle(a, c, b);
        } else {
            mesh.push_triangle(a, b, c);
        }
        let (a, b, c) = (a + count as u32, b + count as u32, c + count as u32);
        if flip_caps {
            mesh.push_triangle(a, b, c);
        } else {
            mesh.push_triangle(a, c, b);
        }
    }

    let mut start_index = 0usize;
    for length in loops {
        for index in 0..length {
            let a = (start_index + index) as u32;
            let b = (start_index + (index + 1) % length) as u32;
            let c = b + count as u32;
            let d = a + count as u32;
            if flip {
                mesh.push_triangle(a, b, c);
                mesh.push_triangle(a, c, d);
            } else {
                mesh.push_triangle(a, c, b);
                mesh.push_triangle(a, d, c);
            }
        }
        start_index += length;
    }

    mesh.remove_degenerate_triangles(ctx.tol.area);
    mesh.closed = Some(mesh.is_edge_manifold());
    Ok(mesh)
}

/// `IfcFacetedBrep` and the variant with voids: a shell of planar faces.
pub struct FacetedBrep;

impl SolidEvaluator for FacetedBrep {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcFacetedBrep",
            "IfcFacetedBrepWithVoids",
            "IfcManifoldSolidBrep",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let mut mesh = Mesh64::new();
        let outer = item
            .attr("Outer")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Outer"))?;
        append_shell(ctx, outer, &mut mesh)?;

        // Voids are inner shells wound the other way; appending them is enough to render.
        if let Some(voids) = item.attr("Voids").as_list() {
            for value in voids {
                if let Some(shell) = value.as_entity() {
                    let _ = append_shell(ctx, shell, &mut mesh);
                }
            }
        }

        if mesh.is_empty() {
            return Err(GeomError::Degenerate(
                "a B-rep with no readable faces".into(),
            ));
        }
        if ctx.settings.weld {
            weld_and_close(&mut mesh, ctx.tol.len);
        }
        // A closed shell with negative volume is inside out.
        if mesh.closed == Some(true) && mesh.fix_orientation() {
            ctx.diag.warn(
                codes::SHELL_REORIENTED,
                item.id(),
                "shell was inside out, flipped",
            );
        }
        Ok(mesh)
    }
}

/// `IfcAdvancedBrep`: faces trimmed on their own surfaces, planar or parametric.
///
/// A face that cannot be trimmed is diagnosed, and a shell that does not close
/// carries `W_NON_MANIFOLD_INPUT` rather than being presented as complete.
pub struct AdvancedBrep;

impl SolidEvaluator for AdvancedBrep {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcAdvancedBrep", "IfcAdvancedBrepWithVoids"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let outer = item
            .attr("Outer")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Outer"))?;
        let mut mesh = Mesh64::new();
        append_advanced_shell(ctx, outer, &mut mesh)?;

        if let Some(voids) = item.attr("Voids").as_list() {
            for value in voids {
                if let Some(shell) = value.as_entity() {
                    append_advanced_shell(ctx, shell, &mut mesh)?;
                }
            }
        }
        if mesh.is_empty() {
            return Err(GeomError::Degenerate(
                "an advanced B-rep with no readable faces".into(),
            ));
        }

        weld_and_close(&mut mesh, ctx.tol.len);
        for _ in 0..4 {
            if mesh.closed == Some(true) || heal_t_junctions(&mut mesh, ctx.tol.len) == 0 {
                break;
            }
            weld_and_close(&mut mesh, ctx.tol.len);
        }
        // Two faces meeting along a rim may each have cut the same corner off it;
        // no edge count can tell those chords from a real defect.
        if mesh.closed != Some(true)
            && tessifc_mesh::split_coincident_edges(&mut mesh, ctx.tol.len) > 0
        {
            weld_and_close(&mut mesh, ctx.tol.len);
        }
        if mesh.closed != Some(true) {
            let (boundary_edges, overused_edges) = non_manifold_edge_counts(&mesh);
            ctx.diag.warn(
                codes::NON_MANIFOLD_INPUT,
                item.id(),
                format!(
                    "advanced B-rep emitted with all faces but a non-manifold shell \
                     ({boundary_edges} boundary edges, {overused_edges} overused edges)"
                ),
            );
        } else {
            mesh.fix_orientation();
        }
        Ok(mesh)
    }
}

fn non_manifold_edge_counts(mesh: &Mesh64) -> (usize, usize) {
    let mut uses = std::collections::HashMap::<(u32, u32), u32>::new();
    for triangle in mesh.indices.chunks_exact(3) {
        for (a, b) in [
            (triangle[0], triangle[1]),
            (triangle[1], triangle[2]),
            (triangle[2], triangle[0]),
        ] {
            *uses.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    (
        uses.values().filter(|uses| **uses == 1).count(),
        uses.values().filter(|uses| **uses > 2).count(),
    )
}

fn append_advanced_shell(
    ctx: &EvalCtx<'_>,
    shell: Entity<'_>,
    mesh: &mut Mesh64,
) -> Result<(), GeomError> {
    let faces = shell
        .attr("CfsFaces")
        .as_list()
        .ok_or_else(|| GeomError::missing("CfsFaces"))?;
    let mut shell_mesh = Mesh64::new();
    for value in faces {
        let face = value
            .as_entity()
            .ok_or_else(|| GeomError::missing("advanced face"))?;
        append_advanced_face(ctx, face, &mut shell_mesh)?;
    }
    mesh.append(&shell_mesh);
    Ok(())
}

fn append_advanced_face(
    ctx: &EvalCtx<'_>,
    face: Entity<'_>,
    mesh: &mut Mesh64,
) -> Result<(), GeomError> {
    let surface = face
        .attr("FaceSurface")
        .as_entity()
        .ok_or_else(|| GeomError::missing("FaceSurface"))?;
    // A plane keeps its own path: the loops are already in a plane, so there is
    // nothing to invert and nothing to refine.
    if !surface.is_a("IfcPlane") {
        let parametric = ctx.registry().surface(ctx, surface)?;
        if !matches!(parametric.kind, SurfaceKind::Plane) {
            // A ruled strip between the boundary curves is exact on a developable patch and
            // agrees with its neighbours; take it when it stays on the surface.
            let mut ruled = Mesh64::new();
            if append_ruled_advanced_face(ctx, face, &mut ruled).is_ok()
                && !ruled.is_empty()
                && strip_follows_surface(&parametric, &ruled, ctx)
            {
                mesh.append(&ruled);
                return Ok(());
            }
            return append_trimmed_surface_face(ctx, face, &parametric, mesh);
        }
    }
    let bounds = face
        .attr("Bounds")
        .as_list()
        .ok_or_else(|| GeomError::missing("Bounds"))?;
    let mut loops = Vec::new();
    for value in bounds {
        let bound = value
            .as_entity()
            .ok_or_else(|| GeomError::missing("advanced face bound"))?;
        let edge_loop = bound
            .attr("Bound")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Bound"))?;
        if !edge_loop.is_a("IfcEdgeLoop") {
            return Err(GeomError::Unsupported(format!(
                "{} on an advanced face",
                edge_loop.class_name()
            )));
        }
        let mut points = straight_edge_loop_points(ctx, edge_loop)?;
        if !bound.attr("Orientation").as_bool().unwrap_or(true) {
            points.reverse();
        }
        loops.push(PlanarFaceLoop {
            points,
            declared_outer: bound.is_a("IfcFaceOuterBound"),
        });
    }
    let arranged = arrange_planar_face_loops(loops, ctx.tol.len)?;
    if arranged.recovered {
        ctx.diag.warn(
            codes::FACE_BOUND_RECOVERED,
            face.id(),
            "enclosing coplanar loop selected because the declared IfcFaceOuterBound was contained by another bound",
        );
    }

    let indices = triangulate_face(&arranged.outer, &arranged.holes)?;
    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(&arranged.outer);
    for hole in &arranged.holes {
        mesh.positions.extend_from_slice(hole);
    }
    // Ear clipping drops a boundary point on a straight run, and the face across
    // the edge still uses it; put such points back or the shell stops closing.
    let mut successor: std::collections::HashMap<u32, u32> = Default::default();
    let mut at = base;
    for ring in std::iter::once(&arranged.outer).chain(arranged.holes.iter()) {
        for step in 0..ring.len() as u32 {
            successor.insert(at + step, at + (step + 1) % ring.len() as u32);
        }
        at += ring.len() as u32;
    }
    let mut triangles: Vec<[u32; 3]> = indices
        .chunks_exact(3)
        .map(|triangle| [base + triangle[0], base + triangle[1], base + triangle[2]])
        .collect();
    if let Some(restored) = tessifc_mesh::restore_boundary_vertices(triangles.clone(), &successor) {
        triangles = restored;
    }
    for triangle in triangles {
        mesh.push_triangle(triangle[0], triangle[1], triangle[2]);
    }
    Ok(())
}

struct PlanarFaceLoop {
    points: Vec<DVec3>,
    declared_outer: bool,
}

struct ArrangedPlanarFaceLoops {
    outer: Vec<DVec3>,
    holes: Vec<Vec<DVec3>>,
    recovered: bool,
}

/// Put the geometrically enclosing loop first, validating the rest as holes.
///
/// Exporters mislabel `IfcFaceOuterBound`; one loop must enclose the rest or the face is refused.
fn arrange_planar_face_loops(
    loops: Vec<PlanarFaceLoop>,
    tolerance: f64,
) -> Result<ArrangedPlanarFaceLoops, GeomError> {
    if loops.is_empty() {
        return Err(GeomError::Degenerate(
            "an advanced face with no usable edge loops".into(),
        ));
    }
    let declared = loops.iter().position(|loop_| loop_.declared_outer);
    let enclosing = loops
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            planar_loop_area(&left.points).total_cmp(&planar_loop_area(&right.points))
        })
        .map(|(index, _)| index)
        .expect("a non-empty loop list has a largest loop");
    let basis = PlaneBasis::from_points(&loops[enclosing].points)
        .ok_or_else(|| GeomError::Degenerate("a planar face with a zero-area bound".into()))?;
    let outer_2d: Vec<DVec2> = loops[enclosing]
        .points
        .iter()
        .map(|point| basis.project(*point))
        .collect();
    for (index, loop_) in loops.iter().enumerate() {
        if index == enclosing {
            continue;
        }
        if loop_
            .points
            .iter()
            .any(|point| (*point - basis.origin).dot(basis.normal).abs() > tolerance)
        {
            return Err(GeomError::Degenerate(
                "an advanced planar face with non-coplanar bounds".into(),
            ));
        }
        if loop_
            .points
            .iter()
            .map(|point| basis.project(*point))
            .any(|point| !point_in_or_on_polygon(point, &outer_2d, tolerance))
        {
            return Err(GeomError::Degenerate(
                "a planar advanced face with disjoint or crossing bounds".into(),
            ));
        }
    }

    let recovered = declared.is_some_and(|index| index != enclosing);
    let mut outer = Vec::new();
    let mut holes = Vec::with_capacity(loops.len().saturating_sub(1));
    for (index, loop_) in loops.into_iter().enumerate() {
        if index == enclosing {
            outer = loop_.points;
        } else {
            holes.push(loop_.points);
        }
    }
    Ok(ArrangedPlanarFaceLoops {
        outer,
        holes,
        recovered,
    })
}

fn planar_loop_area(points: &[DVec3]) -> f64 {
    let mut area_vector = DVec3::ZERO;
    for index in 0..points.len() {
        area_vector += points[index].cross(points[(index + 1) % points.len()]);
    }
    area_vector.length() * 0.5
}

fn point_in_or_on_polygon(point: DVec2, polygon: &[DVec2], tolerance: f64) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let edge = b - a;
        let to_point = point - a;
        let edge_length = edge.length();
        if edge_length > f64::EPSILON
            && edge.perp_dot(to_point).abs() <= tolerance * edge_length
            && to_point.dot(edge) >= -tolerance * edge_length
            && to_point.dot(edge) <= edge.length_squared() + tolerance * edge_length
        {
            return true;
        }
        if (a.y > point.y) != (b.y > point.y) {
            let crossing_x = a.x + (point.y - a.y) * (b.x - a.x) / (b.y - a.y);
            if point.x < crossing_x {
                inside = !inside;
            }
        }
    }
    inside
}

fn straight_edge_loop_points(
    ctx: &EvalCtx<'_>,
    edge_loop: Entity<'_>,
) -> Result<Vec<DVec3>, GeomError> {
    let runs = advanced_edge_loop_runs(ctx, edge_loop)?;
    let mut points = Vec::new();
    for run in runs {
        let points_before_end = run.points.len() - 1;
        for point in run.points.into_iter().take(points_before_end) {
            if points
                .last()
                .is_none_or(|previous: &DVec3| (*previous - point).length() > ctx.tol.len)
            {
                points.push(point);
            }
        }
    }
    if points.len() < 3 {
        return Err(GeomError::Degenerate(
            "an advanced edge loop with fewer than three corners".into(),
        ));
    }
    Ok(points)
}

struct AdvancedEdgeRun {
    points: Vec<DVec3>,
    parameters: Option<(u32, Vec<DVec2>)>,
    curved: bool,
}

fn advanced_edge_loop_runs(
    ctx: &EvalCtx<'_>,
    edge_loop: Entity<'_>,
) -> Result<Vec<AdvancedEdgeRun>, GeomError> {
    let edges = edge_loop
        .attr("EdgeList")
        .as_list()
        .ok_or_else(|| GeomError::missing("EdgeList"))?;
    let mut runs: Vec<AdvancedEdgeRun> = Vec::new();
    let mut previous_end: Option<DVec3> = None;
    for value in edges {
        let oriented = value
            .as_entity()
            .ok_or_else(|| GeomError::missing("oriented edge"))?;
        let edge = oriented
            .attr("EdgeElement")
            .as_entity()
            .ok_or_else(|| GeomError::missing("EdgeElement"))?;
        let forward = oriented.attr("Orientation").as_bool().unwrap_or(true);
        let mut run = advanced_edge_run(ctx, edge)?;
        if !forward {
            run.points.reverse();
            if let Some((_, uv)) = &mut run.parameters {
                uv.reverse();
            }
        }
        if ctx.settings.repair_surface_curves
            && let Some(previous) = runs.last()
        {
            let previous_end = *previous.points.last().expect("edge endpoint");
            let start_matches = previous_end.distance(run.points[0]) <= ctx.tol.len;
            let end_matches =
                previous_end.distance(*run.points.last().expect("edge endpoint")) <= ctx.tol.len;
            let parameter_reversal = match (&previous.parameters, &run.parameters) {
                (Some((prior_surface, prior_uv)), Some((surface, uv)))
                    if prior_surface == surface =>
                {
                    let last = *prior_uv.last().expect("edge parameter endpoint");
                    last.distance(uv[0]) > 1e-9
                        && last.distance(*uv.last().expect("edge parameter endpoint")) <= 1e-9
                }
                _ => false,
            };
            if end_matches && (!start_matches || parameter_reversal) {
                run.points.reverse();
                if let Some((_, uv)) = &mut run.parameters {
                    uv.reverse();
                }
                ctx.diag.warn(
                    codes::EDGE_ORIENTATION_RECOVERED,
                    oriented.id(),
                    "inconsistent edge orientation reversed to restore boundary continuity",
                );
            }
        }
        let start = run.points[0];
        let end = *run.points.last().expect("an edge always has an end");
        if let Some(previous) = previous_end
            && (previous - start).length() > ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "an advanced edge loop whose consecutive edges do not meet".into(),
            ));
        }
        runs.push(run);
        previous_end = Some(end);
    }
    if let (Some(first), Some(last)) = (runs.first(), previous_end)
        && (first.points[0] - last).length() > ctx.tol.len
    {
        return Err(GeomError::Degenerate(
            "an advanced edge loop that is not closed".into(),
        ));
    }
    if runs.is_empty() {
        return Err(GeomError::Degenerate(
            "an advanced edge loop with no edges".into(),
        ));
    }
    Ok(runs)
}

/// Points for one topological edge in `EdgeStart` to `EdgeEnd` order.
fn advanced_edge_run(ctx: &EvalCtx<'_>, edge: Entity<'_>) -> Result<AdvancedEdgeRun, GeomError> {
    let start = advanced_vertex_point(ctx, edge.attr("EdgeStart").as_entity())?;
    let end = advanced_vertex_point(ctx, edge.attr("EdgeEnd").as_entity())?;
    let curve = edge
        .attr("EdgeGeometry")
        .as_entity()
        .ok_or_else(|| GeomError::missing("EdgeGeometry"))?;
    let same_sense = edge.attr("SameSense").as_bool().unwrap_or(true);
    advanced_curve_run(ctx, curve, start, end, same_sense)
}

fn advanced_curve_run(
    ctx: &EvalCtx<'_>,
    curve: Entity<'_>,
    start: DVec3,
    end: DVec3,
    same_sense: bool,
) -> Result<AdvancedEdgeRun, GeomError> {
    if curve.is_a("IfcSurfaceCurve") {
        if let Some(run) = crate::eval::pcurves::edge_run(ctx, curve, start, end, same_sense)? {
            return Ok(AdvancedEdgeRun {
                points: run.points,
                curved: true,
                parameters: Some((run.surface_id, run.uv)),
            });
        }
        let basis = curve
            .attr("Curve3D")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Curve3D"))?;
        return ctx.nested(|| advanced_curve_run(ctx, basis, start, end, same_sense));
    }
    if curve.is_a("IfcLine") {
        return Ok(AdvancedEdgeRun {
            points: vec![start, end],
            curved: false,
            parameters: None,
        });
    }
    if curve.is_a("IfcCircle") || curve.is_a("IfcEllipse") {
        return Ok(AdvancedEdgeRun {
            points: conic_arc_between(ctx, curve, start, end, same_sense)?,
            curved: true,
            parameters: None,
        });
    }
    if curve.is_a("IfcPolyline") {
        let points = polyline_edge_between(ctx, curve, start, end, same_sense)?;
        return Ok(AdvancedEdgeRun {
            curved: points.len() > 2,
            points,
            parameters: None,
        });
    }
    if curve.is_a("IfcBSplineCurveWithKnots") {
        return Ok(AdvancedEdgeRun {
            points: bspline_edge_between(ctx, curve, start, end, same_sense)?,
            curved: true,
            parameters: None,
        });
    }
    Err(GeomError::Unsupported(format!(
        "{} edge geometry on an advanced face",
        curve.class_name()
    )))
}

/// Does every triangle of a ruled strip stay on the surface it claims?
///
/// The strip is built from the boundary curves alone, so on a doubly curved
/// patch its inside sags away from the surface. Measuring that is the only
/// honest way to decide whether it may be used.
fn strip_follows_surface(surface: &Surface, strip: &Mesh64, ctx: &EvalCtx<'_>) -> bool {
    let tolerance = ctx.settings.chord_tolerance_m.max(ctx.tol.len);
    for triangle in strip.indices.chunks_exact(3) {
        let corners: Vec<DVec3> = triangle
            .iter()
            .filter_map(|&index| strip.positions.get(index as usize).copied())
            .collect();
        if corners.len() != 3 {
            return false;
        }
        let centre = (corners[0] + corners[1] + corners[2]) / 3.0;
        let Some(uv) = surface.invert(centre, ctx.tol.len) else {
            return false;
        };
        if (surface.point(uv) - centre).length() > tolerance {
            return false;
        }
    }
    true
}

/// Rounds of refinement. Each halves the edges that are still too coarse.
const MAX_REFINEMENT_ROUNDS: usize = 12;

/// Tessellate a face as the region of its surface that its edges bound: the
/// edges go back into (u, v), the region is triangulated and refined to the
/// chord tolerance there, and the result is lifted back. Boundary points keep
/// the edge evaluators' exact positions so the face welds to its neighbours.
fn append_trimmed_surface_face(
    ctx: &EvalCtx<'_>,
    face: Entity<'_>,
    surface: &Surface,
    mesh: &mut Mesh64,
) -> Result<(), GeomError> {
    let bounds = face
        .attr("Bounds")
        .as_list()
        .ok_or_else(|| GeomError::missing("Bounds"))?;
    let mut loops: Vec<(Vec<DVec3>, Vec<DVec2>, bool)> = Vec::new();
    let mut all_explicit = true;
    let face_surface_id = face
        .attr("FaceSurface")
        .as_entity()
        .map(|entity| entity.id());
    for value in bounds {
        let bound = value
            .as_entity()
            .ok_or_else(|| GeomError::missing("advanced face bound"))?;
        let edge_loop = bound
            .attr("Bound")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Bound"))?;
        if !edge_loop.is_a("IfcEdgeLoop") {
            return Err(GeomError::Unsupported(format!(
                "{} on an advanced face",
                edge_loop.class_name()
            )));
        }
        let mut runs = advanced_edge_loop_runs(ctx, edge_loop)?;
        if !bound.attr("Orientation").as_bool().unwrap_or(true) {
            runs.reverse();
            for run in &mut runs {
                run.points.reverse();
                if let Some((_, uv)) = &mut run.parameters {
                    uv.reverse();
                }
            }
        }
        let explicit = runs.iter().all(|run| {
            run.parameters
                .as_ref()
                .is_some_and(|(id, _)| Some(*id) == face_surface_id)
        });
        if explicit {
            let mut points = Vec::new();
            let mut parameters: Vec<DVec2> = Vec::new();
            for run in &runs {
                let uv = &run.parameters.as_ref().expect("explicit parameters").1;
                for (&point, &parameter) in run.points.iter().zip(uv) {
                    if parameters
                        .last()
                        .is_none_or(|last| last.distance(parameter) > 1e-10)
                    {
                        points.push(point);
                        parameters.push(parameter);
                    }
                }
            }
            if parameters.len() > 1
                && parameters[0].distance(*parameters.last().expect("parameters exist")) <= 1e-10
            {
                parameters.pop();
                points.pop();
            }
            if points.len() < 3 {
                return Err(GeomError::Degenerate(
                    "a parameter-space bound has fewer than three points".into(),
                ));
            }
            loops.push((points, parameters, bound.is_a("IfcFaceOuterBound")));
            continue;
        }
        all_explicit = false;
        let mut points: Vec<DVec3> = Vec::new();
        for run in &runs {
            for point in &run.points {
                if points
                    .last()
                    .is_none_or(|last| last.distance(*point) > ctx.tol.len)
                {
                    points.push(*point);
                }
            }
        }
        while points.len() > 1 && points[0].distance(points[points.len() - 1]) <= ctx.tol.len {
            points.pop();
        }
        if points.len() < 3 {
            continue;
        }
        if face
            .attr("Bounds")
            .as_list()
            .is_some_and(|bounds| bounds.count() == 1)
            && let Some(patch) = super::surface_regions::spherical_cap(
                surface,
                ctx,
                &points,
                face.attr("SameSense").as_bool().unwrap_or(true),
                face.id(),
            )?
        {
            mesh.append(&patch);
            return Ok(());
        }
        // A boundary through a pole cannot be trimmed in (u, v): every longitude
        // meets there and either half of the surface fits the loop.
        if points
            .iter()
            .any(|point| surface.is_singular(*point, ctx.tol.len))
        {
            return Err(GeomError::Unsupported(
                "a face bounded through its surface's own pole".into(),
            ));
        }
        let parameters = invert_loop(surface, &points, ctx.tol.len).ok_or_else(|| {
            GeomError::Degenerate("an advanced face whose edges are not on its own surface".into())
        })?;
        loops.push((points, parameters, bound.is_a("IfcFaceOuterBound")));
    }
    if loops.is_empty() {
        return Err(GeomError::Degenerate(
            "an advanced face with no usable bound".into(),
        ));
    }

    if loops.len() == 1
        && let Some(mut patch) =
            rectangular_parameter_patch(surface, ctx, &loops[0].0, &loops[0].1, face.id())?
    {
        if !face.attr("SameSense").as_bool().unwrap_or(true) {
            patch.flip_winding();
        }
        mesh.append(&patch);
        return Ok(());
    }

    // The enclosing loop is the one of largest area, as in the planar path.
    let areas: Vec<f64> = loops.iter().map(|(_, uv, _)| loop_area(uv).abs()).collect();
    let outer = areas
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(index, _)| index)
        .unwrap_or(0);
    // A face wrapping a whole period is a seam face (an IfcSeamCurve used twice in
    // one loop): the region is the entire band, which no (u, v) polygon can say.
    let (u_period, v_period) = surface.periods();
    for (_, parameters, _) in &loops {
        if all_explicit {
            break;
        }
        for (period, axis) in [(u_period, 0usize), (v_period, 1usize)] {
            let Some(period) = period else { continue };
            let span = parameters
                .iter()
                .map(|at| if axis == 0 { at.x } else { at.y })
                .fold((f64::MAX, f64::MIN), |acc, at| {
                    (acc.0.min(at), acc.1.max(at))
                });
            if span.1 - span.0 >= period - ctx.tol.angle.max(1e-9) {
                return Err(GeomError::Unsupported(
                    "a face that wraps the whole of its own surface".into(),
                ));
            }
        }
    }
    if areas[outer] <= ctx.tol.area {
        return Err(GeomError::Degenerate(
            "an advanced face with no area on its surface".into(),
        ));
    }
    if loops[outer].2 != loops.iter().any(|(_, _, declared)| *declared) || !loops[outer].2 {
        ctx.diag.warn(
            codes::FACE_BOUND_RECOVERED,
            face.id(),
            "enclosing loop on the surface selected because the declared IfcFaceOuterBound was contained by another bound",
        );
    }

    // Holes wind against the outer loop, which is what the triangulator wants.
    let outward = loop_area(&loops[outer].1) > 0.0;
    let mut world: Vec<DVec3> = Vec::new();
    let mut uv: Vec<DVec2> = Vec::new();
    let mut rings: Vec<Vec<u32>> = Vec::new();
    for (index, (points, parameters, _)) in loops.iter().enumerate() {
        let hole = index != outer;
        let forward = (loop_area(parameters) > 0.0) == outward;
        let keep = if hole { !forward } else { forward };
        let order: Vec<usize> = if keep {
            (0..points.len()).collect()
        } else {
            (0..points.len()).rev().collect()
        };
        let mut ring = Vec::with_capacity(order.len());
        for at in order {
            ring.push(world.len() as u32);
            world.push(points[at]);
            uv.push(parameters[at]);
        }
        if hole {
            rings.push(ring);
        } else {
            rings.insert(0, ring);
        }
    }
    if !outward {
        // The triangulator reads a counter-clockwise outer loop; flipping every
        // loop keeps the holes opposite it.
        for ring in &mut rings {
            ring.reverse();
        }
    }

    // A shared rim arc is often a straight run in (u, v); ear clipping would cut a
    // chord off it on both sides of the edge, so hide the run and put it back after.
    let corners: Vec<Vec<u32>> = rings
        .iter()
        .map(|ring| straight_run_corners(ring, &uv))
        .collect();
    if corners.iter().any(|ring| ring.len() < 3) {
        return Err(GeomError::Degenerate(
            "a face bound with no corners on its own surface".into(),
        ));
    }
    let polygon = Polygon2 {
        outer: corners[0].iter().map(|&index| uv[index as usize]).collect(),
        holes: corners[1..]
            .iter()
            .map(|ring| ring.iter().map(|&index| uv[index as usize]).collect())
            .collect(),
    };
    let local = triangulate_polygon(&polygon)
        .map_err(|error| GeomError::Triangulation(format!("a face on its surface: {error}")))?;
    let flat: Vec<u32> = corners.concat();
    let mut triangles: Vec<[u32; 3]> = Vec::with_capacity(local.len() / 3);
    for triangle in local.chunks_exact(3) {
        let corners: Option<Vec<u32>> = triangle
            .iter()
            .map(|&index| flat.get(index as usize).copied())
            .collect();
        let corners = corners.ok_or_else(|| {
            GeomError::Triangulation("a triangle outside the face's own loops".into())
        })?;
        triangles.push([corners[0], corners[1], corners[2]]);
    }

    // Ear clipping drops a boundary point on a straight run that the face across
    // the edge still uses; put them back before anything else touches the patch.
    let mut successor: std::collections::HashMap<u32, u32> = Default::default();
    for ring in &rings {
        for step in 0..ring.len() {
            successor.insert(ring[step], ring[(step + 1) % ring.len()]);
        }
    }
    if let Some(restored) = tessifc_mesh::restore_boundary_vertices(triangles.clone(), &successor) {
        triangles = restored;
    }

    let fixed = world.len();
    if fixed > ctx.settings.max_surface_vertices as usize {
        return Err(GeomError::LimitReached("surface boundary vertices".into()));
    }
    refine_patch(surface, ctx, &mut world, &mut uv, &mut triangles, fixed);

    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(&world);
    for triangle in &triangles {
        let [a, b, c] = *triangle;
        if face.attr("SameSense").as_bool().unwrap_or(true) {
            mesh.push_triangle(base + a, base + b, base + c);
        } else {
            mesh.push_triangle(base + a, base + c, base + b);
        }
    }
    Ok(())
}

/// Grid an explicit rectangular parameter region without long diagonals across a seam.
fn rectangular_parameter_patch(
    surface: &Surface,
    ctx: &EvalCtx<'_>,
    boundary: &[DVec3],
    parameters: &[DVec2],
    face_id: u32,
) -> Result<Option<Mesh64>, GeomError> {
    let low = parameters
        .iter()
        .copied()
        .reduce(DVec2::min)
        .unwrap_or(DVec2::ZERO);
    let high = parameters
        .iter()
        .copied()
        .reduce(DVec2::max)
        .unwrap_or(DVec2::ZERO);
    let span = high - low;
    if span.min_element() <= 0.0 {
        return Ok(None);
    }
    let eps = span.min_element() * 1e-9;
    let on_border = |p: DVec2| {
        (p.x - low.x).abs() < eps
            || (p.x - high.x).abs() < eps
            || (p.y - low.y).abs() < eps
            || (p.y - high.y).abs() < eps
    };
    if parameters.iter().any(|&p| !on_border(p))
        || (loop_area(parameters).abs() - span.x * span.y).abs() > span.x * span.y * 1e-8
    {
        return Ok(None);
    }
    for i in 0..parameters.len() {
        let delta = parameters[(i + 1) % parameters.len()] - parameters[i];
        if delta.x.abs() > eps && delta.y.abs() > eps {
            return Ok(None);
        }
    }
    let sorted = |axis: usize| {
        let mut values: Vec<f64> = parameters.iter().map(|p| p[axis]).collect();
        values.sort_by(f64::total_cmp);
        values.dedup_by(|a, b| (*a - *b).abs() < eps);
        values
    };
    let mut us = sorted(0);
    let mut vs = sorted(1);
    let limit = ctx.settings.max_surface_vertices as usize;
    if us.len().saturating_mul(vs.len()) > limit {
        return Err(GeomError::LimitReached("surface boundary grid".into()));
    }
    let mut met = false;
    for _ in 0..12 {
        let mut split_u = vec![false; us.len() - 1];
        let mut split_v = vec![false; vs.len() - 1];
        for (i, u) in us.windows(2).enumerate() {
            for (j, v) in vs.windows(2).enumerate() {
                let a = surface.point(DVec2::new(u[0], v[0]));
                let b = surface.point(DVec2::new(u[1], v[0]));
                let c = surface.point(DVec2::new(u[1], v[1]));
                let d = surface.point(DVec2::new(u[0], v[1]));
                if !a.is_finite() || !b.is_finite() || !c.is_finite() || !d.is_finite() {
                    return Err(GeomError::Degenerate("non-finite surface grid".into()));
                }
                let error_u = surface
                    .point(DVec2::new((u[0] + u[1]) * 0.5, v[0]))
                    .distance((a + b) * 0.5)
                    .max(
                        surface
                            .point(DVec2::new((u[0] + u[1]) * 0.5, v[1]))
                            .distance((c + d) * 0.5),
                    );
                let error_v = surface
                    .point(DVec2::new(u[0], (v[0] + v[1]) * 0.5))
                    .distance((a + d) * 0.5)
                    .max(
                        surface
                            .point(DVec2::new(u[1], (v[0] + v[1]) * 0.5))
                            .distance((b + c) * 0.5),
                    );
                let centre = surface.point(DVec2::new((u[0] + u[1]) * 0.5, (v[0] + v[1]) * 0.5));
                let error = centre.distance((a + c) * 0.5).max(error_u).max(error_v);
                if error > ctx.settings.chord_tolerance_m {
                    split_u[i] |= error_u >= error_v;
                    split_v[j] |= error_v >= error_u;
                }
            }
        }
        let nu = split_u.iter().filter(|v| **v).count();
        let nv = split_v.iter().filter(|v| **v).count();
        if nu + nv == 0 {
            met = true;
            break;
        }
        if (us.len() + nu).saturating_mul(vs.len() + nv) > limit {
            break;
        }
        let split = |values: &mut Vec<f64>, flags: Vec<bool>| {
            let mut next = Vec::with_capacity(values.len() + flags.iter().filter(|v| **v).count());
            for (i, pair) in values.windows(2).enumerate() {
                next.push(pair[0]);
                if flags[i] {
                    next.push((pair[0] + pair[1]) * 0.5);
                }
            }
            next.push(*values.last().expect("grid axis exists"));
            *values = next;
        };
        split(&mut us, split_u);
        split(&mut vs, split_v);
    }
    if us.len().saturating_mul(vs.len()) > limit {
        return Err(GeomError::LimitReached("surface boundary grid".into()));
    }
    if !met {
        ctx.diag.warn(
            codes::TESSELLATION_TOLERANCE_UNMET,
            face_id,
            "surface grid reached its refinement budget before meeting chord tolerance",
        );
    }
    let mut mesh = Mesh64::new();
    for &v in &vs {
        for &u in &us {
            let uv = DVec2::new(u, v);
            let mut point = surface.point(uv);
            if on_border(uv) {
                for i in 0..parameters.len() {
                    let next = (i + 1) % parameters.len();
                    let delta = parameters[next] - parameters[i];
                    if delta.length_squared() == 0.0 {
                        continue;
                    }
                    let t =
                        ((uv - parameters[i]).dot(delta) / delta.length_squared()).clamp(0.0, 1.0);
                    if uv.distance(parameters[i].lerp(parameters[next], t)) < eps {
                        point = boundary[i].lerp(boundary[next], t);
                        break;
                    }
                }
            }
            mesh.positions.push(point);
        }
    }
    let stride = us.len() as u32;
    for j in 0..vs.len() - 1 {
        for i in 0..us.len() - 1 {
            let a = (j * us.len() + i) as u32;
            let b = a + 1;
            let d = a + stride;
            let c = d + 1;
            mesh.push_triangle(a, b, c);
            mesh.push_triangle(a, c, d);
        }
    }
    mesh.remove_degenerate_triangles(ctx.tol.area);
    Ok(Some(mesh))
}

/// The corners of a loop: the points that are not on a straight run in (u, v).
///
/// Always at least the whole ring when nothing is straight, and never fewer
/// than three points while the ring encloses anything.
fn straight_run_corners(ring: &[u32], uv: &[DVec2]) -> Vec<u32> {
    if ring.len() < 4 {
        return ring.to_vec();
    }
    let mut out = Vec::with_capacity(ring.len());
    for step in 0..ring.len() {
        let previous = uv[ring[(step + ring.len() - 1) % ring.len()] as usize];
        let here = uv[ring[step] as usize];
        let next = uv[ring[(step + 1) % ring.len()] as usize];
        let (into, away) = (here - previous, next - here);
        let turn = into.x * away.y - into.y * away.x;
        if turn.abs() > 1e-12 * into.length() * away.length() {
            out.push(ring[step]);
        }
    }
    if out.len() < 3 { ring.to_vec() } else { out }
}

/// Twice the signed area of a loop in parameter space.
fn loop_area(points: &[DVec2]) -> f64 {
    let mut area = 0.0;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }
    area * 0.5
}

/// Invert a loop of points onto a surface, following it across any seam.
///
/// A periodic parameter is unwrapped as the loop is walked, so a patch that
/// straddles the seam comes back as one region rather than two.
fn invert_loop(surface: &Surface, points: &[DVec3], tol: f64) -> Option<Vec<DVec2>> {
    let (u_period, v_period) = surface.periods();
    let mut out: Vec<DVec2> = Vec::with_capacity(points.len());
    for point in points {
        let mut uv = surface.invert(*point, tol)?;
        if let Some(previous) = out.last() {
            uv.x = unwrap(previous.x, uv.x, u_period);
            uv.y = unwrap(previous.y, uv.y, v_period);
        }
        out.push(uv);
    }
    // A closed loop that came back a whole period away from where it started
    // has been walked the wrong way round the seam.
    if let (Some(first), Some(last)) = (out.first(), out.last()) {
        let closing = *first - *last;
        if let Some(period) = u_period
            && closing.x.abs() > period * 0.75
            && closing.x.abs() < period * 1.25
        {
            // The loop goes all the way round: that is a seam, not an error.
        }
    }
    Some(out)
}

/// Move `value` by whole periods to sit nearest `previous`.
fn unwrap(previous: f64, value: f64, period: Option<f64>) -> f64 {
    let Some(period) = period else {
        return value;
    };
    if period <= 0.0 || !period.is_finite() {
        return value;
    }
    let steps = ((previous - value) / period).round();
    value + steps * period
}

/// Add points inside a patch until it follows its surface within tolerance.
///
/// Edges are marked and split as a set, so both triangles sharing an edge see
/// the same split and the patch stays conforming. Boundary edges are never
/// marked, which is what keeps the face welded to the ones beside it.
fn refine_patch(
    surface: &Surface,
    ctx: &EvalCtx<'_>,
    world: &mut Vec<DVec3>,
    uv: &mut Vec<DVec2>,
    triangles: &mut Vec<[u32; 3]>,
    fixed: usize,
) {
    let tolerance = ctx.settings.chord_tolerance_m.max(ctx.tol.len);
    let boundary: std::collections::HashSet<(u32, u32)> = {
        let mut directed: std::collections::HashSet<(u32, u32)> = Default::default();
        for triangle in triangles.iter() {
            for step in 0..3 {
                directed.insert((triangle[step], triangle[(step + 1) % 3]));
            }
        }
        directed
            .iter()
            .filter(|(from, to)| !directed.contains(&(*to, *from)))
            .map(|(from, to)| (*from.min(to), *from.max(to)))
            .collect()
    };

    for _ in 0..MAX_REFINEMENT_ROUNDS {
        let mut marked: std::collections::HashSet<(u32, u32)> = Default::default();
        let mut centres: Vec<usize> = Vec::new();
        for (index, triangle) in triangles.iter().enumerate() {
            if deviation(surface, world, uv, triangle, &boundary) <= tolerance {
                continue;
            }
            let mut longest: Option<(f64, (u32, u32))> = None;
            for step in 0..3 {
                let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
                let key = (a.min(b), a.max(b));
                if boundary.contains(&key) {
                    continue;
                }
                let length = (uv[a as usize] - uv[b as usize]).length();
                if longest.is_none_or(|(known, _)| length > known) {
                    longest = Some((length, key));
                }
            }
            match longest {
                Some((_, key)) => {
                    marked.insert(key);
                }
                // Every edge is on the boundary: the only way in is the middle.
                None => centres.push(index),
            }
        }
        if marked.is_empty() && centres.is_empty() {
            break;
        }
        if world.len() + marked.len() + centres.len() > ctx.settings.max_surface_vertices as usize {
            break;
        }

        let mut middle: std::collections::HashMap<(u32, u32), u32> = Default::default();
        let mut keys: Vec<(u32, u32)> = marked.into_iter().collect();
        keys.sort_unstable();
        for key in keys {
            let at = (uv[key.0 as usize] + uv[key.1 as usize]) * 0.5;
            middle.insert(key, world.len() as u32);
            uv.push(at);
            world.push(surface.point(at));
        }
        let mut centre_of: std::collections::HashMap<usize, u32> = Default::default();
        for &index in &centres {
            let triangle = triangles[index];
            let at =
                (uv[triangle[0] as usize] + uv[triangle[1] as usize] + uv[triangle[2] as usize])
                    / 3.0;
            centre_of.insert(index, world.len() as u32);
            uv.push(at);
            world.push(surface.point(at));
        }

        let mut next: Vec<[u32; 3]> = Vec::with_capacity(triangles.len() * 2);
        for (index, triangle) in triangles.iter().enumerate() {
            if let Some(&centre) = centre_of.get(&index) {
                for step in 0..3 {
                    next.push([triangle[step], triangle[(step + 1) % 3], centre]);
                }
                continue;
            }
            let cut: Vec<Option<u32>> = (0..3)
                .map(|step| {
                    let (a, b) = (triangle[step], triangle[(step + 1) % 3]);
                    middle.get(&(a.min(b), a.max(b))).copied()
                })
                .collect();
            match cut.iter().filter(|entry| entry.is_some()).count() {
                0 => next.push(*triangle),
                1 => {
                    let step = cut.iter().position(Option::is_some).unwrap();
                    let m = cut[step].unwrap();
                    let (a, b, c) = (
                        triangle[step],
                        triangle[(step + 1) % 3],
                        triangle[(step + 2) % 3],
                    );
                    next.push([a, m, c]);
                    next.push([m, b, c]);
                }
                2 => {
                    // The uncut edge decides which corner keeps its own triangle.
                    let whole = cut.iter().position(Option::is_none).unwrap();
                    let (a, b, c) = (
                        triangle[whole],
                        triangle[(whole + 1) % 3],
                        triangle[(whole + 2) % 3],
                    );
                    let bc = cut[(whole + 1) % 3].unwrap();
                    let ca = cut[(whole + 2) % 3].unwrap();
                    next.push([a, b, bc]);
                    next.push([a, bc, ca]);
                    next.push([ca, bc, c]);
                }
                _ => {
                    let (ab, bc, ca) = (cut[0].unwrap(), cut[1].unwrap(), cut[2].unwrap());
                    next.push([triangle[0], ab, ca]);
                    next.push([ab, triangle[1], bc]);
                    next.push([ca, bc, triangle[2]]);
                    next.push([ab, bc, ca]);
                }
            }
        }
        *triangles = next;
    }
    if triangles
        .iter()
        .any(|triangle| deviation(surface, world, uv, triangle, &boundary) > tolerance)
    {
        ctx.diag.warn(
            codes::TESSELLATION_TOLERANCE_UNMET,
            0,
            "surface patch reached its refinement budget before meeting chord tolerance",
        );
    }
    let _ = fixed;
}

/// How far a triangle strays from the surface it is meant to lie on.
///
/// A boundary edge is not measured. Its points came from the edge curve rather
/// than from the surface, so it may sit a little off and there is nothing to be
/// done about it: splitting it would move the edge away from the face beside
/// this one. Measuring it would mean refining for ever.
fn deviation(
    surface: &Surface,
    world: &[DVec3],
    uv: &[DVec2],
    triangle: &[u32; 3],
    boundary: &std::collections::HashSet<(u32, u32)>,
) -> f64 {
    let corners = [
        world[triangle[0] as usize],
        world[triangle[1] as usize],
        world[triangle[2] as usize],
    ];
    let parameters = [
        uv[triangle[0] as usize],
        uv[triangle[1] as usize],
        uv[triangle[2] as usize],
    ];
    let mut worst: f64 = 0.0;
    for step in 0..3 {
        let next = (step + 1) % 3;
        let (a, b) = (triangle[step], triangle[next]);
        if boundary.contains(&(a.min(b), a.max(b))) {
            continue;
        }
        let middle = (parameters[step] + parameters[next]) * 0.5;
        let chord = (corners[step] + corners[next]) * 0.5;
        worst = worst.max((surface.point(middle) - chord).length());
    }
    let centre = (parameters[0] + parameters[1] + parameters[2]) / 3.0;
    let flat = (corners[0] + corners[1] + corners[2]) / 3.0;
    worst.max((surface.point(centre) - flat).length())
}

/// Tessellate a ruled patch between two conic boundary edges.
///
/// Following the actual edge samples keeps adjacent faces watertight.
fn append_ruled_advanced_face(
    ctx: &EvalCtx<'_>,
    face: Entity<'_>,
    mesh: &mut Mesh64,
) -> Result<(), GeomError> {
    let mut bounds = face
        .attr("Bounds")
        .as_list()
        .ok_or_else(|| GeomError::missing("Bounds"))?;
    let bound = bounds
        .next()
        .and_then(|value| value.as_entity())
        .ok_or_else(|| GeomError::missing("advanced face bound"))?;
    if bounds.next().is_some() {
        return Err(GeomError::Unsupported(
            "a ruled advanced face with holes or multiple bounds".into(),
        ));
    }
    let edge_loop = bound
        .attr("Bound")
        .as_entity()
        .ok_or_else(|| GeomError::missing("Bound"))?;
    if !edge_loop.is_a("IfcEdgeLoop") {
        return Err(GeomError::Unsupported(format!(
            "{} on a ruled advanced face",
            edge_loop.class_name()
        )));
    }
    let mut runs = advanced_edge_loop_runs(ctx, edge_loop)?;
    if !bound.attr("Orientation").as_bool().unwrap_or(true) {
        runs.reverse();
        for run in &mut runs {
            run.points.reverse();
            if let Some((_, uv)) = &mut run.parameters {
                uv.reverse();
            }
        }
    }
    let curved: Vec<&AdvancedEdgeRun> = runs.iter().filter(|run| run.curved).collect();
    if curved.len() != 2
        || curved.iter().any(|run| run.points.len() < 3)
        || runs
            .iter()
            .filter(|run| !run.curved)
            .any(|run| run.points.len() != 2)
    {
        return Err(GeomError::Unsupported(
            "a ruled advanced face not bounded by two curved edges and straight connectors".into(),
        ));
    }

    let a = &curved[0].points;
    let b: Vec<DVec3> = curved[1].points.iter().copied().rev().collect();
    let base_a = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(a);
    let base_b = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(&b);

    // Zip the two arcs by normalised progress: a quad when both advance, else one triangle.
    let mut ia = 0usize;
    let mut ib = 0usize;
    while ia + 1 < a.len() || ib + 1 < b.len() {
        let next_a = if ia + 1 < a.len() {
            (ia + 1) as f64 / (a.len() - 1) as f64
        } else {
            f64::INFINITY
        };
        let next_b = if ib + 1 < b.len() {
            (ib + 1) as f64 / (b.len() - 1) as f64
        } else {
            f64::INFINITY
        };
        if (next_a - next_b).abs() <= 1e-12 {
            mesh.push_triangle(
                base_a + ia as u32,
                base_a + ia as u32 + 1,
                base_b + ib as u32 + 1,
            );
            mesh.push_triangle(
                base_a + ia as u32,
                base_b + ib as u32 + 1,
                base_b + ib as u32,
            );
            ia += 1;
            ib += 1;
        } else if next_a < next_b {
            mesh.push_triangle(
                base_a + ia as u32,
                base_a + ia as u32 + 1,
                base_b + ib as u32,
            );
            ia += 1;
        } else {
            mesh.push_triangle(
                base_a + ia as u32,
                base_b + ib as u32 + 1,
                base_b + ib as u32,
            );
            ib += 1;
        }
    }
    Ok(())
}

fn advanced_vertex_point(
    ctx: &EvalCtx<'_>,
    vertex: Option<Entity<'_>>,
) -> Result<DVec3, GeomError> {
    let vertex = vertex.ok_or_else(|| GeomError::missing("edge vertex"))?;
    let geometry = vertex
        .attr("VertexGeometry")
        .as_entity()
        .ok_or_else(|| GeomError::missing("VertexGeometry"))?;
    cartesian_point(geometry, &ctx.units)
        .ok_or_else(|| GeomError::missing("CartesianPoint vertex geometry"))
}

/// Append every face of an `IfcClosedShell` or `IfcOpenShell`.
fn append_shell(ctx: &EvalCtx<'_>, shell: Entity<'_>, mesh: &mut Mesh64) -> Result<(), GeomError> {
    let faces = shell
        .attr("CfsFaces")
        .as_list()
        .ok_or_else(|| GeomError::missing("CfsFaces"))?;
    for value in faces {
        let Some(face) = value.as_entity() else {
            continue;
        };
        if let Err(error) = append_face(ctx, face, mesh) {
            ctx.diag.warn(error.code(), face.id(), error.to_string());
        }
    }
    Ok(())
}

/// Append one `IfcFace`, with its outer bound and any inner bounds as holes.
fn append_face(ctx: &EvalCtx<'_>, face: Entity<'_>, mesh: &mut Mesh64) -> Result<(), GeomError> {
    if face.is_a("IfcAdvancedFace") {
        return append_advanced_face(ctx, face, mesh);
    }
    let bounds = face
        .attr("Bounds")
        .as_list()
        .ok_or_else(|| GeomError::missing("Bounds"))?;
    let mut outer: Vec<DVec3> = Vec::new();
    let mut holes: Vec<Vec<DVec3>> = Vec::new();

    for value in bounds {
        let Some(bound) = value.as_entity() else {
            continue;
        };
        let Some(polygon) = bound.attr("Bound").as_entity() else {
            continue;
        };
        let mut points = polygon_points(ctx, polygon);
        if points.len() < 3 {
            continue;
        }
        // Orientation false means the loop runs the other way round.
        if !bound.attr("Orientation").as_bool().unwrap_or(true) {
            points.reverse();
        }
        if bound.is_a("IfcFaceOuterBound") || outer.is_empty() {
            outer = points;
        } else {
            holes.push(points);
        }
    }

    if outer.len() < 3 {
        return Err(GeomError::Degenerate(
            "a face with no usable outer bound".into(),
        ));
    }

    let indices = triangulate_face(&outer, &holes)?;
    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(&outer);
    for hole in &holes {
        mesh.positions.extend_from_slice(hole);
    }
    for triangle in indices.chunks_exact(3) {
        mesh.push_triangle(base + triangle[0], base + triangle[1], base + triangle[2]);
    }
    Ok(())
}

/// Points of an `IfcPolyLoop`.
fn polygon_points(ctx: &EvalCtx<'_>, polygon: Entity<'_>) -> Vec<DVec3> {
    let mut points = Vec::new();
    if let Some(list) = polygon.attr("Polygon").as_list() {
        for value in list {
            if let Some(entity) = value.as_entity()
                && let Some(point) = cartesian_point(entity, &ctx.units)
            {
                points.push(point);
            }
        }
    }
    points
}

/// The surface-model family: a bag of faces that is not claimed to be a solid.
///
/// All three classes open up to sets of `IfcFace`.
pub struct SurfaceModel;

impl SolidEvaluator for SurfaceModel {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcFaceBasedSurfaceModel",
            "IfcShellBasedSurfaceModel",
            "IfcConnectedFaceSet",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let mut mesh = Mesh64::new();

        if item.is_a("IfcConnectedFaceSet") {
            append_shell(ctx, item, &mut mesh)?;
        } else {
            // FbsmFaces on one, SbsmBoundary on the other; whichever is present.
            let shells = item
                .attr("FbsmFaces")
                .as_list()
                .or_else(|| item.attr("SbsmBoundary").as_list())
                .ok_or_else(|| GeomError::missing("FbsmFaces or SbsmBoundary"))?;
            for value in shells {
                let Some(shell) = value.as_entity() else {
                    continue;
                };
                if let Err(error) = append_shell(ctx, shell, &mut mesh) {
                    ctx.diag.warn(error.code(), shell.id(), error.to_string());
                }
            }
        }

        if mesh.is_empty() {
            return Err(GeomError::Degenerate(
                "a surface model with no readable faces".into(),
            ));
        }
        if ctx.settings.weld {
            weld_and_close(&mut mesh, ctx.tol.len);
        }
        // A closed surface model with negative volume is inside out, like a B-rep.
        if mesh.closed == Some(true) && mesh.fix_orientation() {
            ctx.diag.warn(
                codes::SHELL_REORIENTED,
                item.id(),
                "shell was inside out, flipped",
            );
        }
        Ok(mesh)
    }
}

/// `IfcBooleanResult` and `IfcBooleanClippingResult`.
///
/// Half-space clips and convex differences are exact; anything else emits the body un-cut.
pub struct Boolean;

impl SolidEvaluator for Boolean {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcBooleanResult", "IfcBooleanClippingResult"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        // A result several operands share is built once; a tree reusing its
        // leaves would otherwise cost exponential work.
        if let Some(mesh) = ctx.cached_boolean(item.id()) {
            return Ok(mesh);
        }
        let mesh = evaluate_boolean(ctx, item)?;
        ctx.cache_boolean(item.id(), &mesh);
        Ok(mesh)
    }
}

/// One boolean result, uncached.
fn evaluate_boolean(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
    let registry = ctx.registry();
    let first = item
        .attr("FirstOperand")
        .as_entity()
        .ok_or_else(|| GeomError::missing("FirstOperand"))?;
    let operator = item
        .attr("Operator")
        .as_text()
        .map(|text| String::from_utf8_lossy(text.raw()).into_owned())
        .unwrap_or_else(|| "DIFFERENCE".into());
    let second = item.attr("SecondOperand").as_entity();
    // A difference opens its whole chain below, so a nested chain is walked
    // once; the other operators start from the first operand.
    let mut mesh = if operator == "DIFFERENCE" && second.is_some() {
        Mesh64::new()
    } else {
        registry.solid(ctx, first)?
    };

    let watch = crate::context::Stopwatch::start();
    let complaint = match (operator.as_str(), second) {
        ("UNION", Some(other)) => match registry.solid(ctx, other) {
            Ok(part) => {
                match tessifc_mesh::union_general_or_reason(&mesh, &part, ctx.tol.len) {
                    Ok(joined) => {
                        mesh = joined;
                        None
                    }
                    // Two shells side by side: right only where they do not overlap.
                    Err(why) => {
                        mesh.append(&part);
                        Some(why)
                    }
                }
            }
            Err(error) => Some(error.to_string()),
        },
        ("INTERSECTION", Some(other)) => match registry.solid(ctx, other) {
            Ok(part) => {
                match tessifc_mesh::intersection_general_or_reason(&mesh, &part, ctx.tol.len) {
                    Ok(common) => {
                        mesh = common;
                        None
                    }
                    Err(why) => Some(why),
                }
            }
            Err(error) => Some(error.to_string()),
        },
        ("DIFFERENCE", Some(_)) => match evaluate_difference(ctx, registry, item) {
            Some(difference) => {
                mesh = difference.body;
                let mut reason = difference.refused.join("; ");
                if !difference.cutters.is_empty() {
                    match tessifc_mesh::difference_convex_many(
                        &mesh,
                        &difference.cutters,
                        ctx.tol.len,
                    )
                    .or_else(|| {
                        tessifc_mesh::difference_extrusion_many(
                            &mesh,
                            &difference.cutters,
                            ctx.tol.len,
                        )
                    })
                    .ok_or_else(String::new)
                    .or_else(|_| {
                        tessifc_mesh::difference_prismatic_many_or_reason(
                            &mesh,
                            &difference.cutters,
                            ctx.tol.len,
                        )
                    }) {
                        Ok(result) => mesh = result,
                        Err(why) => {
                            if !reason.is_empty() {
                                reason.push_str("; ");
                            }
                            reason.push_str(&why);
                        }
                    }
                }
                (!reason.is_empty()).then_some(reason)
            }
            None => {
                mesh = registry.solid(ctx, first)?;
                Some("the first operand could not be evaluated".into())
            }
        },
        (other_operator, _) => Some(format!("{other_operator} is not supported")),
    };
    ctx.time.add_boolean(watch.ms());

    let inherited = ctx.boolean_outcome(first.id()).merge(
        second
            .map(|other| ctx.boolean_outcome(other.id()))
            .unwrap_or_default(),
    );
    let own = if complaint.is_some() {
        crate::BooleanStatus::Refused
    } else {
        crate::BooleanStatus::Exact
    };
    ctx.record_boolean(item.id(), inherited.merge(own));
    if let Some(reason) = complaint {
        ctx.diag.warn(
            codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE,
            item.id(),
            format!("{operator} not performed in clip-only mode ({reason}); body emitted un-cut"),
        );
    }
    Ok(mesh)
}

/// The clipping plane of an unbounded `IfcHalfSpaceSolid`, negative side as material.
///
/// The bounded subtypes are refused here and fall through to the convex path.
fn half_space_plane(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Option<Plane> {
    if !item.is_a("IfcHalfSpaceSolid")
        || item.is_a("IfcPolygonalBoundedHalfSpace")
        || item.is_a("IfcBoxedHalfSpace")
    {
        return None;
    }
    base_plane(ctx, item)
}

/// A chain of differences, opened up.
///
/// `((A - B1) - B2)` is `A - (B1 u B2)`, and only the second form keeps `A` convex.
pub struct Difference {
    /// The innermost operand with every unbounded half space clipped off, still convex.
    pub body: Mesh64,
    /// The bounded solids still to be subtracted.
    pub cutters: Vec<Mesh64>,
    /// Cutters that could not be turned into solids, with the reason.
    pub refused: Vec<String>,
}

/// The longest first-operand chain a difference is opened along.
const MAX_DIFFERENCE_CHAIN: usize = 4096;

/// Open up a difference chain, or `None` if `item` is not one.
pub fn evaluate_difference(
    ctx: &EvalCtx<'_>,
    registry: &Registry,
    item: Entity<'_>,
) -> Option<Difference> {
    if !is_difference(item) {
        return None;
    }
    // Walk down the first operands, collecting the second ones.
    let mut operands = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = item;
    loop {
        if seen.len() >= MAX_DIFFERENCE_CHAIN {
            ctx.diag.warn(
                codes::GEOMETRY_LIMIT_REACHED,
                item.id(),
                format!("boolean operand chain exceeds {MAX_DIFFERENCE_CHAIN} operands"),
            );
            ctx.record_boolean(item.id(), crate::BooleanStatus::Refused);
            return None;
        }
        // A chain that comes back to an operand it already visited never ends.
        if !seen.insert(current.id()) {
            ctx.diag.warn(
                codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE,
                item.id(),
                "a boolean result whose first operand chain forms a cycle",
            );
            return None;
        }
        let second = current.attr("SecondOperand").as_entity();
        let first = current.attr("FirstOperand").as_entity()?;
        if let Some(second) = second {
            operands.push(second);
        } else {
            ctx.diag.warn(
                codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE,
                current.id(),
                "boolean SecondOperand is missing",
            );
            ctx.record_boolean(item.id(), crate::BooleanStatus::Refused);
        }
        if is_difference(first) {
            current = first;
        } else {
            current = first;
            break;
        }
    }

    let mut body = registry.solid(ctx, current).ok()?;
    let mut cutters = Vec::new();
    let mut refused = Vec::new();
    if ctx.boolean_outcome(current.id()) == crate::BooleanStatus::Refused {
        refused.push("first operand contains a refused boolean".into());
    }
    for operand in operands {
        // A plane clip is exact and keeps the body convex, so take those first.
        if let Some(plane) = half_space_plane(ctx, operand) {
            let result = tessifc_mesh::clip(&body, &plane.flipped(), ctx.tol.len);
            match result.outcome {
                tessifc_mesh::ClipOutcome::Open => {
                    refused.push("the body is not a closed shell".into())
                }
                tessifc_mesh::ClipOutcome::Removed => {
                    // Nothing is left to draw, which is a fact about the file worth a line.
                    ctx.diag.warn(
                        codes::DEGENERATE_GEOMETRY,
                        item.id(),
                        "a half space clip removed the whole body",
                    );
                    body = result.mesh;
                }
                _ => body = result.mesh,
            }
            continue;
        }
        let solid = if operand.is_a("IfcPolygonalBoundedHalfSpace") {
            polygonal_bounded_solid(ctx, registry, operand, &body)
        } else {
            registry.solid(ctx, operand)
        };
        if ctx.boolean_outcome(operand.id()) == crate::BooleanStatus::Refused {
            refused.push(format!(
                "operand #{} contains a refused boolean",
                operand.id()
            ));
        }
        match solid {
            Ok(solid) if !solid.is_empty() => cutters.push(solid),
            Ok(_) => {}
            Err(error) => refused.push(error.to_string()),
        }
    }
    ctx.record_boolean(
        item.id(),
        if refused.is_empty() {
            crate::BooleanStatus::Exact
        } else {
            crate::BooleanStatus::Refused
        },
    );
    Some(Difference {
        body,
        cutters,
        refused,
    })
}

fn is_difference(item: Entity<'_>) -> bool {
    if !item.is_a("IfcBooleanResult") {
        return false;
    }
    item.attr("Operator")
        .as_text()
        .map(|text| text.raw().eq_ignore_ascii_case(b"DIFFERENCE"))
        .unwrap_or(false)
}

/// Turn an `IfcPolygonalBoundedHalfSpace` into a solid big enough to cut with.
///
/// The boundary is extruded past `body` both ways, then clipped by the base plane.
fn polygonal_bounded_solid(
    ctx: &EvalCtx<'_>,
    registry: &Registry,
    item: Entity<'_>,
    body: &Mesh64,
) -> Result<Mesh64, GeomError> {
    let boundary = item
        .attr("PolygonalBoundary")
        .as_entity()
        .ok_or_else(|| GeomError::missing("PolygonalBoundary"))?;
    let curve = registry.curve(ctx, boundary)?;
    let outline: Vec<glam::DVec2> = curve.points.iter().map(|point| point.truncate()).collect();
    if outline.len() < 3 {
        return Err(GeomError::Degenerate(
            "a polygonal boundary with fewer than three points".into(),
        ));
    }

    // Long enough to pass right through the body from either side.
    let reach = match body.bounds() {
        Some((low, high)) => (high - low).length().max(ctx.tol.len) * 4.0 + 1.0,
        None => 1.0,
    };
    let profile = crate::registry::Profile2D::new(outline);
    let mut prism = extrude(&profile, DVec3::Z * (2.0 * reach), ctx)?;
    prism.transform(&DMat4::from_translation(DVec3::Z * -reach));

    // Move the prism into Position's frame before the base plane cuts it.
    prism.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));

    let plane = base_plane(ctx, item).ok_or_else(|| GeomError::missing("BaseSurface"))?;
    let clipped = tessifc_mesh::clip(&prism, &plane, ctx.tol.len);
    match clipped.outcome {
        tessifc_mesh::ClipOutcome::Removed => Err(GeomError::Degenerate(
            "the bounded half space is empty".into(),
        )),
        tessifc_mesh::ClipOutcome::Open => Err(GeomError::Degenerate(
            "the bounded half space did not close".into(),
        )),
        _ => Ok(clipped.mesh),
    }
}

/// The oriented base plane of any `IfcHalfSpaceSolid`.
fn base_plane(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Option<Plane> {
    let surface = item.attr("BaseSurface").as_entity()?;
    if !surface.is_a("IfcPlane") {
        return None;
    }
    let frame = axis2_placement_3d(surface.attr("Position"), &ctx.units);
    let origin = frame.transform_point3(DVec3::ZERO);
    let normal = frame.transform_vector3(DVec3::Z).normalize_or_zero();
    let agreement = item.attr("AgreementFlag").as_bool().unwrap_or(true);
    // AgreementFlag true puts the material on the side the normal points away from;
    // the returned plane keeps its negative side as the material.
    Plane::from_point_normal(origin, if agreement { normal } else { -normal })
}

/// `IfcMappedItem`: geometry defined once and placed many times.
///
/// The source is cached by id, so a hundred identical chairs cost one evaluation.
pub struct MappedItem;

impl SolidEvaluator for MappedItem {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcMappedItem"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let mut mesh = Mesh64::new();
        for part in mapped_item_parts(ctx, item)? {
            mesh.append(&part.mesh);
        }
        Ok(mesh)
    }
}

/// One item of a mapped representation, and the style that item carries.
#[derive(Clone, Debug, PartialEq)]
pub struct MappedPart {
    /// The item's mesh, shared across every placement of the family.
    pub mesh: Arc<Mesh64>,
    /// The style on the item itself, if it has one.
    pub colour: Option<Rgba>,
}

/// One use of a mapped representation: the shared source and where it goes.
///
/// The source is evaluated once per model; only the placement is per product.
pub struct MappedUse {
    /// The `IfcShapeRepresentation` the map points at, identifying the shared source.
    pub representation_id: u32,
    /// Its items, in source space, evaluated once.
    pub parts: Arc<Vec<MappedPart>>,
    /// Source space into the using item's space: `MappingTarget` after `MappingOrigin`.
    pub placement: DMat4,
}

/// A mapped item's shared source and the transform for this use of it.
pub fn mapped_item_use(ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<MappedUse, GeomError> {
    let source = item
        .attr("MappingSource")
        .as_entity()
        .ok_or_else(|| GeomError::missing("MappingSource"))?;
    let representation = source
        .attr("MappedRepresentation")
        .as_entity()
        .ok_or_else(|| GeomError::missing("MappedRepresentation"))?;

    let parts = ctx.mapped_parts(representation.id(), || {
        let registry = ctx.registry();
        let mut source_parts: Vec<MappedPart> = Vec::new();
        let items = representation
            .attr("Items")
            .as_list()
            .ok_or_else(|| GeomError::missing("Items"))?;
        let mut seen_items = std::collections::HashSet::new();
        for value in items {
            let Some(entity) = value.as_entity() else {
                continue;
            };
            if !seen_items.insert(entity.id()) {
                continue;
            }
            // A face set with a colour map is one part per colour, as in a product.
            if entity.is_a("IfcTessellatedFaceSet")
                && let Some(groups) = crate::eval::tessellated::coloured_parts(ctx, entity)
            {
                let own = crate::style::item_style(ctx, entity);
                for (colour, mesh) in groups {
                    source_parts.push(MappedPart {
                        mesh: Arc::new(mesh),
                        colour: colour.or(own),
                    });
                }
                continue;
            }
            match registry.solid(ctx, entity) {
                Ok(mesh) => {
                    if !mesh.is_empty() {
                        source_parts.push(MappedPart {
                            mesh: Arc::new(mesh),
                            colour: crate::style::item_style(ctx, entity),
                        });
                    }
                }
                Err(error) => ctx.diag.warn(error.code(), entity.id(), error.to_string()),
            }
        }
        if source_parts.is_empty() {
            return Err(GeomError::Degenerate(
                "a mapped item with no readable geometry".into(),
            ));
        }
        Ok(source_parts)
    })?;

    // Origin first, then target.
    let origin = axis2_placement_3d(source.attr("MappingOrigin"), &ctx.units);
    let target = item
        .attr("MappingTarget")
        .as_entity()
        .map(|operator| transformation_operator(operator, &ctx.units))
        .unwrap_or(DMat4::IDENTITY);
    Ok(MappedUse {
        representation_id: representation.id(),
        parts,
        placement: target * origin,
    })
}

/// A mapped item's source items, each placed by the mapping target.
///
/// One entry per item, because their styles differ.
pub fn mapped_item_parts(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
) -> Result<Vec<MappedPart>, GeomError> {
    let shared = mapped_item_use(ctx, item)?;
    Ok(shared
        .parts
        .iter()
        .map(|part| {
            let mut mesh = (*part.mesh).clone();
            mesh.transform(&shared.placement);
            MappedPart {
                mesh: Arc::new(mesh),
                colour: part.colour,
            }
        })
        .collect())
}

/// Register every solid evaluator in this file.
pub fn register(registry: &mut Registry) {
    registry.register_solid(Box::new(ExtrudedAreaSolid));
    registry.register_solid(Box::new(FacetedBrep));
    registry.register_solid(Box::new(AdvancedBrep));
    registry.register_solid(Box::new(SurfaceModel));
    registry.register_solid(Box::new(Boolean));
    registry.register_solid(Box::new(MappedItem));
    registry.register_solid(Box::new(RevolvedAreaSolid));
    registry.register_solid(Box::new(ExtrudedAreaSolidTapered));
    registry.register_solid(Box::new(SweptSurface));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tests::{eval_solid, eval_solid_with_diagnostics, model_of};

    #[test]
    fn a_rectangular_extrusion_is_a_box() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,3.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,4.);\n",
        );
        let mesh = eval_solid(&model, 3).unwrap();
        assert_eq!(
            mesh.closed,
            Some(true),
            "an extruded rectangle is a closed solid"
        );
        let volume = mesh.signed_volume();
        assert!(
            (volume - 24.0).abs() < 1e-9,
            "2 x 3 x 4 should be 24, got {volume}"
        );
        let area = mesh.surface_area();
        // 2*(2*3) + 2*(2*4) + 2*(3*4) = 12 + 16 + 24 = 52
        assert!((area - 52.0).abs() < 1e-9, "got {area}");
    }

    #[test]
    fn an_oblique_extrusion_shears_rather_than_stretching() {
        // Depth is along the direction: the height is 4 * cos(45) = 2.828.
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);\n\
             #2=IFCDIRECTION((0.7071067811865476,0.,0.7071067811865476));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,4.);\n",
        );
        let mesh = eval_solid(&model, 3).unwrap();
        let expected = 1.0 * 4.0 * std::f64::consts::FRAC_1_SQRT_2;
        let volume = mesh.signed_volume();
        assert!(
            (volume - expected).abs() < 1e-9,
            "an oblique sweep is a shear: expected {expected}, got {volume}"
        );
        let (lo, hi) = mesh.bounds().unwrap();
        assert!(
            (hi.z - lo.z - expected).abs() < 1e-9,
            "height should be 4 cos(45)"
        );
    }

    #[test]
    fn a_downward_extrusion_is_still_right_way_out() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #2=IFCDIRECTION((0.,0.,-1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n",
        );
        let mesh = eval_solid(&model, 3).unwrap();
        assert!(
            mesh.signed_volume() > 0.0,
            "normals must point outward whichever way it sweeps"
        );
        assert!((mesh.signed_volume() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn a_hollow_profile_extrudes_to_a_tube() {
        let model = model_of(
            "#1=IFCRECTANGLEHOLLOWPROFILEDEF(.AREA.,$,$,4.,4.,1.,$,$);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.);\n",
        );
        let mesh = eval_solid(&model, 3).unwrap();
        // (4*4 - 2*2) * 2 = 24
        assert!(
            (mesh.signed_volume() - 24.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn the_position_moves_the_solid() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCCARTESIANPOINT((10.,20.,30.));\n\
             #4=IFCAXIS2PLACEMENT3D(#3,$,$);\n\
             #5=IFCEXTRUDEDAREASOLID(#1,#4,#2,2.);\n",
        );
        let mesh = eval_solid(&model, 5).unwrap();
        let (lo, hi) = mesh.bounds().unwrap();
        let centre = (lo + hi) * 0.5;
        assert!(
            (centre - DVec3::new(10.0, 20.0, 31.0)).length() < 1e-9,
            "got {centre}"
        );
    }

    #[test]
    fn an_open_profile_extrudes_to_a_ribbon_and_says_so() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,0.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#3);\n",
            "#5=IFCDIRECTION((0.,0.,1.));\n",
            "#6=IFCEXTRUDEDAREASOLID(#4,$,#5,2.);\n",
        ));
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 6);
        let mesh = mesh.unwrap();
        assert_eq!(mesh.closed, Some(false));
        assert!(
            (mesh.surface_area() - 2.0).abs() < 1e-9,
            "one 1 by 2 ribbon"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == crate::error::codes::OPEN_PROFILE_SURFACE)
        );
    }

    #[test]
    fn a_surface_of_linear_extrusion_has_no_caps() {
        let model = model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.CURVE.,$,$,1.,1.);\n",
            "#2=IFCDIRECTION((0.,0.,1.));\n",
            "#3=IFCSURFACEOFLINEAREXTRUSION(#1,$,#2,2.);\n",
        ));
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 3);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(false));
        assert!(
            (mesh.surface_area() - 8.0).abs() < 1e-9,
            "four 1 by 2 walls"
        );
    }

    #[test]
    fn a_surface_of_revolution_of_a_line_is_a_cylinder_wall() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((1.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,2.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#3);\n",
            "#5=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#6=IFCDIRECTION((0.,1.,0.));\n",
            "#7=IFCAXIS1PLACEMENT(#5,#6);\n",
            "#8=IFCSURFACEOFREVOLUTION(#4,$,#7);\n",
        ));
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 8);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(false));
        let area = mesh.surface_area();
        let expected = 2.0 * std::f64::consts::PI * 1.0 * 2.0;
        assert!((area - expected).abs() < 0.02 * expected, "got {area}");
    }

    #[test]
    fn a_zero_depth_extrusion_is_refused() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,0.);\n",
        );
        assert!(matches!(
            eval_solid(&model, 3),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn a_brep_cube_is_closed_and_measures_right() {
        let model = model_of(&brep_cube());
        let mesh = eval_solid(&model, 100).unwrap();
        assert_eq!(mesh.closed, Some(true), "a welded cube shell is watertight");
        assert!(
            (mesh.signed_volume() - 1.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
        assert!((mesh.surface_area() - 6.0).abs() < 1e-9);
        assert_eq!(
            mesh.positions.len(),
            8,
            "welding should recover eight corners"
        );
    }

    #[test]
    fn a_planar_advanced_brep_reads_oriented_edge_loops() {
        let model = model_of(PLANAR_ADVANCED_TETRAHEDRON);
        let mesh = eval_solid(&model, 71).unwrap();
        assert_eq!(mesh.closed, Some(true));
        assert_eq!(mesh.positions.len(), 4);
        assert!((mesh.signed_volume() - 1.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn advanced_polyline_edges_honour_both_orientation_flags() {
        for (points, sense) in [("#1,#2", ".T."), ("#2,#1", ".F.")] {
            let source = PLANAR_ADVANCED_TETRAHEDRON.replace(
                "#12=IFCEDGECURVE(#5,#6,#11,.T.);",
                &format!("#101=IFCPOLYLINE(({points}));#12=IFCEDGECURVE(#5,#6,#101,{sense});"),
            );
            let mesh = eval_solid(&model_of(&source), 71).unwrap();
            assert_eq!(mesh.closed, Some(true));
            assert!((mesh.signed_volume() - 1.0 / 6.0).abs() < 1e-9);
        }
    }

    #[test]
    fn shell_surface_models_dispatch_advanced_faces() {
        let source =
            format!("{PLANAR_ADVANCED_TETRAHEDRON}\n#72=IFCSHELLBASEDSURFACEMODEL((#70));");
        let mesh = eval_solid(&model_of(&source), 72).unwrap();
        assert_eq!(mesh.closed, Some(true));
        assert!((mesh.signed_volume() - 1.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn an_explicit_pcurve_can_bound_a_full_cylinder_period() {
        let source = "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
            #3=IFCCARTESIANPOINT((0.,0.,2.));#4=IFCAXIS2PLACEMENT3D(#3,$,$);#5=IFCCYLINDRICALSURFACE(#2,1.);\n\
            #6=IFCCIRCLE(#2,1.);#7=IFCCIRCLE(#4,1.);#8=IFCDIRECTION((0.,0.,1.));#9=IFCVECTOR(#8,2.);\n\
            #10=IFCCARTESIANPOINT((1.,0.,0.));#11=IFCVERTEXPOINT(#10);#12=IFCCARTESIANPOINT((1.,0.,2.));#13=IFCVERTEXPOINT(#12);#14=IFCLINE(#10,#9);\n\
            #20=IFCCARTESIANPOINT((0.,0.));#21=IFCCARTESIANPOINT((6.283185307179586,0.));\n\
            #22=IFCCARTESIANPOINT((6.283185307179586,2.));#23=IFCCARTESIANPOINT((0.,2.));\n\
            #24=IFCPOLYLINE((#20,#21));#25=IFCPOLYLINE((#21,#22));#26=IFCPOLYLINE((#22,#23));#27=IFCPOLYLINE((#23,#20));\n\
            #28=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',2,0.00001,#2,$);\n\
            \n\
            \n\
            #34=IFCPCURVE(#5,#24);#35=IFCPCURVE(#5,#25);#36=IFCPCURVE(#5,#26);#37=IFCPCURVE(#5,#27);\n\
            #40=IFCSURFACECURVE(#6,(#34),.PCURVE_S1.);#41=IFCSURFACECURVE(#14,(#35),.PCURVE_S1.);\n\
            #42=IFCSURFACECURVE(#7,(#36),.PCURVE_S1.);#43=IFCSURFACECURVE(#14,(#37),.PCURVE_S1.);\n\
            #50=IFCEDGECURVE(#11,#11,#40,.T.);#51=IFCEDGECURVE(#11,#13,#41,.T.);\n\
            #52=IFCEDGECURVE(#13,#13,#42,.T.);#53=IFCEDGECURVE(#13,#11,#43,.T.);\n\
            #54=IFCORIENTEDEDGE(*,*,#50,.T.);#55=IFCORIENTEDEDGE(*,*,#51,.T.);\n\
            #56=IFCORIENTEDEDGE(*,*,#52,.T.);#57=IFCORIENTEDEDGE(*,*,#53,.T.);\n\
            #60=IFCEDGELOOP((#54,#55,#56,#57));#61=IFCFACEOUTERBOUND(#60,.T.);#62=IFCADVANCEDFACE((#61),#5,.T.);\n\
            #63=IFCOPENSHELL((#62));#64=IFCSHELLBASEDSURFACEMODEL((#63));";
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model_of(source), 64);
        let mesh = mesh.unwrap_or_else(|error| panic!("{error:?}: {diagnostics:?}"));
        assert!(!mesh.is_empty(), "{diagnostics:?}");
        let area: f64 = mesh
            .indices
            .chunks_exact(3)
            .map(|tri| {
                let a = mesh.positions[tri[0] as usize];
                let b = mesh.positions[tri[1] as usize];
                let c = mesh.positions[tri[2] as usize];
                (b - a).cross(c - a).length() * 0.5
            })
            .sum();
        assert!(
            (area - 4.0 * std::f64::consts::PI).abs() < 0.05,
            "area {area}"
        );
        assert!(
            mesh.positions
                .iter()
                .all(|point| (point.truncate().length() - 1.0).abs() < 1e-10)
        );
        let reversed = source
            .replace(
                "IFCADVANCEDFACE((#61),#5,.T.)",
                "IFCADVANCEDFACE((#61),#5,.F.)",
            )
            .replace("IFCFACEOUTERBOUND(#60,.T.)", "IFCFACEOUTERBOUND(#60,.F.)");
        let opposite = eval_solid(&model_of(&reversed), 64).unwrap();
        let orientation = |mesh: &Mesh64| {
            mesh.indices
                .chunks_exact(3)
                .map(|triangle| {
                    let a = mesh.positions[triangle[0] as usize];
                    let b = mesh.positions[triangle[1] as usize];
                    let c = mesh.positions[triangle[2] as usize];
                    let centre = (a + b + c) / 3.0;
                    (b - a)
                        .cross(c - a)
                        .dot(DVec3::new(centre.x, centre.y, 0.0))
                })
                .sum::<f64>()
        };
        assert!(orientation(&mesh) > 0.0);
        assert!(orientation(&opposite) < 0.0);

        let implicit = source.replace(".PCURVE_S1.", ".CURVE3D.").replace(
            "IFCEDGECURVE(#13,#13,#42,.T.)",
            "IFCEDGECURVE(#13,#13,#42,.F.)",
        );
        let implicit = eval_solid(&model_of(&implicit), 64).unwrap();
        assert!(orientation(&implicit) > 0.0);
        let area: f64 = implicit
            .indices
            .chunks_exact(3)
            .map(|t| {
                let a = implicit.positions[t[0] as usize];
                let b = implicit.positions[t[1] as usize];
                let c = implicit.positions[t[2] as usize];
                (b - a).cross(c - a).length() * 0.5
            })
            .sum();
        assert!(
            (area - 4.0 * std::f64::consts::PI).abs() < 0.05,
            "area {area}"
        );
    }

    #[test]
    fn a_surface_grid_checks_its_input_budget_before_refining() {
        let model = model_of("#1=IFCCARTESIANPOINT((0.,0.,0.));");
        let settings = crate::Settings {
            max_surface_vertices: 64,
            ..crate::Settings::default()
        };
        let sink = crate::DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            crate::Units::default(),
            crate::Tolerances::default(),
            &settings,
            &sink,
        );
        let mut uv = Vec::new();
        for i in 0..8 {
            uv.push(DVec2::new(i as f64 / 8.0, 0.0));
        }
        for i in 0..8 {
            uv.push(DVec2::new(1.0, i as f64 / 8.0));
        }
        for i in 0..8 {
            uv.push(DVec2::new(1.0 - i as f64 / 8.0, 1.0));
        }
        for i in 0..8 {
            uv.push(DVec2::new(0.0, 1.0 - i as f64 / 8.0));
        }
        let boundary: Vec<_> = uv.iter().map(|p| p.extend(0.0)).collect();
        let surface = Surface::new(SurfaceKind::Plane, DMat4::IDENTITY);
        let result = rectangular_parameter_patch(&surface, &ctx, &boundary, &uv, 1);
        assert!(matches!(result, Err(GeomError::LimitReached(_))));
    }

    #[test]
    fn recovering_an_inconsistent_edge_orientation_is_opt_in() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCCARTESIANPOINT((1.,0.,0.));#3=IFCCARTESIANPOINT((0.,1.,0.));\n\
            #4=IFCVERTEXPOINT(#1);#5=IFCVERTEXPOINT(#2);#6=IFCVERTEXPOINT(#3);\n\
            #7=IFCPOLYLINE((#1,#2));#8=IFCPOLYLINE((#2,#3));#9=IFCPOLYLINE((#3,#1));\n\
            #10=IFCEDGECURVE(#4,#5,#7,.T.);#11=IFCEDGECURVE(#5,#6,#8,.T.);#12=IFCEDGECURVE(#6,#4,#9,.T.);\n\
            #13=IFCORIENTEDEDGE(*,*,#10,.T.);#14=IFCORIENTEDEDGE(*,*,#11,.F.);#15=IFCORIENTEDEDGE(*,*,#12,.T.);\n\
            #16=IFCEDGELOOP((#13,#14,#15));",
        );
        for enabled in [false, true] {
            let settings = crate::Settings {
                repair_surface_curves: enabled,
                ..crate::Settings::default()
            };
            let sink = crate::DiagnosticSink::default();
            let registry = crate::Registry::defaults(model.image().schema);
            let ctx = EvalCtx::new(
                &model,
                crate::Units::default(),
                crate::Tolerances::default(),
                &settings,
                &sink,
            )
            .with_registry(&registry);
            let runs = advanced_edge_loop_runs(&ctx, model.entity(16).unwrap());
            assert_eq!(runs.is_ok(), enabled);
            if enabled {
                assert!(
                    sink.take()
                        .iter()
                        .any(|d| d.code == codes::EDGE_ORIENTATION_RECOVERED)
                );
            }
        }
    }

    #[test]
    fn a_mislabelled_planar_outer_bound_is_recovered_by_containment() {
        let square = |low: f64, high: f64| {
            vec![
                DVec3::new(low, low, 0.0),
                DVec3::new(high, low, 0.0),
                DVec3::new(high, high, 0.0),
                DVec3::new(low, high, 0.0),
            ]
        };
        let arranged = arrange_planar_face_loops(
            vec![
                PlanarFaceLoop {
                    points: square(1.0, 3.0),
                    declared_outer: true,
                },
                PlanarFaceLoop {
                    points: square(0.0, 4.0),
                    declared_outer: false,
                },
            ],
            1e-9,
        )
        .unwrap();
        assert!(arranged.recovered);
        assert!((planar_loop_area(&arranged.outer) - 16.0).abs() < 1e-12);
        assert!((planar_loop_area(&arranged.holes[0]) - 4.0).abs() < 1e-12);

        let indices = triangulate_face(&arranged.outer, &arranged.holes).unwrap();
        let mut points = arranged.outer;
        points.extend_from_slice(&arranged.holes[0]);
        let area: f64 = indices
            .chunks_exact(3)
            .map(|triangle| {
                let a = points[triangle[0] as usize];
                let b = points[triangle[1] as usize];
                let c = points[triangle[2] as usize];
                (b - a).cross(c - a).length() * 0.5
            })
            .sum();
        assert!((area - 12.0).abs() < 1e-12);
    }

    #[test]
    fn disjoint_planar_face_bounds_are_refused_instead_of_guessed() {
        let result = arrange_planar_face_loops(
            vec![
                PlanarFaceLoop {
                    points: vec![DVec3::ZERO, DVec3::X, DVec3::new(1.0, 1.0, 0.0)],
                    declared_outer: true,
                },
                PlanarFaceLoop {
                    points: vec![
                        DVec3::new(2.0, 0.0, 0.0),
                        DVec3::new(3.0, 0.0, 0.0),
                        DVec3::new(3.0, 1.0, 0.0),
                    ],
                    declared_outer: false,
                },
            ],
            1e-9,
        );
        assert!(
            matches!(result, Err(GeomError::Degenerate(message)) if message.contains("disjoint"))
        );
    }

    #[test]
    fn an_invalid_conic_advanced_brep_is_refused_instead_of_projected() {
        let source =
            PLANAR_ADVANCED_TETRAHEDRON.replace("#11=IFCLINE(#1,#10);", "#11=IFCCIRCLE(#50,1.);");
        let model = model_of(&source);
        assert!(matches!(
            eval_solid(&model, 71),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn a_fully_tessellated_open_advanced_shell_is_emitted_with_a_warning() {
        let source = PLANAR_ADVANCED_TETRAHEDRON.replace(
            "#70=IFCCLOSEDSHELL((#60,#61,#62,#63));",
            "#70=IFCCLOSEDSHELL((#60,#61,#62));",
        );
        let model = model_of(&source);
        let (result, diagnostics) = eval_solid_with_diagnostics(&model, 71);
        let mesh = result.expect("all three readable faces are useful to the viewer");
        assert_eq!(mesh.closed, Some(false));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == codes::NON_MANIFOLD_INPUT && diagnostic.express_id == Some(71)
        }));
    }

    /// A quarter of a torus written the way a real exporter writes a handle
    /// fillet: a trimmed circle revolved about an axis, bounded by four arcs.
    /// This is the shape that used to be filled in with a Coons patch.
    const REVOLVED_TORUS_PATCH: &str = "        #10=IFCCARTESIANPOINT((0.,0.,0.));
        #11=IFCDIRECTION((0.,0.,1.));
        #12=IFCDIRECTION((1.,0.,0.));
        #13=IFCAXIS1PLACEMENT(#10,#11);
        #20=IFCCARTESIANPOINT((0.6,0.,0.));
        #21=IFCDIRECTION((0.,-1.,0.));
        #22=IFCAXIS2PLACEMENT3D(#20,#21,#12);
        #23=IFCCIRCLE(#22,0.2);
        #24=IFCCARTESIANPOINT((0.8,0.,0.));
        #25=IFCCARTESIANPOINT((0.6,0.,0.2));
        #26=IFCTRIMMEDCURVE(#23,(#24),(#25),.T.,.CARTESIAN.);
        #27=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#26);
        #30=IFCSURFACEOFREVOLUTION(#27,$,#13);";

    #[test]
    fn a_revolved_patch_is_evaluated_on_its_own_surface() {
        let model = model_of(REVOLVED_TORUS_PATCH);
        let surface = crate::eval::tests::eval_surface(&model, 30).expect("a surface");
        assert!(matches!(surface.kind, SurfaceKind::Revolution { .. }));
        // Every point of the surface is one minor radius from the tube centre,
        // to within the sagitta of the profile's own chords.
        for (u, v) in [(0.0, 0.0), (1.0, 0.5), (-2.0, 1.0)] {
            let point = surface.point(DVec2::new(u, v));
            let axial = DVec3::new(point.x, point.y, 0.0);
            let centre = axial.normalize_or(DVec3::X) * 0.6;
            assert!(
                ((point - centre).length() - 0.2).abs() < 1e-3,
                "{point:?} is not on the tube"
            );
            let back = surface.invert(point, 1e-9).expect("inverted");
            assert!((surface.point(back) - point).length() < 1e-9);
        }
    }

    #[test]
    fn a_ruled_strip_is_refused_when_it_leaves_its_surface() {
        let model = model_of(REVOLVED_TORUS_PATCH);
        let surface = crate::eval::tests::eval_surface(&model, 30).expect("a surface");
        let settings = crate::context::Settings::default();
        let sink = crate::context::DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            crate::units::Units::from_model(&model),
            crate::context::Tolerances::default(),
            &settings,
            &sink,
        );
        // A flat quad across the quarter torus: the middle sags off the tube.
        let mut flat = Mesh64::new();
        for corner in [
            surface.point(DVec2::new(0.0, 0.0)),
            surface.point(DVec2::new(1.5, 0.0)),
            surface.point(DVec2::new(1.5, 1.0)),
            surface.point(DVec2::new(0.0, 1.0)),
        ] {
            flat.push_vertex(corner);
        }
        flat.push_triangle(0, 1, 2);
        flat.push_triangle(0, 2, 3);
        assert!(
            !strip_follows_surface(&surface, &flat, &ctx),
            "a flat quad across a torus quarter is not on the torus"
        );
        // One thin strip along a single ruling stays on it.
        let mut thin = Mesh64::new();
        for corner in [
            surface.point(DVec2::new(0.0, 0.0)),
            surface.point(DVec2::new(0.01, 0.0)),
            surface.point(DVec2::new(0.01, 0.02)),
        ] {
            thin.push_vertex(corner);
        }
        thin.push_triangle(0, 1, 2);
        assert!(strip_follows_surface(&surface, &thin, &ctx));
    }

    const PLANAR_ADVANCED_TETRAHEDRON: &str = "#1=IFCCARTESIANPOINT((0.,0.,0.));
         #2=IFCCARTESIANPOINT((1.,0.,0.));
         #3=IFCCARTESIANPOINT((0.,1.,0.));
         #4=IFCCARTESIANPOINT((0.,0.,1.));
         #5=IFCVERTEXPOINT(#1);
         #6=IFCVERTEXPOINT(#2);
         #7=IFCVERTEXPOINT(#3);
         #8=IFCVERTEXPOINT(#4);
         #9=IFCDIRECTION((1.,0.,0.));
         #10=IFCVECTOR(#9,1.);
         #11=IFCLINE(#1,#10);
         #12=IFCEDGECURVE(#5,#6,#11,.T.);
         #13=IFCEDGECURVE(#5,#7,#11,.T.);
         #14=IFCEDGECURVE(#5,#8,#11,.T.);
         #15=IFCEDGECURVE(#6,#7,#11,.T.);
         #16=IFCEDGECURVE(#6,#8,#11,.T.);
         #17=IFCEDGECURVE(#7,#8,#11,.T.);
         #20=IFCORIENTEDEDGE(*,*,#13,.T.);
         #21=IFCORIENTEDEDGE(*,*,#15,.F.);
         #22=IFCORIENTEDEDGE(*,*,#12,.F.);
         #23=IFCORIENTEDEDGE(*,*,#12,.T.);
         #24=IFCORIENTEDEDGE(*,*,#16,.T.);
         #25=IFCORIENTEDEDGE(*,*,#14,.F.);
         #26=IFCORIENTEDEDGE(*,*,#14,.T.);
         #27=IFCORIENTEDEDGE(*,*,#17,.F.);
         #28=IFCORIENTEDEDGE(*,*,#13,.F.);
         #29=IFCORIENTEDEDGE(*,*,#15,.T.);
         #30=IFCORIENTEDEDGE(*,*,#17,.T.);
         #31=IFCORIENTEDEDGE(*,*,#16,.F.);
         #40=IFCEDGELOOP((#20,#21,#22));
         #41=IFCEDGELOOP((#23,#24,#25));
         #42=IFCEDGELOOP((#26,#27,#28));
         #43=IFCEDGELOOP((#29,#30,#31));
         #44=IFCFACEOUTERBOUND(#40,.T.);
         #45=IFCFACEOUTERBOUND(#41,.T.);
         #46=IFCFACEOUTERBOUND(#42,.T.);
         #47=IFCFACEOUTERBOUND(#43,.T.);
         #50=IFCAXIS2PLACEMENT3D(#1,$,$);
         #51=IFCPLANE(#50);
         #60=IFCADVANCEDFACE((#44),#51,.T.);
         #61=IFCADVANCEDFACE((#45),#51,.T.);
         #62=IFCADVANCEDFACE((#46),#51,.T.);
         #63=IFCADVANCEDFACE((#47),#51,.T.);
         #70=IFCCLOSEDSHELL((#60,#61,#62,#63));
         #71=IFCADVANCEDBREP(#70);
";

    /// A unit cube as an IfcFacetedBrep, the way an exporter writes one.
    fn brep_cube() -> String {
        let corners = [
            (0.0, 0.0, 0.0),
            (1.0, 0.0, 0.0),
            (1.0, 1.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (1.0, 0.0, 1.0),
            (1.0, 1.0, 1.0),
            (0.0, 1.0, 1.0),
        ];
        let mut text = String::new();
        for (index, (x, y, z)) in corners.iter().enumerate() {
            text.push_str(&format!(
                "#{}=IFCCARTESIANPOINT(({x:.1},{y:.1},{z:.1}));\n",
                index + 1
            ));
        }
        // Faces wound counter-clockwise seen from outside.
        let faces = [
            [1, 4, 3, 2],
            [5, 6, 7, 8],
            [1, 2, 6, 5],
            [2, 3, 7, 6],
            [3, 4, 8, 7],
            [4, 1, 5, 8],
        ];
        let mut face_ids = Vec::new();
        for (index, face) in faces.iter().enumerate() {
            let loop_id = 20 + index * 3;
            let bound_id = loop_id + 1;
            let face_id = loop_id + 2;
            let points: Vec<String> = face.iter().map(|corner| format!("#{corner}")).collect();
            text.push_str(&format!(
                "#{loop_id}=IFCPOLYLOOP(({}));\n",
                points.join(",")
            ));
            text.push_str(&format!("#{bound_id}=IFCFACEOUTERBOUND(#{loop_id},.T.);\n"));
            text.push_str(&format!("#{face_id}=IFCFACE((#{bound_id}));\n"));
            face_ids.push(format!("#{face_id}"));
        }
        text.push_str(&format!("#99=IFCCLOSEDSHELL(({}));\n", face_ids.join(",")));
        text.push_str("#100=IFCFACETEDBREP(#99);\n");
        text
    }

    #[test]
    fn an_inside_out_brep_is_flipped() {
        // The same cube with every face wound the other way.
        let source = brep_cube().replace(
            "#20=IFCPOLYLOOP((#1,#4,#3,#2));",
            "#20=IFCPOLYLOOP((#2,#3,#4,#1));",
        );
        let model = model_of(&source);
        let mesh = eval_solid(&model, 100).unwrap();
        assert!(
            mesh.signed_volume() > 0.0,
            "orientation must end up outward"
        );
    }

    /// A 2 x 2 x 2 box standing on the origin, as #1..#3.
    const UNIT_BOX: &str = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);
         #2=IFCDIRECTION((0.,0.,1.));
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.);
";

    #[test]
    fn a_half_space_clipping_result_actually_clips() {
        // Plane at z = 1 facing up; AgreementFlag true makes the material everything
        // below it, and the DIFFERENCE takes that away.
        let model = model_of(&format!(
            "{UNIT_BOX}#4=IFCCARTESIANPOINT((0.,0.,1.));
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);
             #6=IFCPLANE(#5);
             #7=IFCHALFSPACESOLID(#6,.T.);
             #8=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#3,#7);
"
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 8);
        let mesh = mesh.unwrap();
        assert!(
            (mesh.signed_volume() - 4.0).abs() < 1e-9,
            "half of an 8 m3 box, got {}",
            mesh.signed_volume()
        );
        let (lo, hi) = mesh.bounds().unwrap();
        assert!(
            (lo.z - 1.0).abs() < 1e-9,
            "the bottom should be the cut plane, got {}",
            lo.z
        );
        assert!(
            (hi.z - 2.0).abs() < 1e-9,
            "and the top should be untouched, got {}",
            hi.z
        );
        assert!(
            diagnostics.is_empty(),
            "an exact clip has nothing to report: {diagnostics:?}"
        );
    }

    #[test]
    fn the_agreement_flag_chooses_the_side() {
        let model = model_of(&format!(
            "{UNIT_BOX}#4=IFCCARTESIANPOINT((0.,0.,1.));
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);
             #6=IFCPLANE(#5);
             #7=IFCHALFSPACESOLID(#6,.F.);
             #8=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#3,#7);
"
        ));
        let mesh = eval_solid(&model, 8).unwrap();
        let (_, hi) = mesh.bounds().unwrap();
        assert!(
            (hi.z - 1.0).abs() < 1e-9,
            "the other half this time, got {}",
            hi.z
        );
        assert!((mesh.signed_volume() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_solid_second_operand_is_subtracted_exactly() {
        // A 1 x 1 column punched through the 2 x 2 x 2 box.
        let model = model_of(&format!(
            "{UNIT_BOX}#4=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);
             #5=IFCCARTESIANPOINT((0.,0.,-1.));
             #6=IFCAXIS2PLACEMENT3D(#5,$,$);
             #7=IFCEXTRUDEDAREASOLID(#4,#6,#2,4.);
             #8=IFCBOOLEANRESULT(.DIFFERENCE.,#3,#7);
"
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 8);
        let mesh = mesh.unwrap();
        assert!(
            (mesh.signed_volume() - (8.0 - 2.0)).abs() < 1e-9,
            "8 less a 1x1x2 column, got {}",
            mesh.signed_volume()
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn an_unsupported_boolean_emits_the_body_and_says_so() {
        // A cutter that cannot be built is refused, and the body comes through whole.
        let model = model_of(&format!(
            "{UNIT_BOX}#4=IFCCARTESIANPOINT((0.,0.,1.));
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);
             #6=IFCPLANE(#5);
             #7=IFCPOLYGONALBOUNDEDHALFSPACE(#6,.T.,#5,#20);
             #8=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#3,#7);
"
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 8);
        let mesh = mesh.unwrap();
        assert!(
            (mesh.signed_volume() - 8.0).abs() < 1e-9,
            "the un-cut body comes through whole"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE),
            "the caller must be told the cut did not happen"
        );
    }

    #[test]
    fn a_union_appends_both_operands() {
        let model = model_of(&format!(
            "{UNIT_BOX}#4=IFCCARTESIANPOINT((10.,0.,0.));
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);
             #6=IFCEXTRUDEDAREASOLID(#1,#5,#2,2.);
             #7=IFCBOOLEANRESULT(.UNION.,#3,#6);
"
        ));
        let mesh = eval_solid(&model, 7).unwrap();
        assert!(
            (mesh.signed_volume() - 16.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn a_mapped_item_places_its_source() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.);\n\
             #4=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
             #7=IFCREPRESENTATIONMAP(#5,#6);\n\
             #8=IFCCARTESIANPOINT((5.,0.,0.));\n\
             #9=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#8,1.,$);\n\
             #10=IFCMAPPEDITEM(#7,#9);\n",
        );
        let mesh = eval_solid(&model, 10).unwrap();
        let (lo, hi) = mesh.bounds().unwrap();
        let centre = (lo + hi) * 0.5;
        assert!(
            (centre.x - 5.0).abs() < 1e-9,
            "the mapping target should move it, got {centre}"
        );
        assert!((mesh.signed_volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn a_mapped_representation_ignores_a_repeated_item_reference() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.);\n\
             #4=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3,#3));\n\
             #7=IFCREPRESENTATIONMAP(#5,#6);\n\
             #8=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#4,1.,$);\n\
             #9=IFCMAPPEDITEM(#7,#8);\n",
        );
        let mesh = eval_solid(&model, 9).unwrap();

        assert_eq!(mesh.triangle_count(), 12);
        assert!((mesh.signed_volume() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn a_mapped_item_with_a_scale_scales() {
        let model = model_of(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,2.);\n\
             #4=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
             #7=IFCREPRESENTATIONMAP(#5,#6);\n\
             #8=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #9=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#8,2.,$);\n\
             #10=IFCMAPPEDITEM(#7,#9);\n",
        );
        let mesh = eval_solid(&model, 10).unwrap();
        // Twice in every direction is eight times the volume.
        assert!(
            (mesh.signed_volume() - 64.0).abs() < 1e-8,
            "got {}",
            mesh.signed_volume()
        );
    }

    /// A 1 by 2 rectangle revolved about a world axis through the origin.
    ///
    /// The angle is in radians, since the fixture declares no units.
    fn axis_source(axis: &str, centre: &str, angle: f64) -> String {
        format!(
            concat!(
                "#1=IFCCARTESIANPOINT(({centre}));
",
                "#2=IFCAXIS2PLACEMENT2D(#1,$);
",
                "#3=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,1.,2.);
",
                "#4=IFCCARTESIANPOINT((0.,0.,0.));
",
                "#5=IFCDIRECTION(({axis}));
",
                "#6=IFCAXIS1PLACEMENT(#4,#5);
",
                "#7=IFCCARTESIANPOINT((0.,0.,0.));
",
                "#8=IFCAXIS2PLACEMENT3D(#7,$,$);
",
                "#9=IFCREVOLVEDAREASOLID(#3,#8,#6,{angle});
"
            ),
            axis = axis,
            centre = centre,
            angle = angle
        )
    }

    /// The same quarter ring written six ways must come out the same way round.
    /// The winding depends on the sweep sign and on which side of the axis the
    /// profile sits, so an axis or an angle written negated must not invert it.
    #[test]
    fn a_revolution_faces_outward_whatever_axis_the_file_uses() {
        let quarter = std::f64::consts::TAU / 4.0;
        let cases = [
            ("+Y, profile at +x", "0.,1.,0.", "3.,0.", quarter),
            ("+Y, negated angle", "0.,1.,0.", "3.,0.", -quarter),
            ("-Y, negated angle", "0.,-1.,0.", "3.,0.", -quarter),
            ("+X, profile at +y", "1.,0.,0.", "0.,3.", quarter),
            ("-X, negated angle", "-1.,0.,0.", "0.,3.", -quarter),
            ("+Y, profile at -x", "0.,1.,0.", "-3.,0.", quarter),
        ];
        for (name, axis, centre, angle) in cases {
            let model = model_of(&axis_source(axis, centre, angle));
            let mesh = eval_solid(&model, 9).unwrap();
            let volume = mesh.signed_volume();
            assert!(mesh.is_edge_manifold(), "{name} is not manifold");
            assert!(
                (volume - 9.421).abs() < 0.01,
                "{name} came out at {volume}, expected the quarter ring right way out"
            );
        }
    }

    /// A 1 by 2 rectangle whose centroid sits `inner` from the axis, spun about it.
    fn ring_source(angle: f64, inner: f64) -> String {
        format!(
            concat!(
                "#1=IFCCARTESIANPOINT((0.,0.));\n",
                "#2=IFCAXIS2PLACEMENT2D(#1,$);\n",
                "#3=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,1.,2.);\n",
                "#4=IFCCARTESIANPOINT(({inner},0.,0.));\n",
                "#5=IFCDIRECTION((0.,1.,0.));\n",
                "#6=IFCAXIS1PLACEMENT(#4,#5);\n",
                "#7=IFCCARTESIANPOINT((0.,0.,0.));\n",
                "#8=IFCAXIS2PLACEMENT3D(#7,$,$);\n",
                "#9=IFCREVOLVEDAREASOLID(#3,#8,#6,{angle});\n"
            ),
            inner = -inner,
            angle = angle
        )
    }

    #[test]
    fn a_full_revolution_is_a_closed_ring() {
        // The centroid orbits at radius 3.
        let model = model_of(&ring_source(std::f64::consts::TAU, 3.0));
        let mesh = eval_solid(&model, 9).unwrap();
        assert!(mesh.is_edge_manifold(), "a full turn closes on itself");
        let volume = mesh.signed_volume();
        let exact = 2.0 * std::f64::consts::TAU * 3.0;
        assert!(
            volume > 0.0,
            "a revolved solid is not inside out, got {volume}"
        );
        assert!(
            volume < exact && (exact - volume) / exact < 0.02,
            "got {volume}, want about {exact}"
        );
    }

    #[test]
    fn a_partial_revolution_is_capped_at_both_ends() {
        let model = model_of(&ring_source(std::f64::consts::FRAC_PI_2, 3.0));
        let mesh = eval_solid(&model, 9).unwrap();
        assert!(
            mesh.is_edge_manifold(),
            "a quarter turn needs both end caps or it is not a solid"
        );
        let volume = mesh.signed_volume();
        let exact = 2.0 * std::f64::consts::TAU * 3.0 / 4.0;
        assert!(
            volume > 0.0,
            "a revolved solid is not inside out, got {volume}"
        );
        assert!(
            volume < exact && (exact - volume) / exact < 0.03,
            "got {volume}, want about {exact}"
        );
    }

    /// The same 1 by 2 ring section, written the other way round so it is clockwise.
    const CLOCKWISE_RING_SECTION: &str = "#1=IFCCARTESIANPOINT((-0.5,-1.));
         #2=IFCCARTESIANPOINT((-0.5,1.));
         #3=IFCCARTESIANPOINT((0.5,1.));
         #4=IFCCARTESIANPOINT((0.5,-1.));
         #5=IFCPOLYLINE((#1,#2,#3,#4,#1));
         #6=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#5);
         #7=IFCCARTESIANPOINT((-3.,0.,0.));
         #8=IFCDIRECTION((0.,1.,0.));
         #9=IFCAXIS1PLACEMENT(#7,#8);
         #10=IFCCARTESIANPOINT((0.,0.,0.));
         #11=IFCAXIS2PLACEMENT3D(#10,$,$);
";

    #[test]
    fn a_clockwise_profile_revolved_backwards_is_still_right_way_out() {
        let model = model_of(&format!(
            "{CLOCKWISE_RING_SECTION}#12=IFCREVOLVEDAREASOLID(#6,#11,#9,-1.5707963267948966);
"
        ));
        let mesh = eval_solid(&model, 12).unwrap();
        assert!(
            mesh.is_edge_manifold(),
            "a quarter turn needs both end caps"
        );
        let volume = mesh.signed_volume();
        let exact = 2.0 * std::f64::consts::TAU * 3.0 / 4.0;
        assert!(
            volume > 0.0,
            "a revolved solid is not inside out, got {volume}"
        );
        assert!(
            volume < exact && (exact - volume) / exact < 0.03,
            "got {volume}, want about {exact}"
        );
    }

    #[test]
    fn a_clockwise_profile_revolved_forwards_is_still_right_way_out() {
        let model = model_of(&format!(
            "{CLOCKWISE_RING_SECTION}#12=IFCREVOLVEDAREASOLID(#6,#11,#9,1.5707963267948966);
"
        ));
        let mesh = eval_solid(&model, 12).unwrap();
        let volume = mesh.signed_volume();
        let exact = 2.0 * std::f64::consts::TAU * 3.0 / 4.0;
        assert!(
            volume > 0.0,
            "a revolved solid is not inside out, got {volume}"
        );
        assert!(
            volume < exact && (exact - volume) / exact < 0.03,
            "got {volume}, want about {exact}"
        );
    }

    #[test]
    fn a_revolution_of_zero_angle_is_refused() {
        let model = model_of(&ring_source(0.0, 3.0));
        assert!(eval_solid(&model, 9).is_err());
    }

    #[test]
    fn a_revolution_spins_about_the_axis_the_file_gives() {
        // The axis is the y axis through x = -3; the ring keeps the profile's 2 units in y.
        let model = model_of(&ring_source(std::f64::consts::TAU, 3.0));
        let mesh = eval_solid(&model, 9).unwrap();
        let (low, high) = mesh.bounds().unwrap();
        assert!(
            (high.y - 1.0).abs() < 1e-9 && (low.y + 1.0).abs() < 1e-9,
            "y {low} {high}"
        );
        // Outer radius 3.5 about x = -3.
        assert!((high.x - 0.5).abs() < 0.02, "outer edge, got {high}");
        assert!((low.x + 6.5).abs() < 0.02, "far edge, got {low}");
    }

    /// A 3 by 2 rectangle written clockwise, as #1 to #6.
    const CLOCKWISE_RECTANGLE: &str = "#1=IFCCARTESIANPOINT((0.,0.));
         #2=IFCCARTESIANPOINT((0.,2.));
         #3=IFCCARTESIANPOINT((3.,2.));
         #4=IFCCARTESIANPOINT((3.,0.));
         #5=IFCPOLYLINE((#1,#2,#3,#4,#1));
         #6=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#5);
";

    #[test]
    fn a_clockwise_outline_extrudes_the_right_way_out() {
        let model = model_of(&format!(
            "{CLOCKWISE_RECTANGLE}#7=IFCDIRECTION((0.,0.,1.));
             #8=IFCEXTRUDEDAREASOLID(#6,$,#7,4.);
"
        ));
        let mesh = eval_solid(&model, 8).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let volume = mesh.signed_volume();
        assert!(
            (volume - 24.0).abs() < 1e-9,
            "3 x 2 x 4 is 24, got {volume}"
        );
    }

    #[test]
    fn a_clockwise_outline_extruded_downwards_is_right_way_out() {
        let model = model_of(&format!(
            "{CLOCKWISE_RECTANGLE}#7=IFCDIRECTION((0.,0.,-1.));
             #8=IFCEXTRUDEDAREASOLID(#6,$,#7,4.);
"
        ));
        let mesh = eval_solid(&model, 8).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let volume = mesh.signed_volume();
        assert!(
            (volume - 24.0).abs() < 1e-9,
            "3 x 2 x 4 is 24, got {volume}"
        );
    }

    #[test]
    fn a_cutter_with_a_clockwise_outline_still_cuts() {
        // The same column as the exact-subtraction case, written clockwise.
        let model = model_of(&format!(
            "{UNIT_BOX}#4=IFCCARTESIANPOINT((-0.5,-0.5));
             #5=IFCCARTESIANPOINT((-0.5,0.5));
             #6=IFCCARTESIANPOINT((0.5,0.5));
             #7=IFCCARTESIANPOINT((0.5,-0.5));
             #8=IFCPOLYLINE((#4,#5,#6,#7,#4));
             #9=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#8);
             #10=IFCCARTESIANPOINT((0.,0.,-1.));
             #11=IFCAXIS2PLACEMENT3D(#10,$,$);
             #12=IFCEXTRUDEDAREASOLID(#9,#11,#2,4.);
             #13=IFCBOOLEANRESULT(.DIFFERENCE.,#3,#12);
"
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 13);
        let mesh = mesh.unwrap();
        assert!(
            (mesh.signed_volume() - 6.0).abs() < 1e-9,
            "8 less a 1x1x2 column, got {}",
            mesh.signed_volume()
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn a_clockwise_taper_is_right_way_out() {
        // 4 by 4 to 2 by 2 over 3, both outlines clockwise.
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((-2.,-2.));\n",
            "#2=IFCCARTESIANPOINT((-2.,2.));\n",
            "#3=IFCCARTESIANPOINT((2.,2.));\n",
            "#4=IFCCARTESIANPOINT((2.,-2.));\n",
            "#5=IFCPOLYLINE((#1,#2,#3,#4,#1));\n",
            "#6=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#5);\n",
            "#7=IFCCARTESIANPOINT((-1.,-1.));\n",
            "#8=IFCCARTESIANPOINT((-1.,1.));\n",
            "#9=IFCCARTESIANPOINT((1.,1.));\n",
            "#10=IFCCARTESIANPOINT((1.,-1.));\n",
            "#11=IFCPOLYLINE((#7,#8,#9,#10,#7));\n",
            "#12=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#11);\n",
            "#13=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#14=IFCAXIS2PLACEMENT3D(#13,$,$);\n",
            "#15=IFCDIRECTION((0.,0.,1.));\n",
            "#16=IFCEXTRUDEDAREASOLIDTAPERED(#6,#14,#15,3.,#12);\n"
        ));
        let mesh = eval_solid(&model, 16).unwrap();
        assert!(mesh.is_edge_manifold());
        let volume = mesh.signed_volume();
        assert!((volume - 28.0).abs() < 1e-9, "got {volume}, want 28");
    }

    #[test]
    fn a_tapered_extrusion_narrows_instead_of_staying_square() {
        // 4 by 4 at the bottom, 2 by 2 at the top, 3 tall: a frustum.
        let model = model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,4.);\n",
            "#2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#4=IFCAXIS2PLACEMENT3D(#3,$,$);\n",
            "#5=IFCDIRECTION((0.,0.,1.));\n",
            "#6=IFCEXTRUDEDAREASOLIDTAPERED(#1,#4,#5,3.,#2);\n"
        ));
        let mesh = eval_solid(&model, 6).unwrap();
        assert!(mesh.is_edge_manifold());
        // A prism would hold 48; the frustum holds h/3 (A1 + A2 + sqrt(A1 A2)) = 28.
        let volume = mesh.signed_volume().abs();
        assert!((volume - 28.0).abs() < 1e-9, "got {volume}, want 28");
        let (low, high) = mesh.bounds().unwrap();
        assert!((high.z - 3.0).abs() < 1e-12 && (low.z).abs() < 1e-12);
    }

    #[test]
    fn a_taper_between_unlike_outlines_is_lofted_and_reported() {
        // A square to a circle: the corners do not correspond, so both outlines are
        // resampled and lofted, and the file is told the corners moved.
        let model = model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,4.);\n",
            "#2=IFCCIRCLEPROFILEDEF(.AREA.,$,$,1.);\n",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#4=IFCAXIS2PLACEMENT3D(#3,$,$);\n",
            "#5=IFCDIRECTION((0.,0.,1.));\n",
            "#6=IFCEXTRUDEDAREASOLIDTAPERED(#1,#4,#5,3.,#2);\n"
        ));
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 6);
        let mesh = mesh.unwrap();
        assert_eq!(mesh.closed, Some(true));
        let volume = mesh.signed_volume().abs();
        let (cylinder, prism) = (std::f64::consts::PI * 3.0, 48.0);
        assert!(
            volume > cylinder && volume < prism,
            "a loft lies between the two prisms, got {volume}"
        );
        let top_radius = mesh
            .positions
            .iter()
            .filter(|p| (p.z - 3.0).abs() < 1e-9)
            .map(|p| p.truncate().length())
            .fold(0.0, f64::max);
        assert!((top_radius - 1.0).abs() < 1e-9, "the top is the circle");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == crate::error::codes::PROFILE_DETAIL_APPROXIMATED),
            "the approximation has to be announced, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_taper_between_two_circles_is_an_exact_frustum() {
        // Different radii tessellate to different counts; resampling makes them agree.
        let model = model_of(concat!(
            "#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,0.2);\n",
            "#2=IFCCIRCLEPROFILEDEF(.AREA.,$,$,0.1);\n",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#4=IFCAXIS2PLACEMENT3D(#3,$,$);\n",
            "#5=IFCDIRECTION((0.,0.,1.));\n",
            "#6=IFCEXTRUDEDAREASOLIDTAPERED(#1,#4,#5,0.5,#2);\n"
        ));
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 6);
        let mesh = mesh.unwrap();
        let volume = mesh.signed_volume().abs();
        let frustum = 0.5 * std::f64::consts::PI * (0.04 + 0.02 + 0.01) / 3.0;
        assert!(
            (volume - frustum).abs() < 0.01 * frustum,
            "got {volume}, want {frustum}"
        );
        assert!(
            !diagnostics
                .iter()
                .any(|d| d.code == crate::error::codes::PROFILE_DETAIL_APPROXIMATED),
            "two circles need no apology, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_taper_whose_hole_corners_disagree_is_lofted_and_reported() {
        // Same 4 by 4 outline both ends, but a 4-corner hole against a 5-corner one.
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((-2.,-2.));\n",
            "#2=IFCCARTESIANPOINT((2.,-2.));\n",
            "#3=IFCCARTESIANPOINT((2.,2.));\n",
            "#4=IFCCARTESIANPOINT((-2.,2.));\n",
            "#5=IFCPOLYLINE((#1,#2,#3,#4,#1));\n",
            "#6=IFCCARTESIANPOINT((-0.5,-0.5));\n",
            "#7=IFCCARTESIANPOINT((-0.5,0.5));\n",
            "#8=IFCCARTESIANPOINT((0.5,0.5));\n",
            "#9=IFCCARTESIANPOINT((0.5,-0.5));\n",
            "#10=IFCPOLYLINE((#6,#7,#8,#9,#6));\n",
            "#11=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#5,(#10));\n",
            "#12=IFCCARTESIANPOINT((0.,0.7));\n",
            "#13=IFCPOLYLINE((#6,#7,#12,#8,#9,#6));\n",
            "#14=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#5,(#13));\n",
            "#15=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#16=IFCAXIS2PLACEMENT3D(#15,$,$);\n",
            "#17=IFCDIRECTION((0.,0.,1.));\n",
            "#18=IFCEXTRUDEDAREASOLIDTAPERED(#11,#16,#17,4.,#14);\n"
        ));
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 18);
        let mesh = mesh.unwrap();
        assert_eq!(mesh.closed, Some(true), "the loft stays closed");
        let volume = mesh.signed_volume().abs();
        // The hole is about a unit in area at both ends; resampling the square to
        // five corners trims it a little, so the band is loose but the hole is there.
        assert!(
            volume > 4.0 * (16.0 - 1.2) && volume < 4.0 * (16.0 - 0.8),
            "a lofted hole, got {volume}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == crate::error::codes::PROFILE_DETAIL_APPROXIMATED),
            "the approximation has to be announced, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_tapered_extrusion_is_not_silently_treated_as_a_prism() {
        // Without its own evaluator this class would fall back to a plain prism.
        let model = model_of(concat!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,4.);\n",
            "#2=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#4=IFCAXIS2PLACEMENT3D(#3,$,$);\n",
            "#5=IFCDIRECTION((0.,0.,1.));\n",
            "#6=IFCEXTRUDEDAREASOLIDTAPERED(#1,#4,#5,3.,#2);\n"
        ));
        let mesh = eval_solid(&model, 6).unwrap();
        let (_, high) = mesh.bounds().unwrap();
        let top_corners = mesh
            .positions
            .iter()
            .filter(|p| (p.z - high.z).abs() < 1e-9)
            .filter(|p| p.x.abs() > 1.5)
            .count();
        assert_eq!(
            top_corners, 0,
            "the top belongs to the end profile, not the start"
        );
    }

    #[test]
    fn a_boolean_tree_that_shares_its_leaves_is_built_once_per_node() {
        // Each level unions the level below with itself; without memoisation the
        // leaf would be evaluated once per path, two to the twentieth times.
        let mut source = format!("{UNIT_BOX}#9=IFCBOOLEANRESULT(.UNION.,#3,#3);\n");
        for id in 10..30 {
            source.push_str(&format!(
                "#{id}=IFCBOOLEANRESULT(.UNION.,#{},#{});\n",
                id - 1,
                id - 1
            ));
        }
        let model = model_of(&source);
        let mesh = eval_solid(&model, 29).unwrap();
        assert!(
            (mesh.signed_volume() - 8.0).abs() < 1e-6,
            "got {}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn a_long_clipping_chain_keeps_its_body() {
        // Thirty clips that remove nothing; the body used to be lost past twenty-four.
        let mut source = format!(
            "{UNIT_BOX}#4=IFCCARTESIANPOINT((0.,0.,-5.));\n#5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCPLANE(#5);\n#7=IFCHALFSPACESOLID(#6,.T.);\n"
        );
        let mut previous = 3;
        for id in 10..40 {
            source.push_str(&format!(
                "#{id}=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#{previous},#7);\n"
            ));
            previous = id;
        }
        let model = model_of(&source);
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 39);
        let mesh = mesh.unwrap();
        assert!(
            (mesh.signed_volume() - 8.0).abs() < 1e-9,
            "got {}",
            mesh.signed_volume()
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn a_hole_wound_like_its_outline_still_extrudes_right_way_out() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((4.,0.));\n\
             #3=IFCCARTESIANPOINT((4.,4.));\n#4=IFCCARTESIANPOINT((0.,4.));\n\
             #5=IFCPOLYLINE((#1,#2,#3,#4,#1));\n\
             #6=IFCCARTESIANPOINT((1.,1.));\n#7=IFCCARTESIANPOINT((2.,1.));\n\
             #8=IFCCARTESIANPOINT((2.,2.));\n#9=IFCCARTESIANPOINT((1.,2.));\n\
             #10=IFCPOLYLINE((#6,#7,#8,#9,#6));\n\
             #11=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#5,(#10));\n\
             #12=IFCDIRECTION((0.,0.,1.));\n\
             #13=IFCEXTRUDEDAREASOLID(#11,$,#12,2.);\n",
        );
        let mesh = eval_solid(&model, 13).unwrap();
        assert_eq!(mesh.closed, Some(true));
        assert!(
            (mesh.signed_volume() - 30.0).abs() < 1e-9,
            "a hole wall facing inwards would count wrong, got {}",
            mesh.signed_volume()
        );
    }
}
