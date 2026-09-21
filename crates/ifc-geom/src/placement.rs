// SPDX-License-Identifier: Apache-2.0
//! `IfcLocalPlacement` chains resolved to world transforms. `RefDirection` is
//! projected onto the plane normal to `Axis`; shared chains are resolved once.

use crate::units::Units;
use glam::{DMat4, DVec3};
use rustc_hash::FxHashMap;
use tessifc_model::{Entity, Model, Value};

/// Read a 3D point, in metres.
pub fn cartesian_point(entity: Entity<'_>, units: &Units) -> Option<DVec3> {
    let coordinates = entity.attr("Coordinates");
    let mut values = coordinates.as_list()?.floats();
    let x = values.next()?;
    let y = values.next().unwrap_or(0.0);
    // A 2D point used where 3D is expected is legal and common in profiles.
    let z = values.next().unwrap_or(0.0);
    Some(DVec3::new(
        units.length(x),
        units.length(y),
        units.length(z),
    ))
}

/// Read a direction. Not scaled: a direction is a ratio, not a length.
pub fn direction(entity: Entity<'_>) -> Option<DVec3> {
    let ratios = entity.attr("DirectionRatios");
    let mut values = ratios.as_list()?.floats();
    let x = values.next()?;
    let y = values.next().unwrap_or(0.0);
    let z = values.next().unwrap_or(0.0);
    let vector = DVec3::new(x, y, z);
    // A zero direction would normalise to NaN; let the caller pick a default.
    if vector.length_squared() > 0.0 {
        Some(vector.normalize())
    } else {
        None
    }
}

/// Build an orthonormal frame from an axis and a reference direction.
///
/// The reference is projected normal to the axis, or replaced by any perpendicular.
pub fn orthonormal_frame(axis: Option<DVec3>, reference: Option<DVec3>) -> (DVec3, DVec3, DVec3) {
    let z = axis.unwrap_or(DVec3::Z);
    let z = if z.length_squared() > 0.0 {
        z.normalize()
    } else {
        DVec3::Z
    };

    // The standard's default is the global X projected normal to the axis; only
    // an axis along X itself falls through to the perpendicular below.
    let candidate = reference.unwrap_or(DVec3::X);

    let projected = candidate - z * candidate.dot(z);
    let x = if projected.length_squared() > 1e-20 {
        projected.normalize()
    } else {
        // Reference parallel to the axis: pick any perpendicular.
        let helper = if z.dot(DVec3::X).abs() > 0.9 {
            DVec3::Y
        } else {
            DVec3::X
        };
        (helper - z * helper.dot(z)).normalize()
    };

    (x, z.cross(x), z)
}

/// Resolve an `IfcAxis2Placement3D` to a transform; missing is the identity.
pub fn axis2_placement_3d(value: Value<'_>, units: &Units) -> DMat4 {
    let Some(entity) = value.as_entity() else {
        return DMat4::IDENTITY;
    };
    if entity.is_a("IfcAxis2Placement2D") {
        return axis2_placement_2d(value, units);
    }
    let origin = entity
        .attr("Location")
        .as_entity()
        .and_then(|point| cartesian_point(point, units))
        .unwrap_or(DVec3::ZERO);
    let axis = entity.attr("Axis").as_entity().and_then(direction);
    let reference = entity.attr("RefDirection").as_entity().and_then(direction);
    let (x, y, z) = orthonormal_frame(axis, reference);
    DMat4::from_cols(
        x.extend(0.0),
        y.extend(0.0),
        z.extend(0.0),
        origin.extend(1.0),
    )
}

