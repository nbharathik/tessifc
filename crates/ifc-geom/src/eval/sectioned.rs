// SPDX-License-Identifier: Apache-2.0
//! IFC4X3 sectioned solids and surfaces: cross sections placed along an
//! alignment curve and lofted between their stations.

use crate::context::EvalCtx;
use crate::error::{GeomError, codes};
use crate::eval::alignment::{SpatialCurve, curve_measure, linear_frame, spatial_curve};
use crate::eval::sweeps::MAX_SWEEP_VERTICES;
use crate::registry::{Profile2D, Registry, SolidEvaluator};
use glam::{DMat4, DVec2, DVec3};
use std::sync::Arc;
use tessifc_mesh::{Mesh64, triangulate_polygon};
use tessifc_model::Entity;

/// Upper bound on the cross sections one item may list.
pub const MAX_SECTIONS: usize = 65_536;

/// One cross section: its profile, its distance along the directrix, and its
/// frame relative to the curve's own frame there.
struct Section {
    s: f64,
    profile: Profile2D,
    /// Offsets and axes of the placement, in the curve frame at `s`.
    local: DMat4,
}

/// `IfcSectionedSolidHorizontal`: closed sections lofted along the directrix.
pub struct SectionedSolidHorizontal;

impl SolidEvaluator for SectionedSolidHorizontal {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSectionedSolidHorizontal"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let (curve, sections) = read_sections(ctx, item)?;
        if sections.iter().any(|section| section.profile.open) {
            ctx.diag.warn(
                codes::OPEN_PROFILE_SURFACE,
                item.id(),
                "IfcSectionedSolidHorizontal of open cross sections; a surface was built",
            );
        }
        loft(ctx, item, &curve, sections, true)
    }
}

/// `IfcSectionedSurface`: sections lofted along the directrix, never capped.
pub struct SectionedSurface;

impl SolidEvaluator for SectionedSurface {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSectionedSurface"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let (curve, sections) = read_sections(ctx, item)?;
        loft(ctx, item, &curve, sections, false)
    }
}

/// The entities of a list attribute, refused before anything is read when it is over the cap.
fn bounded_entities<'a>(item: Entity<'a>, name: &str) -> Result<Vec<Entity<'a>>, GeomError> {
    let list = item
        .attr(name)
        .as_list()
        .ok_or_else(|| GeomError::missing(name))?;
    if list.count() > MAX_SECTIONS {
        return Err(GeomError::LimitReached("cross sections".into()));
    }
    item.attr(name)
        .as_list()
        .ok_or_else(|| GeomError::missing(name))?
        .map(|value| {
            value
                .as_entity()
                .ok_or_else(|| GeomError::missing("a cross section"))
        })
        .collect()
}

