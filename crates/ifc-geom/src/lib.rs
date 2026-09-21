// SPDX-License-Identifier: Apache-2.0
//! IFC geometry evaluation: units, placements, curves, profiles and solids.
//! A [`Registry`] maps IFC class to evaluator; adding a class is one file.
//!
//! ```no_run
//! use tessifc_geom::{DiagnosticSink, EvalCtx, Registry, Settings, Tolerances, Units};
//! use tessifc_model::Model;
//!
//! # let model: Model = unimplemented!();
//! let units = Units::from_model(&model);
//! let settings = Settings::default();
//! let sink = DiagnosticSink::default();
//! let ctx = EvalCtx::new(&model, units, Tolerances::default(), &settings, &sink);
//! let registry = Registry::defaults(model.image().schema);
//!
//! for wall in model.entities_of_type("IfcWall") {
//!     // ... find its body representation items, then:
//!     // let mesh = registry.solid(&ctx, item)?;
//!     let _ = wall;
//! }
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod context;
pub mod error;
pub mod eval;
pub mod georef;
pub mod grid;
pub mod placement;
pub mod product;
pub mod registry;
pub mod style;
pub mod units;

pub use context::{
    DiagnosticSink, EvalCaches, EvalCtx, PhaseTimings, Settings, Stopwatch, Timings, Tolerances,
};
pub use error::{GeomError, codes};
pub use placement::PlacementCache;
pub use product::{
    BooleanStatus, PartGeometry, ProductCategory, ProductPart, Provenance, Representation,
    SharedKey, product_category, product_colour, product_mesh, product_parts, product_transform,
    representation_of, should_include,
};
pub use registry::{
    BSplineSurface, CurveEvaluator, Polyline3, Profile2D, ProfileEvaluator, Registry,
    SolidEvaluator, Surface, SurfaceEvaluator, SurfaceKind,
};
pub use style::{
    Material, Rgba, Texture, TextureGenerator, TextureSource, class_colour, decode_step_binary,
    item_colour,
};
pub use units::Units;
