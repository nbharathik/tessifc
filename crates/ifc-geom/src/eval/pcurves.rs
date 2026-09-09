// SPDX-License-Identifier: Apache-2.0
//! Surface-parameter curves preserve topology across seams and periodic boundaries.

use crate::registry::SurfaceKind;
use crate::{EvalCtx, GeomError, Settings, Tolerances, Units};
use glam::{DVec2, DVec3};
use tessifc_model::Entity;

pub(crate) struct ParameterEdge {
    pub surface_id: u32,
    pub points: Vec<DVec3>,
    pub uv: Vec<DVec2>,
}

pub(crate) fn edge_run(
    ctx: &EvalCtx<'_>,
    curve: Entity<'_>,
    start: DVec3,
    end: DVec3,
    same_sense: bool,
) -> Result<Option<ParameterEdge>, GeomError> {
    let master = curve
        .attr("MasterRepresentation")
        .as_text()
        .map(|text| String::from_utf8_lossy(text.raw()).into_owned())
        .unwrap_or_else(|| "CURVE3D".into());
    let index = match master.as_str() {
        "CURVE3D" => return Ok(None),
        "PCURVE_S1" => 0,
        "PCURVE_S2" => 1,
        _ => {
            return Err(GeomError::Degenerate(
                "invalid surface curve master representation".into(),
            ));
        }
    };
    let pcurve = curve
        .attr("AssociatedGeometry")
        .as_list()
        .ok_or_else(|| GeomError::missing("AssociatedGeometry"))?
        .nth(index)
        .and_then(|value| value.as_entity())
        .filter(|entity| entity.is_a("IfcPcurve"))
        .ok_or_else(|| GeomError::missing("master pcurve"))?;
    let basis = pcurve
        .attr("BasisSurface")
        .as_entity()
        .ok_or_else(|| GeomError::missing("BasisSurface"))?;
    let surface = ctx.registry().surface(ctx, basis)?;
    let reference = pcurve
        .attr("ReferenceCurve")
        .as_entity()
        .ok_or_else(|| GeomError::missing("ReferenceCurve"))?;
    if !reference.is_a("IfcCurve") {
        return Err(GeomError::missing("ReferenceCurve must be an IfcCurve"));
    }
    // Parameters are dimensionless until interpreted by the basis surface.
    let settings = Settings {
        chord_tolerance_m: 1e-6,
        ..ctx.settings.clone()
    };
    let parameters = EvalCtx::new(
        ctx.model,
        Units::default(),
        Tolerances::from_precision(1e-10),
        &settings,
        ctx.diag,
    )
    .with_registry(ctx.registry());
    let line = ctx.registry().curve(&parameters, reference)?;
    let convert = |point: DVec3| -> Result<DVec2, GeomError> {
        let at = point.truncate();
        Ok(match surface.kind {
            SurfaceKind::BSpline(_) => at,
            SurfaceKind::Plane => at * ctx.units.length_to_m,
            SurfaceKind::Cylinder { .. } => {
                DVec2::new(ctx.units.angle(at.x), ctx.units.length(at.y))
            }
            SurfaceKind::Sphere { .. } | SurfaceKind::Torus { .. } => at * ctx.units.angle_to_rad,
            _ => {
                return Err(GeomError::Unsupported(
                    "pcurve on a swept surface parameterisation".into(),
                ));
            }
        })
    };
    let mut parameters = line
        .points
        .into_iter()
        .map(convert)
        .collect::<Result<Vec<_>, _>>()?;
    if !same_sense {
        parameters.reverse();
    }
    let (mut points, mut uv) =
        project_parameters(&surface, &parameters, ctx.settings.chord_tolerance_m)?;
    let tolerance = ctx.tol.len.max(ctx.settings.chord_tolerance_m * 2.0);
    let matches_ends = |points: &[DVec3]| {
        points
            .first()
            .is_some_and(|point| point.distance(start) <= tolerance)
            && points
                .last()
                .is_some_and(|point| point.distance(end) <= tolerance)
    };
    let length = |points: &[DVec3]| {
        points
            .windows(2)
            .map(|pair| pair[0].distance(pair[1]))
            .sum::<f64>()
    };
    let closed_mismatch = start.distance(end) <= ctx.tol.len
        && curve
            .attr("Curve3D")
            .as_entity()
            .and_then(|item| ctx.registry().curve(ctx, item).ok())
            .is_some_and(|reference| {
                reference.closed && length(&reference.points) > length(&points) * 2.0 + tolerance
            });
    let mut repaired = false;
    if (!matches_ends(&points) || closed_mismatch)
        && ctx.settings.repair_pcurve_domains
        && let SurfaceKind::BSpline(spline) = &surface.kind
    {
        let (low, high) = spline.domain();
        let max = parameters
            .iter()
            .copied()
            .reduce(DVec2::max)
            .unwrap_or(DVec2::ZERO);
        if low.abs().max_element() < 1e-12 && parameters.iter().all(|p| p.min_element() >= 0.0) {
            let factor = |axis: usize| {
                if max[axis] > 0.0 && max[axis] < high[axis] * 0.01 {
                    high[axis] / max[axis]
                } else {
                    1.0
                }
            };
            let scale = DVec2::new(factor(0), factor(1));
            let candidate: Vec<_> = parameters.iter().map(|p| *p * scale).collect();
            if scale.is_finite()
                && scale != DVec2::ONE
                && let Ok((candidate_points, candidate_uv)) =
                    project_parameters(&surface, &candidate, ctx.settings.chord_tolerance_m)
                && matches_ends(&candidate_points)
                && let Some(reference) = curve.attr("Curve3D").as_entity()
            {
                let original_agrees = ctx
                    .registry()
                    .curve(ctx, reference)
                    .is_ok_and(|r| paths_agree(&candidate_points, &r.points, tolerance));
                let recovered_agrees = !original_agrees
                    && ctx.settings.repair_surface_curves
                    && reference.is_a("IfcRationalBSplineCurveWithKnots")
                    && super::curves::reweighted_reference_points(ctx, reference)
                        .is_ok_and(|r| paths_agree(&candidate_points, &r, tolerance));
                if original_agrees || recovered_agrees {
                    repaired = true;
                    points = candidate_points;
                    uv = candidate_uv;
                    let (code, message) = if recovered_agrees {
                        (
                            crate::codes::PCURVE_REFERENCE_RECOVERED,
                            "pcurve domain recovered after reweighting inconsistent reference control points; surface and edge vertices retained",
                        )
                    } else {
                        (
                            crate::codes::PCURVE_DOMAIN_RECOVERED,
                            "pcurve parameter scale recovered from the surface domain and verified against the complete 3D curve",
                        )
                    };
                    ctx.diag.warn(code, pcurve.id(), message);
                }
            }
        }
    }

    if (closed_mismatch && !repaired)
        || points
            .first()
            .is_none_or(|point| point.distance(start) > tolerance)
        || points
            .last()
            .is_none_or(|point| point.distance(end) > tolerance)
    {
        if ctx.settings.repair_surface_curves
            && let Some(reference) = curve.attr("Curve3D").as_entity()
            && let Ok(mut reference) = ctx.registry().curve(ctx, reference)
        {
            if !same_sense {
                reference.points.reverse();
            }
            if matches_ends(&reference.points)
                && let Some(parameters) = verified_reference_parameters(
                    &surface,
                    &reference.points,
                    ctx.settings.chord_tolerance_m,
                )
            {
                ctx.diag.warn(crate::codes::PCURVE_3D_FALLBACK, pcurve.id(),
                    "invalid pcurve replaced by the 3D reference after verifying sampled distances to the basis surface");
                return Ok(Some(ParameterEdge {
                    surface_id: basis.id(),
                    points: reference.points,
                    uv: parameters,
                }));
            }
        }
        return Err(GeomError::Degenerate(format!(
            "pcurve #{} endpoints differ from edge vertices by {} m and {} m (tolerance {} m)",
            pcurve.id(),
            points.first().map_or(f64::INFINITY, |p| p.distance(start)),
            points.last().map_or(f64::INFINITY, |p| p.distance(end)),
            tolerance
        )));
    }
    points[0] = start;
    let last = points.len() - 1;
    points[last] = end;
    Ok(Some(ParameterEdge {
        surface_id: basis.id(),
        points,
        uv,
    }))
}