/// The directrix and the sections in order along it, with their frames.
fn read_sections(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
) -> Result<(Arc<SpatialCurve>, Vec<Section>), GeomError> {
    let directrix = item
        .attr("Directrix")
        .as_entity()
        .ok_or_else(|| GeomError::missing("Directrix"))?;
    let curve = spatial_curve(ctx, directrix)?;
    let profiles = bounded_entities(item, "CrossSections")?;
    let placements = bounded_entities(item, "CrossSectionPositions")?;
    if profiles.len() < 2 || profiles.len() != placements.len() {
        return Err(GeomError::Degenerate(
            "a sectioned item needs one position per cross section, and two sections at least"
                .into(),
        ));
    }
    let registry = ctx.registry();
    let mut sections = Vec::with_capacity(profiles.len());
    for (profile, placement) in profiles.into_iter().zip(placements) {
        let profile = registry.profile(ctx, profile)?;
        if !placement.is_a("IfcAxis2PlacementLinear") {
            return Err(GeomError::Unsupported(format!(
                "{} as a cross section position",
                placement.class_name()
            )));
        }
        let location = placement
            .attr("Location")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Location"))?;
        let distance = curve_measure(location.attr("DistanceAlong"), &ctx.units)
            .ok_or_else(|| GeomError::missing("DistanceAlong"))?;
        let s = curve.distance_of(distance);
        if !s.is_finite() {
            return Err(GeomError::Degenerate(
                "a cross section at a non-finite distance".into(),
            ));
        }
        // The placement's offsets and axes, kept relative to the curve frame so
        // that stations between two sections can blend them.
        let local = curve.frame_at(s).matrix().inverse() * linear_frame(ctx, placement)?;
        sections.push(Section { s, profile, local });
    }

    let tolerance = ctx.tol.len;
    if sections.windows(2).all(|pair| pair[1].s < pair[0].s) {
        sections.reverse();
    }
    if sections
        .windows(2)
        .any(|pair| pair[1].s < pair[0].s - tolerance)
    {
        return Err(GeomError::Degenerate(
            "cross sections out of order along the directrix".into(),
        ));
    }
    let length = curve.length();
    let mut moved = false;
    for section in &mut sections {
        let inside = section.s.clamp(0.0, length);
        if (inside - section.s).abs() > tolerance {
            moved = true;
            section.s = inside;
        }
    }
    if moved {
        ctx.diag.warn(
            codes::SWEEP_PARAMETERS_APPROXIMATED,
            item.id(),
            "cross sections beyond the directrix were moved to its ends",
        );
    }
    Ok((curve, sections))
}

