// SPDX-License-Identifier: Apache-2.0
//! Parametric surfaces and their inversion: a point at (u, v), and the (u, v)
//! of a point. Advanced faces are trimmed and triangulated in the surface's own
//! parameters and lifted back onto it.

use crate::context::EvalCtx;
use crate::error::GeomError;
use crate::placement::{axis2_placement_3d, cartesian_point, direction};
use crate::registry::{Registry, Surface, SurfaceEvaluator, SurfaceKind};
use glam::{DMat4, DVec2, DVec3};
use tessifc_model::Entity;

/// Most control points a B-spline surface may declare in one direction.
const MAX_CONTROL_POINTS: usize = 4096;
/// Total control points in one surface, before allocating weights or evaluation scratch.
const MAX_CONTROL_NET_POINTS: usize = 65_536;

/// `IfcPlane`, and the bounded surfaces that resolve to a basis surface.
struct Planar;

impl SurfaceEvaluator for Planar {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcPlane"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError> {
        Ok(Surface::new(
            SurfaceKind::Plane,
            axis2_placement_3d(item.attr("Position"), &ctx.units),
        ))
    }
}

/// A surface that carries a trim or a boundary over another surface.
///
/// The trim is the face's own business: an advanced face is bounded by its
/// edges, and a bounded surface used as a face surface only says which
/// surface those edges lie on.
struct Bounded;

impl SurfaceEvaluator for Bounded {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcCurveBoundedPlane",
            "IfcCurveBoundedSurface",
            "IfcRectangularTrimmedSurface",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError> {
        let basis = item
            .attr("BasisSurface")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BasisSurface"))?;
        ctx.registry().surface(ctx, basis)
    }
}

/// `IfcCylindricalSurface`, `IfcSphericalSurface` and `IfcToroidalSurface`.
struct Elementary;

impl SurfaceEvaluator for Elementary {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcCylindricalSurface",
            "IfcSphericalSurface",
            "IfcToroidalSurface",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError> {
        let frame = axis2_placement_3d(item.attr("Position"), &ctx.units);
        let radius = |name: &str| -> Result<f64, GeomError> {
            let value = ctx.units.length(
                item.attr(name)
                    .as_f64()
                    .ok_or_else(|| GeomError::missing(name))?,
            );
            if !value.is_finite() || value <= 0.0 {
                return Err(GeomError::Degenerate(format!("a {name} of {value}")));
            }
            Ok(value)
        };
        let kind = if item.is_a("IfcCylindricalSurface") {
            SurfaceKind::Cylinder {
                radius: radius("Radius")?,
            }
        } else if item.is_a("IfcSphericalSurface") {
            SurfaceKind::Sphere {
                radius: radius("Radius")?,
            }
        } else {
            SurfaceKind::Torus {
                major: radius("MajorRadius")?,
                minor: radius("MinorRadius")?,
            }
        };
        Ok(Surface::new(kind, frame))
    }
}

/// `IfcSurfaceOfLinearExtrusion` and `IfcSurfaceOfRevolution` as surfaces.
///
/// The same two classes also have a solid-table entry that returns an uncapped
/// ribbon for a top-level item. This is the face-surface reading of them.
struct Swept;

impl SurfaceEvaluator for Swept {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSurfaceOfLinearExtrusion", "IfcSurfaceOfRevolution"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError> {
        let frame = axis2_placement_3d(item.attr("Position"), &ctx.units);
        let swept = item
            .attr("SweptCurve")
            .as_entity()
            .ok_or_else(|| GeomError::missing("SweptCurve"))?;
        let (points, open) = swept_curve_points(ctx, swept)?;
        if points.len() < 2 {
            return Err(GeomError::Degenerate("a swept curve of one point".into()));
        }

