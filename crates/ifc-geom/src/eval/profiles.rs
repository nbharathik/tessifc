// SPDX-License-Identifier: Apache-2.0
//! Profiles: the 2D outlines that every swept solid starts from.
//!
//! Every outline comes out in metres, in profile space, with `Position` applied.

use crate::context::EvalCtx;
use crate::error::GeomError;
use crate::placement::axis2_placement_2d;
use crate::registry::{Profile2D, ProfileEvaluator, Registry};
use glam::{DMat4, DVec2, DVec3};
use tessifc_model::Entity;

/// Apply a profile's optional `Position` to an outline.
fn place(profile: Entity<'_>, ctx: &EvalCtx<'_>, points: &mut [DVec2]) {
    let matrix = axis2_placement_2d(profile.attr("Position"), &ctx.units);
    if matrix == DMat4::IDENTITY {
        return;
    }
    for point in points {
        let moved = matrix.transform_point3(DVec3::new(point.x, point.y, 0.0));
        *point = DVec2::new(moved.x, moved.y);
    }
}

/// Read a closed loop of points from a 2D polyline or indexed curve.
fn loop_of(ctx: &EvalCtx<'_>, curve: Entity<'_>) -> Result<Vec<DVec2>, GeomError> {
    let registry = ctx.registry();
    let polyline = registry.curve(ctx, curve)?;
    let mut points: Vec<DVec2> = polyline
        .points
        .iter()
        .map(|point| DVec2::new(point.x, point.y))
        .collect();
    // Drop a repeated closing point; a zero-length edge breaks ear clipping.
    if points.len() > 1 {
        let first = points[0];
        if (points[points.len() - 1] - first).length() <= ctx.tol.len {
            points.pop();
        }
    }
    if points.len() < 3 {
        return Err(GeomError::Degenerate(format!(
            "a profile boundary with {} points encloses nothing",
            points.len()
        )));
    }
    Ok(points)
}

/// A circle, as a closed polygon.
fn circle_points(centre: DVec2, radius: f64, segments: u32) -> Vec<DVec2> {
    (0..segments)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / segments as f64;
            centre + DVec2::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect()
}

/// `IfcRectangleProfileDef` and its hollow and rounded relatives.
pub struct RectangleProfile;

impl ProfileEvaluator for RectangleProfile {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcRectangleProfileDef",
            "IfcRectangleHollowProfileDef",
            "IfcRoundedRectangleProfileDef",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let x = ctx.units.length(
            item.attr("XDim")
                .as_f64()
                .ok_or_else(|| GeomError::missing("XDim"))?,
        );
        let y = ctx.units.length(
            item.attr("YDim")
                .as_f64()
                .ok_or_else(|| GeomError::missing("YDim"))?,
        );
        if x <= ctx.tol.len || y <= ctx.tol.len {
            return Err(GeomError::Degenerate(format!(
                "a rectangle {x} by {y} has no area"
            )));
        }
        // Centred on its origin; XDim is the full width.
        let (hx, hy) = (x * 0.5, y * 0.5);
        let mut outer = vec![
            DVec2::new(-hx, -hy),
            DVec2::new(hx, -hy),
            DVec2::new(hx, hy),
            DVec2::new(-hx, hy),
        ];
        place(item, ctx, &mut outer);

        let mut profile = Profile2D::new(outer);
        if let Some(thickness) = item.attr("WallThickness").as_f64() {
            let thickness = ctx.units.length(thickness);
            if thickness > ctx.tol.len && thickness * 2.0 < x.min(y) {
                let (ix, iy) = (hx - thickness, hy - thickness);
                let mut hole = vec![
                    DVec2::new(-ix, -iy),
                    DVec2::new(-ix, iy),
                    DVec2::new(ix, iy),
                    DVec2::new(ix, -iy),
                ];
                place(item, ctx, &mut hole);
                profile.holes.push(hole);
            }
        }
        Ok(profile)
    }
}

/// `IfcCircleProfileDef` and `IfcCircleHollowProfileDef`.
pub struct CircleProfile;

impl ProfileEvaluator for CircleProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCircleProfileDef", "IfcCircleHollowProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let radius = ctx.units.length(
            item.attr("Radius")
                .as_f64()
                .ok_or_else(|| GeomError::missing("Radius"))?,
        );
        if radius <= ctx.tol.len {
            return Err(GeomError::Degenerate(format!(
                "a circle of radius {radius} has no area"
            )));
        }
        let segments = ctx.segments_for_radius(radius);
        let mut outer = circle_points(DVec2::ZERO, radius, segments);
        place(item, ctx, &mut outer);
        let mut profile = Profile2D::new(outer);

        if let Some(thickness) = item.attr("WallThickness").as_f64() {
            let thickness = ctx.units.length(thickness);
            if thickness > ctx.tol.len && thickness < radius {
                let mut hole = circle_points(DVec2::ZERO, radius - thickness, segments);
                // A hole is wound against the outer loop.
                hole.reverse();
                place(item, ctx, &mut hole);
                profile.holes.push(hole);
            }
        }
        Ok(profile)
    }
}

/// A doubly symmetric structural I section, including its four root fillets.
pub struct IShapeProfile;