fn verified_reference_parameters(
    surface: &crate::Surface,
    points: &[DVec3],
    tolerance: f64,
) -> Option<Vec<DVec2>> {
    if points.len() < 2 || points.len() > 4096 {
        return None;
    }
    let mut parameters = Vec::with_capacity(points.len());
    for point in points {
        let uv = surface.invert(*point, tolerance)?;
        if !point.is_finite() || surface.point(uv).distance(*point) > tolerance {
            return None;
        }
        parameters.push(uv);
    }
    for pair in points.windows(2) {
        for t in [0.25, 0.5, 0.75] {
            let point = pair[0].lerp(pair[1], t);
            let uv = surface.invert(point, tolerance)?;
            if surface.point(uv).distance(point) > tolerance {
                return None;
            }
        }
    }
    Some(parameters)
}

fn project_parameters(
    surface: &crate::Surface,
    parameters: &[DVec2],
    tolerance: f64,
) -> Result<(Vec<DVec3>, Vec<DVec2>), GeomError> {
    let first = *parameters
        .first()
        .ok_or_else(|| GeomError::Degenerate("empty pcurve".into()))?;
    let mut uv = vec![first];
    let mut points = vec![surface.point(first)];
    if !first.is_finite() || !points[0].is_finite() {
        return Err(GeomError::Degenerate("non-finite pcurve point".into()));
    }

    for pair in parameters.windows(2) {
        let mut stack = vec![(pair[0], pair[1], 0u32)];
        while let Some((from, to, depth)) = stack.pop() {
            if points.len() + stack.len() >= 65_536 {
                return Err(GeomError::LimitReached("pcurve subdivision points".into()));
            }
            let a = surface.point(from);
            let b = surface.point(to);
            if !a.is_finite() || !b.is_finite() {
                return Err(GeomError::Degenerate("non-finite pcurve point".into()));
            }
            let mut error: f64 = 0.0;
            for fraction in [0.25, 0.5, 0.75] {
                let point = surface.point(from.lerp(to, fraction));
                if !point.is_finite() {
                    return Err(GeomError::Degenerate("non-finite pcurve point".into()));
                }
                error = error.max(point.distance(a.lerp(b, fraction)));
            }
            if error > tolerance {
                if depth >= 16 {
                    return Err(GeomError::LimitReached(
                        "pcurve subdivision depth before tolerance was met".into(),
                    ));
                }
                let middle = (from + to) * 0.5;
                stack.push((middle, to, depth + 1));
                stack.push((from, middle, depth + 1));
            } else {
                uv.push(to);
                points.push(b);
            }
        }
    }
    Ok((points, uv))
}

