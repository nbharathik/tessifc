// SPDX-License-Identifier: Apache-2.0
//! Solids given by numbers rather than by an outline.
//!
//! CSG primitives plus the two axis-aligned boxes; none needs a profile or a boolean.

use crate::context::EvalCtx;
use crate::error::{GeomError, codes};
use crate::placement::{axis2_placement_3d, cartesian_point};
use crate::registry::{Registry, SolidEvaluator};
use glam::DVec3;
use tessifc_mesh::Mesh64;
use tessifc_model::Entity;

/// Read a positive length in metres.
fn length_of(ctx: &EvalCtx<'_>, item: Entity<'_>, name: &'static str) -> Result<f64, GeomError> {
    let value = item
        .attr(name)
        .as_f64()
        .ok_or_else(|| GeomError::missing(name))?;
    let metres = ctx.units.length(value);
    if !metres.is_finite() || metres <= ctx.tol.len {
        return Err(GeomError::Degenerate(format!(
            "{name} is {metres}, which encloses nothing"
        )));
    }
    Ok(metres)
}

/// An axis-aligned box from `low` to `high`, wound outwards.
pub(crate) fn box_mesh(low: DVec3, high: DVec3) -> Mesh64 {
    let mut mesh = Mesh64::with_capacity(8, 36);
    for index in 0..8u32 {
        mesh.push_vertex(DVec3::new(
            if index & 1 == 0 { low.x } else { high.x },
            if index & 2 == 0 { low.y } else { high.y },
            if index & 4 == 0 { low.z } else { high.z },
        ));
    }
    // Each face counter-clockwise seen from outside.
    for face in [
        [0u32, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ] {
        mesh.push_triangle(face[0], face[1], face[2]);
        mesh.push_triangle(face[0], face[2], face[3]);
    }
    mesh.closed = Some(true);
    mesh
}

/// `IfcBlock`: a box with one corner at its own origin.
pub struct Block;

impl SolidEvaluator for Block {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcBlock"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let x = length_of(ctx, item, "XLength")?;
        let y = length_of(ctx, item, "YLength")?;
        let z = length_of(ctx, item, "ZLength")?;
        let mut mesh = box_mesh(DVec3::ZERO, DVec3::new(x, y, z));
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// `IfcRectangularPyramid`: a rectangular base at z = 0 with the apex above its centre.
pub struct RectangularPyramid;

impl SolidEvaluator for RectangularPyramid {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcRectangularPyramid"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let x = length_of(ctx, item, "XLength")?;
        let y = length_of(ctx, item, "YLength")?;
        let height = length_of(ctx, item, "Height")?;

        let mut mesh = Mesh64::with_capacity(5, 18);
        mesh.push_vertex(DVec3::new(0.0, 0.0, 0.0));
        mesh.push_vertex(DVec3::new(x, 0.0, 0.0));
        mesh.push_vertex(DVec3::new(x, y, 0.0));
        mesh.push_vertex(DVec3::new(0.0, y, 0.0));
        mesh.push_vertex(DVec3::new(x * 0.5, y * 0.5, height));
        // Base looking down, then one triangle per base edge up to the apex.
        mesh.push_triangle(0, 2, 1);
        mesh.push_triangle(0, 3, 2);
        for edge in 0..4u32 {
            mesh.push_triangle(edge, (edge + 1) % 4, 4);
        }
        mesh.closed = Some(true);
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// `IfcRightCircularCylinder`: base at z = 0, top at `Height`.
pub struct RightCircularCylinder;

impl SolidEvaluator for RightCircularCylinder {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcRightCircularCylinder"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let radius = length_of(ctx, item, "Radius")?;
        let height = length_of(ctx, item, "Height")?;
        let segments = ctx.segments_for_radius(radius).max(3);
        let mesh = revolved_shell(
            &[(0.0, 0.0), (radius, 0.0), (radius, height), (0.0, height)],
            segments,
            ctx,
        )?;
        let mut mesh = mesh;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// `IfcRightCircularCone`: base at z = 0 with the apex at `Height`.
pub struct RightCircularCone;

impl SolidEvaluator for RightCircularCone {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcRightCircularCone"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let radius = length_of(ctx, item, "BottomRadius")?;
        let height = length_of(ctx, item, "Height")?;
        let segments = ctx.segments_for_radius(radius).max(3);
        let mut mesh = revolved_shell(&[(0.0, 0.0), (radius, 0.0), (0.0, height)], segments, ctx)?;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// `IfcSphere`, centred on its own origin.
pub struct Sphere;

impl SolidEvaluator for Sphere {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcSphere"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let radius = length_of(ctx, item, "Radius")?;
        let segments = ctx.segments_for_radius(radius).max(4);
        // Half a circle in the rz plane; the poles sit on the axis so they close.
        let rings = (segments / 2).max(2);
        let mut section = Vec::with_capacity(rings as usize + 1);
        for index in 0..=rings {
            let angle =
                -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * index as f64 / rings as f64;
            section.push((radius * angle.cos(), radius * angle.sin()));
        }
        let mut mesh = revolved_shell(&section, segments, ctx)?;
        mesh.transform(&axis2_placement_3d(item.attr("Position"), &ctx.units));
        Ok(mesh)
    }
}

/// Spin a section given as `(radius, z)` pairs a full turn about the z axis.
///
/// A point on the axis becomes one vertex, which keeps apexes and poles closed.
pub(crate) fn revolved_shell(
    section: &[(f64, f64)],
    segments: u32,
    ctx: &EvalCtx<'_>,
) -> Result<Mesh64, GeomError> {
    if section.len() < 2 || segments < 3 {
        return Err(GeomError::Degenerate(
            "a revolution needs at least two section points".into(),
        ));
    }
    let steps = segments as usize;
    let mut mesh = Mesh64::with_capacity(section.len() * steps, section.len() * steps * 6);

    // One vertex per section point per step, except on the axis.
    let mut rows: Vec<Vec<u32>> = Vec::with_capacity(section.len());
    for &(radius, z) in section {
        if radius.abs() <= ctx.tol.len {
            let index = mesh.push_vertex(DVec3::new(0.0, 0.0, z));
            rows.push(vec![index; steps]);
            continue;
        }
        let mut row = Vec::with_capacity(steps);
        for step in 0..steps {
            let angle = std::f64::consts::TAU * step as f64 / steps as f64;
            row.push(mesh.push_vertex(DVec3::new(radius * angle.cos(), radius * angle.sin(), z)));
        }
        rows.push(row);
    }

    for band in 0..rows.len() - 1 {
        let (lower, upper) = (&rows[band], &rows[band + 1]);
        for step in 0..steps {
            let next = (step + 1) % steps;
            let (a, b, c, d) = (lower[step], lower[next], upper[next], upper[step]);
            // A degenerate quad at the axis collapses to one triangle.
            if a != b {
                mesh.push_triangle(a, b, c);
            }
            if c != d {
                mesh.push_triangle(a, c, d);
            }
        }
    }

    mesh.remove_degenerate_triangles(ctx.tol.area);
    mesh.closed = Some(mesh.is_edge_manifold());
    Ok(mesh)
}

/// `IfcCsgSolid`: a wrapper around whatever its tree root is.
pub struct CsgSolid;

impl SolidEvaluator for CsgSolid {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcCsgSolid"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let root = item
            .attr("TreeRootExpression")
            .as_entity()
            .ok_or_else(|| GeomError::missing("TreeRootExpression"))?;
        ctx.registry().solid(ctx, root)
    }
}

/// `IfcBoundingBox` drawn as a solid.
///
/// A last resort: a crate in the right place beats nothing at all.
pub struct BoundingBox;

impl SolidEvaluator for BoundingBox {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcBoundingBox"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let corner = item
            .attr("Corner")
            .as_entity()
            .and_then(|point| cartesian_point(point, &ctx.units))
            .unwrap_or(DVec3::ZERO);
        let x = length_of(ctx, item, "XDim")?;
        let y = length_of(ctx, item, "YDim")?;
        let z = length_of(ctx, item, "ZDim")?;
        ctx.diag.warn(
            codes::BOUNDING_BOX_SUBSTITUTED,
            item.id(),
            "drawn as its bounding box: the file gives no other body",
        );
        Ok(box_mesh(corner, corner + DVec3::new(x, y, z)))
    }
}

/// `IfcBoxedHalfSpace`: a half space with an explicit box around it.
///
/// Bounded, it is an ordinary convex solid the convex difference can subtract.
pub struct BoxedHalfSpace;

impl SolidEvaluator for BoxedHalfSpace {
    fn classes(&self) -> &'static [&'static str] {
        &["IfcBoxedHalfSpace"]
    }

    fn evaluate(&self, ctx: &EvalCtx<'_>, item: Entity<'_>) -> Result<Mesh64, GeomError> {
        let enclosure = item
            .attr("Enclosure")
            .as_entity()
            .ok_or_else(|| GeomError::missing("Enclosure"))?;
        let corner = enclosure
            .attr("Corner")
            .as_entity()
            .and_then(|point| cartesian_point(point, &ctx.units))
            .unwrap_or(DVec3::ZERO);
        let x = length_of(ctx, enclosure, "XDim")?;
        let y = length_of(ctx, enclosure, "YDim")?;
        let z = length_of(ctx, enclosure, "ZDim")?;
        let box_solid = box_mesh(corner, corner + DVec3::new(x, y, z));

        // The surface placement decides the cut; AgreementFlag decides which side survives.
        let surface = item
            .attr("BaseSurface")
            .as_entity()
            .ok_or_else(|| GeomError::missing("BaseSurface"))?;
        let placement = axis2_placement_3d(surface.attr("Position"), &ctx.units);
        let origin = placement.transform_point3(DVec3::ZERO);
        let mut normal = placement.transform_vector3(DVec3::Z).normalize_or_zero();
        if normal == DVec3::ZERO {
            return Err(GeomError::Degenerate(
                "a half space whose base surface has no normal".into(),
            ));
        }
        // The clip keeps the negative side, which is the material when the flag agrees.
        if !item.attr("AgreementFlag").as_bool().unwrap_or(true) {
            normal = -normal;
        }
        let plane = tessifc_mesh::Plane::from_point_normal(origin, normal)
            .ok_or_else(|| GeomError::Degenerate("a degenerate half space plane".into()))?;
        let cut = tessifc_mesh::clip::clip(&box_solid, &plane, ctx.tol.len);
        if cut.mesh.is_empty() {
            return Err(GeomError::Degenerate(
                "a boxed half space whose box lies entirely outside it".into(),
            ));
        }
        Ok(cut.mesh)
    }
}

/// Register the primitive evaluators.
pub fn register(registry: &mut Registry) {
    registry.register_solid(Box::new(Block));
    registry.register_solid(Box::new(RectangularPyramid));
    registry.register_solid(Box::new(RightCircularCylinder));
    registry.register_solid(Box::new(RightCircularCone));
    registry.register_solid(Box::new(Sphere));
    registry.register_solid(Box::new(CsgSolid));
    registry.register_solid(Box::new(BoundingBox));
    registry.register_solid(Box::new(BoxedHalfSpace));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tests::{eval_solid, model_of};

    #[test]
    fn a_block_is_its_three_lengths() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCBLOCK(#2,2.,3.,4.);\n"
        ));
        let mesh = eval_solid(&model, 3).unwrap();
        assert!((mesh.signed_volume().abs() - 24.0).abs() < 1e-9);
        let (low, high) = mesh.bounds().unwrap();
        assert!((low - DVec3::ZERO).length() < 1e-12, "low {low}");
        assert!(
            (high - DVec3::new(2.0, 3.0, 4.0)).length() < 1e-12,
            "high {high}"
        );
        assert!(mesh.is_edge_manifold());
    }

    #[test]
    fn a_pyramid_holds_a_third_of_its_box() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCRECTANGULARPYRAMID(#2,2.,3.,4.);\n"
        ));
        let mesh = eval_solid(&model, 3).unwrap();
        let volume = mesh.signed_volume().abs();
        assert!((volume - 24.0 / 3.0).abs() < 1e-9, "got {volume}");
        assert!(mesh.is_edge_manifold());
    }

    #[test]
    fn a_cylinder_stands_on_its_placement() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCRIGHTCIRCULARCYLINDER(#2,4.,1.);\n"
        ));
        let mesh = eval_solid(&model, 3).unwrap();
        let (low, high) = mesh.bounds().unwrap();
        // The base circle sits on the placement, the top face at Height.
        assert!(
            low.z.abs() < 1e-12 && (high.z - 4.0).abs() < 1e-12,
            "z {low} {high}"
        );
        let volume = mesh.signed_volume().abs();
        let exact = std::f64::consts::PI * 4.0;
        assert!(
            volume < exact && (exact - volume) / exact < 0.02,
            "got {volume}"
        );
        assert!(mesh.is_edge_manifold(), "a cylinder is a closed solid");
    }

    #[test]
    fn a_cone_closes_at_its_apex() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCRIGHTCIRCULARCONE(#2,3.,2.);\n"
        ));
        let mesh = eval_solid(&model, 3).unwrap();
        let volume = mesh.signed_volume().abs();
        let exact = std::f64::consts::PI * 4.0 * 3.0 / 3.0;
        assert!(
            volume < exact && (exact - volume) / exact < 0.03,
            "got {volume}"
        );
        assert!(mesh.is_edge_manifold(), "the apex must not leave a hole");
        let (low, high) = mesh.bounds().unwrap();
        assert!(
            low.z.abs() < 1e-12 && (high.z - 3.0).abs() < 1e-12,
            "the base sits on the placement and the apex is Height above it, z {low} {high}"
        );
    }

    #[test]
    fn a_cylinder_follows_a_placement_away_from_the_origin() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,10.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCRIGHTCIRCULARCYLINDER(#2,4.,1.);\n"
        ));
        let mesh = eval_solid(&model, 3).unwrap();
        let (low, high) = mesh.bounds().unwrap();
        assert!(
            (low.z - 10.0).abs() < 1e-12 && (high.z - 14.0).abs() < 1e-12,
            "z {low} {high}"
        );
    }

    #[test]
    fn a_sphere_is_closed_at_both_poles() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCSPHERE(#2,1.);\n"
        ));
        let mesh = eval_solid(&model, 3).unwrap();
        assert!(mesh.is_edge_manifold(), "poles must close");
        let volume = mesh.signed_volume().abs();
        let exact = 4.0 / 3.0 * std::f64::consts::PI;
        assert!(
            volume < exact && (exact - volume) / exact < 0.05,
            "got {volume}"
        );
    }

    #[test]
    fn a_csg_solid_follows_its_root() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCBLOCK(#2,1.,1.,1.);\n",
            "#4=IFCCSGSOLID(#3);\n"
        ));
        let mesh = eval_solid(&model, 4).unwrap();
        assert!((mesh.signed_volume().abs() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_bounding_box_becomes_a_crate() {
        let model = model_of(concat!(
            "#1=IFCCARTESIANPOINT((1.,2.,3.));\n",
            "#2=IFCBOUNDINGBOX(#1,2.,2.,2.);\n"
        ));
        let (mesh, diagnostics) = crate::eval::tests::eval_solid_with_diagnostics(&model, 2);
        let mesh = mesh.unwrap();
        let (low, high) = mesh.bounds().unwrap();
        assert!(
            (low - DVec3::new(1.0, 2.0, 3.0)).length() < 1e-12,
            "low {low}"
        );
        assert!(
            (high - DVec3::new(3.0, 4.0, 5.0)).length() < 1e-12,
            "high {high}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::BOUNDING_BOX_SUBSTITUTED),
            "a substituted box has to say so"
        );
    }

    #[test]
    fn a_boxed_half_space_keeps_the_side_the_flag_asks_for() {
        // A plane through z = 0, box from -1 to 1 in every axis.
        let source = concat!(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n",
            "#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
            "#3=IFCPLANE(#2);\n",
            "#4=IFCCARTESIANPOINT((-1.,-1.,-1.));\n",
            "#5=IFCBOUNDINGBOX(#4,2.,2.,2.);\n",
            "#6=IFCBOXEDHALFSPACE(#3,.T.,#5);\n"
        );
        let model = model_of(source);
        let mesh = eval_solid(&model, 6).unwrap();
        let (low, high) = mesh.bounds().unwrap();
        // Agreement keeps the material below the surface: half the box.
        assert!(
            (mesh.signed_volume().abs() - 4.0).abs() < 1e-9,
            "half a box of 8"
        );
        assert!(
            high.z <= 1e-9,
            "kept side reaches up to the plane, got {high}"
        );
        assert!((low.z + 1.0).abs() < 1e-9);

        let flipped = model_of(&source.replace(".T.,#5", ".F.,#5"));
        let mesh = eval_solid(&flipped, 6).unwrap();
        let (low, _) = mesh.bounds().unwrap();
        assert!(low.z >= -1e-9, "the other side, got {low}");
    }
}