impl ProfileEvaluator for IShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcIShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        if !item
            .class_name()
            .eq_ignore_ascii_case("IfcIShapeProfileDef")
        {
            return Err(GeomError::Unsupported(item.class_name()));
        }
        let dimension = |name| {
            item.attr(name)
                .as_f64()
                .map(|value| ctx.units.length(value))
                .ok_or_else(|| GeomError::missing(name))
        };
        let width = dimension("OverallWidth")?;
        let depth = dimension("OverallDepth")?;
        let web = dimension("WebThickness")?;
        let flange = dimension("FlangeThickness")?;
        if !width.is_finite()
            || !depth.is_finite()
            || !web.is_finite()
            || !flange.is_finite()
            || width <= ctx.tol.len
            || depth <= ctx.tol.len
            || web <= ctx.tol.len
            || flange <= ctx.tol.len
            || web >= width - ctx.tol.len
            || flange * 2.0 >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "an I profile whose flange or web dimensions do not fit its overall size".into(),
            ));
        }
        let edge_radius = item
            .attr("FlangeEdgeRadius")
            .as_f64()
            .map(|value| ctx.units.length(value))
            .unwrap_or(0.0);
        let slope = item.attr("FlangeSlope").as_f64().unwrap_or(0.0);
        note_dropped_details(
            ctx,
            item,
            &[("FlangeEdgeRadius", edge_radius), ("FlangeSlope", slope)],
        );
        let maximum_fillet = ((width - web) * 0.5).min((depth - flange * 2.0) * 0.5);
        let fillet = item
            .attr("FilletRadius")
            .as_f64()
            .map(|value| ctx.units.length(value))
            .unwrap_or(0.0);
        if !fillet.is_finite() || fillet < 0.0 || fillet > maximum_fillet + ctx.tol.len {
            return Err(GeomError::Degenerate(
                "an I profile whose root fillet does not fit between web and flanges".into(),
            ));
        }

        let (half_width, half_depth, half_web) = (width * 0.5, depth * 0.5, web * 0.5);
        let bottom = -half_depth + flange;
        let top = half_depth - flange;
        let mut outer = vec![
            DVec2::new(-half_width, -half_depth),
            DVec2::new(half_width, -half_depth),
            DVec2::new(half_width, bottom),
        ];
        if fillet > ctx.tol.len {
            let quarter_segments = ctx.segments_for_radius(fillet).div_ceil(4).max(2);
            outer.push(DVec2::new(half_web + fillet, bottom));
            push_arc(
                &mut outer,
                DVec2::new(half_web + fillet, bottom + fillet),
                fillet,
                -std::f64::consts::FRAC_PI_2,
                -std::f64::consts::PI,
                quarter_segments,
            );
            outer.push(DVec2::new(half_web, top - fillet));
            push_arc(
                &mut outer,
                DVec2::new(half_web + fillet, top - fillet),
                fillet,
                std::f64::consts::PI,
                std::f64::consts::FRAC_PI_2,
                quarter_segments,
            );
        } else {
            outer.push(DVec2::new(half_web, bottom));
            outer.push(DVec2::new(half_web, top));
        }
        outer.extend([
            DVec2::new(half_width, top),
            DVec2::new(half_width, half_depth),
            DVec2::new(-half_width, half_depth),
            DVec2::new(-half_width, top),
        ]);
        if fillet > ctx.tol.len {
            let quarter_segments = ctx.segments_for_radius(fillet).div_ceil(4).max(2);
            outer.push(DVec2::new(-half_web - fillet, top));
            push_arc(
                &mut outer,
                DVec2::new(-half_web - fillet, top - fillet),
                fillet,
                std::f64::consts::FRAC_PI_2,
                0.0,
                quarter_segments,
            );
            outer.push(DVec2::new(-half_web, bottom + fillet));
            push_arc(
                &mut outer,
                DVec2::new(-half_web - fillet, bottom + fillet),
                fillet,
                0.0,
                -std::f64::consts::FRAC_PI_2,
                quarter_segments,
            );
        } else {
            outer.push(DVec2::new(-half_web, top));
            outer.push(DVec2::new(-half_web, bottom));
        }
        outer.push(DVec2::new(-half_width, bottom));
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

fn push_arc(
    points: &mut Vec<DVec2>,
    centre: DVec2,
    radius: f64,
    start: f64,
    end: f64,
    segments: u32,
) {
    for step in 1..=segments {
        let amount = step as f64 / segments as f64;
        let angle = start + (end - start) * amount;
        points.push(centre + DVec2::new(angle.cos(), angle.sin()) * radius);
    }
}

/// `IfcArbitraryClosedProfileDef`, the variant with voids, and the open profile.
pub struct ArbitraryProfile;

impl ProfileEvaluator for ArbitraryProfile {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcArbitraryClosedProfileDef",
            "IfcArbitraryProfileDefWithVoids",
            "IfcArbitraryOpenProfileDef",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        if item.is_a("IfcArbitraryOpenProfileDef") {
            // An open profile keeps its ends apart: it is a polyline, not a loop.
            let curve = item
                .attr("Curve")
                .as_entity()
                .ok_or_else(|| GeomError::missing("Curve"))?;
            let polyline = ctx.registry().curve(ctx, curve)?;
            let mut points: Vec<DVec2> = polyline
                .points
                .iter()
                .map(|point| DVec2::new(point.x, point.y))
                .collect();
            points.dedup_by(|a, b| (*a - *b).length() <= ctx.tol.len);
            if points.len() < 2 {
                return Err(GeomError::Degenerate(
                    "an open profile with fewer than two points".into(),
                ));
            }
            return Ok(Profile2D::open(points));
        }
        let boundary = item
            .attr("OuterCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("OuterCurve"))?;
        let outer = loop_of(ctx, boundary)?;
        let mut profile = Profile2D::new(outer);

        if let Some(inner) = item.attr("InnerCurves").as_list() {
            for value in inner {
                let Some(curve) = value.as_entity() else {
                    continue;
                };
                match loop_of(ctx, curve) {
                    Ok(hole) => profile.holes.push(hole),
                    Err(error) => {
                        // An unreadable hole is not cut; better a solid slab than none.
                        ctx.diag.warn(
                            crate::error::codes::DEGENERATE_GEOMETRY,
                            curve.id(),
                            format!("inner boundary skipped: {error}"),
                        );
                    }
                }
            }
        }
        // A hole wound like the outer boundary would put its side walls inside out.
        let outward = tessifc_mesh::signed_area(&profile.outer) >= 0.0;
        for hole in &mut profile.holes {
            if (tessifc_mesh::signed_area(hole) >= 0.0) == outward {
                hole.reverse();
            }
        }
        Ok(profile)
    }
}

/// `IfcAsymmetricIShapeProfileDef`: an I whose two flanges differ.
///
/// IFC2X3 spells the bottom flange with the plain I profile's attribute names.
pub struct AsymmetricIShapeProfile;

