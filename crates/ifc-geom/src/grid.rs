// SPDX-License-Identifier: Apache-2.0
//! `IfcGridPlacement`: a product placed at the crossing of two grid axes.

use crate::context::EvalCtx;
use crate::error::GeomError;
use glam::{DMat4, DVec2, DVec3};
use tessifc_model::{Entity, Model};

/// The placement's matrix in its grid's coordinates, and the placement it is relative to.
///
/// The parent is the grid's own placement unless the file names one.
pub fn grid_placement(
    ctx: &EvalCtx<'_>,
    model: &Model,
    placement: Entity<'_>,
) -> Result<(DMat4, Option<u32>), GeomError> {
    let location = placement
        .attr("PlacementLocation")
        .as_entity()
        .ok_or_else(|| GeomError::missing("PlacementLocation"))?;
    let crossing = virtual_intersection(ctx, location)?;

    // The x axis: an explicit direction, a line to a second crossing, or the
    // first axis's own tangent.
    let x_axis = match placement.attr("PlacementRefDirection").as_entity() {
        Some(reference) if reference.is_a("IfcDirection") => crate::placement::direction(reference)
            .map(|direction| DVec2::new(direction.x, direction.y))
            .unwrap_or(crossing.tangent),
        Some(reference) if reference.is_a("IfcVirtualGridIntersection") => {
            let other = virtual_intersection(ctx, reference)?;
            other.point - crossing.point
        }
        _ => crossing.tangent,
    };
    let x_axis = x_axis.normalize_or(crossing.tangent);
    let x = DVec3::new(x_axis.x, x_axis.y, 0.0);
    let y = DVec3::Z.cross(x);
    let origin = DVec3::new(crossing.point.x, crossing.point.y, crossing.elevation);
    let local = DMat4::from_cols(
        x.extend(0.0),
        y.extend(0.0),
        DVec3::Z.extend(0.0),
        origin.extend(1.0),
    );

    let parent = match placement.attr("PlacementRelTo").as_entity() {
        Some(parent) => Some(parent.id()),
        None => grid_of(model, crossing.first_axis)
            .and_then(|grid| grid.attr("ObjectPlacement").as_entity())
            .map(|entity| entity.id()),
    };
    Ok((local, parent))
}

/// Where two offset axis curves cross, in grid coordinates.
struct Crossing {
    point: DVec2,
    /// Unit tangent of the first axis's offset curve at the crossing.
    tangent: DVec2,
    /// The optional third offset, along the grid's z axis.
    elevation: f64,
    first_axis: u32,
}

fn virtual_intersection(
    ctx: &EvalCtx<'_>,
    intersection: Entity<'_>,
) -> Result<Crossing, GeomError> {
    let mut axes = intersection
        .attr("IntersectingAxes")
        .as_list()
        .ok_or_else(|| GeomError::missing("IntersectingAxes"))?
        .filter_map(|value| value.as_entity());
    let first = axes
        .next()
        .ok_or_else(|| GeomError::missing("IntersectingAxes[1]"))?;
    let second = axes
        .next()
        .ok_or_else(|| GeomError::missing("IntersectingAxes[2]"))?;
    let offsets: Vec<f64> = intersection
        .attr("OffsetDistances")
        .as_list()
        .map(|list| list.floats().map(|value| ctx.units.length(value)).collect())
        .unwrap_or_default();
    let a = axis_polyline(ctx, first, offsets.first().copied().unwrap_or(0.0))?;
    let b = axis_polyline(ctx, second, offsets.get(1).copied().unwrap_or(0.0))?;
    let (point, tangent) = intersect_polylines(&a, &b, ctx.tol.len)
        .ok_or_else(|| GeomError::Degenerate("grid axes that do not cross".into()))?;
    Ok(Crossing {
        point,
        tangent,
        elevation: offsets.get(2).copied().unwrap_or(0.0),
        first_axis: first.id(),
    })
}

/// An axis curve as a 2D polyline, in the axis's sense, offset to its left.
fn axis_polyline(
    ctx: &EvalCtx<'_>,
    axis: Entity<'_>,
    offset: f64,
) -> Result<Vec<DVec2>, GeomError> {
    let curve = axis
        .attr("AxisCurve")
        .as_entity()
        .ok_or_else(|| GeomError::missing("AxisCurve"))?;
    let polyline = ctx.registry().curve(ctx, curve)?;
    let mut points: Vec<DVec2> = polyline
        .points
        .iter()
        .map(|point| point.truncate())
        .collect();
    points.dedup_by(|a, b| (*a - *b).length() <= ctx.tol.len);
    if points.len() < 2 {
        return Err(GeomError::Degenerate(
            "a grid axis with fewer than two points".into(),
        ));
    }
    if axis.attr("SameSense").as_bool() == Some(false) {
        points.reverse();
    }
    if offset.abs() > ctx.tol.len {
        // Positive offsets lie to the left of the axis, anticlockwise from its tangent.
        let lifted: Vec<DVec3> = points.iter().map(|p| p.extend(0.0)).collect();
        points = crate::eval::curves::offset_polyline(&lifted, false, offset, |tangent| {
            DVec3::new(-tangent.y, tangent.x, 0.0)
        })
        .iter()
        .map(|p| p.truncate())
        .collect();
    }
    Ok(points)
}

