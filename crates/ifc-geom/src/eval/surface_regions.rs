// SPDX-License-Identifier: Apache-2.0
//! Bounded surface regions with a coordinate frame chosen from their boundary,
//! and loops that go round a surface turned into polygons with a seam.

use crate::{EvalCtx, GeomError, Surface, SurfaceKind, codes};
use glam::{DMat4, DQuat, DVec2, DVec3};
use tessifc_mesh::Mesh64;

/// Rounds of seam subdivision; each doubles the pieces.
const MAX_SEAM_DOUBLINGS: u32 = 7;

/// A loop of a face on its surface: the edge samples and their parameters.
pub(crate) struct FaceLoop {
    pub points: Vec<DVec3>,
    pub uv: Vec<DVec2>,
    pub declared_outer: bool,
    /// The periodic axis and the signed period between this loop's end and
    /// its start: a loop that goes round the surface with no seam edge.
    pub wrap: Option<(usize, f64)>,
}

/// A sphere is exact under any pole axis: pick the one that keeps every
/// boundary point furthest from the poles, so a loop through or round the
/// surface's own poles becomes an ordinary region.
pub(crate) fn reparametrised_sphere(surface: &Surface, boundary: &[DVec3]) -> Option<Surface> {
    let SurfaceKind::Sphere { radius } = surface.kind else {
        return None;
    };
    let centre = surface.frame.transform_point3(DVec3::ZERO);
    let directions: Vec<DVec3> = boundary
        .iter()
        .filter_map(|point| (*point - centre).try_normalize())
        .collect();
    if directions.len() < 3 {
        return None;
    }
    let clearance = |axis: DVec3| {
        directions
            .iter()
            .map(|direction| direction.dot(axis).abs().min(1.0).acos())
            .fold(f64::INFINITY, f64::min)
    };
    let current = surface.frame.z_axis.truncate().normalize_or_zero();
    let mut candidates = vec![
        surface.frame.x_axis.truncate().normalize_or_zero(),
        surface.frame.y_axis.truncate().normalize_or_zero(),
    ];
    let fit: DVec3 = boundary
        .iter()
        .zip(boundary.iter().cycle().skip(1))
        .map(|(a, b)| (*a - centre).cross(*b - centre))
        .sum();
    if let Some(fit) = fit.try_normalize() {
        candidates.push(fit);
    }
    let best = candidates
        .into_iter()
        .filter(|axis| axis.length_squared() > 0.0)
        .map(|axis| (clearance(axis), axis))
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    if best.0 <= clearance(current) + 1e-9 {
        return None;
    }
    let local = surface
        .frame
        .inverse()
        .transform_vector3(best.1)
        .normalize();
    let rotation = DMat4::from_quat(DQuat::from_rotation_arc(DVec3::Z, local));
    Some(Surface::new(
        SurfaceKind::Sphere { radius },
        surface.frame * rotation,
    ))
}

/// Move `value` by whole periods to sit nearest `previous`.
pub(crate) fn unwrap(previous: f64, value: f64, period: Option<f64>) -> f64 {
    let Some(period) = period else {
        return value;
    };
    if period <= 0.0 || !period.is_finite() {
        return value;
    }
    let steps = ((previous - value) / period).round();
    value + steps * period
}

/// Invert a loop of points onto a surface, following it across any seam.
///
/// A periodic parameter is unwrapped as the loop is walked. A point at a
/// singularity of the surface, an apex or a pole, has no parameter of its own
/// along the periodic axis: it takes the one it is reached with and the one
/// it is left with, as two entries, so the loop stays a polygon whose
/// collapsed edge is that point. `long_way` sends the departure the other way
/// round the period.
pub(crate) fn invert_loop(
    surface: &Surface,
    points: &[DVec3],
    tol: f64,
    long_way: bool,
) -> Option<(Vec<DVec3>, Vec<DVec2>, bool)> {
    let (u_period, v_period) = surface.periods();
    let singular: Vec<bool> = points
        .iter()
        .map(|point| surface.is_singular(*point, tol))
        .collect();
    let first_regular = singular.iter().position(|flag| !flag)?;
    let has_apex = singular.iter().any(|flag| *flag);
    let count = points.len();
    let mut out_points: Vec<DVec3> = Vec::with_capacity(count + 2);
    let mut out_uv: Vec<DVec2> = Vec::with_capacity(count + 2);
    for step in 0..count {
        let index = (first_regular + step) % count;
        let point = points[index];
        let mut uv = surface.invert(point, tol)?;
        if singular[index] {
            // The regular point after this apex, for the departure.
            let next = (1..count)
                .map(|ahead| (index + ahead) % count)
                .find(|&candidate| !singular[candidate])?;
            let next_uv = surface.invert(points[next], tol)?;
            let previous = *out_uv.last()?;
            let mut arrive = uv;
            arrive.x = previous.x;
            arrive.y = unwrap(previous.y, arrive.y, v_period);
            let mut depart = uv;
            depart.x = unwrap(previous.x, next_uv.x, u_period);
            depart.y = arrive.y;
            if long_way && let Some(period) = u_period {
                depart.x += if depart.x >= previous.x {
                    -period
                } else {
                    period
                };
            }
            out_points.push(point);
            out_uv.push(arrive);
            out_points.push(point);
            out_uv.push(depart);
            continue;
        }
        if let Some(previous) = out_uv.last() {
            uv.x = unwrap(previous.x, uv.x, u_period);
            uv.y = unwrap(previous.y, uv.y, v_period);
        }
        out_points.push(point);
        out_uv.push(uv);
    }
    Some((out_points, out_uv, has_apex))
}

/// The periodic axis a walked loop went round, and the signed period from its
/// start to where it closes.
pub(crate) fn detect_wrap(
    uv: &[DVec2],
    periods: (Option<f64>, Option<f64>),
) -> Option<(usize, f64)> {
    let (first, last) = (uv.first()?, uv.last()?);
    for (axis, period) in [(0usize, periods.0), (1usize, periods.1)] {
        let Some(period) = period else {
            continue;
        };
        let travelled = last[axis] - first[axis];
        if travelled.abs() > 0.5 * period {
            return Some((axis, travelled.signum() * period));
        }
    }
    None
}

/// The far side of a band: another loop round the surface, or one point.
enum FarSide<'a> {
    Loop(&'a FaceLoop),
    Point { point: DVec3, across: f64 },
}

/// Turn the loops that go round the surface into one polygon with a seam.
///
/// Two such loops bound the band between them; one with a vertex loop at an
/// apex, or with a singular end of the surface on its material side, bounds
/// the band up to that point. The seam is cut at a sample of the first loop
/// that no other loop crosses, and both copies of it carry the same points,
/// so the mesh welds along it.
pub(crate) fn resolve_bands(
    ctx: &EvalCtx<'_>,
    surface: &Surface,
    loops: Vec<FaceLoop>,
    apexes: &[DVec3],
    same_sense: bool,
) -> Result<Vec<FaceLoop>, GeomError> {
    let wrapping: Vec<usize> = (0..loops.len())
        .filter(|&index| loops[index].wrap.is_some())
        .collect();
    if wrapping.is_empty() {
        return Ok(loops);
    }
    let (axis, signed_period) = loops[wrapping[0]].wrap.expect("a wrapping loop");
    if wrapping
        .iter()
        .any(|&index| loops[index].wrap.is_some_and(|(other, _)| other != axis))
    {
        return Err(GeomError::Unsupported(
            "a face whose loops go round its surface both ways".into(),
        ));
    }
    let first = wrapping[0];
    let far = match (wrapping.len(), apexes.first()) {
        (2, _) => FarSide::Loop(&loops[wrapping[1]]),
        (1, Some(apex)) => {
            let uv = surface.invert(*apex, ctx.tol.len).ok_or_else(|| {
                GeomError::Degenerate("a vertex loop that is not on its face's surface".into())
            })?;
            FarSide::Point {
                point: *apex,
                across: uv[1 - axis],
            }
        }
        (1, None) => {
            // The material is to the left of the walk when the loop is counter-clockwise.
            let upwards = (signed_period > 0.0) == same_sense;
            let (point, across) =
                singular_end(surface, axis, upwards, ctx.tol.len).ok_or_else(|| {
                    GeomError::Unsupported(
                        "a face that goes round its surface with nothing to close it".into(),
                    )
                })?;
            FarSide::Point { point, across }
        }
        _ => {
            return Err(GeomError::Unsupported(
                "a face with more than two loops round its surface".into(),
            ));
        }
    };
    let holes: Vec<usize> = (0..loops.len())
        .filter(|index| !wrapping.contains(index))
        .collect();
    let period = signed_period.abs();
    let band = &loops[first];
    let mut chosen: Option<(FaceLoop, Vec<FaceLoop>)> = None;
    for cut in 0..band.points.len() {
        let Some(polygon) = band_polygon(ctx, surface, band, cut, &far, axis, signed_period) else {
            continue;
        };
        let (low, high) = polygon
            .uv
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), at| {
                (low.min(at[axis]), high.max(at[axis]))
            });
        let centre = 0.5 * (low + high);
        let mut shifted = Vec::with_capacity(holes.len());
        let mut crosses = false;
        for &index in &holes {
            let hole = &loops[index];
            let mean = hole.uv.iter().map(|at| at[axis]).sum::<f64>() / hole.uv.len() as f64;
            let shift = ((centre - mean) / period).round() * period;
            let uv: Vec<DVec2> = hole
                .uv
                .iter()
                .map(|at| {
                    let mut moved = *at;
                    moved[axis] += shift;
                    moved
                })
                .collect();
            let epsilon = 1e-9 * period;
            if uv
                .iter()
                .any(|at| at[axis] < low + epsilon || at[axis] > high - epsilon)
            {
                crosses = true;
                break;
            }
            shifted.push(FaceLoop {
                points: hole.points.clone(),
                uv,
                declared_outer: hole.declared_outer,
                wrap: None,
            });
        }
        if !crosses {
            chosen = Some((polygon, shifted));
            break;
        }
    }
    let Some((polygon, mut holes)) = chosen else {
        return Err(GeomError::Unsupported(
            "a face whose holes cover every seam position round its surface".into(),
        ));
    };
    let mut out = vec![polygon];
    out.append(&mut holes);
    Ok(out)
}

