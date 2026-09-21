// SPDX-License-Identifier: Apache-2.0
//! Why an evaluator could not produce geometry.
//!
//! Every variant maps to a stable diagnostic code, so failure is per element.

use tessifc_mesh::TriangulateError;
use tessifc_step::DiagCode;

/// What went wrong evaluating one representation item.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GeomError {
    /// A caller supplied an invalid geometry option.
    #[error("invalid geometry settings: {0}")]
    InvalidSettings(String),

    /// A geometry input exceeded a bounded resource limit.
    #[error("geometry limit reached: {0}")]
    LimitReached(String),
    /// No evaluator is registered for this class.
    #[error("{0} is not supported yet")]
    Unsupported(String),

    /// A required attribute was absent or the wrong shape.
    #[error("{0}")]
    Missing(String),

    /// The item describes nothing: zero depth, zero area, too few points.
    #[error("degenerate: {0}")]
    Degenerate(String),

    /// Triangulation refused the polygon.
    #[error("triangulation failed: {0}")]
    Triangulation(String),

    /// The representation graph nested past the limit, usually a cycle.
    #[error("representation nests deeper than {0} levels")]
    TooDeep(u32),

    /// A boolean the kernel refused; the body is emitted un-cut.
    #[error("{0}")]
    BooleanRefused(String),
}

impl GeomError {
    /// The stable diagnostic code for this failure.
    pub fn code(&self) -> DiagCode {
        match self {
            GeomError::InvalidSettings(_) => codes::INVALID_SETTINGS,
            GeomError::LimitReached(_) => codes::GEOMETRY_LIMIT_REACHED,
            GeomError::Unsupported(_) => codes::UNSUPPORTED_ITEM,
            GeomError::Missing(_) => codes::MISSING_ATTRIBUTE,
            GeomError::Degenerate(_) => codes::DEGENERATE_GEOMETRY,
            GeomError::Triangulation(_) => codes::TRIANGULATION_FAILED,
            GeomError::TooDeep(_) => codes::REPRESENTATION_TOO_DEEP,
            GeomError::BooleanRefused(_) => codes::BOOLEAN_REFUSED,
        }
    }

    /// Convenience for a missing attribute.
    pub fn missing(what: &str) -> Self {
        GeomError::Missing(format!("{what} is missing or unreadable"))
    }
}

impl From<TriangulateError> for GeomError {
    fn from(error: TriangulateError) -> Self {
        GeomError::Triangulation(error.to_string())
    }
}

/// Diagnostic codes this crate produces.
///
/// String constants rather than an enum, so a downstream crate can add its own.
pub mod codes {
    use tessifc_step::DiagCode;

    /// A geometry option is invalid; evaluation was refused.
    pub const INVALID_SETTINGS: DiagCode = DiagCode("E_INVALID_GEOMETRY_SETTINGS");
    /// A bounded geometry operation could not complete within its resource limit.
    pub const GEOMETRY_LIMIT_REACHED: DiagCode = DiagCode("E_GEOMETRY_LIMIT_REACHED");
    /// Tessellation reached a configured limit before satisfying the accuracy request.
    pub const TESSELLATION_TOLERANCE_UNMET: DiagCode = DiagCode("W_TESSELLATION_TOLERANCE_UNMET");

    /// A pcurve domain scale was recovered and checked against its 3D curve.
    pub const PCURVE_DOMAIN_RECOVERED: DiagCode = DiagCode("W_PCURVE_DOMAIN_RECOVERED");
    /// A pcurve recovery required correcting its inconsistent reference weights.
    pub const PCURVE_REFERENCE_RECOVERED: DiagCode = DiagCode("W_PCURVE_REFERENCE_RECOVERED");
    /// An unusable pcurve was replaced by a surface-verified 3D reference.
    pub const PCURVE_3D_FALLBACK: DiagCode = DiagCode("W_PCURVE_3D_FALLBACK");
    /// An inconsistent oriented edge was reversed to join its adjacent edges.
    pub const EDGE_ORIENTATION_RECOVERED: DiagCode = DiagCode("W_EDGE_ORIENTATION_RECOVERED");
    /// No evaluator for this representation item class.
    pub const UNSUPPORTED_ITEM: DiagCode = DiagCode("E_UNSUPPORTED_ITEM");
    /// A required attribute was absent or unreadable.
    pub const MISSING_ATTRIBUTE: DiagCode = DiagCode("E_MISSING_ATTRIBUTE");
    /// The item describes no geometry: zero depth, zero area, too few points.
    pub const DEGENERATE_GEOMETRY: DiagCode = DiagCode("E_DEGENERATE_GEOMETRY");
    /// A polygon could not be triangulated.
    pub const TRIANGULATION_FAILED: DiagCode = DiagCode("E_TRIANGULATION_FAILED");
    /// The representation graph nests too deeply, usually a cycle.
    pub const REPRESENTATION_TOO_DEEP: DiagCode = DiagCode("E_REPRESENTATION_TOO_DEEP");
    /// A boolean the kernel refused; the body is emitted un-cut.
    pub const BOOLEAN_REFUSED: DiagCode = DiagCode("W_BOOLEAN_REFUSED");

