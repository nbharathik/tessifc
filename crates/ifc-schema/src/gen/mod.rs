// SPDX-License-Identifier: Apache-2.0
//! Generated schema tables. Do not edit by hand.
//!
//! Each module is generated from the official buildingSMART EXPRESS file for
//! that schema and committed, so building TessIFC needs neither Python nor
//! network access. The EXPRESS sources are published under CC BY-ND 4.0 and
//! are never committed; see NOTICE.

/// IFC2X3 TC1 tables.
#[cfg(feature = "schema-ifc2x3")]
pub mod ifc2x3;
/// IFC4 ADD2 TC1 tables.
#[cfg(feature = "schema-ifc4")]
pub mod ifc4;
/// IFC4X3 ADD2 tables.
#[cfg(feature = "schema-ifc4x3")]
pub mod ifc4x3;