/// The singular end of a surface along the across axis: a pole, or the
/// point where a revolved section meets its axis.
fn singular_end(
    surface: &Surface,
    axis: usize,
    upwards: bool,
    tolerance: f64,
) -> Option<(DVec3, f64)> {
    if axis != 0 {
        return None;
    }
    let across = match &surface.kind {
        SurfaceKind::Sphere { .. } => {
            if upwards {
                std::f64::consts::FRAC_PI_2
            } else {
                -std::f64::consts::FRAC_PI_2
            }
        }
        SurfaceKind::Revolution {
            section, closed, ..
        } => {
            if *closed || section.len() < 2 {
                return None;
            }
            if upwards {
                (section.len() - 1) as f64
            } else {
                0.0
            }
        }
        _ => return None,
    };
    let point = surface.point(DVec2::new(0.0, across));
    surface
        .is_singular(point, tolerance)
        .then_some((point, across))
}

/// The polygon of a band: the loop from its cut sample round to the same
/// sample one period on, up the seam, back along the far side and down again.
fn band_polygon(
    ctx: &EvalCtx<'_>,
    surface: &Surface,
    band: &FaceLoop,
    cut: usize,
    far: &FarSide<'_>,
    axis: usize,
    signed_period: f64,
) -> Option<FaceLoop> {
    let across = 1 - axis;
    let period = signed_period.abs();
    let count = band.points.len();
    let mut points: Vec<DVec3> = Vec::with_capacity(count * 2 + 8);
    let mut uv: Vec<DVec2> = Vec::with_capacity(count * 2 + 8);
    for step in 0..=count {
        let index = (cut + step) % count;
        let mut at = band.uv[index];
        if let Some(previous) = uv.last() {
            at[axis] = unwrap(previous[axis], at[axis], Some(period));
        }
        if step == count {
            at[axis] = uv[0][axis] + signed_period;
        }
        points.push(band.points[index]);
        uv.push(at);
    }
    let near = uv[0][axis];
    let far_along = uv[count][axis];

    let (far_points, far_uv): (Vec<DVec3>, Vec<DVec2>) = match far {
        FarSide::Point {
            point,
            across: level,
        } => {
            let mut high = DVec2::ZERO;
            high[axis] = far_along;
            high[across] = *level;
            let mut low = high;
            low[axis] = near;
            (vec![*point, *point], vec![high, low])
        }
        FarSide::Loop(other) => {
            let other_count = other.points.len();
            let distance = |value: f64| {
                let offset = (value - near).rem_euclid(period);
                offset.min(period - offset)
            };
            let start = (0..other_count).min_by(|&a, &b| {
                distance(other.uv[a][axis]).total_cmp(&distance(other.uv[b][axis]))
            })?;
            // Walk the far loop against this loop's direction, from the far
            // end of the seam back to the near end.
            let backwards =
                other.wrap.map(|(_, travel)| travel.signum())? == signed_period.signum();
            let mut far_points = Vec::with_capacity(other_count + 1);
            let mut far_uv: Vec<DVec2> = Vec::with_capacity(other_count + 1);
            for step in 0..=other_count {
                let index = if backwards {
                    (start + other_count - (step % other_count)) % other_count
                } else {
                    (start + step) % other_count
                };
                let mut at = other.uv[index];
                match far_uv.last() {
                    None => {
                        at[axis] += ((far_along - at[axis]) / period).round() * period;
                    }
                    Some(previous) => {
                        at[axis] = unwrap(previous[axis], at[axis], Some(period));
                    }
                }
                if step == other_count {
                    at[axis] = far_uv[0][axis] - signed_period;
                }
                far_points.push(other.points[index]);
                far_uv.push(at);
            }
            (far_points, far_uv)
        }
    };

    let seam = seam_points(ctx, surface, uv[count], far_uv[0]);
    for (point, at) in &seam {
        points.push(*point);
        uv.push(*at);
    }
    points.extend_from_slice(&far_points);
    uv.extend_from_slice(&far_uv);
    for (point, at) in seam.iter().rev() {
        let mut moved = *at;
        moved[axis] -= signed_period;
        points.push(*point);
        uv.push(moved);
    }
    Some(FaceLoop {
        points,
        uv,
        declared_outer: true,
        wrap: None,
    })
}

/// Points inside a seam between two parameters, enough that the chords
/// between them follow the surface within the chord tolerance.
fn seam_points(
    ctx: &EvalCtx<'_>,
    surface: &Surface,
    from: DVec2,
    to: DVec2,
) -> Vec<(DVec3, DVec2)> {
    let tolerance = ctx.settings.chord_tolerance_m.max(ctx.tol.len);
    let mut pieces = 1usize;
    for _ in 0..MAX_SEAM_DOUBLINGS {
        let met = (0..pieces).all(|piece| {
            let a = from.lerp(to, piece as f64 / pieces as f64);
            let b = from.lerp(to, (piece + 1) as f64 / pieces as f64);
            let chord = (surface.point(a) + surface.point(b)) * 0.5;
            surface.point((a + b) * 0.5).distance(chord) <= tolerance
        });
        if met {
            break;
        }
        pieces *= 2;
    }
    (1..pieces)
        .map(|piece| {
            let at = from.lerp(to, piece as f64 / pieces as f64);
            (surface.point(at), at)
        })
        .collect()
}