impl ProfileEvaluator for AsymmetricIShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcAsymmetricIShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let first = |names: &[&'static str]| -> Option<f64> {
            names
                .iter()
                .find_map(|name| item.attr(name).as_f64())
                .map(|value| ctx.units.length(value))
        };
        let required = |names: &[&'static str]| -> Result<f64, GeomError> {
            let value = first(names).ok_or_else(|| GeomError::missing(names[0]))?;
            if !value.is_finite() {
                return Err(GeomError::Degenerate(format!(
                    "{} is not a number",
                    names[0]
                )));
            }
            Ok(value)
        };
        let bottom_width = required(&["BottomFlangeWidth", "OverallWidth"])?;
        let depth = required(&["OverallDepth"])?;
        let web = required(&["WebThickness"])?;
        let bottom_flange = required(&["BottomFlangeThickness", "FlangeThickness"])?;
        let top_width = required(&["TopFlangeWidth"])?;
        let top_flange = first(&["TopFlangeThickness"])
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(bottom_flange);
        if bottom_width <= ctx.tol.len
            || top_width <= ctx.tol.len
            || depth <= ctx.tol.len
            || web <= ctx.tol.len
            || bottom_flange <= ctx.tol.len
            || web >= bottom_width.min(top_width) - ctx.tol.len
            || bottom_flange + top_flange >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "an asymmetric I profile whose flanges or web do not fit its overall size".into(),
            ));
        }
        note_dropped_details(
            ctx,
            item,
            &[
                (
                    "BottomFlangeEdgeRadius",
                    optional_length(ctx, item, "BottomFlangeEdgeRadius"),
                ),
                (
                    "BottomFlangeSlope",
                    item.attr("BottomFlangeSlope").as_f64().unwrap_or(0.0),
                ),
                (
                    "TopFlangeEdgeRadius",
                    optional_length(ctx, item, "TopFlangeEdgeRadius"),
                ),
                (
                    "TopFlangeSlope",
                    item.attr("TopFlangeSlope").as_f64().unwrap_or(0.0),
                ),
            ],
        );

        let (hd, hw) = (depth * 0.5, web * 0.5);
        let (bw, tw) = (bottom_width * 0.5, top_width * 0.5);
        let bottom_inner = -hd + bottom_flange;
        let top_inner = hd - top_flange;
        let web_height = top_inner - bottom_inner;
        let bottom_fillet = first(&["BottomFlangeFilletRadius", "FilletRadius"])
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(0.0)
            .min(bw - hw)
            .min(web_height * 0.5);
        let top_fillet = optional_length(ctx, item, "TopFlangeFilletRadius")
            .min(tw - hw)
            .min(web_height * 0.5);

        let mut outer = vec![
            DVec2::new(-bw, -hd),
            DVec2::new(bw, -hd),
            DVec2::new(bw, bottom_inner),
        ];
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(hw, bottom_inner),
            DVec2::new(hw, top_inner),
            bottom_fillet,
        );
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(hw, top_inner),
            DVec2::new(tw, top_inner),
            top_fillet,
        );
        outer.extend([
            DVec2::new(tw, top_inner),
            DVec2::new(tw, hd),
            DVec2::new(-tw, hd),
            DVec2::new(-tw, top_inner),
        ]);
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(-hw, top_inner),
            DVec2::new(-hw, bottom_inner),
            top_fillet,
        );
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(-hw, bottom_inner),
            DVec2::new(-bw, bottom_inner),
            bottom_fillet,
        );
        outer.push(DVec2::new(-bw, bottom_inner));
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcDerivedProfileDef`: another profile with a transform applied.
///
/// `IfcMirroredProfileDef` derives its operator, so the mirror is applied here.
pub struct DerivedProfile;

impl ProfileEvaluator for DerivedProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcDerivedProfileDef", "IfcMirroredProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let parent = item
            .attr("ParentProfile")
            .as_entity()
            .ok_or_else(|| GeomError::missing("ParentProfile"))?;
        let registry = ctx.registry();
        let mut profile = registry.profile(ctx, parent)?;

        if let Some(operator) = item.attr("Operator").as_entity() {
            let matrix = crate::placement::transformation_operator(operator, &ctx.units);
            let apply = |points: &mut Vec<DVec2>| {
                for point in points.iter_mut() {
                    let moved = matrix.transform_point3(DVec3::new(point.x, point.y, 0.0));
                    *point = DVec2::new(moved.x, moved.y);
                }
            };
            apply(&mut profile.outer);
            for hole in &mut profile.holes {
                apply(hole);
            }
        }
        if item.is_a("IfcMirroredProfileDef") {
            // Mirror about the parent's y axis; reversing keeps the winding.
            let mirror = |points: &mut Vec<DVec2>| {
                for point in points.iter_mut() {
                    point.x = -point.x;
                }
                points.reverse();
            };
            mirror(&mut profile.outer);
            for hole in &mut profile.holes {
                mirror(hole);
            }
        }
        Ok(profile)
    }
}

/// `IfcCompositeProfileDef`: several profiles side by side.
///
/// `Profile2D` carries one outer loop, so the largest wins and the rest are reported.
pub struct CompositeProfile;

impl ProfileEvaluator for CompositeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCompositeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let profiles = item
            .attr("Profiles")
            .as_list()
            .ok_or_else(|| GeomError::missing("Profiles"))?;
        let registry = ctx.registry();
        let mut best: Option<(f64, Profile2D)> = None;
        let mut count = 0;
        for value in profiles {
            let Some(entity) = value.as_entity() else {
                continue;
            };
            count += 1;
            let Ok(profile) = registry.profile(ctx, entity) else {
                continue;
            };
            let area = tessifc_mesh::signed_area(&profile.outer).abs();
            if best
                .as_ref()
                .map(|(current, _)| area > *current)
                .unwrap_or(true)
            {
                best = Some((area, profile));
            }
        }
        let (_, profile) = best.ok_or_else(|| {
            GeomError::Degenerate("a composite profile with no readable parts".into())
        })?;
        if count > 1 {
            ctx.diag.warn(
                crate::error::codes::DEGENERATE_GEOMETRY,
                item.id(),
                format!("composite profile: {count} parts, only the largest is swept"),
            );
        }
        Ok(profile)
    }
}

/// Read a length attribute and convert it to metres.
fn length_of(ctx: &EvalCtx<'_>, item: Entity<'_>, name: &'static str) -> Result<f64, GeomError> {
    let value = item
        .attr(name)
        .as_f64()
        .ok_or_else(|| GeomError::missing(name))?;
    let metres = ctx.units.length(value);
    if !metres.is_finite() {
        return Err(GeomError::Degenerate(format!("{name} is not a number")));
    }
    Ok(metres)
}

/// An optional non-negative length in metres; absent or nonsense reads as zero.
fn optional_length(ctx: &EvalCtx<'_>, item: Entity<'_>, name: &str) -> f64 {
    match item.attr(name).as_f64() {
        Some(value) => {
            let metres = ctx.units.length(value);
            if metres.is_finite() && metres > 0.0 {
                metres
            } else {
                0.0
            }
        }
        None => 0.0,
    }
}

/// Say that a detail of a parametric profile was flattened.
///
/// Edge radii and slopes are millimetres on a section; dropping the beam would lose more.
fn note_dropped_details(ctx: &EvalCtx<'_>, item: Entity<'_>, dropped: &[(&str, f64)]) {
    let named: Vec<&str> = dropped
        .iter()
        .filter(|(_, value)| value.abs() > ctx.tol.len)
        .map(|(name, _)| *name)
        .collect();
    if named.is_empty() {
        return;
    }
    ctx.diag.warn(
        crate::error::codes::PROFILE_DETAIL_APPROXIMATED,
        item.id(),
        format!(
            "{}: {} ignored, corners left square",
            item.class_name(),
            named.join(" and ")
        ),
    );
}

/// Round off the concave corner the outline is about to turn at, filling it in.
///
/// Convex turns and negligible radii push the corner unrounded.
fn push_root_fillet(
    points: &mut Vec<DVec2>,
    ctx: &EvalCtx<'_>,
    corner: DVec2,
    next: DVec2,
    radius: f64,
) {
    let Some(previous) = points.last().copied() else {
        points.push(corner);
        return;
    };
    let along = (corner - previous).normalize_or_zero();
    let towards = (next - corner).normalize_or_zero();
    // Wound counter-clockwise, so a right turn is the concave corner.
    let turn = along.x * towards.y - along.y * towards.x;
    if radius <= ctx.tol.len || along == DVec2::ZERO || towards == DVec2::ZERO || turn >= 0.0 {
        points.push(corner);
        return;
    }
    let start = corner - along * radius;
    let end = corner + towards * radius;
    let centre = start + towards * radius;
    let start_angle = (start - centre).to_angle();
    let mut end_angle = (end - centre).to_angle();
    // The short way round, never the reflex angle.
    while end_angle - start_angle > std::f64::consts::PI {
        end_angle -= std::f64::consts::TAU;
    }
    while start_angle - end_angle > std::f64::consts::PI {
        end_angle += std::f64::consts::TAU;
    }
    let segments = ctx.segments_for_radius(radius).div_ceil(4).max(2);
    points.push(start);
    push_arc(points, centre, radius, start_angle, end_angle, segments);
}

/// `IfcLShapeProfileDef`: an angle section.
///
/// Centred on its bounding box, with the corner of the L at the lower left.
pub struct LShapeProfile;

impl ProfileEvaluator for LShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcLShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let depth = length_of(ctx, item, "Depth")?;
        // Width is optional: absent means an equal-legged angle.
        let width = match item.attr("Width").as_f64() {
            Some(value) => ctx.units.length(value),
            None => depth,
        };
        let thickness = length_of(ctx, item, "Thickness")?;
        if depth <= ctx.tol.len
            || width <= ctx.tol.len
            || thickness <= ctx.tol.len
            || thickness >= width - ctx.tol.len
            || thickness >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "an L profile whose thickness does not fit inside its legs".into(),
            ));
        }
        let edge_radius = optional_length(ctx, item, "EdgeRadius");
        let slope = item.attr("LegSlope").as_f64().unwrap_or(0.0);
        note_dropped_details(
            ctx,
            item,
            &[("EdgeRadius", edge_radius), ("LegSlope", slope)],
        );
        let fillet = optional_length(ctx, item, "FilletRadius")
            .min(width - thickness)
            .min(depth - thickness);

        let (hw, hd) = (width * 0.5, depth * 0.5);
        let inner_x = -hw + thickness;
        let inner_y = -hd + thickness;
        let mut outer = vec![
            DVec2::new(-hw, -hd),
            DVec2::new(hw, -hd),
            DVec2::new(hw, inner_y),
        ];
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(inner_x, inner_y),
            DVec2::new(inner_x, hd),
            fillet,
        );
        outer.push(DVec2::new(inner_x, hd));
        outer.push(DVec2::new(-hw, hd));
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcUShapeProfileDef`: a channel, web on the left, opening towards +x.
pub struct UShapeProfile;