/// Resolve an `IfcAxis2Placement2D` to a transform in the XY plane.
pub fn axis2_placement_2d(value: Value<'_>, units: &Units) -> DMat4 {
    let Some(entity) = value.as_entity() else {
        return DMat4::IDENTITY;
    };
    let origin = entity
        .attr("Location")
        .as_entity()
        .and_then(|point| cartesian_point(point, units))
        .unwrap_or(DVec3::ZERO);
    let reference = entity
        .attr("RefDirection")
        .as_entity()
        .and_then(direction)
        .unwrap_or(DVec3::X);
    let x = DVec3::new(reference.x, reference.y, 0.0);
    let x = if x.length_squared() > 0.0 {
        x.normalize()
    } else {
        DVec3::X
    };
    let y = DVec3::new(-x.y, x.x, 0.0);
    DMat4::from_cols(
        x.extend(0.0),
        y.extend(0.0),
        DVec3::Z.extend(0.0),
        origin.extend(1.0),
    )
}

/// Resolve an `IfcCartesianTransformationOperator3D`, uniform or not.
pub fn transformation_operator(entity: Entity<'_>, units: &Units) -> DMat4 {
    let origin = entity
        .attr("LocalOrigin")
        .as_entity()
        .and_then(|point| cartesian_point(point, units))
        .unwrap_or(DVec3::ZERO);

    let axis1 = entity.attr("Axis1").as_entity().and_then(direction);
    let axis2 = entity.attr("Axis2").as_entity().and_then(direction);
    let axis3 = entity.attr("Axis3").as_entity().and_then(direction);

    // Axis1 is X and Axis3 is Z, which defaults to global Z. Axis2 only chooses the
    // sign of Y, so a mirroring operator stays a mirror.
    let (x, right_handed_y, z) = orthonormal_frame(Some(axis3.unwrap_or(DVec3::Z)), axis1);
    let y = match axis2 {
        Some(reference) if reference.dot(right_handed_y) < 0.0 => -right_handed_y,
        _ => right_handed_y,
    };

    let scale = entity.attr("Scale").as_f64().unwrap_or(1.0);
    let scale2 = entity.attr("Scale2").as_f64().unwrap_or(scale);
    let scale3 = entity.attr("Scale3").as_f64().unwrap_or(scale);

    DMat4::from_cols(
        (x * scale).extend(0.0),
        (y * scale2).extend(0.0),
        (z * scale3).extend(0.0),
        origin.extend(1.0),
    )
}

/// Resolves and caches placement chains for one model.
///
/// Without the cache resolution is quadratic in a deep spatial tree.
#[derive(Default)]
pub struct PlacementCache {
    resolved: FxHashMap<u32, DMat4>,
}

impl PlacementCache {
    /// An empty cache.
    pub fn new() -> Self {
        PlacementCache::default()
    }

    /// How many placements have been resolved.
    pub fn len(&self) -> usize {
        self.resolved.len()
    }

    /// True when nothing has been resolved yet.
    pub fn is_empty(&self) -> bool {
        self.resolved.is_empty()
    }

    /// The world transform of an `IfcObjectPlacement`.
    ///
    /// Follows `PlacementRelTo` to the root; a cycle stops at a depth limit.
    /// Placements other than `IfcLocalPlacement` take their fallback; see
    /// [`PlacementCache::world_with`].
    pub fn world(&mut self, model: &Model, placement: Entity<'_>, units: &Units) -> DMat4 {
        self.world_with(model, placement, units, &|_| None)
    }

    /// The world transform, with every placement that is not an
    /// `IfcLocalPlacement` offered to `resolver` first.
    ///
    /// The resolver returns the placement's matrix and the placement that
    /// matrix is relative to; the chain continues from there. When it declines,
    /// an `IfcLinearPlacement` uses its `CartesianPosition` and anything else
    /// the identity, both relative to `PlacementRelTo`.
    pub fn world_with(
        &mut self,
        model: &Model,
        placement: Entity<'_>,
        units: &Units,
        resolver: &PlacementResolver<'_>,
    ) -> DMat4 {
        self.resolve(model, placement, units, resolver, 0)
    }