/// Redistribute an open polyline's points uniformly by arc length, `count` of them.
fn resample_open(points: &[DVec2], count: usize) -> Vec<DVec2> {
    if points.len() < 2 || count < 2 {
        return points.to_vec();
    }
    let mut cumulative = Vec::with_capacity(points.len());
    let mut total = 0.0;
    cumulative.push(0.0);
    for pair in points.windows(2) {
        total += (pair[1] - pair[0]).length();
        cumulative.push(total);
    }
    if total <= 0.0 {
        return points.to_vec();
    }
    (0..count)
        .map(|step| {
            let target = total * step as f64 / (count - 1) as f64;
            let index = cumulative
                .partition_point(|length| *length < target)
                .clamp(1, points.len() - 1);
            let span = cumulative[index] - cumulative[index - 1];
            let fraction = if span > 0.0 {
                ((target - cumulative[index - 1]) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            points[index - 1] + (points[index] - points[index - 1]) * fraction
        })
        .collect()
}

/// Give every section the same corner counts, outline and holes alike.
///
/// Returns the loops as `(corner count, closed)` and whether the sections are open.
fn harmonise(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
    sections: &mut [Section],
) -> Result<(Vec<(usize, bool)>, bool), GeomError> {
    let open = sections.iter().any(|section| section.profile.open);
    let hole_count = sections[0].profile.holes.len();
    if open {
        for section in sections.iter_mut() {
            section.profile.holes.clear();
        }
    } else if sections
        .iter()
        .any(|section| section.profile.holes.len() != hole_count)
    {
        ctx.diag.warn(
            codes::PROFILE_DETAIL_APPROXIMATED,
            item.id(),
            "sections with different numbers of holes; lofted without holes",
        );
        for section in sections.iter_mut() {
            section.profile.holes.clear();
        }
    }
    let outer_count = sections
        .iter()
        .map(|section| section.profile.outer.len())
        .max()
        .unwrap_or(0);
    let hole_counts: Vec<usize> = (0..sections[0].profile.holes.len())
        .map(|hole| {
            sections
                .iter()
                .map(|section| section.profile.holes[hole].len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut resampled = false;
    for section in sections.iter_mut() {
        let profile = &mut section.profile;
        if profile.outer.len() != outer_count {
            profile.outer = if open {
                resample_open(&profile.outer, outer_count)
            } else {
                crate::eval::solids::resample_loop(&profile.outer, outer_count)
            };
            resampled = true;
        }
        for (hole, &count) in profile.holes.iter_mut().zip(&hole_counts) {
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
    let mut loops = vec![(outer_count, !open)];
    loops.extend(hole_counts.iter().filter(|&&n| n >= 3).map(|&n| (n, true)));
    let count: usize = loops.iter().map(|(n, _)| n).sum();
    if count < 2 || (!open && count < 3) {
        return Err(GeomError::Degenerate(
            "a sectioned item with too few corners".into(),
        ));
    }
    Ok((loops, open))
}

/// A section's corners in the curve frame at its station: profile x along the
/// lateral, y along the up, through the placement's offsets and axes.
fn local_corners(section: &Section) -> Vec<DVec3> {
    let profile = &section.profile;
    profile
        .outer
        .iter()
        .chain(
            profile
                .holes
                .iter()
                .filter(|hole| hole.len() >= 3)
                .flatten(),
        )
        .map(|point| {
            section
                .local
                .transform_point3(DVec3::new(0.0, point.x, point.y))
        })
        .collect()
}

/// Loft the sections along the curve; `solid` asks for caps and a closed shell.
fn loft(
    ctx: &EvalCtx<'_>,
    item: Entity<'_>,
    curve: &SpatialCurve,
    mut sections: Vec<Section>,
    solid: bool,
) -> Result<Mesh64, GeomError> {
    let (loops, open) = harmonise(ctx, item, &mut sections)?;
    let count: usize = loops.iter().map(|(n, _)| n).sum();
    let tolerance = ctx.tol.len;
    let first = sections[0].s;
    let last = sections[sections.len() - 1].s;
    if last - first <= tolerance {
        return Err(GeomError::Degenerate(
            "cross sections that all sit at one station".into(),
        ));
    }
    let stations = curve.stations(ctx, first, last)?;
    let rings = stations.len() + sections.len();
    if rings
        .checked_mul(count)
        .is_none_or(|n| n > MAX_SWEEP_VERTICES)
    {
        return Err(GeomError::Degenerate(
            "a sectioned item larger than the vertex budget".into(),
        ));
    }
    let capped = solid && !open;
    let cap_start = if capped {
        triangulate_polygon(&sections[0].profile.to_polygon())?
    } else {
        Vec::new()
    };
    let cap_end = if capped {
        triangulate_polygon(&sections[sections.len() - 1].profile.to_polygon())?
    } else {
        Vec::new()
    };

    let mut mesh = Mesh64::with_capacity(rings * count, rings * count * 6);
    let mut ring_count = 0usize;
    let mut emit = |mesh: &mut Mesh64, s: f64, corners: &[DVec3]| {
        let frame = curve.frame_at(s).matrix();
        for corner in corners {
            mesh.positions.push(frame.transform_point3(*corner));
        }
        ring_count += 1;
    };
    let corners = |section: &Section| -> Result<Vec<DVec3>, GeomError> {
        let corners = local_corners(section);
        if corners.len() != count {
            return Err(GeomError::Degenerate(
                "a cross section whose corners could not be matched".into(),
            ));
        }
        Ok(corners)
    };
    let mut from = corners(&sections[0])?;
    emit(&mut mesh, sections[0].s, &from);
    for pair in sections.windows(2) {
        let (a, b) = (pair[0].s, pair[1].s);
        let to = corners(&pair[1])?;
        // Stations strictly inside the span blend the two sections; the span
        // always ends with the next section exactly as placed.
        if b - a > tolerance {
            let mut blended = vec![DVec3::ZERO; count];
            for &s in stations
                .iter()
                .filter(|&&s| s > a + tolerance && s < b - tolerance)
            {
                let u = (s - a) / (b - a);
                for (out, (start, end)) in blended.iter_mut().zip(from.iter().zip(&to)) {
                    *out = *start + (*end - *start) * u;
                }
                emit(&mut mesh, s, &blended);
            }
        }
        emit(&mut mesh, b, &to);
        from = to;
    }

    if mesh.positions.iter().any(|point| !point.is_finite()) {
        return Err(GeomError::Degenerate(
            "a non-finite cross section point".into(),
        ));
    }

    // Profile x is the lateral and y the up, so the loft advances along the tangent.
    let flip_sides = open || tessifc_mesh::signed_area(&sections[0].profile.outer) >= 0.0;
    let mut start = 0usize;
    for &(length, closed_loop) in &loops {
        let edges = if closed_loop { length } else { length - 1 };
        for index in 0..edges {
            let a_local = start + index;
            let b_local = start + (index + 1) % length;
            for ring in 0..ring_count - 1 {
                let a = (ring * count + a_local) as u32;
                let b = (ring * count + b_local) as u32;
                let c = ((ring + 1) * count + b_local) as u32;
                let d = ((ring + 1) * count + a_local) as u32;
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
    let last_ring = ((ring_count - 1) * count) as u32;
    for triangle in cap_start.chunks_exact(3) {
        mesh.push_triangle(triangle[0], triangle[2], triangle[1]);
    }
    for triangle in cap_end.chunks_exact(3) {
        mesh.push_triangle(
            triangle[0] + last_ring,
            triangle[1] + last_ring,
            triangle[2] + last_ring,
        );
    }

    mesh.remove_degenerate_triangles(ctx.tol.area);
    tessifc_mesh::weld_and_close(&mut mesh, tolerance);
    mesh.closed = Some(capped && mesh.is_edge_manifold());
    if mesh.closed == Some(true) {
        mesh.fix_orientation();
    }
    Ok(mesh)
}

/// Register the sectioned evaluators.
pub fn register(registry: &mut Registry) {
    registry.register_solid(Box::new(SectionedSolidHorizontal));
    registry.register_solid(Box::new(SectionedSurface));
}

#[cfg(all(test, feature = "schema-ifc4x3"))]
mod tests {
    use super::*;
    use crate::eval::tests::{eval_solid, eval_solid_with_diagnostics, model_of_schema};
    use tessifc_model::Model;

    const SCHEMA: &str = "IFC4X3_ADD2";

    /// A straight ten metre composite as `#1..#7`, with `#7` the curve.
    fn straight() -> Vec<String> {
        vec![
            "#1=IFCCARTESIANPOINT((0.,0.));".into(),
            "#2=IFCDIRECTION((1.,0.));".into(),
            "#3=IFCAXIS2PLACEMENT2D(#1,#2);".into(),
            "#4=IFCVECTOR(#2,1.);".into(),
            "#5=IFCLINE(#1,#4);".into(),
            "#6=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#5);"
                .into(),
            "#7=IFCCOMPOSITECURVE((#6),.F.);".into(),
        ]
    }

    /// A linear placement on curve `#curve` at `distance`, as `#id` and `#id + 1`.
    fn position(id: u32, curve: u32, distance: f64) -> Vec<String> {
        vec![
            format!(
                "#{id}=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE({distance}),$,$,$,#{curve});"
            ),
            format!("#{}=IFCAXIS2PLACEMENTLINEAR(#{id},$,$);", id + 1),
        ]
    }

    fn model(lines: Vec<String>) -> Model {
        let mut source = lines.join("\n");
        source.push('\n');
        model_of_schema(SCHEMA, &source)
    }

    #[test]
    fn a_box_between_two_sections_on_a_line_is_a_prism() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(11, 7, 2.0));
        lines.extend(position(13, 7, 8.0));
        lines.push("#15=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10),(#12,#14));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 15);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        assert!((mesh.signed_volume().abs() - 0.48).abs() < 1e-9);
        let (low, high) = mesh.bounds().unwrap();
        // Profile x runs to the left of the directrix, y up.
        assert!((low - DVec3::new(2.0, -0.2, -0.1)).length() < 1e-9, "{low}");
        assert!((high - DVec3::new(8.0, 0.2, 0.1)).length() < 1e-9, "{high}");
    }

    #[test]
    fn a_box_along_an_arc_keeps_the_pappus_volume() {
        let quarter = 10.0 * std::f64::consts::FRAC_PI_2;
        let mut lines = vec![
            "#1=IFCCARTESIANPOINT((0.,0.));".to_string(),
            "#2=IFCDIRECTION((1.,0.));".to_string(),
            "#3=IFCAXIS2PLACEMENT2D(#1,#2);".to_string(),
            "#4=IFCCIRCLE(#3,10.);".to_string(),
            format!(
                "#5=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({quarter}),#4);"
            ),
            "#6=IFCCOMPOSITECURVE((#5),.F.);".to_string(),
            "#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".to_string(),
        ];
        lines.extend(position(11, 6, 0.0));
        lines.extend(position(13, 6, quarter));
        lines.push("#15=IFCSECTIONEDSOLIDHORIZONTAL(#6,(#10,#10),(#12,#14));".into());
        let mesh = eval_solid(&model(lines), 15).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let expected = 0.08 * quarter;
        let volume = mesh.signed_volume().abs();
        assert!(
            (volume - expected).abs() < 0.005 * expected,
            "{volume} vs {expected}"
        );
    }

    #[test]
    fn a_box_along_a_gradient_is_cut_perpendicular_to_the_slope() {
        let slope: f64 = 0.5;
        let along = 10.0 * (1.0 + slope * slope).sqrt();
        let mut lines = straight();
        lines.push(format!("#8=IFCDIRECTION((1.,{slope}));"));
        lines.push("#9=IFCAXIS2PLACEMENT2D(#1,#8);".into());
        lines.push(format!(
            "#10=IFCCURVESEGMENT(.CONTINUOUS.,#9,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({along}),#5);"
        ));
        lines.push("#11=IFCGRADIENTCURVE((#10),.F.,#7,$);".into());
        lines.push("#12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(13, 11, 2.0));
        lines.extend(position(15, 11, 8.0));
        lines.push("#17=IFCSECTIONEDSOLIDHORIZONTAL(#11,(#12,#12),(#14,#16));".into());
        let mesh = eval_solid(&model(lines), 17).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let expected = 0.08 * 6.0 * (1.0 + slope * slope).sqrt();
        let volume = mesh.signed_volume().abs();
        assert!((volume - expected).abs() < 1e-9, "{volume} vs {expected}");
    }

    #[test]
    fn a_growing_section_lofts_linearly() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.push("#11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.8,0.4);".into());
        lines.extend(position(12, 7, 0.0));
        lines.extend(position(14, 7, 6.0));
        lines.push("#16=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#11),(#13,#15));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 16);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        // Simpson over a frustum: L / 6 (A1 + 4 A_mid + A2).
        let expected = 6.0 / 6.0 * (0.08 + 4.0 * 0.18 + 0.32);
        assert!((mesh.signed_volume().abs() - expected).abs() < 1e-9);
    }

    #[test]
    fn a_lateral_offset_and_an_axis_on_the_placement_move_the_section() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        // Offset one metre to the left, and the profile's y turned to the lateral.
        lines.push("#11=IFCDIRECTION((0.,1.,0.));".into());
        lines.push("#12=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(2.),1.,$,$,#7);".into());
        lines.push("#13=IFCAXIS2PLACEMENTLINEAR(#12,#11,$);".into());
        lines.push("#14=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(8.),1.,$,$,#7);".into());
        lines.push("#15=IFCAXIS2PLACEMENTLINEAR(#14,#11,$);".into());
        lines.push("#16=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10),(#13,#15));".into());
        let mesh = eval_solid(&model(lines), 16).unwrap();
        assert!((mesh.signed_volume().abs() - 0.48).abs() < 1e-9);
        let (low, high) = mesh.bounds().unwrap();
        // Profile y (0.2 tall) now runs along the lateral, x (0.4 wide) along -up.
        assert!((low - DVec3::new(2.0, 0.9, -0.2)).length() < 1e-9, "{low}");
        assert!((high - DVec3::new(8.0, 1.1, 0.2)).length() < 1e-9, "{high}");
    }

    #[test]
    fn sections_listed_backwards_are_lofted_forwards() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(11, 7, 8.0));
        lines.extend(position(13, 7, 2.0));
        lines.push("#15=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10),(#12,#14));".into());
        let mesh = eval_solid(&model(lines), 15).unwrap();
        assert_eq!(mesh.closed, Some(true));
        assert!((mesh.signed_volume().abs() - 0.48).abs() < 1e-9);
    }

    #[test]
    fn sections_out_of_order_are_refused() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(11, 7, 2.0));
        lines.extend(position(13, 7, 8.0));
        lines.extend(position(15, 7, 5.0));
        lines.push("#17=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10,#10),(#12,#14,#16));".into());
        let result = eval_solid(&model(lines), 17);
        assert!(
            matches!(&result, Err(GeomError::Degenerate(why)) if why.contains("order")),
            "{result:?}"
        );
    }

    #[test]
    fn a_single_section_is_refused() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(11, 7, 2.0));
        lines.push("#13=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10),(#12));".into());
        assert!(matches!(
            eval_solid(&model(lines), 13),
            Err(GeomError::Degenerate(_))
        ));
    }

    #[test]
    fn a_section_beyond_the_directrix_is_moved_to_its_end_and_reported() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(11, 7, 4.0));
        lines.extend(position(13, 7, 12.0));
        lines.push("#15=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10),(#12,#14));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 15);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::SWEEP_PARAMETERS_APPROXIMATED),
            "{diagnostics:?}"
        );
        let mesh = mesh.unwrap();
        assert!((mesh.signed_volume().abs() - 0.48).abs() < 1e-9);
    }

    #[test]
    fn sections_whose_corners_differ_are_resampled_and_reported() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.push("#11=IFCCIRCLEPROFILEDEF(.AREA.,$,$,0.2);".into());
        lines.extend(position(12, 7, 0.0));
        lines.extend(position(14, 7, 6.0));
        lines.push("#16=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#11),(#13,#15));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 16);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::PROFILE_DETAIL_APPROXIMATED),
            "{diagnostics:?}"
        );
        assert_eq!(mesh.unwrap().closed, Some(true));
    }

    #[test]
    fn sections_with_different_hole_counts_lose_their_holes_and_say_so() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEHOLLOWPROFILEDEF(.AREA.,$,$,0.4,0.2,0.05,$,$);".into());
        lines.push("#11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(12, 7, 0.0));
        lines.extend(position(14, 7, 6.0));
        lines.push("#16=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#11),(#13,#15));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 16);
        assert!(
            diagnostics.iter().any(
                |d| d.code == codes::PROFILE_DETAIL_APPROXIMATED && d.message.contains("holes")
            ),
            "{diagnostics:?}"
        );
        assert!((mesh.unwrap().signed_volume().abs() - 0.48).abs() < 1e-9);
    }

    #[test]
    fn a_sectioned_surface_of_open_cross_profiles_has_no_caps() {
        let mut lines = straight();
        lines.push("#10=IFCOPENCROSSPROFILEDEF(.CURVE.,$,.T.,(2.,2.),(-0.02,0.02),$,$);".into());
        lines.extend(position(11, 7, 0.0));
        lines.extend(position(13, 7, 10.0));
        lines.push("#15=IFCSECTIONEDSURFACE(#7,(#12,#14),(#10,#10));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 15);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(false));
        let expected = 40.0 / (0.02f64).cos();
        let area = mesh.surface_area();
        assert!((area - expected).abs() < 1e-9, "{area} vs {expected}");
        let (low, high) = mesh.bounds().unwrap();
        assert!(
            (low.y).abs() < 1e-9 && (high.y - 4.0).abs() < 1e-9,
            "{low} {high}"
        );
    }

    #[test]
    fn a_solid_of_open_sections_is_built_as_a_surface_and_reported() {
        let mut lines = straight();
        lines.push("#10=IFCOPENCROSSPROFILEDEF(.CURVE.,$,.T.,(2.,2.),(-0.02,0.02),$,$);".into());
        lines.extend(position(11, 7, 0.0));
        lines.extend(position(13, 7, 10.0));
        lines.push("#15=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10),(#12,#14));".into());
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model(lines), 15);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::OPEN_PROFILE_SURFACE),
            "{diagnostics:?}"
        );
        assert_eq!(mesh.unwrap().closed, Some(false));
    }

    #[test]
    fn a_step_between_two_sections_at_one_station_is_a_face() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.push("#11=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.8,0.4);".into());
        lines.extend(position(12, 7, 0.0));
        lines.extend(position(14, 7, 5.0));
        lines.extend(position(16, 7, 5.0));
        lines.extend(position(18, 7, 10.0));
        lines.push(
            "#20=IFCSECTIONEDSOLIDHORIZONTAL(#7,(#10,#10,#11,#11),(#13,#15,#17,#19));".into(),
        );
        let mesh = eval_solid(&model(lines), 20).unwrap();
        assert_eq!(mesh.closed, Some(true));
        let expected = 0.08 * 5.0 + 0.32 * 5.0;
        assert!((mesh.signed_volume().abs() - expected).abs() < 1e-9);
    }

    #[test]
    fn a_list_over_the_section_cap_is_refused_before_it_is_read() {
        let mut lines = straight();
        lines.push("#10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,0.4,0.2);".into());
        lines.extend(position(11, 7, 2.0));
        let refs = vec!["#10"; MAX_SECTIONS + 1].join(",");
        let positions = vec!["#12"; MAX_SECTIONS + 1].join(",");
        lines.push(format!(
            "#15=IFCSECTIONEDSOLIDHORIZONTAL(#7,({refs}),({positions}));"
        ));
        assert!(matches!(
            eval_solid(&model(lines), 15),
            Err(GeomError::LimitReached(_))
        ));
    }

    #[test]
    fn a_loft_over_the_vertex_budget_is_a_limit_not_a_panic() {
        // Sixty-five thousand corners around a full circle of forty stations.
        let full = 10.0 * std::f64::consts::TAU;
        let widths = vec!["0.001"; MAX_SECTIONS].join(",");
        let slopes = vec!["0."; MAX_SECTIONS].join(",");
        let mut lines = vec![
            "#1=IFCCARTESIANPOINT((0.,0.));".to_string(),
            "#2=IFCDIRECTION((1.,0.));".to_string(),
            "#3=IFCAXIS2PLACEMENT2D(#1,#2);".to_string(),
            "#4=IFCCIRCLE(#3,10.);".to_string(),
            format!(
                "#5=IFCCURVESEGMENT(.CONTINUOUS.,#3,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({full}),#4);"
            ),
            "#6=IFCCOMPOSITECURVE((#5),.F.);".to_string(),
            format!("#10=IFCOPENCROSSPROFILEDEF(.CURVE.,$,.T.,({widths}),({slopes}),$,$);"),
        ];
        lines.extend(position(11, 6, 0.0));
        lines.extend(position(13, 6, full));
        lines.push("#15=IFCSECTIONEDSURFACE(#6,(#12,#14),(#10,#10));".into());
        let result = eval_solid(&model(lines), 15);
        assert!(
            matches!(&result, Err(GeomError::Degenerate(why)) if why.contains("budget")),
            "{result:?}"
        );
    }
}