impl ProfileEvaluator for UShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcUShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let depth = length_of(ctx, item, "Depth")?;
        let width = length_of(ctx, item, "FlangeWidth")?;
        let web = length_of(ctx, item, "WebThickness")?;
        let flange = length_of(ctx, item, "FlangeThickness")?;
        if depth <= ctx.tol.len
            || width <= ctx.tol.len
            || web <= ctx.tol.len
            || flange <= ctx.tol.len
            || web >= width - ctx.tol.len
            || flange * 2.0 >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "a U profile whose web or flanges do not fit its overall size".into(),
            ));
        }
        let edge_radius = optional_length(ctx, item, "EdgeRadius");
        let slope = item.attr("FlangeSlope").as_f64().unwrap_or(0.0);
        note_dropped_details(
            ctx,
            item,
            &[("EdgeRadius", edge_radius), ("FlangeSlope", slope)],
        );
        let fillet = optional_length(ctx, item, "FilletRadius")
            .min(width - web)
            .min((depth - flange * 2.0) * 0.5);

        let (hw, hd) = (width * 0.5, depth * 0.5);
        let web_face = -hw + web;
        let (bottom, top) = (-hd + flange, hd - flange);
        let mut outer = vec![
            DVec2::new(-hw, -hd),
            DVec2::new(hw, -hd),
            DVec2::new(hw, bottom),
        ];
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(web_face, bottom),
            DVec2::new(web_face, top),
            fillet,
        );
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(web_face, top),
            DVec2::new(hw, top),
            fillet,
        );
        outer.push(DVec2::new(hw, top));
        outer.push(DVec2::new(hw, hd));
        outer.push(DVec2::new(-hw, hd));
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcCShapeProfileDef`: a lipped channel; `Girth` is the lip along the flange outside.
pub struct CShapeProfile;

impl ProfileEvaluator for CShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let depth = length_of(ctx, item, "Depth")?;
        let width = length_of(ctx, item, "Width")?;
        let wall = length_of(ctx, item, "WallThickness")?;
        let girth = length_of(ctx, item, "Girth")?;
        if depth <= ctx.tol.len
            || width <= ctx.tol.len
            || wall <= ctx.tol.len
            || wall * 2.0 >= width - ctx.tol.len
            || wall * 2.0 >= depth - ctx.tol.len
            || girth <= wall
            || girth * 2.0 >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "a C profile whose wall or girth does not fit its overall size".into(),
            ));
        }
        // The internal fillet is modelled as a square fold.
        note_dropped_details(
            ctx,
            item,
            &[(
                "InternalFilletRadius",
                optional_length(ctx, item, "InternalFilletRadius"),
            )],
        );

        let (hw, hd) = (width * 0.5, depth * 0.5);
        let mut outer = vec![
            DVec2::new(-hw, -hd),
            DVec2::new(hw, -hd),
            DVec2::new(hw, -hd + girth),
            DVec2::new(hw - wall, -hd + girth),
            DVec2::new(hw - wall, -hd + wall),
            DVec2::new(-hw + wall, -hd + wall),
            DVec2::new(-hw + wall, hd - wall),
            DVec2::new(hw - wall, hd - wall),
            DVec2::new(hw - wall, hd - girth),
            DVec2::new(hw, hd - girth),
            DVec2::new(hw, hd),
            DVec2::new(-hw, hd),
        ];
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcTShapeProfileDef`: flange on top, web hanging from its centre.
pub struct TShapeProfile;