    // `units` is threaded through rather than stored: a cache outlives one evaluation.
    #[allow(clippy::only_used_in_recursion)]
    fn resolve(
        &mut self,
        model: &Model,
        placement: Entity<'_>,
        units: &Units,
        resolver: &PlacementResolver<'_>,
        depth: u32,
    ) -> DMat4 {
        if let Some(cached) = self.resolved.get(&placement.id()) {
            return *cached;
        }
        // Deeper than any real spatial tree; a cyclic file stops here.
        if depth > 64 || !placement.exists() {
            return DMat4::IDENTITY;
        }

        let (local, parent) = if placement.is_a("IfcLocalPlacement") {
            (
                axis2_placement_3d(placement.attr("RelativePlacement"), units),
                placement.attr("PlacementRelTo").as_entity(),
            )
        } else {
            match resolver(placement) {
                Some((local, parent)) => (local, parent.and_then(|id| model.entity(id))),
                None => (
                    placement_fallback(placement, units),
                    placement.attr("PlacementRelTo").as_entity(),
                ),
            }
        };

        let world = match parent {
            Some(parent) => self.resolve(model, parent, units, resolver, depth + 1) * local,
            None => local,
        };
        self.resolved.insert(placement.id(), world);
        world
    }
}

/// What a placement the resolver declined is worth on its own.
fn placement_fallback(placement: Entity<'_>, units: &Units) -> DMat4 {
    if placement.is_a("IfcLinearPlacement") {
        // The optional cached position is the file's own answer for the frame.
        let position = placement.attr("CartesianPosition");
        if position.as_entity().is_some() {
            return axis2_placement_3d(position, units);
        }
    }
    DMat4::IDENTITY
}