fn paths_agree(a: &[DVec3], b: &[DVec3], tolerance: f64) -> bool {
    if a.len() < 2 || b.len() < 2 || a.len().saturating_mul(b.len()) > 2_000_000 {
        return false;
    }
    let near = |points: &[DVec3], path: &[DVec3]| {
        points.iter().all(|point| {
            path.windows(2).any(|pair| {
                let delta = pair[1] - pair[0];
                let t = if delta.length_squared() > 0.0 {
                    ((*point - pair[0]).dot(delta) / delta.length_squared()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                point.distance(pair[0].lerp(pair[1], t)) <= tolerance
            })
        })
    };
    near(a, b) && near(b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tests::model_of;

    #[test]
    fn inconsistent_rational_references_require_opt_in_and_surface_agreement() {
        let source = "#1=IFCCARTESIANPOINT((1.,0.,0.));#2=IFCCARTESIANPOINT((1.,0.,1.));\n\
            #3=IFCCARTESIANPOINT((1.,1.,0.));#4=IFCCARTESIANPOINT((1.,1.,1.));\n\
            #5=IFCCARTESIANPOINT((0.,1.,0.));#6=IFCCARTESIANPOINT((0.,1.,1.));\n\
            #7=IFCRATIONALBSPLINESURFACEWITHKNOTS(2,1,((#1,#2),(#3,#4),(#5,#6)),.UNSPECIFIED.,.F.,.F.,.F.,(3,3),(2,2),(0.,1.),(0.,1.),.UNSPECIFIED.,((1.,1.),(0.5,0.5),(1.,1.)));\n\
            #8=IFCCARTESIANPOINT((2.,2.,0.));\n\
            #9=IFCRATIONALBSPLINECURVEWITHKNOTS(2,(#1,#8,#5),.UNSPECIFIED.,.F.,.F.,(3,3),(0.,1.),.UNSPECIFIED.,(1.,0.5,1.));\n\
            #10=IFCCARTESIANPOINT((0.,0.));#11=IFCCARTESIANPOINT((0.0001,0.));\n\
            #12=IFCPOLYLINE((#10,#11));#13=IFCPCURVE(#7,#12);#14=IFCSURFACECURVE(#9,(#13),.PCURVE_S1.);";
        for (data, enabled, accepted) in [
            (source.to_owned(), false, false),
            (source.to_owned(), true, true),
            (source.replace("(2.,2.,0.)", "(9.,9.,0.)"), true, false),
        ] {
            let model = model_of(&data);
            let settings = Settings {
                repair_surface_curves: enabled,
                ..Settings::default()
            };
            let sink = crate::DiagnosticSink::default();
            let ctx = EvalCtx::new(
                &model,
                Units::default(),
                Tolerances::default(),
                &settings,
                &sink,
            );
            let result = edge_run(&ctx, model.entity(14).unwrap(), DVec3::X, DVec3::Y, true);
            assert_eq!(
                result.is_ok(),
                accepted,
                "{result:?}",
                result = result.as_ref().map(|_| ())
            );
            if accepted {
                assert!(
                    sink.take()
                        .iter()
                        .any(|d| d.code == crate::codes::PCURVE_REFERENCE_RECOVERED)
                );
            }
        }
    }

    #[test]
    fn a_collapsed_pcurve_can_use_only_a_verified_reference() {
        let source = "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCAXIS2PLACEMENT3D(#1,$,$);#3=IFCPLANE(#2);\n\
            #4=IFCCARTESIANPOINT((1.,0.,0.));#5=IFCCARTESIANPOINT((0.5,0.,1.));\n\
            #6=IFCCARTESIANPOINT((0.,0.));#7=IFCPOLYLINE((#6,#6));\n\
            #8=IFCPCURVE(#3,#7);#9=IFCPOLYLINE((#1,#4));#10=IFCSURFACECURVE(#9,(#8),.PCURVE_S1.);";
        for (data, enabled, accepted) in [
            (source.to_owned(), false, false),
            (source.to_owned(), true, true),
            (source.replace("((#1,#4))", "((#1,#5,#4))"), true, false),
        ] {
            let model = model_of(&data);
            let settings = Settings {
                repair_surface_curves: enabled,
                ..Settings::default()
            };
            let sink = crate::DiagnosticSink::default();
            let ctx = EvalCtx::new(
                &model,
                Units::default(),
                Tolerances::default(),
                &settings,
                &sink,
            );
            let result = edge_run(&ctx, model.entity(10).unwrap(), DVec3::ZERO, DVec3::X, true);
            assert_eq!(result.is_ok(), accepted);
            if accepted {
                assert!(
                    sink.take()
                        .iter()
                        .any(|d| d.code == crate::codes::PCURVE_3D_FALLBACK)
                );
            }
        }
    }

    #[test]
    fn a_domain_repair_requires_independent_curve_agreement() {
        let source = "#1=IFCCARTESIANPOINT((0.,0.,0.));#2=IFCCARTESIANPOINT((0.,1.,0.));\n\
            #3=IFCCARTESIANPOINT((1.,0.,0.));#4=IFCCARTESIANPOINT((1.,1.,0.));\n\
            #5=IFCBSPLINESURFACEWITHKNOTS(1,1,((#1,#2),(#3,#4)),.PLANE_SURF.,.F.,.F.,.F.,(2,2),(2,2),(0.,1.),(0.,1.),.UNSPECIFIED.);\n\
            #6=IFCCARTESIANPOINT((0.,0.));#7=IFCCARTESIANPOINT((0.0001,0.));#8=IFCPOLYLINE((#6,#7));\n\
            #9=IFCPCURVE(#5,#8);#10=IFCPOLYLINE((#1,#3));#11=IFCSURFACECURVE(#10,(#9),.PCURVE_S1.);";
        for (source, repair, expected) in [
            (source.to_string(), true, true),
            (source.to_string(), false, false),
            (
                source.replace("IFCPOLYLINE((#1,#3))", "IFCPOLYLINE((#1,#4,#3))"),
                true,
                false,
            ),
            (
                source.replace("(#9),.PCURVE_S1.", "(#5,#9),.PCURVE_S1."),
                true,
                false,
            ),
        ] {
            let model = model_of(&source);
            let settings = Settings {
                repair_pcurve_domains: repair,
                ..Settings::default()
            };
            let sink = crate::DiagnosticSink::default();
            let ctx = EvalCtx::new(
                &model,
                Units::default(),
                Tolerances::default(),
                &settings,
                &sink,
            );
            let run = edge_run(&ctx, model.entity(11).unwrap(), DVec3::ZERO, DVec3::X, true);
            assert_eq!(run.is_ok(), expected);
            if expected {
                assert_eq!(run.unwrap().unwrap().uv.last(), Some(&DVec2::X));
                assert!(
                    sink.take()
                        .iter()
                        .any(|d| d.code == crate::codes::PCURVE_DOMAIN_RECOVERED)
                );
            }
        }
    }
}