pub(crate) fn spherical_cap(
    surface: &Surface,
    ctx: &EvalCtx<'_>,
    boundary: &[DVec3],
    same_sense: bool,
    face_id: u32,
) -> Result<Option<Mesh64>, GeomError> {
    let SurfaceKind::Sphere { radius } = surface.kind else {
        return Ok(None);
    };
    if boundary.len() < 3 {
        return Ok(None);
    }
    let centre = surface.frame.transform_point3(DVec3::ZERO);
    let normal: DVec3 = boundary
        .iter()
        .zip(boundary.iter().cycle().skip(1))
        .map(|(a, b)| (*a - centre).cross(*b - centre))
        .sum();
    let Some(normal) = normal.try_normalize() else {
        return Ok(None);
    };
    let height = (boundary[0] - centre).dot(normal);
    let tolerance = ctx.tol.len.max(radius * 1e-9);
    if boundary.iter().any(|point| {
        !point.is_finite()
            || ((point - centre).length() - radius).abs() > tolerance
            || ((point - centre).dot(normal) - height).abs() > tolerance
    }) {
        return Ok(None);
    }
    let circle_centre = centre + normal * height;
    let mut turn = 0.0;
    for (a, b) in boundary.iter().zip(boundary.iter().cycle().skip(1)) {
        let a = *a - circle_centre;
        let b = *b - circle_centre;
        let angle = normal.dot(a.cross(b)).atan2(a.dot(b));
        if angle <= 0.0 || angle >= std::f64::consts::PI {
            return Ok(None);
        }
        turn += angle;
    }
    if (turn - std::f64::consts::TAU).abs() > 1e-6 {
        return Ok(None);
    }
    let axis = if same_sense { normal } else { -normal };
    let angle = ((boundary[0] - centre).dot(axis) / radius)
        .clamp(-1.0, 1.0)
        .acos();
    let step = std::f64::consts::TAU / ctx.segments_for_radius(radius) as f64 * 0.2;
    let rows = (angle / step).ceil().max(1.0) as usize;
    if boundary.len().saturating_mul(rows).saturating_add(1)
        > ctx.settings.max_surface_vertices as usize
    {
        return Err(GeomError::LimitReached("spherical cap vertices".into()));
    }
    let mut mesh = Mesh64::new();
    mesh.positions.extend_from_slice(boundary);
    let radial: Vec<DVec3> = boundary
        .iter()
        .map(|p| (*p - circle_centre).normalize())
        .collect();
    for row in 1..rows {
        let theta = angle * (1.0 - row as f64 / rows as f64);
        for direction in &radial {
            mesh.positions
                .push(centre + radius * (axis * theta.cos() + *direction * theta.sin()));
        }
    }
    let pole = mesh.positions.len() as u32;
    mesh.positions.push(centre + radius * axis);
    let stride = boundary.len();
    for row in 0..rows {
        for i in 0..stride {
            let a = (row * stride + i) as u32;
            let b = (row * stride + (i + 1) % stride) as u32;
            if row + 1 == rows {
                mesh.push_triangle(a, b, pole);
            } else {
                let c = b + stride as u32;
                let d = a + stride as u32;
                mesh.push_triangle(a, b, c);
                mesh.push_triangle(a, c, d);
            }
        }
    }
    let unmet = mesh.indices.chunks_exact(3).any(|t| {
        let a = mesh.positions[t[0] as usize];
        let b = mesh.positions[t[1] as usize];
        let c = mesh.positions[t[2] as usize];
        [
            (a + b) * 0.5,
            (b + c) * 0.5,
            (c + a) * 0.5,
            (a + b + c) / 3.0,
        ]
        .iter()
        .any(|point| (radius - (*point - centre).length()).abs() > ctx.settings.chord_tolerance_m)
    });
    if unmet {
        ctx.diag.warn(
            codes::TESSELLATION_TOLERANCE_UNMET,
            face_id,
            "spherical cap boundary or interior sampling exceeds chord tolerance",
        );
    }
    mesh.remove_degenerate_triangles(ctx.tol.area);
    Ok(Some(mesh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiagnosticSink, Settings, Tolerances, Units};
    use glam::DMat4;

    #[test]
    fn a_spherical_cap_uses_the_boundary_frame_and_preserves_orientation() {
        let model = super::super::tests::model_of("#1=IFCCARTESIANPOINT((0.,0.,0.));");
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );
        // The sphere's own poles are on X, on this circular boundary in the XY plane.
        let surface = Surface::new(
            SurfaceKind::Sphere { radius: 0.5 },
            DMat4::from_rotation_y(std::f64::consts::FRAC_PI_2),
        );
        for (reverse, same_sense, expected_z) in
            [(false, true, 1.0), (true, false, 1.0), (true, true, -1.0)]
        {
            let mut boundary: Vec<_> = (0..72)
                .map(|i| {
                    let a = i as f64 * std::f64::consts::TAU / 72.0;
                    DVec3::new(a.cos() * 0.5, a.sin() * 0.5, 0.0)
                })
                .collect();
            if reverse {
                boundary.reverse()
            }
            let mesh = spherical_cap(&surface, &ctx, &boundary, same_sense, 1)
                .unwrap()
                .unwrap();
            assert_eq!(&mesh.positions[..boundary.len()], &boundary);
            assert!(mesh.positions.iter().all(|p| p.z * expected_z >= -1e-12));
            let mut area = 0.0;
            let mut volume = 0.0;
            for t in mesh.indices.chunks_exact(3) {
                let a = mesh.positions[t[0] as usize];
                let b = mesh.positions[t[1] as usize];
                let c = mesh.positions[t[2] as usize];
                area += (b - a).cross(c - a).length() * 0.5;
                volume += a.dot(b.cross(c)) / 6.0;
            }
            assert!((area - std::f64::consts::PI * 0.5).abs() < 0.01);
            assert!((volume.abs() - std::f64::consts::PI / 12.0).abs() < 0.002);
            assert_eq!(volume > 0.0, same_sense);
        }
        assert!(sink.take().is_empty());
        let off_surface = [DVec3::X, DVec3::Y, -DVec3::X];
        assert!(
            spherical_cap(&surface, &ctx, &off_surface, true, 1)
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod region_tests {
    use crate::codes;
    use crate::eval::tests::{eval_solid_with_diagnostics, model_of};
    use tessifc_model::Model;

    fn lines(items: &[&str]) -> String {
        let mut text = items.join("\n");
        text.push('\n');
        text
    }

    fn within(measured: f64, expected: f64, relative: f64) -> bool {
        (measured - expected).abs() <= relative * expected
    }

    /// The `surf-cone-apex` fixture; `#39` is the item.
    fn cone_with_apex_loop() -> (Model, u32) {
        (model_of(&cone_with_apex_loop_source()), 39)
    }

    fn cone_with_apex_loop_source() -> String {
        lines(&[
            "#1=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));",
            "#4=IFCDIRECTION((0.,0.,1.));",
            "#5=IFCDIRECTION((1.,0.,0.));",
            "#6=IFCAXIS2PLACEMENT3D(#3,#4,#5);",
            "#7=IFCCIRCLE(#6,0.5);",
            "#8=IFCEDGECURVE(#2,#2,#7,.T.);",
            "#9=IFCORIENTEDEDGE(*,*,#8,.T.);",
            "#10=IFCEDGELOOP((#9));",
            "#11=IFCFACEOUTERBOUND(#10,.T.);",
            "#12=IFCCARTESIANPOINT((0.,0.,1.));",
            "#13=IFCVERTEXPOINT(#12);",
            "#14=IFCVERTEXLOOP(#13);",
            "#15=IFCFACEBOUND(#14,.T.);",
            "#16=IFCCARTESIANPOINT((0.5,0.));",
            "#17=IFCCARTESIANPOINT((0.,1.));",
            "#18=IFCPOLYLINE((#16,#17));",
            "#19=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#18);",
            "#20=IFCCARTESIANPOINT((0.,0.,0.));",
            "#21=IFCDIRECTION((0.,-1.,0.));",
            "#22=IFCDIRECTION((1.,0.,0.));",
            "#23=IFCAXIS2PLACEMENT3D(#20,#21,#22);",
            "#24=IFCCARTESIANPOINT((0.,0.,0.));",
            "#25=IFCDIRECTION((0.,1.,0.));",
            "#26=IFCAXIS1PLACEMENT(#24,#25);",
            "#27=IFCSURFACEOFREVOLUTION(#19,#23,#26);",
            "#28=IFCADVANCEDFACE((#11,#15),#27,.T.);",
            "#29=IFCORIENTEDEDGE(*,*,#8,.F.);",
            "#30=IFCEDGELOOP((#29));",
            "#31=IFCFACEOUTERBOUND(#30,.T.);",
            "#32=IFCCARTESIANPOINT((0.,0.,0.));",
            "#33=IFCDIRECTION((0.,0.,-1.));",
            "#34=IFCDIRECTION((1.,0.,0.));",
            "#35=IFCAXIS2PLACEMENT3D(#32,#33,#34);",
            "#36=IFCPLANE(#35);",
            "#37=IFCADVANCEDFACE((#31),#36,.T.);",
            "#38=IFCCLOSEDSHELL((#28,#37));",
            "#39=IFCADVANCEDBREP(#38);",
        ])
    }

    /// The `surf-cone-slice` fixture; `#92` is the item.
    fn cone_slice() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((0.,0.5,0.));",
            "#4=IFCVERTEXPOINT(#3);",
            "#5=IFCCARTESIANPOINT((0.,0.,0.));",
            "#6=IFCDIRECTION((0.,0.,1.));",
            "#7=IFCDIRECTION((1.,0.,0.));",
            "#8=IFCAXIS2PLACEMENT3D(#5,#6,#7);",
            "#9=IFCCIRCLE(#8,0.5);",
            "#10=IFCEDGECURVE(#2,#4,#9,.T.);",
            "#11=IFCORIENTEDEDGE(*,*,#10,.T.);",
            "#12=IFCCARTESIANPOINT((0.,0.,1.));",
            "#13=IFCVERTEXPOINT(#12);",
            "#14=IFCCARTESIANPOINT((0.,0.5,0.));",
            "#15=IFCDIRECTION((0.,-0.44721359549995793,0.89442719099991586));",
            "#16=IFCVECTOR(#15,1.);",
            "#17=IFCLINE(#14,#16);",
            "#18=IFCEDGECURVE(#4,#13,#17,.T.);",
            "#19=IFCORIENTEDEDGE(*,*,#18,.T.);",
            "#20=IFCCARTESIANPOINT((0.,0.,1.));",
            "#21=IFCDIRECTION((0.44721359549995793,0.,-0.89442719099991586));",
            "#22=IFCVECTOR(#21,1.);",
            "#23=IFCLINE(#20,#22);",
            "#24=IFCEDGECURVE(#13,#2,#23,.T.);",
            "#25=IFCORIENTEDEDGE(*,*,#24,.T.);",
            "#26=IFCEDGELOOP((#11,#19,#25));",
            "#27=IFCFACEOUTERBOUND(#26,.T.);",
            "#28=IFCCARTESIANPOINT((0.5,0.));",
            "#29=IFCCARTESIANPOINT((0.,1.));",
            "#30=IFCPOLYLINE((#28,#29));",
            "#31=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#30);",
            "#32=IFCCARTESIANPOINT((0.,0.,0.));",
            "#33=IFCDIRECTION((0.,-1.,0.));",
            "#34=IFCDIRECTION((1.,0.,0.));",
            "#35=IFCAXIS2PLACEMENT3D(#32,#33,#34);",
            "#36=IFCCARTESIANPOINT((0.,0.,0.));",
            "#37=IFCDIRECTION((0.,1.,0.));",
            "#38=IFCAXIS1PLACEMENT(#36,#37);",
            "#39=IFCSURFACEOFREVOLUTION(#31,#35,#38);",
            "#40=IFCADVANCEDFACE((#27),#39,.T.);",
            "#41=IFCORIENTEDEDGE(*,*,#10,.F.);",
            "#42=IFCCARTESIANPOINT((0.,0.,0.));",
            "#43=IFCVERTEXPOINT(#42);",
            "#44=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#45=IFCDIRECTION((-1.,0.,0.));",
            "#46=IFCVECTOR(#45,1.);",
            "#47=IFCLINE(#44,#46);",
            "#48=IFCEDGECURVE(#2,#43,#47,.T.);",
            "#49=IFCORIENTEDEDGE(*,*,#48,.T.);",
            "#50=IFCCARTESIANPOINT((0.,0.,0.));",
            "#51=IFCDIRECTION((0.,1.,0.));",
            "#52=IFCVECTOR(#51,1.);",
            "#53=IFCLINE(#50,#52);",
            "#54=IFCEDGECURVE(#43,#4,#53,.T.);",
            "#55=IFCORIENTEDEDGE(*,*,#54,.T.);",
            "#56=IFCEDGELOOP((#41,#49,#55));",
            "#57=IFCFACEOUTERBOUND(#56,.T.);",
            "#58=IFCCARTESIANPOINT((0.,0.,0.));",
            "#59=IFCDIRECTION((0.,0.,-1.));",
            "#60=IFCDIRECTION((1.,0.,0.));",
            "#61=IFCAXIS2PLACEMENT3D(#58,#59,#60);",
            "#62=IFCPLANE(#61);",
            "#63=IFCADVANCEDFACE((#57),#62,.T.);",
            "#64=IFCORIENTEDEDGE(*,*,#48,.F.);",
            "#65=IFCORIENTEDEDGE(*,*,#24,.F.);",
            "#66=IFCCARTESIANPOINT((0.,0.,0.));",
            "#67=IFCDIRECTION((0.,0.,1.));",
            "#68=IFCVECTOR(#67,1.);",
            "#69=IFCLINE(#66,#68);",
            "#70=IFCEDGECURVE(#43,#13,#69,.T.);",
            "#71=IFCORIENTEDEDGE(*,*,#70,.F.);",
            "#72=IFCEDGELOOP((#64,#65,#71));",
            "#73=IFCFACEOUTERBOUND(#72,.T.);",
            "#74=IFCCARTESIANPOINT((0.,0.,0.));",
            "#75=IFCDIRECTION((0.,-1.,0.));",
            "#76=IFCDIRECTION((1.,0.,0.));",
            "#77=IFCAXIS2PLACEMENT3D(#74,#75,#76);",
            "#78=IFCPLANE(#77);",
            "#79=IFCADVANCEDFACE((#73),#78,.T.);",
            "#80=IFCORIENTEDEDGE(*,*,#54,.F.);",
            "#81=IFCORIENTEDEDGE(*,*,#70,.T.);",
            "#82=IFCORIENTEDEDGE(*,*,#18,.F.);",
            "#83=IFCEDGELOOP((#80,#81,#82));",
            "#84=IFCFACEOUTERBOUND(#83,.T.);",
            "#85=IFCCARTESIANPOINT((0.,0.,0.));",
            "#86=IFCDIRECTION((-1.,0.,0.));",
            "#87=IFCDIRECTION((0.,1.,0.));",
            "#88=IFCAXIS2PLACEMENT3D(#85,#86,#87);",
            "#89=IFCPLANE(#88);",
            "#90=IFCADVANCEDFACE((#84),#89,.T.);",
            "#91=IFCCLOSEDSHELL((#40,#63,#79,#90));",
            "#92=IFCADVANCEDBREP(#91);",
        ]);
        (model_of(&source), 92)
    }

    /// The `surf-sphere-zone` fixture; `#48` is the item.
    fn sphere_zone() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.45825756949558399,0.,-0.20000000000000001));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((0.,0.,-0.20000000000000001));",
            "#4=IFCDIRECTION((0.,0.,1.));",
            "#5=IFCDIRECTION((1.,0.,0.));",
            "#6=IFCAXIS2PLACEMENT3D(#3,#4,#5);",
            "#7=IFCCIRCLE(#6,0.45825756949558399);",
            "#8=IFCEDGECURVE(#2,#2,#7,.T.);",
            "#9=IFCORIENTEDEDGE(*,*,#8,.T.);",
            "#10=IFCEDGELOOP((#9));",
            "#11=IFCFACEOUTERBOUND(#10,.T.);",
            "#12=IFCCARTESIANPOINT((0.40000000000000002,0.,0.29999999999999999));",
            "#13=IFCVERTEXPOINT(#12);",
            "#14=IFCCARTESIANPOINT((0.,0.,0.29999999999999999));",
            "#15=IFCDIRECTION((0.,0.,1.));",
            "#16=IFCDIRECTION((1.,0.,0.));",
            "#17=IFCAXIS2PLACEMENT3D(#14,#15,#16);",
            "#18=IFCCIRCLE(#17,0.40000000000000002);",
            "#19=IFCEDGECURVE(#13,#13,#18,.T.);",
            "#20=IFCORIENTEDEDGE(*,*,#19,.F.);",
            "#21=IFCEDGELOOP((#20));",
            "#22=IFCFACEBOUND(#21,.T.);",
            "#23=IFCCARTESIANPOINT((0.,0.,0.));",
            "#24=IFCDIRECTION((0.,0.,1.));",
            "#25=IFCDIRECTION((1.,0.,0.));",
            "#26=IFCAXIS2PLACEMENT3D(#23,#24,#25);",
            "#27=IFCSPHERICALSURFACE(#26,0.5);",
            "#28=IFCADVANCEDFACE((#11,#22),#27,.T.);",
            "#29=IFCORIENTEDEDGE(*,*,#8,.F.);",
            "#30=IFCEDGELOOP((#29));",
            "#31=IFCFACEOUTERBOUND(#30,.T.);",
            "#32=IFCCARTESIANPOINT((0.,0.,-0.20000000000000001));",
            "#33=IFCDIRECTION((0.,0.,-1.));",
            "#34=IFCDIRECTION((1.,0.,0.));",
            "#35=IFCAXIS2PLACEMENT3D(#32,#33,#34);",
            "#36=IFCPLANE(#35);",
            "#37=IFCADVANCEDFACE((#31),#36,.T.);",
            "#38=IFCORIENTEDEDGE(*,*,#19,.T.);",
            "#39=IFCEDGELOOP((#38));",
            "#40=IFCFACEOUTERBOUND(#39,.T.);",
            "#41=IFCCARTESIANPOINT((0.,0.,0.29999999999999999));",
            "#42=IFCDIRECTION((0.,0.,1.));",
            "#43=IFCDIRECTION((1.,0.,0.));",
            "#44=IFCAXIS2PLACEMENT3D(#41,#42,#43);",
            "#45=IFCPLANE(#44);",
            "#46=IFCADVANCEDFACE((#40),#45,.T.);",
            "#47=IFCCLOSEDSHELL((#28,#37,#46));",
            "#48=IFCADVANCEDBREP(#47);",
        ]);
        (model_of(&source), 48)
    }

    /// The `surf-sphere-holed-cap` fixture; `#39` is the item.
    fn holed_dome() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((0.,0.,0.));",
            "#4=IFCDIRECTION((0.,0.,1.));",
            "#5=IFCDIRECTION((1.,0.,0.));",
            "#6=IFCAXIS2PLACEMENT3D(#3,#4,#5);",
            "#7=IFCCIRCLE(#6,0.5);",
            "#8=IFCEDGECURVE(#2,#2,#7,.T.);",
            "#9=IFCORIENTEDEDGE(*,*,#8,.T.);",
            "#10=IFCEDGELOOP((#9));",
            "#11=IFCFACEOUTERBOUND(#10,.T.);",
            "#12=IFCCARTESIANPOINT((0.43301270189221935,0.,0.25000000000000006));",
            "#13=IFCVERTEXPOINT(#12);",
            "#14=IFCCARTESIANPOINT((0.3415063509461097,0.,0.3415063509461097));",
            "#15=IFCDIRECTION((-0.70710678118654757,-0.,-0.70710678118654757));",
            "#16=IFCDIRECTION((0.70710678118654757,0.,-0.70710678118654757));",
            "#17=IFCAXIS2PLACEMENT3D(#14,#15,#16);",
            "#18=IFCCIRCLE(#17,0.12940952255126037);",
            "#19=IFCEDGECURVE(#13,#13,#18,.T.);",
            "#20=IFCORIENTEDEDGE(*,*,#19,.T.);",
            "#21=IFCEDGELOOP((#20));",
            "#22=IFCFACEBOUND(#21,.T.);",
            "#23=IFCCARTESIANPOINT((0.,0.,0.));",
            "#24=IFCDIRECTION((0.,0.,1.));",
            "#25=IFCDIRECTION((1.,0.,0.));",
            "#26=IFCAXIS2PLACEMENT3D(#23,#24,#25);",
            "#27=IFCSPHERICALSURFACE(#26,0.5);",
            "#28=IFCADVANCEDFACE((#11,#22),#27,.T.);",
            "#29=IFCORIENTEDEDGE(*,*,#8,.F.);",
            "#30=IFCEDGELOOP((#29));",
            "#31=IFCFACEOUTERBOUND(#30,.T.);",
            "#32=IFCCARTESIANPOINT((0.,0.,0.));",
            "#33=IFCDIRECTION((0.,0.,-1.));",
            "#34=IFCDIRECTION((1.,0.,0.));",
            "#35=IFCAXIS2PLACEMENT3D(#32,#33,#34);",
            "#36=IFCPLANE(#35);",
            "#37=IFCADVANCEDFACE((#31),#36,.T.);",
            "#38=IFCOPENSHELL((#28,#37));",
            "#39=IFCSHELLBASEDSURFACEMODEL((#38));",
        ]);
        (model_of(&source), 39)
    }

    /// The `surf-tube-hole` fixture; `#99` is the item.
    fn tube_with_window() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.38242109364224425,0.32210884361884551,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((-0.49499624830022271,0.070560004029933607,0.));",
            "#4=IFCVERTEXPOINT(#3);",
            "#5=IFCCARTESIANPOINT((0.,0.,0.));",
            "#6=IFCDIRECTION((0.,0.,1.));",
            "#7=IFCDIRECTION((1.,0.,0.));",
            "#8=IFCAXIS2PLACEMENT3D(#5,#6,#7);",
            "#9=IFCCIRCLE(#8,0.5);",
            "#10=IFCEDGECURVE(#2,#4,#9,.T.);",
            "#11=IFCORIENTEDEDGE(*,*,#10,.T.);",
            "#12=IFCEDGECURVE(#4,#2,#9,.T.);",
            "#13=IFCORIENTEDEDGE(*,*,#12,.T.);",
            "#14=IFCCARTESIANPOINT((0.38242109364224425,0.32210884361884551,2.));",
            "#15=IFCVERTEXPOINT(#14);",
            "#16=IFCCARTESIANPOINT((0.38242109364224425,0.32210884361884551,0.));",
            "#17=IFCDIRECTION((0.,0.,1.));",
            "#18=IFCVECTOR(#17,1.);",
            "#19=IFCLINE(#16,#18);",
            "#20=IFCEDGECURVE(#2,#15,#19,.T.);",
            "#21=IFCORIENTEDEDGE(*,*,#20,.T.);",
            "#22=IFCCARTESIANPOINT((-0.49499624830022271,0.070560004029933607,2.));",
            "#23=IFCVERTEXPOINT(#22);",
            "#24=IFCCARTESIANPOINT((0.,0.,2.));",
            "#25=IFCDIRECTION((0.,0.,1.));",
            "#26=IFCDIRECTION((1.,0.,0.));",
            "#27=IFCAXIS2PLACEMENT3D(#24,#25,#26);",
            "#28=IFCCIRCLE(#27,0.5);",
            "#29=IFCEDGECURVE(#23,#15,#28,.T.);",
            "#30=IFCORIENTEDEDGE(*,*,#29,.F.);",
            "#31=IFCEDGECURVE(#15,#23,#28,.T.);",
            "#32=IFCORIENTEDEDGE(*,*,#31,.F.);",
            "#33=IFCORIENTEDEDGE(*,*,#20,.F.);",
            "#34=IFCEDGELOOP((#11,#13,#21,#30,#32,#33));",
            "#35=IFCFACEOUTERBOUND(#34,.T.);",
            "#36=IFCCARTESIANPOINT((-0.24513041067034971,-0.4357878862067941,0.39999999999999997));",
            "#37=IFCVERTEXPOINT(#36);",
            "#38=IFCCARTESIANPOINT((-0.24513041067034971,-0.4357878862067941,1.));",
            "#39=IFCVERTEXPOINT(#38);",
            "#40=IFCCARTESIANPOINT((-0.24513041067034971,-0.4357878862067941,0.39999999999999997));",
            "#41=IFCDIRECTION((0.,0.,1.));",
            "#42=IFCVECTOR(#41,1.);",
            "#43=IFCLINE(#40,#42);",
            "#44=IFCEDGECURVE(#37,#39,#43,.T.);",
            "#45=IFCORIENTEDEDGE(*,*,#44,.T.);",
            "#46=IFCCARTESIANPOINT((0.043749491719723199,-0.49808230441792034,1.));",
            "#47=IFCVERTEXPOINT(#46);",
            "#48=IFCCARTESIANPOINT((0.,0.,1.));",
            "#49=IFCDIRECTION((0.,0.,1.));",
            "#50=IFCDIRECTION((1.,0.,0.));",
            "#51=IFCAXIS2PLACEMENT3D(#48,#49,#50);",
            "#52=IFCCIRCLE(#51,0.5);",
            "#53=IFCEDGECURVE(#39,#47,#52,.T.);",
            "#54=IFCORIENTEDEDGE(*,*,#53,.T.);",
            "#55=IFCCARTESIANPOINT((0.043749491719723199,-0.49808230441792034,0.39999999999999997));",
            "#56=IFCVERTEXPOINT(#55);",
            "#57=IFCCARTESIANPOINT((0.043749491719723199,-0.49808230441792034,0.39999999999999997));",
            "#58=IFCDIRECTION((0.,0.,1.));",
            "#59=IFCVECTOR(#58,1.);",
            "#60=IFCLINE(#57,#59);",
            "#61=IFCEDGECURVE(#56,#47,#60,.T.);",
            "#62=IFCORIENTEDEDGE(*,*,#61,.F.);",
            "#63=IFCCARTESIANPOINT((0.,0.,0.39999999999999997));",
            "#64=IFCDIRECTION((0.,0.,1.));",
            "#65=IFCDIRECTION((1.,0.,0.));",
            "#66=IFCAXIS2PLACEMENT3D(#63,#64,#65);",
            "#67=IFCCIRCLE(#66,0.5);",
            "#68=IFCEDGECURVE(#37,#56,#67,.T.);",
            "#69=IFCORIENTEDEDGE(*,*,#68,.F.);",
            "#70=IFCEDGELOOP((#45,#54,#62,#69));",
            "#71=IFCFACEBOUND(#70,.T.);",
            "#72=IFCCARTESIANPOINT((0.,0.,0.));",
            "#73=IFCDIRECTION((0.,0.,1.));",
            "#74=IFCDIRECTION((1.,0.,0.));",
            "#75=IFCAXIS2PLACEMENT3D(#72,#73,#74);",
            "#76=IFCCYLINDRICALSURFACE(#75,0.5);",
            "#77=IFCADVANCEDFACE((#35,#71),#76,.T.);",
            "#78=IFCORIENTEDEDGE(*,*,#12,.F.);",
            "#79=IFCORIENTEDEDGE(*,*,#10,.F.);",
            "#80=IFCEDGELOOP((#78,#79));",
            "#81=IFCFACEOUTERBOUND(#80,.T.);",
            "#82=IFCCARTESIANPOINT((0.,0.,0.));",
            "#83=IFCDIRECTION((0.,0.,1.));",
            "#84=IFCDIRECTION((1.,0.,0.));",
            "#85=IFCAXIS2PLACEMENT3D(#82,#83,#84);",
            "#86=IFCPLANE(#85);",
            "#87=IFCADVANCEDFACE((#81),#86,.F.);",
            "#88=IFCORIENTEDEDGE(*,*,#31,.T.);",
            "#89=IFCORIENTEDEDGE(*,*,#29,.T.);",
            "#90=IFCEDGELOOP((#88,#89));",
            "#91=IFCFACEOUTERBOUND(#90,.T.);",
            "#92=IFCCARTESIANPOINT((0.,0.,2.));",
            "#93=IFCDIRECTION((0.,0.,1.));",
            "#94=IFCDIRECTION((1.,0.,0.));",
            "#95=IFCAXIS2PLACEMENT3D(#92,#93,#94);",
            "#96=IFCPLANE(#95);",
            "#97=IFCADVANCEDFACE((#91),#96,.T.);",
            "#98=IFCOPENSHELL((#77,#87,#97));",
            "#99=IFCSHELLBASEDSURFACEMODEL((#98));",
        ]);
        (model_of(&source), 99)
    }

    /// The `surf-cylinder-oblique` fixture; `#58` is the item.
    fn oblique_cylinder() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.38242109364224425,0.32210884361884551,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((-0.49499624830022271,0.070560004029933607,0.));",
            "#4=IFCVERTEXPOINT(#3);",
            "#5=IFCCARTESIANPOINT((0.,0.,0.));",
            "#6=IFCDIRECTION((0.,0.,1.));",
            "#7=IFCDIRECTION((1.,0.,0.));",
            "#8=IFCAXIS2PLACEMENT3D(#5,#6,#7);",
            "#9=IFCCIRCLE(#8,0.5);",
            "#10=IFCEDGECURVE(#2,#4,#9,.T.);",
            "#11=IFCORIENTEDEDGE(*,*,#10,.T.);",
            "#12=IFCEDGECURVE(#4,#2,#9,.T.);",
            "#13=IFCORIENTEDEDGE(*,*,#12,.T.);",
            "#14=IFCCARTESIANPOINT((0.38242109364224425,0.32210884361884551,1.191210546821122));",
            "#15=IFCVERTEXPOINT(#14);",
            "#16=IFCCARTESIANPOINT((0.38242109364224425,0.32210884361884551,0.));",
            "#17=IFCDIRECTION((0.,0.,1.));",
            "#18=IFCVECTOR(#17,1.);",
            "#19=IFCLINE(#16,#18);",
            "#20=IFCEDGECURVE(#2,#15,#19,.T.);",
            "#21=IFCORIENTEDEDGE(*,*,#20,.T.);",
            "#22=IFCCARTESIANPOINT((0.,0.,1.));",
            "#23=IFCDIRECTION((-0.44721359549995793,0.,0.89442719099991586));",
            "#24=IFCDIRECTION((0.89442719099991586,0.,0.44721359549995793));",
            "#25=IFCAXIS2PLACEMENT3D(#22,#23,#24);",
            "#26=IFCELLIPSE(#25,0.55901699437494745,0.5);",
            "#27=IFCEDGECURVE(#15,#15,#26,.T.);",
            "#28=IFCORIENTEDEDGE(*,*,#27,.F.);",
            "#29=IFCORIENTEDEDGE(*,*,#20,.F.);",
            "#30=IFCEDGELOOP((#11,#13,#21,#28,#29));",
            "#31=IFCFACEOUTERBOUND(#30,.T.);",
            "#32=IFCCARTESIANPOINT((0.,0.,0.));",
            "#33=IFCDIRECTION((0.,0.,1.));",
            "#34=IFCDIRECTION((1.,0.,0.));",
            "#35=IFCAXIS2PLACEMENT3D(#32,#33,#34);",
            "#36=IFCCYLINDRICALSURFACE(#35,0.5);",
            "#37=IFCADVANCEDFACE((#31),#36,.T.);",
            "#38=IFCORIENTEDEDGE(*,*,#12,.F.);",
            "#39=IFCORIENTEDEDGE(*,*,#10,.F.);",
            "#40=IFCEDGELOOP((#38,#39));",
            "#41=IFCFACEOUTERBOUND(#40,.T.);",
            "#42=IFCCARTESIANPOINT((0.,0.,0.));",
            "#43=IFCDIRECTION((0.,0.,-1.));",
            "#44=IFCDIRECTION((1.,0.,0.));",
            "#45=IFCAXIS2PLACEMENT3D(#42,#43,#44);",
            "#46=IFCPLANE(#45);",
            "#47=IFCADVANCEDFACE((#41),#46,.T.);",
            "#48=IFCORIENTEDEDGE(*,*,#27,.T.);",
            "#49=IFCEDGELOOP((#48));",
            "#50=IFCFACEOUTERBOUND(#49,.T.);",
            "#51=IFCCARTESIANPOINT((0.,0.,1.));",
            "#52=IFCDIRECTION((-0.44721359549995793,0.,0.89442719099991586));",
            "#53=IFCDIRECTION((0.89442719099991586,0.,0.44721359549995793));",
            "#54=IFCAXIS2PLACEMENT3D(#51,#52,#53);",
            "#55=IFCPLANE(#54);",
            "#56=IFCADVANCEDFACE((#50),#55,.T.);",
            "#57=IFCCLOSEDSHELL((#37,#47,#56));",
            "#58=IFCADVANCEDBREP(#57);",
        ]);
        (model_of(&source), 58)
    }

    /// The `surf-extrusion-pcurve` fixture; `#113` is the item.
    fn extrusion_by_pcurves() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((-0.5,0.,0.));",
            "#4=IFCVERTEXPOINT(#3);",
            "#5=IFCCARTESIANPOINT((0.,0.,0.));",
            "#6=IFCDIRECTION((0.,0.,1.));",
            "#7=IFCDIRECTION((1.,0.,0.));",
            "#8=IFCAXIS2PLACEMENT3D(#5,#6,#7);",
            "#9=IFCCIRCLE(#8,0.5);",
            "#10=IFCCARTESIANPOINT((0.,0.));",
            "#11=IFCDIRECTION((1.,0.));",
            "#12=IFCAXIS2PLACEMENT2D(#10,#11);",
            "#13=IFCCIRCLE(#12,0.5);",
            "#14=IFCTRIMMEDCURVE(#13,(IFCPARAMETERVALUE(-0.78539816339744828)),(IFCPARAMETERVALUE(3.9269908169872414)),.T.,.PARAMETER.);",
            "#15=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#14);",
            "#16=IFCCARTESIANPOINT((0.,0.,0.));",
            "#17=IFCDIRECTION((0.,0.,1.));",
            "#18=IFCDIRECTION((1.,0.,0.));",
            "#19=IFCAXIS2PLACEMENT3D(#16,#17,#18);",
            "#20=IFCDIRECTION((0.,0.,1.));",
            "#21=IFCSURFACEOFLINEAREXTRUSION(#15,#19,#20,3.);",
            "#22=IFCCARTESIANPOINT((0.,0.));",
            "#23=IFCCARTESIANPOINT((3.1415926535897931,0.));",
            "#24=IFCPOLYLINE((#22,#23));",
            "#25=IFCPCURVE(#21,#24);",
            "#26=IFCSURFACECURVE(#9,(#25),.PCURVE_S1.);",
            "#27=IFCEDGECURVE(#2,#4,#26,.T.);",
            "#28=IFCORIENTEDEDGE(*,*,#27,.T.);",
            "#29=IFCCARTESIANPOINT((-0.5,0.,2.));",
            "#30=IFCVERTEXPOINT(#29);",
            "#31=IFCCARTESIANPOINT((-0.5,0.,0.));",
            "#32=IFCDIRECTION((0.,0.,1.));",
            "#33=IFCVECTOR(#32,1.);",
            "#34=IFCLINE(#31,#33);",
            "#35=IFCCARTESIANPOINT((3.1415926535897931,0.));",
            "#36=IFCCARTESIANPOINT((3.1415926535897931,0.66666666666666663));",
            "#37=IFCPOLYLINE((#35,#36));",
            "#38=IFCPCURVE(#21,#37);",
            "#39=IFCSURFACECURVE(#34,(#38),.PCURVE_S1.);",
            "#40=IFCEDGECURVE(#4,#30,#39,.T.);",
            "#41=IFCORIENTEDEDGE(*,*,#40,.T.);",
            "#42=IFCCARTESIANPOINT((0.5,0.,2.));",
            "#43=IFCVERTEXPOINT(#42);",
            "#44=IFCCARTESIANPOINT((0.,0.,2.));",
            "#45=IFCDIRECTION((0.,0.,1.));",
            "#46=IFCDIRECTION((1.,0.,0.));",
            "#47=IFCAXIS2PLACEMENT3D(#44,#45,#46);",
            "#48=IFCCIRCLE(#47,0.5);",
            "#49=IFCCARTESIANPOINT((0.,0.66666666666666663));",
            "#50=IFCCARTESIANPOINT((3.1415926535897931,0.66666666666666663));",
            "#51=IFCPOLYLINE((#49,#50));",
            "#52=IFCPCURVE(#21,#51);",
            "#53=IFCSURFACECURVE(#48,(#52),.PCURVE_S1.);",
            "#54=IFCEDGECURVE(#43,#30,#53,.T.);",
            "#55=IFCORIENTEDEDGE(*,*,#54,.F.);",
            "#56=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#57=IFCDIRECTION((0.,0.,1.));",
            "#58=IFCVECTOR(#57,1.);",
            "#59=IFCLINE(#56,#58);",
            "#60=IFCCARTESIANPOINT((0.,0.));",
            "#61=IFCCARTESIANPOINT((0.,0.66666666666666663));",
            "#62=IFCPOLYLINE((#60,#61));",
            "#63=IFCPCURVE(#21,#62);",
            "#64=IFCSURFACECURVE(#59,(#63),.PCURVE_S1.);",
            "#65=IFCEDGECURVE(#2,#43,#64,.T.);",
            "#66=IFCORIENTEDEDGE(*,*,#65,.F.);",
            "#67=IFCEDGELOOP((#28,#41,#55,#66));",
            "#68=IFCFACEOUTERBOUND(#67,.T.);",
            "#69=IFCADVANCEDFACE((#68),#21,.T.);",
            "#70=IFCCARTESIANPOINT((-0.5,0.,0.));",
            "#71=IFCDIRECTION((1.,0.,0.));",
            "#72=IFCVECTOR(#71,1.);",
            "#73=IFCLINE(#70,#72);",
            "#74=IFCEDGECURVE(#4,#2,#73,.T.);",
            "#75=IFCORIENTEDEDGE(*,*,#74,.T.);",
            "#76=IFCORIENTEDEDGE(*,*,#65,.T.);",
            "#77=IFCCARTESIANPOINT((-0.5,0.,2.));",
            "#78=IFCDIRECTION((1.,0.,0.));",
            "#79=IFCVECTOR(#78,1.);",
            "#80=IFCLINE(#77,#79);",
            "#81=IFCEDGECURVE(#30,#43,#80,.T.);",
            "#82=IFCORIENTEDEDGE(*,*,#81,.F.);",
            "#83=IFCORIENTEDEDGE(*,*,#40,.F.);",
            "#84=IFCEDGELOOP((#75,#76,#82,#83));",
            "#85=IFCFACEOUTERBOUND(#84,.T.);",
            "#86=IFCCARTESIANPOINT((0.,0.,0.));",
            "#87=IFCDIRECTION((0.,-1.,0.));",
            "#88=IFCDIRECTION((1.,0.,0.));",
            "#89=IFCAXIS2PLACEMENT3D(#86,#87,#88);",
            "#90=IFCPLANE(#89);",
            "#91=IFCADVANCEDFACE((#85),#90,.T.);",
            "#92=IFCORIENTEDEDGE(*,*,#27,.F.);",
            "#93=IFCORIENTEDEDGE(*,*,#74,.F.);",
            "#94=IFCEDGELOOP((#92,#93));",
            "#95=IFCFACEOUTERBOUND(#94,.T.);",
            "#96=IFCCARTESIANPOINT((0.,0.,0.));",
            "#97=IFCDIRECTION((0.,0.,-1.));",
            "#98=IFCDIRECTION((1.,0.,0.));",
            "#99=IFCAXIS2PLACEMENT3D(#96,#97,#98);",
            "#100=IFCPLANE(#99);",
            "#101=IFCADVANCEDFACE((#95),#100,.T.);",
            "#102=IFCORIENTEDEDGE(*,*,#54,.T.);",
            "#103=IFCORIENTEDEDGE(*,*,#81,.T.);",
            "#104=IFCEDGELOOP((#102,#103));",
            "#105=IFCFACEOUTERBOUND(#104,.T.);",
            "#106=IFCCARTESIANPOINT((0.,0.,2.));",
            "#107=IFCDIRECTION((0.,0.,1.));",
            "#108=IFCDIRECTION((1.,0.,0.));",
            "#109=IFCAXIS2PLACEMENT3D(#106,#107,#108);",
            "#110=IFCPLANE(#109);",
            "#111=IFCADVANCEDFACE((#105),#110,.T.);",
            "#112=IFCCLOSEDSHELL((#69,#91,#101,#111));",
            "#113=IFCADVANCEDBREP(#112);",
        ]);
        (model_of(&source), 113)
    }

    /// The `surf-revolution-pcurve` fixture; `#107` is the item.
    fn cone_slice_by_pcurves() -> (Model, u32) {
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#2=IFCVERTEXPOINT(#1);",
            "#3=IFCCARTESIANPOINT((0.,0.5,0.));",
            "#4=IFCVERTEXPOINT(#3);",
            "#5=IFCCARTESIANPOINT((0.,0.,0.));",
            "#6=IFCDIRECTION((0.,0.,1.));",
            "#7=IFCDIRECTION((1.,0.,0.));",
            "#8=IFCAXIS2PLACEMENT3D(#5,#6,#7);",
            "#9=IFCCIRCLE(#8,0.5);",
            "#10=IFCCARTESIANPOINT((0.5,0.));",
            "#11=IFCCARTESIANPOINT((0.,1.));",
            "#12=IFCPOLYLINE((#10,#11));",
            "#13=IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#12);",
            "#14=IFCCARTESIANPOINT((0.,0.,0.));",
            "#15=IFCDIRECTION((0.,-1.,0.));",
            "#16=IFCDIRECTION((1.,0.,0.));",
            "#17=IFCAXIS2PLACEMENT3D(#14,#15,#16);",
            "#18=IFCCARTESIANPOINT((0.,0.,0.));",
            "#19=IFCDIRECTION((0.,1.,0.));",
            "#20=IFCAXIS1PLACEMENT(#18,#19);",
            "#21=IFCSURFACEOFREVOLUTION(#13,#17,#20);",
            "#22=IFCCARTESIANPOINT((0.,0.));",
            "#23=IFCCARTESIANPOINT((1.5707963267948966,0.));",
            "#24=IFCPOLYLINE((#22,#23));",
            "#25=IFCPCURVE(#21,#24);",
            "#26=IFCSURFACECURVE(#9,(#25),.PCURVE_S1.);",
            "#27=IFCEDGECURVE(#2,#4,#26,.T.);",
            "#28=IFCORIENTEDEDGE(*,*,#27,.T.);",
            "#29=IFCCARTESIANPOINT((0.,0.,1.));",
            "#30=IFCVERTEXPOINT(#29);",
            "#31=IFCCARTESIANPOINT((0.,0.5,0.));",
            "#32=IFCDIRECTION((0.,-0.44721359549995793,0.89442719099991586));",
            "#33=IFCVECTOR(#32,1.);",
            "#34=IFCLINE(#31,#33);",
            "#35=IFCCARTESIANPOINT((1.5707963267948966,0.));",
            "#36=IFCCARTESIANPOINT((1.5707963267948966,1.));",
            "#37=IFCPOLYLINE((#35,#36));",
            "#38=IFCPCURVE(#21,#37);",
            "#39=IFCSURFACECURVE(#34,(#38),.PCURVE_S1.);",
            "#40=IFCEDGECURVE(#4,#30,#39,.T.);",
            "#41=IFCORIENTEDEDGE(*,*,#40,.T.);",
            "#42=IFCCARTESIANPOINT((0.,0.,1.));",
            "#43=IFCDIRECTION((0.44721359549995793,0.,-0.89442719099991586));",
            "#44=IFCVECTOR(#43,1.);",
            "#45=IFCLINE(#42,#44);",
            "#46=IFCCARTESIANPOINT((0.,1.));",
            "#47=IFCCARTESIANPOINT((0.,0.));",
            "#48=IFCPOLYLINE((#46,#47));",
            "#49=IFCPCURVE(#21,#48);",
            "#50=IFCSURFACECURVE(#45,(#49),.PCURVE_S1.);",
            "#51=IFCEDGECURVE(#30,#2,#50,.T.);",
            "#52=IFCORIENTEDEDGE(*,*,#51,.T.);",
            "#53=IFCEDGELOOP((#28,#41,#52));",
            "#54=IFCFACEOUTERBOUND(#53,.T.);",
            "#55=IFCADVANCEDFACE((#54),#21,.T.);",
            "#56=IFCORIENTEDEDGE(*,*,#27,.F.);",
            "#57=IFCCARTESIANPOINT((0.,0.,0.));",
            "#58=IFCVERTEXPOINT(#57);",
            "#59=IFCCARTESIANPOINT((0.5,0.,0.));",
            "#60=IFCDIRECTION((-1.,0.,0.));",
            "#61=IFCVECTOR(#60,1.);",
            "#62=IFCLINE(#59,#61);",
            "#63=IFCEDGECURVE(#2,#58,#62,.T.);",
            "#64=IFCORIENTEDEDGE(*,*,#63,.T.);",
            "#65=IFCCARTESIANPOINT((0.,0.,0.));",
            "#66=IFCDIRECTION((0.,1.,0.));",
            "#67=IFCVECTOR(#66,1.);",
            "#68=IFCLINE(#65,#67);",
            "#69=IFCEDGECURVE(#58,#4,#68,.T.);",
            "#70=IFCORIENTEDEDGE(*,*,#69,.T.);",
            "#71=IFCEDGELOOP((#56,#64,#70));",
            "#72=IFCFACEOUTERBOUND(#71,.T.);",
            "#73=IFCCARTESIANPOINT((0.,0.,0.));",
            "#74=IFCDIRECTION((0.,0.,-1.));",
            "#75=IFCDIRECTION((1.,0.,0.));",
            "#76=IFCAXIS2PLACEMENT3D(#73,#74,#75);",
            "#77=IFCPLANE(#76);",
            "#78=IFCADVANCEDFACE((#72),#77,.T.);",
            "#79=IFCORIENTEDEDGE(*,*,#63,.F.);",
            "#80=IFCORIENTEDEDGE(*,*,#51,.F.);",
            "#81=IFCCARTESIANPOINT((0.,0.,0.));",
            "#82=IFCDIRECTION((0.,0.,1.));",
            "#83=IFCVECTOR(#82,1.);",
            "#84=IFCLINE(#81,#83);",
            "#85=IFCEDGECURVE(#58,#30,#84,.T.);",
            "#86=IFCORIENTEDEDGE(*,*,#85,.F.);",
            "#87=IFCEDGELOOP((#79,#80,#86));",
            "#88=IFCFACEOUTERBOUND(#87,.T.);",
            "#89=IFCCARTESIANPOINT((0.,0.,0.));",
            "#90=IFCDIRECTION((0.,-1.,0.));",
            "#91=IFCDIRECTION((1.,0.,0.));",
            "#92=IFCAXIS2PLACEMENT3D(#89,#90,#91);",
            "#93=IFCPLANE(#92);",
            "#94=IFCADVANCEDFACE((#88),#93,.T.);",
            "#95=IFCORIENTEDEDGE(*,*,#69,.F.);",
            "#96=IFCORIENTEDEDGE(*,*,#85,.T.);",
            "#97=IFCORIENTEDEDGE(*,*,#40,.F.);",
            "#98=IFCEDGELOOP((#95,#96,#97));",
            "#99=IFCFACEOUTERBOUND(#98,.T.);",
            "#100=IFCCARTESIANPOINT((0.,0.,0.));",
            "#101=IFCDIRECTION((-1.,0.,0.));",
            "#102=IFCDIRECTION((0.,1.,0.));",
            "#103=IFCAXIS2PLACEMENT3D(#100,#101,#102);",
            "#104=IFCPLANE(#103);",
            "#105=IFCADVANCEDFACE((#99),#104,.T.);",
            "#106=IFCCLOSEDSHELL((#55,#78,#94,#105));",
            "#107=IFCADVANCEDBREP(#106);",
        ]);
        (model_of(&source), 107)
    }

    #[test]
    fn a_cone_bounded_by_its_base_circle_and_an_apex_vertex_loop_closes() {
        let (model, item) = cone_with_apex_loop();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        let volume = std::f64::consts::PI * 0.25 / 3.0;
        assert!(
            within(mesh.signed_volume().abs(), volume, 0.01),
            "{}",
            mesh.signed_volume()
        );
        let area = std::f64::consts::PI * 0.5 * (0.5 + 1.25f64.sqrt());
        assert!(
            within(mesh.surface_area(), area, 0.01),
            "{}",
            mesh.surface_area()
        );
    }

    #[test]
    fn a_face_through_the_apex_takes_two_parameters_there() {
        let (model, item) = cone_slice();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        let volume = std::f64::consts::PI * 0.25 / 12.0;
        assert!(
            within(mesh.signed_volume().abs(), volume, 0.01),
            "{}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn two_loops_round_a_sphere_bound_the_zone_between_them() {
        let (model, item) = sphere_zone();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        let volume = std::f64::consts::PI * 0.5 * (3.0 * 0.21 + 3.0 * 0.16 + 0.25) / 6.0;
        assert!(
            within(mesh.signed_volume().abs(), volume, 0.01),
            "{}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn a_dome_keeps_a_hole_away_from_its_pole_and_its_seam() {
        let (model, item) = holed_dome();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let area = 2.0 * std::f64::consts::PI * 0.25 * 15f64.to_radians().cos()
            + std::f64::consts::PI * 0.25;
        assert!(
            within(mesh.surface_area(), area, 0.01),
            "{}",
            mesh.surface_area()
        );
    }

    #[test]
    fn a_window_in_a_seam_face_is_moved_into_the_faces_period() {
        let (model, item) = tube_with_window();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let area = 2.0 * std::f64::consts::PI * 0.5 * 2.0 - 0.5 * 0.6 * 0.6
            + 2.0 * std::f64::consts::PI * 0.25;
        assert!(
            within(mesh.surface_area(), area, 0.01),
            "{}",
            mesh.surface_area()
        );
    }

    #[test]
    fn a_seam_face_with_a_curved_top_wraps_its_whole_surface() {
        let (model, item) = oblique_cylinder();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        assert!(within(
            mesh.signed_volume().abs(),
            std::f64::consts::PI * 0.25,
            0.01
        ));
    }

    #[test]
    fn pcurves_on_an_extrusion_read_the_swept_curves_parameter() {
        let (model, item) = extrusion_by_pcurves();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        assert!(within(
            mesh.signed_volume().abs(),
            std::f64::consts::PI * 0.25,
            0.01
        ));
        let (low, high) = mesh.bounds().unwrap();
        assert!(
            (high.z - 2.0).abs() < 1e-9 && low.z.abs() < 1e-9,
            "{low} {high}"
        );
    }

    #[test]
    fn pcurves_on_a_revolution_read_the_angle_and_the_curve_parameter() {
        let (model, item) = cone_slice_by_pcurves();
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, item);
        let mesh = mesh.unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(mesh.closed, Some(true));
        let volume = std::f64::consts::PI * 0.25 / 12.0;
        assert!(
            within(mesh.signed_volume().abs(), volume, 0.01),
            "{}",
            mesh.signed_volume()
        );
    }

    #[test]
    fn a_vertex_loop_away_from_any_apex_is_noted_and_ignored() {
        // A unit square on a plane with a vertex loop in its middle.
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.,0.,0.));",
            "#2=IFCCARTESIANPOINT((1.,0.,0.));",
            "#3=IFCCARTESIANPOINT((1.,1.,0.));",
            "#4=IFCCARTESIANPOINT((0.,1.,0.));",
            "#5=IFCVERTEXPOINT(#1);",
            "#6=IFCVERTEXPOINT(#2);",
            "#7=IFCVERTEXPOINT(#3);",
            "#8=IFCVERTEXPOINT(#4);",
            "#9=IFCDIRECTION((1.,0.,0.));",
            "#10=IFCVECTOR(#9,1.);",
            "#11=IFCLINE(#1,#10);",
            "#12=IFCDIRECTION((0.,1.,0.));",
            "#13=IFCVECTOR(#12,1.);",
            "#14=IFCLINE(#2,#13);",
            "#15=IFCDIRECTION((-1.,0.,0.));",
            "#16=IFCVECTOR(#15,1.);",
            "#17=IFCLINE(#3,#16);",
            "#18=IFCDIRECTION((0.,-1.,0.));",
            "#19=IFCVECTOR(#18,1.);",
            "#20=IFCLINE(#4,#19);",
            "#21=IFCEDGECURVE(#5,#6,#11,.T.);",
            "#22=IFCEDGECURVE(#6,#7,#14,.T.);",
            "#23=IFCEDGECURVE(#7,#8,#17,.T.);",
            "#24=IFCEDGECURVE(#8,#5,#20,.T.);",
            "#25=IFCORIENTEDEDGE(*,*,#21,.T.);",
            "#26=IFCORIENTEDEDGE(*,*,#22,.T.);",
            "#27=IFCORIENTEDEDGE(*,*,#23,.T.);",
            "#28=IFCORIENTEDEDGE(*,*,#24,.T.);",
            "#29=IFCEDGELOOP((#25,#26,#27,#28));",
            "#30=IFCFACEOUTERBOUND(#29,.T.);",
            "#31=IFCCARTESIANPOINT((0.5,0.5,0.));",
            "#32=IFCVERTEXPOINT(#31);",
            "#33=IFCVERTEXLOOP(#32);",
            "#34=IFCFACEBOUND(#33,.T.);",
            "#35=IFCDIRECTION((0.,0.,1.));",
            "#36=IFCAXIS2PLACEMENT3D(#1,#35,#9);",
            "#37=IFCPLANE(#36);",
            "#38=IFCADVANCEDFACE((#30,#34),#37,.T.);",
            "#39=IFCOPENSHELL((#38));",
            "#40=IFCSHELLBASEDSURFACEMODEL((#39));",
        ]);
        let model = model_of(&source);
        let (mesh, diagnostics) = eval_solid_with_diagnostics(&model, 40);
        let mesh = mesh.unwrap();
        assert!(
            (mesh.surface_area() - 1.0).abs() < 1e-9,
            "{}",
            mesh.surface_area()
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::VERTEX_LOOP_IGNORED),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_lone_loop_round_a_torus_has_nothing_to_close_it() {
        // A meridian circle alone: it goes round the tube and no apex or pole ends the band.
        let source = lines(&[
            "#1=IFCCARTESIANPOINT((0.,0.,0.));",
            "#2=IFCDIRECTION((0.,0.,1.));",
            "#3=IFCDIRECTION((1.,0.,0.));",
            "#4=IFCAXIS2PLACEMENT3D(#1,#2,#3);",
            "#5=IFCTOROIDALSURFACE(#4,0.6,0.2);",
            "#6=IFCCARTESIANPOINT((0.8,0.,0.));",
            "#7=IFCVERTEXPOINT(#6);",
            "#8=IFCCARTESIANPOINT((0.6,0.,0.));",
            "#9=IFCDIRECTION((0.,-1.,0.));",
            "#10=IFCAXIS2PLACEMENT3D(#8,#9,#3);",
            "#11=IFCCIRCLE(#10,0.2);",
            "#12=IFCEDGECURVE(#7,#7,#11,.T.);",
            "#13=IFCORIENTEDEDGE(*,*,#12,.T.);",
            "#14=IFCEDGELOOP((#13));",
            "#15=IFCFACEOUTERBOUND(#14,.T.);",
            "#16=IFCADVANCEDFACE((#15),#5,.T.);",
            "#17=IFCOPENSHELL((#16));",
            "#18=IFCSHELLBASEDSURFACEMODEL((#17));",
        ]);
        let model = model_of(&source);
        let (_, diagnostics) = eval_solid_with_diagnostics(&model, 18);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("nothing to close it")),
            "{diagnostics:?}"
        );
    }
}