    /// The product has no representation this engine can use.
    pub const NO_USABLE_REPRESENTATION: DiagCode = DiagCode("W_NO_USABLE_REPRESENTATION");
    /// A profile outline crosses itself; the result is a guess.
    pub const PROFILE_SELF_INTERSECTING: DiagCode = DiagCode("W_PROFILE_SELF_INTERSECTING");
    /// A shell was inside out and has been flipped.
    pub const SHELL_REORIENTED: DiagCode = DiagCode("W_SHELL_REORIENTED");
    /// The shell still has open or overused edges after welding and T-junction repair.
    pub const NON_MANIFOLD_INPUT: DiagCode = DiagCode("W_NON_MANIFOLD_INPUT");
    /// The declared outer bound was contained by another loop; the enclosing one was used.
    pub const FACE_BOUND_RECOVERED: DiagCode = DiagCode("W_FACE_BOUND_RECOVERED");
    /// The file declared no units and metres were assumed.
    pub const UNITS_ASSUMED: DiagCode = DiagCode("W_UNITS_ASSUMED");
    /// An unimplemented placement kind; the product sits at the origin.
    pub const PLACEMENT_UNSUPPORTED: DiagCode = DiagCode("W_PLACEMENT_UNSUPPORTED");
    /// A bounded sweep was rendered over its full directrix.
    pub const SWEEP_PARAMETERS_APPROXIMATED: DiagCode = DiagCode("W_SWEEP_PARAMETERS_APPROXIMATED");
    /// A style makes a product fully invisible; the colour is passed through as given.
    pub const FULLY_TRANSPARENT_STYLE: DiagCode = DiagCode("W_FULLY_TRANSPARENT_STYLE");
    /// A product with no body is drawn as its bounding box.
    pub const BOUNDING_BOX_SUBSTITUTED: DiagCode = DiagCode("W_BOUNDING_BOX_SUBSTITUTED");
    /// A parametric profile was built with square corners; an edge radius or slope was dropped.
    pub const PROFILE_DETAIL_APPROXIMATED: DiagCode = DiagCode("W_PROFILE_DETAIL_APPROXIMATED");
    /// A curve trim could not be applied; the whole basis curve was used.
    pub const TRIM_IGNORED: DiagCode = DiagCode("W_TRIM_IGNORED");
    /// A solid was asked for from an open profile; a surface was built instead.
    pub const OPEN_PROFILE_SURFACE: DiagCode = DiagCode("W_OPEN_PROFILE_SURFACE");
    /// A polygonal sweep's fillet radius could not be applied; its corners are mitred.
    pub const SWEEP_FILLET_IGNORED: DiagCode = DiagCode("W_SWEEP_FILLET_IGNORED");
    /// A product selected for drawing has only a representation the kernel does
    /// not draw, such as a 2D annotation or a grid's axes.
    pub const NO_DRAWN_REPRESENTATION: DiagCode = DiagCode("W_NO_DRAWN_REPRESENTATION");
    /// Openings were cut through the faces of a body whose inside could not be used.
    pub const OPENING_CUT_ON_SURFACE: DiagCode = DiagCode("W_OPENING_CUT_ON_SURFACE");
    /// An alignment segment does not start where the previous one ends; each keeps its placement.
    pub const ALIGNMENT_SEGMENT_GAP: DiagCode = DiagCode("W_ALIGNMENT_SEGMENT_GAP");
    /// Cant was blended linearly along a segment whose parent curve gives no other shape.
    pub const CANT_APPROXIMATED: DiagCode = DiagCode("W_CANT_APPROXIMATED");
    /// A linear placement's cached position disagrees with the one computed along its curve.
    pub const LINEAR_PLACEMENT_MISMATCH: DiagCode = DiagCode("W_LINEAR_PLACEMENT_MISMATCH");
    /// A vertex loop that is not at an apex or pole of its face's surface bounds no area.
    pub const VERTEX_LOOP_IGNORED: DiagCode = DiagCode("I_VERTEX_LOOP_IGNORED");
    /// A style with several texture layers; only the first is carried.
    pub const TEXTURE_LAYERS_IGNORED: DiagCode = DiagCode("I_TEXTURE_LAYERS_IGNORED");
    /// A texture map whose coordinates do not match the face or face set it maps.
    pub const TEXTURE_MAP_IGNORED: DiagCode = DiagCode("I_TEXTURE_MAP_IGNORED");
    /// A texture coordinate generator in a mode the kernel does not compute.
    pub const TEXTURE_GENERATOR_UNSUPPORTED: DiagCode = DiagCode("I_TEXTURE_GENERATOR_UNSUPPORTED");
    /// A textured part was cut by a boolean, which loses its texture coordinates.
    pub const TEXTURE_DROPPED_BY_BOOLEAN: DiagCode = DiagCode("W_TEXTURE_DROPPED_BY_BOOLEAN");
    /// A texture's pixels were left out of the pack because they are over the size limit.
    pub const TEXTURE_OMITTED: DiagCode = DiagCode("W_TEXTURE_OMITTED");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_code() {
        let cases = [
            GeomError::Unsupported("IfcThing".into()),
            GeomError::Missing("SweptArea".into()),
            GeomError::Degenerate("zero depth".into()),
            GeomError::Triangulation("nope".into()),
            GeomError::TooDeep(24),
            GeomError::BooleanRefused("boolean".into()),
        ];
        for case in cases {
            let code = case.code().as_str();
            assert!(
                code.starts_with("E_") || code.starts_with("W_"),
                "{code} does not follow the naming convention"
            );
        }
    }

    #[test]
    fn messages_say_something_useful() {
        assert_eq!(
            GeomError::Unsupported("IfcSphere".into()).to_string(),
            "IfcSphere is not supported yet"
        );
        assert!(GeomError::missing("Depth").to_string().contains("Depth"));
    }
}
