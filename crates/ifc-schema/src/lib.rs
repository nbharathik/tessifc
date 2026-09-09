// SPDX-License-Identifier: Apache-2.0
//! Generated IFC schema tables and the small runtime that reads them: class
//! ids and names, STEP attribute order, subtype tests and defined-type
//! primitives. The tables under `gen/` are generated; do not edit them by hand.
//!
//! # Example
//!
//! ```
//! use tessifc_schema::{Schema, SchemaId};
//!
//! let schema = Schema::get(SchemaId::Ifc4);
//! let wall = schema.class_by_name("IfcWall").unwrap();
//! // STEP argument order is inherited-first: IfcRoot contributes the first four.
//! assert_eq!(schema.attr_name(wall, 0), Some("GlobalId"));
//! assert!(schema.is_a(wall, schema.class_by_name("IfcProduct").unwrap()));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use core::fmt;

#[cfg(not(any(
    feature = "schema-ifc2x3",
    feature = "schema-ifc4",
    feature = "schema-ifc4x3"
)))]
compile_error!(
    "tessifc-schema needs at least one of schema-ifc2x3, schema-ifc4 or schema-ifc4x3.      Building with none of them would produce a kernel that cannot read any file."
);

// `gen` is a reserved keyword in edition 2024, so the module is named `generated`.
#[path = "gen/mod.rs"]
pub mod generated;

/// Identifier of a class within one schema.
/// Ids are dense from 1 and only meaningful with their [`SchemaId`]; 0 is [`CLASS_UNKNOWN`].
pub type ClassId = u16;

/// The class id for entity types not in the loaded schema; such instances are kept.
pub const CLASS_UNKNOWN: ClassId = 0;

/// The longest class name in any supported schema, used for stack buffers.
pub const MAX_CLASS_NAME_LEN: usize = 96;

/// Which IFC schema a model is expressed in.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum SchemaId {
    /// IFC2X3 TC1.
    Ifc2x3,
    /// IFC4 ADD2 TC1.
    Ifc4,
    /// IFC4X3 ADD2.
    Ifc4x3,
}

impl SchemaId {
    /// The canonical name, as it appears in `FILE_SCHEMA`.
    pub const fn as_str(self) -> &'static str {
        match self {
            SchemaId::Ifc2x3 => "IFC2X3",
            SchemaId::Ifc4 => "IFC4",
            SchemaId::Ifc4x3 => "IFC4X3",
        }
    }

    /// Every schema compiled into this build, in ascending order.
    pub fn all() -> &'static [SchemaId] {
        &[
            #[cfg(feature = "schema-ifc2x3")]
            SchemaId::Ifc2x3,
            #[cfg(feature = "schema-ifc4")]
            SchemaId::Ifc4,
            #[cfg(feature = "schema-ifc4x3")]
            SchemaId::Ifc4x3,
        ]
    }

    /// Resolve the string from a `FILE_SCHEMA` header entry, tolerating exporter spellings.
    ///
    /// ```
    /// # use tessifc_schema::SchemaId;
    /// assert_eq!(SchemaId::detect("IFC4X3_ADD2"), Some((SchemaId::Ifc4x3, false)));
    /// assert_eq!(SchemaId::detect("IFC4X1"), Some((SchemaId::Ifc4x3, true)));
    /// assert_eq!(SchemaId::detect("IFC2X3"), Some((SchemaId::Ifc2x3, false)));
    /// assert_eq!(SchemaId::detect("SOMETHING_ELSE"), None);
    /// ```
    ///
    /// The boolean is `true` when the match is approximate.
    pub fn detect(raw: &str) -> Option<(SchemaId, bool)> {
        let mut buf = [0u8; MAX_CLASS_NAME_LEN];
        let name = fold_ascii_upper(raw.trim(), &mut buf)?;
        // Longest prefixes first: IFC4X3 must win over IFC4.
        let table: &[(&str, SchemaId, bool)] = &[
            ("IFC2X3", SchemaId::Ifc2x3, false),
            ("IFC2X2", SchemaId::Ifc2x3, true),
            ("IFC2X_FINAL", SchemaId::Ifc2x3, true),
            ("IFC2X", SchemaId::Ifc2x3, true),
            ("IFC4X3", SchemaId::Ifc4x3, false),
            ("IFC4X2", SchemaId::Ifc4x3, true),
            ("IFC4X1", SchemaId::Ifc4x3, true),
            ("IFC4", SchemaId::Ifc4, false),
        ];
        let mut best: Option<(SchemaId, bool, usize)> = None;
        for (prefix, id, approx) in table {
            if name.starts_with(prefix)
                && best.map(|(_, _, len)| prefix.len() > len).unwrap_or(true)
            {
                best = Some((*id, *approx, prefix.len()));
            }
        }
        best.map(|(id, approx, _)| (id, approx))
    }
}

