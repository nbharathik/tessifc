// SPDX-License-Identifier: Apache-2.0
//! Triangulation, welding, normals, clipping and bounded mesh booleans.
//! These operations are independent of IFC and use f64 coordinates.
//!
//! ```
//! use tessifc_mesh::{Polygon2, triangulate_polygon};
//! use glam::DVec2;
//!
//! let square = Polygon2::new(vec![
//!     DVec2::new(0.0, 0.0), DVec2::new(1.0, 0.0),
//!     DVec2::new(1.0, 1.0), DVec2::new(0.0, 1.0),
//! ]);
//! assert_eq!(triangulate_polygon(&square).unwrap().len(), 6);
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod bsp;
pub mod clip;
pub mod locality;
pub mod mesh;
pub mod simplify;
pub mod triangulate;
pub mod weld;

pub use bsp::{bsp_cells, bsp_cells_or_reason};
pub use clip::{
    ClipOutcome, Clipped, Plane, clip, convex_cells, convex_cells_or_reason, difference_convex,
    difference_convex_many, difference_extrusion_many, difference_prismatic_many,
    difference_prismatic_many_or_reason, difference_surface_many_or_reason, face_planes,
    intersection_general, intersection_general_or_reason, is_convex, union_general,
    union_general_or_reason,
};
pub use locality::{cache_miss_ratio, optimize_vertex_locality, optimize_vertex_locality_f32};
pub use mesh::{Mesh64, face_normals, newell_normal};
pub use simplify::{merge_coplanar, restore_boundary_vertices};
pub use triangulate::{
    PlaneBasis, Polygon2, TriangulateError, signed_area, triangulate_face, triangulate_polygon,
    triangulation_deviates,
};
pub use weld::{
    heal_t_junctions, orient_triangles_consistently, remove_duplicate_triangles,
    split_coincident_edges, weld, weld_and_close,
};