impl ProfileEvaluator for TShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcTShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let depth = length_of(ctx, item, "Depth")?;
        let width = length_of(ctx, item, "FlangeWidth")?;
        let web = length_of(ctx, item, "WebThickness")?;
        let flange = length_of(ctx, item, "FlangeThickness")?;
        if depth <= ctx.tol.len
            || width <= ctx.tol.len
            || web <= ctx.tol.len
            || flange <= ctx.tol.len
            || web >= width - ctx.tol.len
            || flange >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "a T profile whose web or flange does not fit its overall size".into(),
            ));
        }
        note_dropped_details(
            ctx,
            item,
            &[
                (
                    "FlangeEdgeRadius",
                    optional_length(ctx, item, "FlangeEdgeRadius"),
                ),
                ("WebEdgeRadius", optional_length(ctx, item, "WebEdgeRadius")),
                ("WebSlope", item.attr("WebSlope").as_f64().unwrap_or(0.0)),
                (
                    "FlangeSlope",
                    item.attr("FlangeSlope").as_f64().unwrap_or(0.0),
                ),
            ],
        );
        let fillet = optional_length(ctx, item, "FilletRadius")
            .min((width - web) * 0.5)
            .min(depth - flange);

        let (hw, hd, hweb) = (width * 0.5, depth * 0.5, web * 0.5);
        let under = hd - flange;
        let mut outer = vec![DVec2::new(-hweb, -hd), DVec2::new(hweb, -hd)];
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(hweb, under),
            DVec2::new(hw, under),
            fillet,
        );
        outer.push(DVec2::new(hw, under));
        outer.push(DVec2::new(hw, hd));
        outer.push(DVec2::new(-hw, hd));
        outer.push(DVec2::new(-hw, under));
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(-hweb, under),
            DVec2::new(-hweb, -hd),
            fillet,
        );
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcZShapeProfileDef`: a web with one flange each way.
///
/// The section is `2 * FlangeWidth - WebThickness` wide overall.
pub struct ZShapeProfile;

impl ProfileEvaluator for ZShapeProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcZShapeProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let depth = length_of(ctx, item, "Depth")?;
        let width = length_of(ctx, item, "FlangeWidth")?;
        let web = length_of(ctx, item, "WebThickness")?;
        let flange = length_of(ctx, item, "FlangeThickness")?;
        if depth <= ctx.tol.len
            || width <= ctx.tol.len
            || web <= ctx.tol.len
            || flange <= ctx.tol.len
            || web >= width - ctx.tol.len
            || flange * 2.0 >= depth - ctx.tol.len
        {
            return Err(GeomError::Degenerate(
                "a Z profile whose web or flanges do not fit its overall size".into(),
            ));
        }
        note_dropped_details(
            ctx,
            item,
            &[("EdgeRadius", optional_length(ctx, item, "EdgeRadius"))],
        );
        let fillet = optional_length(ctx, item, "FilletRadius")
            .min(width - web)
            .min((depth - flange * 2.0) * 0.5);

        let hd = depth * 0.5;
        let hweb = web * 0.5;
        let reach = width - hweb;
        let (bottom, top) = (-hd + flange, hd - flange);
        let mut outer = vec![
            DVec2::new(-hweb, -hd),
            DVec2::new(reach, -hd),
            DVec2::new(reach, bottom),
        ];
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(hweb, bottom),
            DVec2::new(hweb, hd),
            fillet,
        );
        outer.push(DVec2::new(hweb, hd));
        outer.push(DVec2::new(-reach, hd));
        outer.push(DVec2::new(-reach, top));
        push_root_fillet(
            &mut outer,
            ctx,
            DVec2::new(-hweb, top),
            DVec2::new(-hweb, -hd),
            fillet,
        );
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcTrapeziumProfileDef`.
///
/// The bottom is centred on the origin; the top starts `TopXOffset` from its left end.
pub struct TrapeziumProfile;