impl fmt::Display for SchemaId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The innermost primitive an attribute reduces to, after aggregates and one level of defined types.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BaseType {
    /// `REAL` or `NUMBER`.
    Real,
    /// `INTEGER`.
    Integer,
    /// `STRING`.
    String,
    /// `BOOLEAN`.
    Boolean,
    /// `LOGICAL`, which unlike `BOOLEAN` has three states.
    Logical,
    /// `BINARY`.
    Binary,
    /// A reference to another entity instance.
    Entity,
    /// An enumeration value, written `.LIKE_THIS.` in a STEP file.
    Enumeration,
    /// A SELECT, which resolves at runtime to whatever was actually written.
    Select,
}

/// How an attribute participates in the STEP record.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AttrKind {
    /// A plain explicit attribute occupying an argument slot.
    Explicit,
    /// An inherited attribute redeclared as `DERIVE`; still occupies its slot, written `*`.
    /// Dropping these would shift every later argument by one.
    DerivedOverride,
}

/// One attribute of a class, in STEP argument order.
#[derive(Copy, Clone, Debug)]
pub struct AttrDef {
    /// The attribute name as written in EXPRESS, for example `SweptArea`.
    pub name: &'static str,
    /// The innermost named type, e.g. `IfcLengthMeasure`; empty for unnamed primitives.
    pub type_name: &'static str,
    /// What the attribute reduces to once aggregates and defined types are peeled away.
    pub base: BaseType,
    /// Aggregate nesting depth: 0 scalar, 1 `LIST OF X`, 2 `LIST OF LIST OF X`.
    pub agg_depth: u8,
    /// Whether the attribute is declared `OPTIONAL`, and so may be `$`.
    pub optional: bool,
    /// Explicit, or an inherited attribute redeclared as derived.
    pub kind: AttrKind,
}

/// One entity class.
#[derive(Copy, Clone, Debug)]
pub struct ClassDef {
    /// Name in EXPRESS casing, for example `IfcExtrudedAreaSolid`.
    pub name: &'static str,
    /// Name folded to upper case, as it appears in a STEP file.
    pub upper: &'static str,
    /// Direct supertype, or `None` for a root; the first one where EXPRESS allows several.
    pub parent: Option<ClassId>,
    /// `ABSTRACT SUPERTYPE`: no instance of this exact class may appear in a file.
    pub is_abstract: bool,
    /// Explicit attributes in flattened inheritance order, which is the STEP argument order.
    pub attrs: &'static [AttrDef],
    /// `INVERSE` attributes, never serialised; used to build the inverse index.
    pub inverse: &'static [AttrDef],
    /// Preorder index in the inheritance forest; with `subtree_end` it makes `is_a` two comparisons.
    pub subtree_start: u16,
    /// One past the preorder index of the last descendant of this class.
    pub subtree_end: u16,
}

/// A defined type such as `IfcLengthMeasure = REAL`, written as `IFCLENGTHMEASURE(3.5)`.
#[derive(Copy, Clone, Debug)]
pub struct WrapType {
    /// Name in EXPRESS casing.
    pub name: &'static str,
    /// Name folded to upper case, as it appears in a STEP file.
    pub upper: &'static str,
    /// The primitive it wraps.
    pub base: BaseType,
    /// Aggregate nesting depth of the wrapped value.
    pub agg_depth: u8,
}

/// An enumeration type and its values, in declaration order.
#[derive(Copy, Clone, Debug)]
pub struct EnumDef {
    /// Name in EXPRESS casing, for example `IfcUnitEnum`.
    pub name: &'static str,
    /// Values in upper case, without the surrounding dots.
    pub values: &'static [&'static str],
}

