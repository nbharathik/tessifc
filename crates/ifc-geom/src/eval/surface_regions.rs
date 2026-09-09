// SPDX-License-Identifier: Apache-2.0
//! Bounded surface regions with a coordinate frame chosen from their boundary.

use crate::{EvalCtx, GeomError, Surface, SurfaceKind, codes};
use glam::DVec3;
use tessifc_mesh::Mesh64;

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
