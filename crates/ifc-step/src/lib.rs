// SPDX-License-Identifier: Apache-2.0
//! STEP-21 (ISO 10303-21) reader for IFC files: bytes in, a flat immutable
//! [`ModelImage`] out, with no interpretation of geometry or units.
//! [`parse`] reports recoverable errors and retains what was readable.
//!
//! # Example
//!
//! ```
//! use tessifc_step::{ParseOptions, parse};
//!
//! let src = std::fs::read("model.ifc").unwrap_or_default();
//! let image = parse(&src, &ParseOptions::default());
//! println!("{} instances, schema {}", image.len(), image.schema);
//! for d in image.diagnostics.items() {
//!     println!("{d}");
//! }
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod diag;
pub mod edit;
pub mod hash;
pub mod image;
pub mod parse;
pub mod strings;
pub mod tape;

pub use diag::{DiagCode, Diagnostic, Diagnostics, Severity};
pub use edit::{
    AttributeEdit, EditError, EditValue, apply_edits, argument_source, leaf_argument_source,
};
pub use image::{Header, IndexEntry, ModelImage};
pub use parse::{ParseOptions, parse};
pub use strings::{StrId, StringArena};
pub use tape::{Cursor, RawValue};

/// Re-exported so callers need not depend on `tessifc-schema` directly.
pub use tessifc_schema::{CLASS_UNKNOWN, ClassId, Schema, SchemaId};