/// A SELECT type and the names of its members, for validation and introspection.
#[derive(Copy, Clone, Debug)]
pub struct SelectDef {
    /// Name in EXPRESS casing, for example `IfcAxis2Placement`.
    pub name: &'static str,
    /// Member type names in declaration order.
    pub members: &'static [&'static str],
}

/// A whole schema: the tables plus the lookup functions over them.
pub struct Schema {
    /// Which schema this is.
    pub id: SchemaId,
    /// Classes indexed by [`ClassId`]. Index 0 is the `UNKNOWN` placeholder.
    pub classes: &'static [ClassDef],
    /// Defined types, sorted by `upper`.
    pub wrap_types: &'static [WrapType],
    /// Enumeration types, sorted by `name`.
    pub enums: &'static [EnumDef],
    /// SELECT types, sorted by `name`.
    pub selects: &'static [SelectDef],
    /// Perfect hash from upper-case class name to class id.
    pub by_name: &'static phf::Map<&'static str, ClassId>,
    /// Perfect hash from upper-case defined type name to index into `wrap_types`.
    pub wrap_by_name: &'static phf::Map<&'static str, u16>,
}

impl Schema {
    /// The table for a schema; panics only if the schema feature was compiled out.
    pub fn get(id: SchemaId) -> &'static Schema {
        match id {
            #[cfg(feature = "schema-ifc2x3")]
            SchemaId::Ifc2x3 => &generated::ifc2x3::SCHEMA,
            #[cfg(feature = "schema-ifc4")]
            SchemaId::Ifc4 => &generated::ifc4::SCHEMA,
            #[cfg(feature = "schema-ifc4x3")]
            SchemaId::Ifc4x3 => &generated::ifc4x3::SCHEMA,
            #[allow(unreachable_patterns)]
            other => panic!(
                "schema {other} is not compiled into this build; enable the \
                 corresponding schema-* feature of tessifc-schema"
            ),
        }
    }

    /// Look up a schema, returning `None` when its feature is compiled out.
    pub fn try_get(id: SchemaId) -> Option<&'static Schema> {
        if SchemaId::all().contains(&id) {
            Some(Schema::get(id))
        } else {
            None
        }
    }

    /// Number of classes, including the `UNKNOWN` placeholder at index 0.
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// Definition of a class id; out-of-range ids yield the `UNKNOWN` entry.
    pub fn class(&self, id: ClassId) -> &'static ClassDef {
        self.classes
            .get(id as usize)
            .unwrap_or(&self.classes[CLASS_UNKNOWN as usize])
    }

    /// Class id for a name in any ASCII casing, or `None`.
    /// Hot path: never allocates, names over [`MAX_CLASS_NAME_LEN`] do not resolve.
    pub fn class_by_name(&self, name: &str) -> Option<ClassId> {
        let mut buf = [0u8; MAX_CLASS_NAME_LEN];
        let upper = fold_ascii_upper(name, &mut buf)?;
        self.by_name.get(upper).copied()
    }

    /// Class id for a name that is already upper case, skipping the fold.
    pub fn class_by_upper(&self, upper: &str) -> Option<ClassId> {
        self.by_name.get(upper).copied()
    }

    /// Is `class` the same as, or a subtype of, `of`? Constant time.
    /// Classes are numbered in preorder, so descendants form a contiguous interval.
    pub fn is_a(&self, class: ClassId, of: ClassId) -> bool {
        let c = self.class(class);
        let o = self.class(of);
        // The UNKNOWN placeholder is a subtype of nothing but itself.
        if class == CLASS_UNKNOWN || of == CLASS_UNKNOWN {
            return class == of;
        }
        o.subtree_start <= c.subtree_start && c.subtree_start < o.subtree_end
    }

    /// [`Schema::is_a`] taking a class name; resolve the id once in loops instead.
    pub fn is_a_name(&self, class: ClassId, of: &str) -> bool {
        match self.class_by_name(of) {
            Some(id) => self.is_a(class, id),
            None => false,
        }
    }

    /// Index of an attribute by name within a class, or `None`; resolve once, not per instance.
    pub fn attr_index(&self, class: ClassId, name: &str) -> Option<usize> {
        self.class(class)
            .attrs
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// Name of the attribute at a STEP argument position.
    pub fn attr_name(&self, class: ClassId, index: usize) -> Option<&'static str> {
        self.class(class).attrs.get(index).map(|a| a.name)
    }

    /// Definition of the attribute at a STEP argument position.
    pub fn attr(&self, class: ClassId, index: usize) -> Option<&'static AttrDef> {
        self.class(class).attrs.get(index)
    }

    /// How many arguments a record of this class carries.
    pub fn arity(&self, class: ClassId) -> usize {
        self.class(class).attrs.len()
    }

    /// The defined type of a given name, in any ASCII casing.
    pub fn wrap_type(&self, name: &str) -> Option<&'static WrapType> {
        let mut buf = [0u8; MAX_CLASS_NAME_LEN];
        let upper = fold_ascii_upper(name, &mut buf)?;
        self.wrap_by_name
            .get(upper)
            .map(|&i| &self.wrap_types[i as usize])
    }

    /// The enumeration type of a given name.
    pub fn enum_def(&self, name: &str) -> Option<&'static EnumDef> {
        self.enums
            .binary_search_by(|e| e.name.cmp(name))
            .ok()
            .map(|i| &self.enums[i])
    }

    /// The SELECT type of a given name.
    pub fn select_def(&self, name: &str) -> Option<&'static SelectDef> {
        self.selects
            .binary_search_by(|s| s.name.cmp(name))
            .ok()
            .map(|i| &self.selects[i])
    }

    /// Iterate every class id, skipping the `UNKNOWN` placeholder.
    pub fn class_ids(&self) -> impl Iterator<Item = ClassId> + use<> {
        1..(self.classes.len() as ClassId)
    }
}