impl ProfileEvaluator for TrapeziumProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcTrapeziumProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let bottom = length_of(ctx, item, "BottomXDim")?;
        let top = length_of(ctx, item, "TopXDim")?;
        let height = length_of(ctx, item, "YDim")?;
        let offset = item
            .attr("TopXOffset")
            .as_f64()
            .map(|value| ctx.units.length(value))
            .unwrap_or(0.0);
        if bottom <= ctx.tol.len || top <= ctx.tol.len || height <= ctx.tol.len {
            return Err(GeomError::Degenerate(
                "a trapezium with a non-positive extent".into(),
            ));
        }
        let (hb, hh) = (bottom * 0.5, height * 0.5);
        let left = -hb + offset;
        let mut outer = vec![
            DVec2::new(-hb, -hh),
            DVec2::new(hb, -hh),
            DVec2::new(left + top, hh),
            DVec2::new(left, hh),
        ];
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcEllipseProfileDef`.
pub struct EllipseProfile;

impl ProfileEvaluator for EllipseProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcEllipseProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let first = length_of(ctx, item, "SemiAxis1")?;
        let second = length_of(ctx, item, "SemiAxis2")?;
        if first <= ctx.tol.len || second <= ctx.tol.len {
            return Err(GeomError::Degenerate(
                "an ellipse with a non-positive semi-axis".into(),
            ));
        }
        // The longer axis decides the segment count.
        let segments = ctx.segments_for_radius(first.max(second));
        let mut outer: Vec<DVec2> = (0..segments)
            .map(|index| {
                let angle = std::f64::consts::TAU * index as f64 / segments as f64;
                DVec2::new(first * angle.cos(), second * angle.sin())
            })
            .collect();
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// `IfcCenterLineProfileDef`: a curve given a thickness.
///
/// The area is the curve offset by half the thickness each way.
pub struct CenterLineProfile;

impl ProfileEvaluator for CenterLineProfile {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCenterLineProfileDef"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Profile2D, GeomError> {
        let curve = item
            .attr("Curve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Curve"))?;
        let thickness = length_of(ctx, item, "Thickness")?;
        if thickness <= ctx.tol.len {
            return Err(GeomError::Degenerate(
                "a centre-line profile with no thickness".into(),
            ));
        }
        let polyline = ctx.registry().curve(ctx, curve)?;
        let mut centre: Vec<DVec2> = Vec::with_capacity(polyline.points.len());
        for point in &polyline.points {
            let flat = DVec2::new(point.x, point.y);
            if centre
                .last()
                .is_none_or(|previous: &DVec2| (*previous - flat).length() > ctx.tol.len)
            {
                centre.push(flat);
            }
        }
        if centre.len() < 2 {
            return Err(GeomError::Degenerate(
                "a centre line of fewer than two distinct points".into(),
            ));
        }

        let half = thickness * 0.5;
        let left = offset_polyline(&centre, half);
        let mut right = offset_polyline(&centre, -half);
        right.reverse();
        let mut outer = left;
        outer.extend(right);
        place(item, ctx, &mut outer);
        Ok(Profile2D::new(outer))
    }
}

/// Offset an open polyline sideways by `distance`, mitring the corners.
///
/// A fold too sharp for a mitre falls back to the plain normal rather than infinity.
fn offset_polyline(points: &[DVec2], distance: f64) -> Vec<DVec2> {
    let normal_of = |from: DVec2, to: DVec2| {
        let direction = (to - from).normalize_or_zero();
        DVec2::new(-direction.y, direction.x)
    };
    let mut out = Vec::with_capacity(points.len());
    for index in 0..points.len() {
        let previous = index
            .checked_sub(1)
            .map(|i| normal_of(points[i], points[index]));
        let next = points
            .get(index + 1)
            .map(|after| normal_of(points[index], *after));
        let offset = match (previous, next) {
            (Some(before), Some(after)) => {
                let bisector = (before + after).normalize_or_zero();
                let cosine = bisector.dot(after);
                if cosine.abs() < 0.25 {
                    after
                } else {
                    bisector / cosine
                }
            }
            (Some(before), None) => before,
            (None, Some(after)) => after,
            (None, None) => DVec2::ZERO,
        };
        out.push(points[index] + offset * distance);
    }
    out
}

/// Register every profile evaluator.
pub fn register(registry: &mut Registry) {
    registry.register_profile(Box::new(RectangleProfile));
    registry.register_profile(Box::new(CircleProfile));
    registry.register_profile(Box::new(IShapeProfile));
    registry.register_profile(Box::new(AsymmetricIShapeProfile));
    registry.register_profile(Box::new(ArbitraryProfile));
    registry.register_profile(Box::new(DerivedProfile));
    registry.register_profile(Box::new(CompositeProfile));
    registry.register_profile(Box::new(LShapeProfile));
    registry.register_profile(Box::new(UShapeProfile));
    registry.register_profile(Box::new(CShapeProfile));
    registry.register_profile(Box::new(TShapeProfile));
    registry.register_profile(Box::new(ZShapeProfile));
    registry.register_profile(Box::new(TrapeziumProfile));
    registry.register_profile(Box::new(EllipseProfile));
    registry.register_profile(Box::new(CenterLineProfile));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tests::{eval_profile, model_of};

    #[test]
    fn a_rectangle_is_centred_on_its_origin() {
        // XDim is the full width, not a half width.
        let model = model_of("#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,2.);\n");
        let profile = eval_profile(&model, 1).unwrap();
        assert_eq!(profile.outer.len(), 4);
        let area = tessifc_mesh::signed_area(&profile.outer).abs();
        assert!((area - 8.0).abs() < 1e-12, "got {area}");
        let xs: Vec<f64> = profile.outer.iter().map(|p| p.x).collect();
        assert!((xs.iter().cloned().fold(f64::MAX, f64::min) + 2.0).abs() < 1e-12);
        assert!((xs.iter().cloned().fold(f64::MIN, f64::max) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn a_hollow_rectangle_gets_a_hole() {
        let model = model_of("#1=IFCRECTANGLEHOLLOWPROFILEDEF(.AREA.,$,$,4.,2.,0.5,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        assert_eq!(profile.holes.len(), 1);
        let hole = tessifc_mesh::signed_area(&profile.holes[0]).abs();
        assert!((hole - 3.0).abs() < 1e-12, "got {hole}");
    }

    #[test]
    fn an_asymmetric_i_has_the_area_of_its_three_plates() {
        let model = model_of(
            "#1=IFCASYMMETRICISHAPEPROFILEDEF(.AREA.,'I',$,0.2,0.4,0.012,0.02,$,0.12,0.016,$,$,$,$,$);\n",
        );
        let profile = eval_profile(&model, 1).unwrap();
        let area = tessifc_mesh::signed_area(&profile.outer);
        let expected = 0.2 * 0.02 + 0.012 * (0.4 - 0.02 - 0.016) + 0.12 * 0.016;
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        let high = profile
            .outer
            .iter()
            .fold(DVec2::splat(f64::NEG_INFINITY), |a, b| a.max(*b));
        // The wider flange is at the bottom, so the top edge is the narrow one.
        assert!((high.y - 0.2).abs() < 1e-12 && (high.x - 0.1).abs() < 1e-12);
        let top_width = profile
            .outer
            .iter()
            .filter(|p| (p.y - 0.2).abs() < 1e-12)
            .map(|p| p.x)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), x| {
                (lo.min(x), hi.max(x))
            });
        assert!((top_width.1 - top_width.0 - 0.12).abs() < 1e-12);
    }

    #[test]
    fn a_mirrored_profile_is_its_parent_flipped_about_y() {
        let model = model_of(concat!(
            "#1=IFCLSHAPEPROFILEDEF(.AREA.,'L',$,0.2,0.1,0.01,$,$,$);\n",
            "#2=IFCMIRROREDPROFILEDEF(.AREA.,'mirror',#1,$,'m');\n",
        ));
        let parent = eval_profile(&model, 1).unwrap();
        let mirrored = eval_profile(&model, 2).unwrap();
        let flipped: Vec<DVec2> = parent
            .outer
            .iter()
            .rev()
            .map(|p| DVec2::new(-p.x, p.y))
            .collect();
        assert_eq!(mirrored.outer.len(), flipped.len());
        // Same loop up to a rotation of the start index.
        let start = flipped
            .iter()
            .position(|p| (*p - mirrored.outer[0]).length() < 1e-12)
            .expect("the mirrored start corner exists in the flipped parent");
        for (index, point) in mirrored.outer.iter().enumerate() {
            let expected = flipped[(start + index) % flipped.len()];
            assert!(
                (*point - expected).length() < 1e-12,
                "{point} vs {expected}"
            );
        }
        let area = tessifc_mesh::signed_area(&mirrored.outer);
        assert!(area > 0.0, "the winding is kept");
    }

    #[test]
    fn an_i_shape_includes_its_root_fillets() {
        let model = model_of("#1=IFCISHAPEPROFILEDEF(.AREA.,'I',$,0.3,0.4,0.02,0.03,0.01,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let area = tessifc_mesh::signed_area(&profile.outer).abs();
        let sharp = 2.0 * 0.3 * 0.03 + (0.4 - 2.0 * 0.03) * 0.02;
        let fillets = 4.0 * 0.01f64.powi(2) * (1.0 - std::f64::consts::FRAC_PI_4);
        assert!((area - (sharp + fillets)).abs() < 2e-5, "got {area}");
        let low = profile
            .outer
            .iter()
            .fold(DVec2::splat(f64::INFINITY), |a, b| a.min(*b));
        let high = profile
            .outer
            .iter()
            .fold(DVec2::splat(f64::NEG_INFINITY), |a, b| a.max(*b));
        assert!((low - DVec2::new(-0.15, -0.2)).length() < 1e-12);
        assert!((high - DVec2::new(0.15, 0.2)).length() < 1e-12);
    }

    #[test]
    fn an_i_shape_with_sloped_flanges_is_drawn_square_and_says_so() {
        let model = model_of("#1=IFCISHAPEPROFILEDEF(.AREA.,'I',$,0.3,0.4,0.02,0.03,$,$,0.1);\n");
        let (profile, diagnostics) = crate::eval::tests::eval_profile_with_diagnostics(&model, 1);
        let profile = profile.unwrap();
        let area = tessifc_mesh::signed_area(&profile.outer).abs();
        let sharp = 2.0 * 0.3 * 0.03 + (0.4 - 2.0 * 0.03) * 0.02;
        assert!((area - sharp).abs() < 1e-12, "got {area}");
        assert!(
            diagnostics.iter().any(|item| {
                item.code == crate::error::codes::PROFILE_DETAIL_APPROXIMATED
                    && item.message.contains("FlangeSlope")
            }),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_wall_thickness_that_would_eat_the_profile_is_ignored() {
        let model = model_of("#1=IFCRECTANGLEHOLLOWPROFILEDEF(.AREA.,$,$,4.,2.,5.,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        assert!(
            profile.holes.is_empty(),
            "a 5 m wall in a 2 m section is not a hole"
        );
    }

    #[test]
    fn a_circle_encloses_the_right_area() {
        let model = model_of("#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,1.);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let area = tessifc_mesh::signed_area(&profile.outer).abs();
        let exact = std::f64::consts::PI;
        // A polygon inscribed in the circle is always a little smaller.
        assert!(
            area < exact,
            "an inscribed polygon cannot exceed the circle"
        );
        assert!(
            (area - exact).abs() / exact < 0.02,
            "got {area}, wanted about {exact}"
        );
    }

    #[test]
    fn a_zero_radius_circle_is_refused() {
        let model = model_of("#1=IFCCIRCLEPROFILEDEF(.AREA.,$,$,0.);\n");
        assert!(matches!(
            eval_profile(&model, 1),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn a_profile_position_moves_the_outline() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((10.,0.));\n\
             #2=IFCAXIS2PLACEMENT2D(#1,$);\n\
             #3=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,4.,2.);\n",
        );
        let profile = eval_profile(&model, 3).unwrap();
        let centre: DVec2 = profile
            .outer
            .iter()
            .copied()
            .fold(DVec2::ZERO, |a, b| a + b)
            / profile.outer.len() as f64;
        assert!(
            (centre - DVec2::new(10.0, 0.0)).length() < 1e-12,
            "got {centre}"
        );
    }

    #[test]
    fn an_arbitrary_profile_reads_its_outer_curve() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((3.,0.));\n\
             #3=IFCCARTESIANPOINT((3.,2.));\n#4=IFCCARTESIANPOINT((0.,2.));\n\
             #5=IFCPOLYLINE((#1,#2,#3,#4,#1));\n\
             #6=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#5);\n",
        );
        let profile = eval_profile(&model, 6).unwrap();
        // The repeated closing point must have been dropped.
        assert_eq!(profile.outer.len(), 4, "got {:?}", profile.outer);
        assert!((tessifc_mesh::signed_area(&profile.outer).abs() - 6.0).abs() < 1e-12);
    }

    #[test]
    fn voids_become_holes() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((4.,0.));\n\
             #3=IFCCARTESIANPOINT((4.,4.));\n#4=IFCCARTESIANPOINT((0.,4.));\n\
             #5=IFCPOLYLINE((#1,#2,#3,#4,#1));\n\
             #6=IFCCARTESIANPOINT((1.,1.));\n#7=IFCCARTESIANPOINT((2.,1.));\n\
             #8=IFCCARTESIANPOINT((2.,2.));\n#9=IFCCARTESIANPOINT((1.,2.));\n\
             #10=IFCPOLYLINE((#6,#7,#8,#9,#6));\n\
             #11=IFCARBITRARYPROFILEDEFWITHVOIDS(.AREA.,$,#5,(#10));\n",
        );
        let profile = eval_profile(&model, 11).unwrap();
        assert_eq!(profile.holes.len(), 1);
        assert!((tessifc_mesh::signed_area(&profile.holes[0]).abs() - 1.0).abs() < 1e-12);
        // Both loops in the file turn the same way; the hole comes out wound against the outline.
        assert!(
            tessifc_mesh::signed_area(&profile.holes[0])
                * tessifc_mesh::signed_area(&profile.outer)
                < 0.0
        );
    }

    #[test]
    fn a_profile_with_two_points_is_refused() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((3.,0.));\n\
             #3=IFCPOLYLINE((#1,#2));\n\
             #4=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#3);\n",
        );
        assert!(matches!(
            eval_profile(&model, 4),
            Err(GeomError::Degenerate(_))
        ));
    }

    /// Area and bounding box together pin an outline down; either alone does not.
    fn area_and_bounds(profile: &Profile2D) -> (f64, DVec2, DVec2) {
        let area = tessifc_mesh::signed_area(&profile.outer).abs();
        let low = profile
            .outer
            .iter()
            .fold(DVec2::splat(f64::MAX), |acc, p| acc.min(*p));
        let high = profile
            .outer
            .iter()
            .fold(DVec2::splat(f64::MIN), |acc, p| acc.max(*p));
        (area, low, high)
    }

    #[test]
    fn an_l_profile_is_centred_on_its_bounding_box() {
        // Depth 200, Width 100, Thickness 10, no radii.
        let model = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.01,$,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        // Two legs sharing one corner square.
        let expected = 0.01 * (0.1 + 0.2 - 0.01);
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        assert!(
            (low - DVec2::new(-0.05, -0.1)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(0.05, 0.1)).length() < 1e-12,
            "high {high}"
        );
    }

    #[test]
    fn an_l_profile_puts_the_short_leg_on_the_x_axis() {
        // A mirrored reading has the same area and bounds and is still wrong.
        let model = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.01,$,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let corner = profile
            .outer
            .iter()
            .any(|p| (*p - DVec2::new(-0.05, -0.1)).length() < 1e-12);
        assert!(
            corner,
            "the outside corner of the L belongs at (-w/2, -d/2)"
        );
        let inside = profile
            .outer
            .iter()
            .any(|p| (*p - DVec2::new(-0.04, 0.1)).length() < 1e-12);
        assert!(inside, "the long leg runs up the +y axis with thickness 10");
    }

    #[test]
    fn an_l_profile_without_a_width_is_equal_legged() {
        let model = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,$,0.01,$,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (_, low, high) = area_and_bounds(&profile);
        assert!(
            (high.x - low.x - 0.2).abs() < 1e-12,
            "width should match depth"
        );
    }

    #[test]
    fn an_l_profile_fillet_adds_material_at_the_inner_corner() {
        let square = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.01,$,$,$);\n");
        let filleted = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.01,0.008,$,$);\n");
        let plain = tessifc_mesh::signed_area(&eval_profile(&square, 1).unwrap().outer).abs();
        let rounded = tessifc_mesh::signed_area(&eval_profile(&filleted, 1).unwrap().outer).abs();
        // The fillet adds the corner square less an inscribed quarter disc,
        // so the bounds hold whatever the segment count.
        let radius: f64 = 0.008;
        let added = rounded - plain;
        let exact = radius * radius * (1.0 - std::f64::consts::FRAC_PI_4);
        assert!(
            added > exact && added < radius * radius,
            "fillet added {added}, want between {exact} and {}",
            radius * radius
        );
        assert!(
            (added - exact) / exact < 0.25,
            "fillet added {added}, too far from the exact {exact}"
        );
    }

    #[test]
    fn a_u_profile_has_the_web_on_the_left() {
        // Depth 200, FlangeWidth 80, WebThickness 6, FlangeThickness 10.
        let model = model_of("#1=IFCUSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.08,0.006,0.01,$,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        let expected = 2.0 * 0.08 * 0.01 + (0.2 - 0.02) * 0.006;
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        assert!(
            (low - DVec2::new(-0.04, -0.1)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(0.04, 0.1)).length() < 1e-12,
            "high {high}"
        );
        // The channel opens towards +x, so the web's inner face is at -width/2 + web.
        assert!(
            profile
                .outer
                .iter()
                .any(|p| (p.x + 0.04 - 0.006).abs() < 1e-12 && p.y.abs() <= 0.09 + 1e-12),
            "the web's inner face belongs at -w/2 + ts"
        );
    }

    #[test]
    fn a_t_profile_hangs_its_web_from_the_flange() {
        // Depth 200, FlangeWidth 100, WebThickness 8, FlangeThickness 12.
        let model = model_of("#1=IFCTSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.008,0.012,$,$,$,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        let expected = 0.1 * 0.012 + (0.2 - 0.012) * 0.008;
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        assert!(
            (low - DVec2::new(-0.05, -0.1)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(0.05, 0.1)).length() < 1e-12,
            "high {high}"
        );
        // The flange is the top edge: the widest points are at +y.
        let widest_y = profile
            .outer
            .iter()
            .filter(|p| (p.x.abs() - 0.05).abs() < 1e-12)
            .map(|p| p.y)
            .fold(f64::MIN, f64::max);
        assert!(
            widest_y > 0.0,
            "the flange belongs on top, got y {widest_y}"
        );
    }

    #[test]
    fn a_z_profile_is_two_flanges_pointing_opposite_ways() {
        // Depth 200, FlangeWidth 60, WebThickness 6, FlangeThickness 10.
        let model = model_of("#1=IFCZSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.06,0.006,0.01,$,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        let expected = 0.2 * 0.006 + 2.0 * (0.06 - 0.006) * 0.01;
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        // FlangeWidth reaches from a flange tip to the far face of the web.
        let half = 0.06 - 0.003;
        assert!(
            (low - DVec2::new(-half, -0.1)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(half, 0.1)).length() < 1e-12,
            "high {high}"
        );
        // Point symmetry about the origin is what makes a Z a Z.
        for point in &profile.outer {
            assert!(
                profile
                    .outer
                    .iter()
                    .any(|other| (*other + *point).length() < 1e-9),
                "a Z profile is symmetric through its centre; {point} has no partner"
            );
        }
    }

    #[test]
    fn a_c_profile_turns_its_flanges_back_into_lips() {
        // Depth 200, Width 80, WallThickness 4, Girth 20.
        let model = model_of("#1=IFCCSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.08,0.004,0.02,$);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        let expected = 0.004 * (0.2 + 2.0 * 0.08 + 2.0 * 0.02 - 4.0 * 0.004);
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        assert!(
            (low - DVec2::new(-0.04, -0.1)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(0.04, 0.1)).length() < 1e-12,
            "high {high}"
        );
        assert_eq!(
            profile.outer.len(),
            12,
            "a lipped channel has twelve corners"
        );
    }

    #[test]
    fn a_trapezium_offsets_its_top_from_the_bottom_left() {
        // Bottom 100, Top 60, Y 40, TopXOffset 20.
        let model = model_of("#1=IFCTRAPEZIUMPROFILEDEF(.AREA.,$,$,0.1,0.06,0.04,0.02);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        let expected = (0.1 + 0.06) * 0.5 * 0.04;
        assert!(
            (area - expected).abs() < 1e-12,
            "got {area}, want {expected}"
        );
        assert!(
            (low - DVec2::new(-0.05, -0.02)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(0.05, 0.02)).length() < 1e-12,
            "high {high}"
        );
    }

    #[test]
    fn an_ellipse_has_the_area_its_semi_axes_promise() {
        let model = model_of("#1=IFCELLIPSEPROFILEDEF(.AREA.,$,$,0.4,0.1);\n");
        let profile = eval_profile(&model, 1).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        let exact = std::f64::consts::PI * 0.4 * 0.1;
        // A polygon inscribes the ellipse, so it is a little smaller.
        assert!(area < exact, "an inscribed polygon cannot be larger");
        assert!(
            (area - exact).abs() / exact < 0.02,
            "got {area}, want about {exact}"
        );
        // Inscribed, so it reaches the semi-axes only to within the chord tolerance.
        assert!((low - DVec2::new(-0.4, -0.1)).length() < 2e-3, "low {low}");
        assert!((high - DVec2::new(0.4, 0.1)).length() < 2e-3, "high {high}");
    }

    #[test]
    fn a_centre_line_profile_becomes_a_band_of_its_thickness() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,0.));\n",
            "#3=IFCPOLYLINE((#1,#2));\n",
            "#4=IFCCENTERLINEPROFILEDEF(.AREA.,$,#3,0.05);\n"
        ));
        let profile = eval_profile(&model, 4).unwrap();
        let (area, low, high) = area_and_bounds(&profile);
        assert!((area - 0.05).abs() < 1e-12, "got {area}, want 0.05");
        assert!(
            (low - DVec2::new(0.0, -0.025)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec2::new(1.0, 0.025)).length() < 1e-12,
            "high {high}"
        );
    }

    #[test]
    fn a_bent_centre_line_mitres_its_corner() {
        // A right-angle fold; mitring keeps the band's width through the corner.
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.));\n",
            "#2=IFCCARTESIANPOINT((1.,0.));\n",
            "#3=IFCCARTESIANPOINT((1.,1.));\n",
            "#4=IFCPOLYLINE((#1,#2,#3));\n",
            "#5=IFCCENTERLINEPROFILEDEF(.AREA.,$,#4,0.1);\n"
        ));
        let profile = eval_profile(&model, 5).unwrap();
        let area = tessifc_mesh::signed_area(&profile.outer).abs();
        // A mitred right angle gains outside what it loses inside: path length times width.
        let expected = 2.0 * 1.0 * 0.1;
        assert!(
            (area - expected).abs() < 1e-9,
            "got {area}, want {expected}"
        );
        // The mitre itself: the outer corner reaches past the path corner.
        assert!(
            profile
                .outer
                .iter()
                .any(|p| (*p - DVec2::new(1.05, -0.05)).length() < 1e-12),
            "the outer mitre belongs at the intersection of the two offsets"
        );
    }

    #[test]
    fn a_dropped_edge_radius_is_reported_rather_than_silently_ignored() {
        let model = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.01,0.008,0.004,$);\n");
        let (result, diagnostics) = crate::eval::tests::eval_profile_with_diagnostics(&model, 1);
        assert!(result.is_ok(), "the beam is still drawn");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == crate::error::codes::PROFILE_DETAIL_APPROXIMATED),
            "an ignored edge radius has to be announced, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_profile_whose_parts_do_not_fit_is_refused() {
        // Thickness larger than the leg it is supposed to sit in.
        let model = model_of("#1=IFCLSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.1,0.2,$,$,$);\n");
        assert!(eval_profile(&model, 1).is_err());
        // A web thicker than the flange it hangs from.
        let model = model_of("#1=IFCUSHAPEPROFILEDEF(.AREA.,$,$,0.2,0.02,0.06,0.01,$,$,$);\n");
        assert!(eval_profile(&model, 1).is_err());
    }
}