/// Resolves a placement that is not an `IfcLocalPlacement` to its local matrix
/// and the id of the placement it is relative to; `None` declines it.
pub type PlacementResolver<'a> = dyn for<'e> Fn(Entity<'e>) -> Option<(DMat4, Option<u32>)> + 'a;

/// The former name of [`PlacementResolver`], kept for one release.
#[deprecated(note = "renamed to PlacementResolver")]
pub type GridResolver<'a> = PlacementResolver<'a>;

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_step::{ParseOptions, parse};

    fn model_of(data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    const METRES: Units = Units {
        length_to_m: 1.0,
        angle_to_rad: 1.0,
        assumed: false,
    };

    #[test]
    fn a_frame_is_orthonormal() {
        let (x, y, z) = orthonormal_frame(Some(DVec3::Z), Some(DVec3::X));
        assert!((x - DVec3::X).length() < 1e-12);
        assert!((y - DVec3::Y).length() < 1e-12);
        assert!((z - DVec3::Z).length() < 1e-12);
    }

    #[test]
    fn a_non_perpendicular_reference_is_projected() {
        // RefDirection is out of the plane and must be projected, not used raw.
        let axis = DVec3::Z;
        let skew = DVec3::new(1.0, 0.0, 0.3).normalize();
        let (x, y, z) = orthonormal_frame(Some(axis), Some(skew));
        assert!(
            x.dot(z).abs() < 1e-12,
            "x must be perpendicular to z, dot was {}",
            x.dot(z)
        );
        assert!((x.length() - 1.0).abs() < 1e-12);
        assert!((y.length() - 1.0).abs() < 1e-12);
        assert!(
            (x.cross(y) - z).length() < 1e-12,
            "the frame must be right handed"
        );
        // And the projection keeps the direction it was pointing in.
        assert!(x.dot(DVec3::X) > 0.9);
    }

    #[test]
    fn a_reference_parallel_to_the_axis_still_gives_a_frame() {
        let (x, y, z) = orthonormal_frame(Some(DVec3::Z), Some(DVec3::Z));
        assert!(x.dot(z).abs() < 1e-12);
        assert!((x.cross(y) - z).length() < 1e-12);
    }

    #[test]
    fn defaults_when_axis_and_reference_are_absent() {
        let (x, y, z) = orthonormal_frame(None, None);
        assert!((x - DVec3::X).length() < 1e-12);
        assert!((y - DVec3::Y).length() < 1e-12);
        assert!((z - DVec3::Z).length() < 1e-12);
    }

    #[test]
    fn a_placement_puts_a_point_where_it_belongs() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((1.,2.,3.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
        );
        let matrix = axis2_placement_3d(Value::Ref(model.entity(2).unwrap()), &METRES);
        let origin = matrix.transform_point3(DVec3::ZERO);
        assert!(
            (origin - DVec3::new(1.0, 2.0, 3.0)).length() < 1e-12,
            "got {origin}"
        );
    }

    #[test]
    fn a_missing_placement_is_the_identity() {
        assert_eq!(axis2_placement_3d(Value::Null, &METRES), DMat4::IDENTITY);
        assert_eq!(axis2_placement_3d(Value::Missing, &METRES), DMat4::IDENTITY);
    }

    #[test]
    fn a_rotated_placement_rotates() {
        // X axis pointing along global Y: a quarter turn about Z.
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCDIRECTION((0.,1.,0.));\n\
             #4=IFCAXIS2PLACEMENT3D(#1,#2,#3);\n",
        );
        let matrix = axis2_placement_3d(Value::Ref(model.entity(4).unwrap()), &METRES);
        let moved = matrix.transform_point3(DVec3::X);
        assert!((moved - DVec3::Y).length() < 1e-12, "got {moved}");
    }

    #[test]
    fn units_are_applied_to_the_origin() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((1000.,2000.,0.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n",
        );
        let millimetres = Units {
            length_to_m: 1e-3,
            angle_to_rad: 1.0,
            assumed: false,
        };
        let matrix = axis2_placement_3d(Value::Ref(model.entity(2).unwrap()), &millimetres);
        let origin = matrix.transform_point3(DVec3::ZERO);
        assert!(
            (origin - DVec3::new(1.0, 2.0, 0.0)).length() < 1e-12,
            "got {origin}"
        );
    }

    #[test]
    fn a_chain_composes_parent_then_child() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((10.,0.,0.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
             #3=IFCLOCALPLACEMENT($,#2);\n\
             #4=IFCCARTESIANPOINT((0.,5.,0.));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCLOCALPLACEMENT(#3,#5);\n",
        );
        let mut cache = PlacementCache::new();
        let world = cache.world(&model, model.entity(6).unwrap(), &METRES);
        let origin = world.transform_point3(DVec3::ZERO);
        assert!(
            (origin - DVec3::new(10.0, 5.0, 0.0)).length() < 1e-12,
            "got {origin}"
        );
    }

    #[test]
    fn a_chain_is_cached() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((1.,0.,0.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
             #3=IFCLOCALPLACEMENT($,#2);\n",
        );
        let mut cache = PlacementCache::new();
        assert!(cache.is_empty());
        cache.world(&model, model.entity(3).unwrap(), &METRES);
        assert_eq!(cache.len(), 1);
        cache.world(&model, model.entity(3).unwrap(), &METRES);
        assert_eq!(cache.len(), 1, "the second call must hit the cache");
    }

    #[test]
    fn a_cyclic_chain_terminates() {
        // A placement that points at itself must not recurse forever.
        let model = model_of(
            "#1=IFCCARTESIANPOINT((1.,0.,0.));\n\
             #2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
             #3=IFCLOCALPLACEMENT(#3,#2);\n",
        );
        let mut cache = PlacementCache::new();
        let world = cache.world(&model, model.entity(3).unwrap(), &METRES);
        assert!(world.is_finite());
    }

    #[test]
    fn a_non_uniform_operator_scales_each_axis() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM($,$,#1,2.,$,3.,4.);\n",
        );
        let matrix = transformation_operator(model.entity(2).unwrap(), &METRES);
        let moved = matrix.transform_point3(DVec3::ONE);
        assert!(
            (moved - DVec3::new(2.0, 3.0, 4.0)).length() < 1e-12,
            "got {moved}"
        );
    }

    #[test]
    fn a_uniform_operator_scales_everything_alike() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#1,2.,$);\n",
        );
        let matrix = transformation_operator(model.entity(2).unwrap(), &METRES);
        let moved = matrix.transform_point3(DVec3::ONE);
        assert!((moved - DVec3::splat(2.0)).length() < 1e-12, "got {moved}");
    }

    #[test]
    fn a_mirroring_operator_without_axis3_stays_a_mirror() {
        // Axis2 pointing at -Y is how a mirrored family is written; Scale stays positive.
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCDIRECTION((1.,0.,0.));\n\
             #3=IFCDIRECTION((0.,-1.,0.));\n\
             #4=IFCCARTESIANTRANSFORMATIONOPERATOR3D(#2,#3,#1,$,$);\n",
        );
        let matrix = transformation_operator(model.entity(4).unwrap(), &METRES);
        assert!(
            matrix.determinant() < 0.0,
            "a mirror has a negative determinant"
        );
        let moved = matrix.transform_point3(DVec3::new(2.0, 1.0, 3.0));
        assert!(
            (moved - DVec3::new(2.0, -1.0, 3.0)).length() < 1e-12,
            "y is mirrored and z is left alone, got {moved}"
        );
    }

    #[test]
    fn a_mirroring_operator_with_axis3_stays_a_mirror() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #2=IFCDIRECTION((1.,0.,0.));\n\
             #3=IFCDIRECTION((0.,-1.,0.));\n\
             #4=IFCDIRECTION((0.,0.,1.));\n\
             #5=IFCCARTESIANTRANSFORMATIONOPERATOR3D(#2,#3,#1,$,#4);\n",
        );
        let matrix = transformation_operator(model.entity(5).unwrap(), &METRES);
        assert!(matrix.determinant() < 0.0, "Axis2 must not be ignored");
        let moved = matrix.transform_point3(DVec3::new(2.0, 1.0, 3.0));
        assert!(
            (moved - DVec3::new(2.0, -1.0, 3.0)).length() < 1e-12,
            "got {moved}"
        );
    }

    #[test]
    fn a_two_dimensional_mirroring_operator_is_unchanged() {
        let model = model_of(
            "#1=IFCCARTESIANPOINT((0.,0.));\n\
             #2=IFCDIRECTION((1.,0.));\n\
             #3=IFCDIRECTION((0.,-1.));\n\
             #4=IFCCARTESIANTRANSFORMATIONOPERATOR2D(#2,#3,#1,$);\n",
        );
        let matrix = transformation_operator(model.entity(4).unwrap(), &METRES);
        let moved = matrix.transform_point3(DVec3::new(2.0, 1.0, 0.0));
        assert!(
            (moved - DVec3::new(2.0, -1.0, 0.0)).length() < 1e-12,
            "a mirrored 2D profile is unaffected, got {moved}"
        );
    }

    #[test]
    fn a_zero_direction_is_refused_rather_than_normalised_to_nan() {
        let model = model_of("#1=IFCDIRECTION((0.,0.,0.));\n");
        assert_eq!(direction(model.entity(1).unwrap()), None);
    }

    #[test]
    fn a_two_dimensional_point_reads_as_z_zero() {
        let model = model_of("#1=IFCCARTESIANPOINT((1.,2.));\n");
        let point = cartesian_point(model.entity(1).unwrap(), &METRES).unwrap();
        assert_eq!(point, DVec3::new(1.0, 2.0, 0.0));
    }

    #[test]
    fn the_default_reference_is_the_projected_global_x_for_any_other_axis() {
        // IfcFirstProjAxis projects X unless the axis is X itself.
        let axis = DVec3::new(0.95, 0.31, 0.0).normalize();
        let (x, _, z) = orthonormal_frame(Some(axis), None);
        assert!(x.dot(z).abs() < 1e-12);
        assert!(
            x.dot(DVec3::X) > 0.0,
            "x keeps pointing the way global X does, got {x}"
        );
    }
}