impl fmt::Debug for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Schema")
            .field("id", &self.id)
            .field("classes", &self.classes.len())
            .field("wrap_types", &self.wrap_types.len())
            .field("enums", &self.enums.len())
            .field("selects", &self.selects.len())
            .finish()
    }
}

/// Fold ASCII to upper case into `buf`; `None` if it does not fit or is not ASCII.
fn fold_ascii_upper<'b>(s: &str, buf: &'b mut [u8; MAX_CLASS_NAME_LEN]) -> Option<&'b str> {
    let bytes = s.as_bytes();
    if bytes.len() > MAX_CLASS_NAME_LEN || !s.is_ascii() {
        return None;
    }
    for (i, b) in bytes.iter().enumerate() {
        buf[i] = b.to_ascii_uppercase();
    }
    // ASCII by the check above, so valid UTF-8.
    core::str::from_utf8(&buf[..bytes.len()]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_detection_prefers_the_longest_match() {
        assert_eq!(
            SchemaId::detect("IFC4X3_ADD2"),
            Some((SchemaId::Ifc4x3, false))
        );
        assert_eq!(SchemaId::detect("IFC4X3"), Some((SchemaId::Ifc4x3, false)));
        assert_eq!(SchemaId::detect("IFC4"), Some((SchemaId::Ifc4, false)));
        assert_eq!(
            SchemaId::detect("IFC4_ADD2_TC1"),
            Some((SchemaId::Ifc4, false))
        );
        assert_eq!(
            SchemaId::detect("IFC2X3_TC1"),
            Some((SchemaId::Ifc2x3, false))
        );
    }

    #[test]
    fn withdrawn_releases_map_approximately() {
        assert_eq!(SchemaId::detect("IFC4X1"), Some((SchemaId::Ifc4x3, true)));
        assert_eq!(SchemaId::detect("IFC4X2"), Some((SchemaId::Ifc4x3, true)));
        assert_eq!(
            SchemaId::detect("IFC2X2_FINAL"),
            Some((SchemaId::Ifc2x3, true))
        );
    }

    #[test]
    fn unknown_schema_is_none() {
        assert_eq!(SchemaId::detect("IFC5"), None);
        assert_eq!(SchemaId::detect(""), None);
        assert_eq!(SchemaId::detect("CIS2"), None);
    }

    #[test]
    fn case_folding_rejects_absurd_names() {
        let mut buf = [0u8; MAX_CLASS_NAME_LEN];
        assert_eq!(fold_ascii_upper("IfcWall", &mut buf), Some("IFCWALL"));
        let long = "A".repeat(MAX_CLASS_NAME_LEN + 1);
        assert_eq!(fold_ascii_upper(&long, &mut buf), None);
        assert_eq!(fold_ascii_upper("Ifc\u{00e4}Wall", &mut buf), None);
    }
}
