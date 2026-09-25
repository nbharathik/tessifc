// SPDX-License-Identifier: Apache-2.0
//! STEP-21 (ISO 10303-21) reader for IFC files: bytes in, a flat immutable
//! [`ModelImage`] out, with no interpretation of geometry or units. [`parse`]
//! reports recoverable errors as diagnostics and keeps what was readable.
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

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

pub mod diag;
pub mod edit;
pub mod hash;
#[cfg(feature = "ifczip")]
pub mod ifczip;
pub mod image;
pub mod parse;
pub mod strings;
pub mod tape;

pub use diag::{DiagCode, Diagnostic, Diagnostics, Severity};
#[cfg(feature = "edit")]
pub use edit::{AttributeEdit, EditValue, apply_edits};
pub use edit::{EditError, argument_source, leaf_argument_source};
#[cfg(feature = "ifczip")]
pub use ifczip::{UnzipOptions, ZipError, is_ifczip, open, open_source, unzip_ifc};
pub use image::{Header, IndexEntry, ModelImage};
pub use parse::{ParseOptions, parse};

/// Parse plain STEP; this build was made without the `ifczip` feature.
#[cfg(not(feature = "ifczip"))]
pub fn open(bytes: &[u8], opts: &ParseOptions) -> ModelImage {
    parse(bytes, opts)
}

/// Parse plain STEP and hand the bytes back; this build reads no archives.
#[cfg(not(feature = "ifczip"))]
pub fn open_source(bytes: Vec<u8>, opts: &ParseOptions) -> (ModelImage, Vec<u8>) {
    (parse(&bytes, opts), bytes)
}
pub use strings::{StrId, StringArena};
pub use tape::{Cursor, RawValue};

/// Re-exported so callers need not depend on `tessifc-schema` directly.
pub use tessifc_schema::{CLASS_UNKNOWN, ClassId, Schema, SchemaId};