        if item.is_a("IfcSurfaceOfLinearExtrusion") {
            let depth = ctx.units.length(
                item.attr("Depth")
                    .as_f64()
                    .ok_or_else(|| GeomError::missing("Depth"))?,
            );
            let along = item
                .attr("ExtrudedDirection")
                .as_entity()
                .and_then(direction)
                .unwrap_or(DVec3::Z);
            if !depth.is_finite() || depth.abs() <= ctx.tol.len || along.z.abs() <= 1e-12 {
                return Err(GeomError::Degenerate(
                    "a surface extruded through nothing, or along its own profile plane".into(),
                ));
            }
            return Ok(Surface::new(
                SurfaceKind::Extrusion {
                    profile: points.iter().map(|point| point.truncate()).collect(),
                    closed: !open,
                    along,
                    depth,
                },
                frame,
            ));
        }

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
        // The profile as radius from the axis and height along it; one that does not
        // lie in a single half-plane is not a surface of revolution and is refused.
        let flat = &points;
        let mut reference = DVec3::ZERO;
        for point in flat {
            let local = *point - origin;
            let radial = local - axis * local.dot(axis);
            if radial.length() > reference.length() {
                reference = radial;
            }
        }
        if reference.length() <= ctx.tol.len {
            return Err(GeomError::Degenerate(
                "a revolution profile that lies on its own axis".into(),
            ));
        }
        let reference = reference.normalize();
        let side = axis.cross(reference);
        let mut section = Vec::with_capacity(flat.len());
        for point in flat {
            let local = *point - origin;
            let height = local.dot(axis);
            let radial = local - axis * height;
            if radial.dot(side).abs() > ctx.tol.len.max(radial.length() * 1e-6) {
                return Err(GeomError::Unsupported(
                    "a revolution profile that is not in one half-plane".into(),
                ));
            }
            section.push(DVec2::new(radial.dot(reference), height));
        }
        Ok(Surface::new(
            SurfaceKind::Revolution {
                section,
                closed: !open,
                origin,
                axis,
                reference,
            },
            frame,
        ))
    }
}

/// `IfcBSplineSurfaceWithKnots` and its rational subtype.
struct BSpline;

impl SurfaceEvaluator for BSpline {
    fn classes(&self) -> &'static [&'static str] {
        &[
            "IfcBSplineSurfaceWithKnots",
            "IfcRationalBSplineSurfaceWithKnots",
        ]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Surface, GeomError> {
        let u_degree = degree(item, "UDegree")?;
        let v_degree = degree(item, "VDegree")?;
        let rows = item
            .attr("ControlPointsList")
            .as_list()
            .ok_or_else(|| GeomError::missing("ControlPointsList"))?;
        let mut net: Vec<Vec<DVec3>> = Vec::new();
        let mut total_points = 0usize;
        for row in rows {
            if net.len() >= MAX_CONTROL_POINTS {
                return Err(GeomError::LimitReached("surface control rows".into()));
            }
            let row = row
                .as_list()
                .ok_or_else(|| GeomError::missing("a control point row"))?;
            let mut points = Vec::new();
            for value in row {
                if points.len() >= MAX_CONTROL_POINTS || total_points >= MAX_CONTROL_NET_POINTS {
                    return Err(GeomError::LimitReached("surface control points".into()));
                }
                total_points += 1;
                let point = value
                    .as_entity()
                    .and_then(|entity| cartesian_point(entity, &ctx.units))
                    .filter(|point| point.is_finite())
                    .ok_or_else(|| GeomError::missing("a control point"))?;
                points.push(point);
            }
            if points.is_empty() || points.len() > MAX_CONTROL_POINTS {
                return Err(GeomError::Degenerate(format!(
                    "a control net row of {} points",
                    points.len()
                )));
            }
            net.push(points);
        }
        if net.is_empty() || net.len() > MAX_CONTROL_POINTS {
            return Err(GeomError::Degenerate(format!(
                "a control net of {} rows",
                net.len()
            )));
        }
        let width = net[0].len();
        if net.iter().any(|row| row.len() != width) {
            return Err(GeomError::Degenerate("a ragged control net".into()));
        }
        if net.len() <= u_degree || width <= v_degree {
            return Err(GeomError::Degenerate(
                "a control net smaller than its own degree".into(),
            ));
        }

        // Rational weights must describe the entire net; omitted entries change the surface.
        let mut weights = Vec::with_capacity(net.len());
        if item.is_a("IfcRationalBSplineSurfaceWithKnots") {
            let rows = item
                .attr("WeightsData")
                .as_list()
                .ok_or_else(|| GeomError::missing("WeightsData"))?;
            for row in rows {
                if weights.len() >= net.len() {
                    return Err(GeomError::Degenerate(
                        "more weight rows than control rows".into(),
                    ));
                }
                let values = row
                    .as_list()
                    .ok_or_else(|| GeomError::missing("weight row"))?;
                let mut row_weights = Vec::with_capacity(width);
                for value in values {
                    if row_weights.len() >= width {
                        return Err(GeomError::Degenerate(
                            "more weights than control points".into(),
                        ));
                    }
                    let weight = value
                        .as_f64()
                        .filter(|w| w.is_finite() && *w > 0.0)
                        .ok_or_else(|| {
                            GeomError::Degenerate("a non-positive or non-finite weight".into())
                        })?;
                    row_weights.push(weight);
                }
                if row_weights.len() != width {
                    return Err(GeomError::Degenerate(
                        "fewer weights than control points".into(),
                    ));
                }
                weights.push(row_weights);
            }
            if weights.len() != net.len() {
                return Err(GeomError::Degenerate(
                    "fewer weight rows than control rows".into(),
                ));
            }
        } else {
            weights = vec![vec![1.0; width]; net.len()];
        }

        let scale = weights.iter().flatten().copied().fold(0.0_f64, f64::max);
        for weight in weights.iter_mut().flatten() {
            *weight /= scale;
            if !weight.is_finite() || *weight <= 0.0 {
                return Err(GeomError::Degenerate(
                    "surface weight range exceeds numeric precision".into(),
                ));
            }
        }

        let u_knots = knot_vector(item, "UMultiplicities", "UKnots", net.len(), u_degree)?;
        let v_knots = knot_vector(item, "VMultiplicities", "VKnots", width, v_degree)?;
        Ok(Surface::new(
            SurfaceKind::BSpline(Box::new(crate::registry::BSplineSurface {
                u_degree,
                v_degree,
                net,
                weights,
                u_knots,
                v_knots,
            })),
            DMat4::IDENTITY,
        ))
    }
}