/// The first crossing of two polylines, with the first one's tangent there.
///
/// Axes that stop short of each other are extended along their end segments.
fn intersect_polylines(a: &[DVec2], b: &[DVec2], tolerance: f64) -> Option<(DVec2, DVec2)> {
    let mut best: Option<(f64, DVec2, DVec2)> = None;
    for (index_a, pair_a) in a.windows(2).enumerate() {
        for (index_b, pair_b) in b.windows(2).enumerate() {
            let Some((s, t, point)) =
                segment_parameters(pair_a[0], pair_a[1], pair_b[0], pair_b[1])
            else {
                continue;
            };
            let extend_a = index_a == 0 || index_a + 2 == a.len();
            let extend_b = index_b == 0 || index_b + 2 == b.len();
            let overshoot = |value: f64, extend: bool| {
                if extend {
                    0.0
                } else {
                    (-value).max(value - 1.0).max(0.0)
                }
            };
            let miss = overshoot(s, extend_a).max(overshoot(t, extend_b));
            let inside = (-1e-9..=1.0 + 1e-9).contains(&s) && (-1e-9..=1.0 + 1e-9).contains(&t);
            let score = if inside { 0.0 } else { miss + 1.0 };
            if miss > tolerance && !extend_a && !extend_b {
                continue;
            }
            let tangent = (pair_a[1] - pair_a[0]).normalize_or_zero();
            if best.as_ref().is_none_or(|(known, _, _)| score < *known) {
                best = Some((score, point, tangent));
            }
        }
    }
    best.map(|(_, point, tangent)| (point, tangent))
}

/// Line parameters of the crossing of two segments, or `None` when parallel.
fn segment_parameters(a0: DVec2, a1: DVec2, b0: DVec2, b1: DVec2) -> Option<(f64, f64, DVec2)> {
    let da = a1 - a0;
    let db = b1 - b0;
    let denominator = da.perp_dot(db);
    if denominator.abs() < 1e-12 {
        return None;
    }
    let delta = b0 - a0;
    let s = delta.perp_dot(db) / denominator;
    let t = delta.perp_dot(da) / denominator;
    Some((s, t, a0 + da * s))
}

/// The grid an axis belongs to.
fn grid_of<'a>(model: &'a Model, axis: u32) -> Option<Entity<'a>> {
    model.entities_of_type("IfcGrid").find(|grid| {
        ["UAxes", "VAxes", "WAxes"].iter().any(|name| {
            grid.attr(name).as_list().is_some_and(|mut list| {
                list.any(|value| value.as_entity().is_some_and(|e| e.id() == axis))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::context::{DiagnosticSink, EvalCtx, Settings, Tolerances};
    use crate::units::Units;
    use glam::DVec3;

    fn grid_model(intersection: &str) -> tessifc_model::Model {
        crate::eval::tests::model_of(&format!(
            concat!(
                "#1=IFCCARTESIANPOINT((1.5,-5.));\n",
                "#2=IFCCARTESIANPOINT((1.5,5.));\n",
                "#3=IFCPOLYLINE((#1,#2));\n",
                "#4=IFCGRIDAXIS('A',#3,.T.);\n",
                "#5=IFCCARTESIANPOINT((-5.,2.));\n",
                "#6=IFCCARTESIANPOINT((5.,2.));\n",
                "#7=IFCPOLYLINE((#5,#6));\n",
                "#8=IFCGRIDAXIS('1',#7,.T.);\n",
                "#9=IFCCARTESIANPOINT((10.,0.,0.));\n",
                "#10=IFCAXIS2PLACEMENT3D(#9,$,$);\n",
                "#11=IFCLOCALPLACEMENT($,#10);\n",
                "#12=IFCGRID('g',$,'Grid',$,$,#11,$,(#4),(#8),$,$);\n",
                "{}\n",
                "#14=IFCGRIDPLACEMENT(#13,$);\n",
                "#15=IFCCOLUMN('c',$,'Column',$,$,#14,$,$,$);\n",
            ),
            intersection
        ))
    }

    fn column_world(model: &tessifc_model::Model) -> glam::DMat4 {
        let units = Units::from_model(model);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(model, units, Tolerances::default(), &settings, &sink);
        let column = model.entity(15).unwrap();
        let world = crate::product::product_transform(&ctx, model, column);
        assert!(sink.take().is_empty(), "no diagnostic expected");
        world
    }

    #[test]
    fn a_column_lands_where_its_axes_cross_inside_the_grid_placement() {
        let model = grid_model("#13=IFCVIRTUALGRIDINTERSECTION((#4,#8),(0.,0.));");
        let world = column_world(&model);
        let origin = world.transform_point3(DVec3::ZERO);
        // The grid itself sits 10 m along x.
        assert!(
            (origin - DVec3::new(11.5, 2.0, 0.0)).length() < 1e-9,
            "{origin}"
        );
        // No reference direction: x follows axis A, which runs along +y.
        let x = world.transform_vector3(DVec3::X);
        assert!((x - DVec3::Y).length() < 1e-9, "{x}");
    }

    #[test]
    fn offsets_move_the_crossing_to_the_left_of_each_axis() {
        let model = grid_model("#13=IFCVIRTUALGRIDINTERSECTION((#4,#8),(0.5,-0.25));");
        let origin = column_world(&model).transform_point3(DVec3::ZERO);
        // Left of +y is -x; left of +x is +y.
        assert!(
            (origin - DVec3::new(11.0, 1.75, 0.0)).length() < 1e-9,
            "{origin}"
        );
    }

    #[test]
    fn a_third_offset_lifts_the_placement() {
        let model = grid_model("#13=IFCVIRTUALGRIDINTERSECTION((#4,#8),(0.,0.,3.));");
        let origin = column_world(&model).transform_point3(DVec3::ZERO);
        assert!(
            (origin - DVec3::new(11.5, 2.0, 3.0)).length() < 1e-9,
            "{origin}"
        );
    }
}