/// The curve a swept surface sweeps, in three dimensions.
///
/// `IfcArbitraryOpenProfileDef` wraps a curve that may itself be 3D, and for a
/// surface of revolution it usually is: the profile lies in a plane containing
/// the axis, which is not the profile evaluator's z = 0 plane. Reading it as a
/// 2D profile would flatten the surface into an annulus.
fn swept_curve_points(
    ctx: &EvalCtx<'_>,
    swept: Entity<'_>,
) -> Result<(Vec<DVec3>, bool), GeomError> {
    for (attribute, open) in [("Curve", true), ("OuterCurve", false)] {
        let Some(curve) = swept.attr(attribute).as_entity() else {
            continue;
        };
        let polyline = ctx.registry().curve(ctx, curve)?;
        if polyline.points.len() >= 2 {
            return Ok((polyline.points, open && !polyline.closed));
        }
    }
    // Anything else is a parametric profile, which is genuinely two-dimensional.
    let profile = ctx.registry().profile(ctx, swept)?;
    Ok((
        profile
            .outer
            .iter()
            .map(|point| DVec3::new(point.x, point.y, 0.0))
            .collect(),
        profile.open,
    ))
}

fn degree(item: Entity<'_>, name: &str) -> Result<usize, GeomError> {
    let value = item
        .attr(name)
        .as_i64()
        .ok_or_else(|| GeomError::missing(name))?;
    if !(1..=16).contains(&value) {
        return Err(GeomError::Degenerate(format!("a {name} of {value}")));
    }
    Ok(value as usize)
}

/// Expand multiplicities into a full knot vector, checked against the net.
fn knot_vector(
    item: Entity<'_>,
    multiplicities: &str,
    knots: &str,
    control_points: usize,
    degree: usize,
) -> Result<Vec<f64>, GeomError> {
    let counts: Vec<i64> = item
        .attr(multiplicities)
        .as_list()
        .ok_or_else(|| GeomError::missing(multiplicities))?
        .take(control_points + degree + 2)
        .map(|value| {
            value
                .as_i64()
                .ok_or_else(|| GeomError::missing(multiplicities))
        })
        .collect::<Result<_, _>>()?;
    let values: Vec<f64> = item
        .attr(knots)
        .as_list()
        .ok_or_else(|| GeomError::missing(knots))?
        .take(control_points + degree + 2)
        .map(|value| value.as_f64().ok_or_else(|| GeomError::missing(knots)))
        .collect::<Result<_, _>>()?;
    if counts.len() != values.len() || counts.is_empty() {
        return Err(GeomError::Degenerate(format!(
            "{} knots against {} multiplicities",
            values.len(),
            counts.len()
        )));
    }
    if values.windows(2).any(|pair| pair[1] <= pair[0]) {
        return Err(GeomError::Degenerate("distinct knots must increase".into()));
    }
    let wanted = control_points + degree + 1;
    let mut out = Vec::with_capacity(wanted);
    for (count, value) in counts.iter().zip(&values) {
        if *count < 1 || *count as u64 > (wanted - out.len()) as u64 {
            return Err(GeomError::Degenerate(
                "a knot multiplicity that does not fit".into(),
            ));
        }
        if !value.is_finite() {
            return Err(GeomError::Degenerate("a knot that is not a number".into()));
        }
        for _ in 0..*count {
            out.push(*value);
        }
    }
    if out.len() != wanted {
        return Err(GeomError::Degenerate(format!(
            "{} knots where {wanted} were needed",
            out.len()
        )));
    }
    if out.windows(2).any(|pair| pair[1] < pair[0]) {
        return Err(GeomError::Degenerate(
            "a knot vector that goes backwards".into(),
        ));
    }
    if out[degree] >= out[control_points] {
        return Err(GeomError::Degenerate(
            "empty surface parameter domain".into(),
        ));
    }
    Ok(out)
}

/// Register every surface evaluator.
pub fn register(registry: &mut Registry) {
    registry.register_surface(Box::new(Planar));
    registry.register_surface(Box::new(Bounded));
    registry.register_surface(Box::new(Elementary));
    registry.register_surface(Box::new(Swept));
    registry.register_surface(Box::new(BSpline));
}

#[cfg(test)]
mod tests {
    use super::super::tests::{eval_surface, model_of};
    use crate::registry::SurfaceKind;
    use glam::{DVec2, DVec3};

    #[test]
    fn rational_surface_weights_match_the_entire_control_net() {
        let fixture = |weights: &str| {
            model_of(&format!(
                "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCCARTESIANPOINT((0.,1.,0.));\n\
             #3=IFCCARTESIANPOINT((1.,0.,0.));#4=IFCCARTESIANPOINT((1.,1.,1.));\n\
             #5=IFCRATIONALBSPLINESURFACEWITHKNOTS(1,1,((#1,#2),(#3,#4)),.UNSPECIFIED.,.F.,.F.,.F.,(2,2),(2,2),(0.,1.),(0.,1.),.UNSPECIFIED.,{weights});"
            ))
        };
        let model = fixture("((1.,1.),(1.,1.))");
        let surface = eval_surface(&model, 5).unwrap();
        assert!((surface.point(DVec2::splat(0.5)) - DVec3::new(0.5, 0.5, 0.25)).length() < 1e-12);
        for weights in [
            "$",
            "()",
            "((1.,1.))",
            "((1.),(1.,1.))",
            "((1.,1.),(1.))",
            "((1.,1.,1.),(1.,1.))",
            "((1.,1.),(1.,0.))",
        ] {
            assert!(
                eval_surface(&fixture(weights), 5).is_err(),
                "accepted {weights}"
            );
        }
    }

    #[test]
    fn tiny_surface_knot_domains_preserve_the_patch() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCCARTESIANPOINT((0.,1.,0.));\n\
             #3=IFCCARTESIANPOINT((1.,0.,0.));#4=IFCCARTESIANPOINT((1.,1.,0.));\n\
             #5=IFCBSPLINESURFACEWITHKNOTS(1,1,((#1,#2),(#3,#4)),.PLANE_SURF.,.F.,.F.,.F.,(2,2),(2,2),(0.,1.E-20),(0.,1.E-20),.UNSPECIFIED.);",
        );
        let surface = eval_surface(&model, 5).unwrap();
        let at = surface.point(DVec2::new(2.5e-21, 7.5e-21));
        assert!(at.distance(DVec3::new(0.25, 0.75, 0.0)) < 1e-12, "{at:?}");
    }

    #[test]
    fn a_control_net_has_a_total_budget() {
        let row = format!("({})", vec!["#1"; 257].join(","));
        let net = vec![row; 257].join(",");
        let model = model_of(&format!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));#5=IFCBSPLINESURFACEWITHKNOTS(1,1,({net}),.UNSPECIFIED.,.F.,.F.,.F.,(257,2),(257,2),(0.,1.),(0.,1.),.UNSPECIFIED.);"
        ));
        assert!(matches!(
            eval_surface(&model, 5),
            Err(crate::GeomError::LimitReached(_))
        ));
    }

    const CYLINDER: &str = "#10=IFCCARTESIANPOINT((0.,0.,0.));\n\
         #11=IFCDIRECTION((0.,0.,1.));\n\
         #12=IFCDIRECTION((1.,0.,0.));\n\
         #13=IFCAXIS2PLACEMENT3D(#10,#11,#12);\n\
         #14=IFCCYLINDRICALSURFACE(#13,0.5);";

    #[test]
    fn a_cylinder_puts_a_point_back_where_it_came_from() {
        let model = model_of(CYLINDER);
        let surface = eval_surface(&model, 14).unwrap();
        assert!(matches!(surface.kind, SurfaceKind::Cylinder { .. }));
        for (u, v) in [(0.0, 0.0), (1.2, 0.7), (std::f64::consts::PI, -2.0)] {
            let point = surface.point(DVec2::new(u, v));
            assert!(
                (point.truncate().length() - 0.5).abs() < 1e-12,
                "{point:?} is not on the cylinder"
            );
            let back = surface.invert(point, 1e-9).expect("inverted");
            assert!(
                (surface.point(back) - point).length() < 1e-9,
                "{back:?} does not come back to {point:?}"
            );
        }
    }

    #[test]
    fn a_sphere_and_a_torus_invert_too() {
        let sphere = model_of(
            "#10=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #13=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
             #14=IFCSPHERICALSURFACE(#13,0.5);",
        );
        let surface = eval_surface(&sphere, 14).unwrap();
        for (u, v) in [(0.0, 0.0), (2.0, 0.6), (-1.0, -0.9)] {
            let point = surface.point(DVec2::new(u, v));
            assert!((point.length() - 0.5).abs() < 1e-12, "{point:?}");
            let back = surface.invert(point, 1e-9).expect("inverted");
            assert!((surface.point(back) - point).length() < 1e-9);
        }
        let torus = model_of(
            "#10=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #13=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
             #14=IFCTOROIDALSURFACE(#13,0.6,0.2);",
        );
        let surface = eval_surface(&torus, 14).unwrap();
        for (u, v) in [(0.0, 0.0), (1.0, 2.0), (-2.5, 0.4)] {
            let point = surface.point(DVec2::new(u, v));
            // Distance from the tube centre circle is the minor radius.
            let axial = DVec3::new(point.x, point.y, 0.0);
            let centre = axial.normalize_or(DVec3::X) * 0.6;
            assert!((point - centre).length() - 0.2 < 1e-12, "{point:?}");
            let back = surface.invert(point, 1e-9).expect("inverted");
            assert!((surface.point(back) - point).length() < 1e-9);
        }
    }

    #[test]
    fn a_plane_is_its_own_parameter_space() {
        let model = model_of(
            "#10=IFCCARTESIANPOINT((1.,2.,3.));\n\
             #13=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
             #14=IFCPLANE(#13);",
        );
        let surface = eval_surface(&model, 14).unwrap();
        let point = surface.point(DVec2::new(0.5, -0.25));
        assert!(
            (point - DVec3::new(1.5, 1.75, 3.0)).length() < 1e-12,
            "{point:?}"
        );
        let back = surface.invert(point, 1e-9).unwrap();
        assert!((back - DVec2::new(0.5, -0.25)).length() < 1e-12, "{back:?}");
    }

    #[test]
    fn a_surface_that_is_not_one_is_refused_not_guessed() {
        let model = model_of(
            "#10=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #13=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
             #14=IFCCYLINDRICALSURFACE(#13,0.);",
        );
        assert!(eval_surface(&model, 14).is_err(), "a radius of zero");
    }
}
