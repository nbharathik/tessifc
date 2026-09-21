// SPDX-License-Identifier: Apache-2.0
//! Typed views over a parsed model image: entities with attributes by name,
//! and the inverse relationships IFC does not store directly. Nothing here
//! allocates per access except string decoding.
//!
//! ```no_run
//! use tessifc_model::Model;
//! use tessifc_step::{ParseOptions, parse};
//!
//! let bytes = std::fs::read("model.ifc")?;
//! let model = Model::new(parse(&bytes, &ParseOptions::default()));
//!
//! for wall in model.entities_of_type("IfcWall") {
//!     let name = wall.attr("Name").as_string().unwrap_or_default();
//!     let openings = model.voids_of(wall.id()).len();
//!     println!("#{} {name}: {openings} openings", wall.id());
//! }
//! # Ok::<(), std::io::Error>(())
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod inverse;
pub mod value;

pub use inverse::{Inverse, Relation};
pub use value::{ListIter, Text, TypedValue, Value};

use std::sync::OnceLock;
use tessifc_schema::{CLASS_UNKNOWN, ClassId, Schema};
use tessifc_step::image::ENTRY_COMPLEX;
use tessifc_step::tape::{Cursor, RawValue};
use tessifc_step::{IndexEntry, ModelImage};

/// A parsed model, with the schema tables and a lazily built inverse index.
pub struct Model {
    image: ModelImage,
    schema: &'static Schema,
    inverse: OnceLock<Inverse>,
}

/// Where a serialized attribute lives in its source record.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AttributeLocation {
    /// Zero-based argument index, local to `leaf_class` for a complex instance.
    pub argument_index: usize,
    /// Complex leaf class, or `None` for a normal entity record.
    pub leaf_class: Option<&'static str>,
}

impl Model {
    /// Wrap a parsed image.
    pub fn new(image: ModelImage) -> Self {
        let schema = image.schema_tables();
        Model {
            image,
            schema,
            inverse: OnceLock::new(),
        }
    }

    /// The underlying image.
    pub fn image(&self) -> &ModelImage {
        &self.image
    }

    /// The schema tables in force for this model.
    pub fn schema(&self) -> &'static Schema {
        self.schema
    }

    /// Give the image back, dropping the views.
    pub fn into_image(self) -> ModelImage {
        self.image
    }

    /// Number of instances.
    pub fn len(&self) -> usize {
        self.image.len()
    }

    /// True when the file yielded no instances.
    pub fn is_empty(&self) -> bool {
        self.image.is_empty()
    }

    /// An entity by express id. `None` when there is no such instance.
    pub fn entity(&self, express_id: u32) -> Option<Entity<'_>> {
        self.image.entry(express_id).map(|_| Entity {
            model: self,
            id: express_id,
        })
    }

    /// An entity handle for an id that may not exist; accessors degrade to [`Value::Missing`].
    pub fn entity_ref(&self, express_id: u32) -> Entity<'_> {
        Entity {
            model: self,
            id: express_id,
        }
    }

    /// Resolve an attribute name to an argument index; hold on to the result outside loops.
    pub fn attr_index(&self, class: ClassId, name: &str) -> Option<usize> {
        self.schema.attr_index(class, name)
    }

    /// Every instance of exactly this class.
    pub fn entities_of_class(&self, class: ClassId) -> impl Iterator<Item = Entity<'_>> {
        self.image
            .ids_of_class(class)
            .iter()
            .map(move |&id| Entity { model: self, id })
    }

    /// Every instance of this class or any subtype, by name; unknown names yield nothing.
    pub fn entities_of_type<'a>(
        &'a self,
        class_name: &str,
    ) -> Box<dyn Iterator<Item = Entity<'a>> + 'a> {
        match self.schema.class_by_name(class_name) {
            Some(class) => Box::new(self.entities_of_type_id(class)),
            None => Box::new(core::iter::empty()),
        }
    }

    /// Every instance of this class or any subtype.
    pub fn entities_of_type_id(&self, class: ClassId) -> impl Iterator<Item = Entity<'_>> {
        self.image
            .ids_of_type(class)
            .map(move |id| Entity { model: self, id })
    }

    /// How many instances of this class or any subtype.
    pub fn count_of_type(&self, class_name: &str) -> usize {
        match self.schema.class_by_name(class_name) {
            Some(class) => self.image.count_of_type(class),
            None => 0,
        }
    }

    /// The inverse index, built on first use.
    pub fn inverse(&self) -> &Inverse {
        self.inverse.get_or_init(|| Inverse::build(self))
    }

    /// Openings that cut this element, through `IfcRelVoidsElement`.
    pub fn voids_of(&self, element: u32) -> &[u32] {
        self.inverse().get(Relation::Voids, element)
    }

    /// What fills this opening, through `IfcRelFillsElement`.
    pub fn fills_of(&self, opening: u32) -> &[u32] {
        self.inverse().get(Relation::Fills, opening)
    }

    /// `IfcStyledItem` instances that style this representation item.
    pub fn styles_of(&self, item: u32) -> &[u32] {
        self.inverse().get(Relation::Styles, item)
    }

    /// The `RelatingMaterial` of each `IfcRelAssociatesMaterial` naming this object.
    /// This is an `IfcMaterialSelect`, so an id may be an `IfcMaterial` or a
    /// layer-set, profile-set or list wrapper that has to be unwrapped.
    pub fn materials_of(&self, object: u32) -> &[u32] {
        self.inverse().get(Relation::Material, object)
    }

    /// Parts of this aggregate.
    pub fn parts_of(&self, whole: u32) -> &[u32] {
        self.inverse().get(Relation::Aggregates, whole)
    }

    /// Aggregate parents of this object, through `IfcRelAggregates`.
    /// Malformed multiple-parent input is preserved so callers can diagnose it.
    pub fn aggregate_parents_of(&self, part: u32) -> &[u32] {
        self.inverse().get(Relation::AggregatedInto, part)
    }

    /// Spatial structures containing this product, through `IfcRelContainedInSpatialStructure`.
    pub fn spatial_containers_of(&self, product: u32) -> &[u32] {
        self.inverse().get(Relation::ContainedIn, product)
    }

    /// `IfcMappedItem` instances that use this representation map.
    pub fn users_of_map(&self, map: u32) -> &[u32] {
        self.inverse().get(Relation::MapUsers, map)
    }

    /// Read one value at a tape cursor; shared by the value types.
    fn read<'a>(&'a self, cursor: &mut Cursor<'a>) -> Value<'a> {
        read_value(self, cursor)
    }
}

impl core::fmt::Debug for Model {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Model")
            .field("schema", &self.image.schema)
            .field("entities", &self.image.len())
            .finish()
    }
}

/// One instance, as a handle. Copying it is free.
#[derive(Copy, Clone)]
pub struct Entity<'a> {
    model: &'a Model,
    id: u32,
}

impl<'a> Entity<'a> {
    /// The express id, the `#n` from the file.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The model this entity belongs to.
    pub fn model(&self) -> &'a Model {
        self.model
    }

    /// False when the id was referenced but never defined.
    pub fn exists(&self) -> bool {
        self.entry().is_some()
    }

    fn entry(&self) -> Option<&'a IndexEntry> {
        self.model.image.entry(self.id)
    }

    /// The class id, or [`CLASS_UNKNOWN`].
    pub fn class(&self) -> ClassId {
        self.entry().map_or(CLASS_UNKNOWN, |e| e.class_id)
    }

    /// The class name; allocates only for classes outside the schema.
    pub fn class_name(&self) -> String {
        self.model.image.class_name_of(self.id)
    }

    /// The one-based source line the record was written on.
    pub fn line(&self) -> u32 {
        self.entry().map_or(0, |e| e.line)
    }

    /// True when written as a complex instance, `#1=(A(...)B(...))`.
    pub fn is_complex(&self) -> bool {
        self.entry().is_some_and(|e| e.flags & ENTRY_COMPLEX != 0)
    }

    /// Is this class the given one, or a subtype of it?
    pub fn is_a(&self, class_name: &str) -> bool {
        match self.model.schema.class_by_name(class_name) {
            Some(class) => self.is_a_id(class),
            None => false,
        }
    }

    /// Is this class the given one, or a subtype of it? For a complex instance, any leaf counts.
    pub fn is_a_id(&self, class: ClassId) -> bool {
        if self.model.schema.is_a(self.class(), class) {
            return true;
        }
        if self.is_complex() {
            for leaf in self.leaves() {
                if self.model.schema.is_a(leaf, class) {
                    return true;
                }
            }
        }
        false
    }

    /// The leaf classes of a complex instance, or an empty list.
    pub fn leaves(&self) -> Vec<ClassId> {
        let mut out = Vec::new();
        if !self.is_complex() {
            return out;
        }
        let Some(mut cursor) = self.model.image.args(self.id) else {
            return out;
        };
        while let Some(value) = cursor.read() {
            match value {
                RawValue::Leaf(class) => {
                    out.push(class);
                    if !cursor.skip_value() {
                        break;
                    }
                }
                _ => break,
            }
        }
        out
    }

    /// The value of an attribute by name; for a complex instance every leaf is searched.
    /// The name is resolved every call: in loops use [`Model::attr_index`] and [`Entity::attr_at`].
    pub fn attr(&self, name: &str) -> Value<'a> {
        if self.is_complex() {
            return self.complex_attr(name);
        }
        match self.model.schema.attr_index(self.class(), name) {
            Some(index) => self.attr_at(index),
            None => Value::Missing,
        }
    }

    /// Locate a named attribute in STEP serialization order.
    /// Complex instances return the leaf class and a leaf-local index.
    pub fn attribute_location(&self, name: &str) -> Option<AttributeLocation> {
        if !self.is_complex() {
            return self
                .model
                .schema
                .attr_index(self.class(), name)
                .map(|argument_index| AttributeLocation {
                    argument_index,
                    leaf_class: None,
                });
        }
        for leaf in self.leaves() {
            let Some(index) = self.model.schema.attr_index(leaf, name) else {
                continue;
            };
            let parent_count = self
                .model
                .schema
                .class(leaf)
                .parent
                .map(|parent| self.model.schema.arity(parent))
                .unwrap_or(0);
            if let Some(argument_index) = index.checked_sub(parent_count) {
                return Some(AttributeLocation {
                    argument_index,
                    leaf_class: Some(self.model.schema.class(leaf).name),
                });
            }
        }
        None
    }

    /// The value of the argument at a position, counting from zero.
    pub fn attr_at(&self, index: usize) -> Value<'a> {
        let Some(entry) = self.entry() else {
            return Value::Missing;
        };
        if entry.flags & ENTRY_COMPLEX != 0 {
            return Value::Missing;
        }
        let mut cursor = self.model.image.args_of(entry);
        if !cursor.skip_values(index) {
            return Value::Missing;
        }
        if cursor.peek().is_none() {
            return Value::Missing;
        }
        self.model.read(&mut cursor)
    }

    /// How many arguments the record actually carries.
    pub fn arity(&self) -> usize {
        let Some(entry) = self.entry() else { return 0 };
        let mut cursor = self.model.image.args_of(entry);
        let mut n = 0;
        while cursor.peek().is_some() && cursor.skip_value() {
            n += 1;
        }
        n
    }

    /// Search the leaves of a complex instance for an attribute.
    fn complex_attr(&self, name: &str) -> Value<'a> {
        let Some(mut cursor) = self.model.image.args(self.id) else {
            return Value::Missing;
        };
        while let Some(value) = cursor.read() {
            let RawValue::Leaf(class) = value else { break };
            // The leaf arguments follow as a bracketed list.
            let mut leaf = cursor;
            let index = self.model.schema.attr_index(class, name);
            if !cursor.skip_value() {
                break;
            }
            let Some(index) = index else { continue };
            if leaf.read() != Some(RawValue::ListStart) {
                continue;
            }
            // A leaf carries only its own class's attributes, so the flattened
            // index is shifted by what the parent contributes.
            let parent_count = self
                .model
                .schema
                .class(class)
                .parent
                .map(|p| self.model.schema.arity(p))
                .unwrap_or(0);
            let Some(local) = index.checked_sub(parent_count) else {
                continue;
            };
            if !leaf.skip_values(local) {
                continue;
            }
            match leaf.peek() {
                None | Some(RawValue::ListEnd) => continue,
                Some(_) => return self.model.read(&mut leaf),
            }
        }
        Value::Missing
    }
}

impl core::fmt::Debug for Entity<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "#{}={}", self.id, self.class_name())
    }
}

/// Read one value at a cursor, advancing it past the whole value.
pub(crate) fn read_value<'a>(model: &'a Model, cursor: &mut Cursor<'a>) -> Value<'a> {
    match cursor.read() {
        None => Value::Missing,
        Some(RawValue::Null) => Value::Null,
        Some(RawValue::Derived) => Value::Derived,
        Some(RawValue::Int(v)) => Value::Int(v),
        Some(RawValue::Real(v)) => Value::Real(v),
        Some(RawValue::Str(id)) => Value::Str(Text::new(model, id)),
        Some(RawValue::Enum(id)) => Value::Enum(Text::new(model, id)),
        Some(RawValue::Binary(id)) => Value::Binary(Text::new(model, id)),
        Some(RawValue::Ref(id)) => Value::Ref(model.entity_ref(id)),
        Some(RawValue::ListStart) => {
            let inner = *cursor;
            // Step the outer cursor over the whole list.
            let mut depth = 1usize;
            while depth > 0 {
                match cursor.read() {
                    Some(RawValue::ListStart) => depth += 1,
                    Some(RawValue::ListEnd) => depth -= 1,
                    Some(_) => {}
                    None => break,
                }
            }
            Value::List(ListIter::new(model, inner))
        }
        Some(RawValue::Typed(name)) => {
            let payload = *cursor;
            cursor.skip_value();
            Value::Typed(TypedValue::new(model, name, payload))
        }
        // A stray list end or leaf marker where a value was expected.
        Some(RawValue::ListEnd) | Some(RawValue::Leaf(_)) => Value::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_step::{ParseOptions, parse};

    fn model_from(src: &str) -> Model {
        Model::new(parse(src.as_bytes(), &ParseOptions::default()))
    }

    const WALL: &str = concat!(
        "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
        "#1=IFCCARTESIANPOINT((1.,2.,3.));\n",
        "#2=IFCDIRECTION((0.,0.,1.));\n",
        "#3=IFCAXIS2PLACEMENT3D(#1,#2,$);\n",
        "#4=IFCWALL('2O2Fr$t4X7Zf8NOew3FLKI',$,'Wall A',$,$,$,$,'TAG',.SOLIDWALL.);\n",
        "#5=IFCEXTRUDEDAREASOLID($,#3,#2,2.5);\n",
        "ENDSEC;\nEND-ISO-10303-21;\n"
    );

    #[test]
    fn attributes_resolve_by_name() {
        let model = model_from(WALL);
        let wall = model.entity(4).unwrap();
        assert_eq!(wall.class_name(), "IfcWall");
        assert_eq!(
            wall.attr("GlobalId").as_string().as_deref(),
            Some("2O2Fr$t4X7Zf8NOew3FLKI")
        );
        assert_eq!(wall.attr("Name").as_string().as_deref(), Some("Wall A"));
        assert!(wall.attr("Description").is_nothing());
        assert_eq!(wall.attr("Tag").as_string().as_deref(), Some("TAG"));
        assert!(
            wall.attr("PredefinedType")
                .as_text()
                .unwrap()
                .is("SOLIDWALL")
        );
        assert!(wall.attr("NoSuchAttribute").is_nothing());
    }

    #[test]
    fn coordinates_read_as_floats() {
        let model = model_from(WALL);
        let point = model.entity(1).unwrap();
        assert_eq!(
            point.attr("Coordinates").as_floats::<3>(),
            Some([1.0, 2.0, 3.0])
        );
        // Asking for more than is there fails rather than inventing a zero.
        assert_eq!(point.attr("Coordinates").as_floats::<4>(), None);
    }

    #[test]
    fn references_resolve_and_dangle_safely() {
        let model = model_from(WALL);
        let placement = model.entity(3).unwrap();
        let location = placement.attr("Location").as_entity().unwrap();
        assert_eq!(location.id(), 1);
        assert!(placement.attr("RefDirection").is_nothing());

        let dangling = model.entity_ref(9999);
        assert!(!dangling.exists());
        assert!(dangling.attr("Anything").is_nothing());
        assert_eq!(dangling.class(), CLASS_UNKNOWN);
    }

    #[test]
    fn is_a_walks_the_inheritance_chain() {
        let model = model_from(WALL);
        let wall = model.entity(4).unwrap();
        assert!(wall.is_a("IfcWall"));
        assert!(wall.is_a("IfcBuildingElement"));
        assert!(wall.is_a("IfcProduct"));
        assert!(wall.is_a("IfcRoot"));
        assert!(!wall.is_a("IfcSlab"));
        assert!(!wall.is_a("IfcNotAClass"));
    }

    #[test]
    fn derived_slots_are_counted() {
        // IfcSIUnit redeclares Dimensions as derived, so it is written as `*`
        // and still occupies argument zero.
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        let unit = model.entity(1).unwrap();
        assert!(matches!(unit.attr("Dimensions"), Value::Derived));
        assert!(unit.attr("UnitType").as_text().unwrap().is("LENGTHUNIT"));
        assert!(unit.attr("Prefix").is_nothing());
        assert!(unit.attr("Name").as_text().unwrap().is("METRE"));
    }

    #[test]
    fn typed_values_unwrap() {
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=IFCPROPERTYSINGLEVALUE('P',$,IFCLENGTHMEASURE(3.5),$);\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        let property = model.entity(1).unwrap();
        let value = property.attr("NominalValue");
        assert_eq!(value.as_f64(), Some(3.5));
        match value {
            Value::Typed(t) => assert!(t.is("IFCLENGTHMEASURE")),
            other => panic!("expected a typed value, got {other:?}"),
        }
    }

    #[test]
    fn complex_instances_expose_every_leaf() {
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=(IFCNAMEDUNIT(*,.LENGTHUNIT.)IFCSIUNIT($,.METRE.));\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        let unit = model.entity(1).unwrap();
        assert!(unit.is_complex());
        assert!(unit.is_a("IfcSIUnit"));
        assert!(unit.is_a("IfcNamedUnit"));
        assert!(unit.attr("UnitType").as_text().unwrap().is("LENGTHUNIT"));
        assert!(unit.attr("Name").as_text().unwrap().is("METRE"));
        assert_eq!(
            unit.attribute_location("UnitType"),
            Some(AttributeLocation {
                argument_index: 1,
                leaf_class: Some("IfcNamedUnit"),
            })
        );
        assert_eq!(
            unit.attribute_location("Name"),
            Some(AttributeLocation {
                argument_index: 1,
                leaf_class: Some("IfcSIUnit"),
            })
        );
    }

    #[test]
    fn the_inverse_index_finds_openings() {
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=IFCWALL('w',$,$,$,$,$,$,$,$);\n",
            "#2=IFCOPENINGELEMENT('o',$,$,$,$,$,$,$,$);\n",
            "#3=IFCRELVOIDSELEMENT('r',$,$,$,#1,#2);\n",
            "#4=IFCDOOR('d',$,$,$,$,$,$,$,$,$,$,$);\n",
            "#5=IFCRELFILLSELEMENT('f',$,$,$,#2,#4);\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        assert_eq!(model.voids_of(1), &[2]);
        assert_eq!(model.fills_of(2), &[4]);
        assert_eq!(model.voids_of(2), &[] as &[u32]);
    }

    #[test]
    fn materials_of_yields_the_material_not_the_relationship() {
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=IFCWALL('w',$,$,$,$,$,$,$,$);\n",
            "#2=IFCMATERIAL('Concrete',$,$);\n",
            "#3=IFCRELASSOCIATESMATERIAL('m',$,$,$,(#1),#2);\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        assert_eq!(model.materials_of(1), &[2]);
        assert!(model.entity(2).unwrap().is_a("IfcMaterial"));
    }

    #[test]
    fn list_emptiness_is_about_lists_only() {
        let model = model_from(WALL);
        let wall = model.entity(4).unwrap();
        // A string is not a list, so it is neither an empty one nor a full one.
        assert!(!wall.attr("Name").list_is_empty());
        assert_eq!(wall.attr("Name").list_len(), 0);
        let point = model.entity(1).unwrap();
        assert_eq!(point.attr("Coordinates").list_len(), 3);
        assert!(!point.attr("Coordinates").list_is_empty());
    }

    #[test]
    fn entities_of_type_includes_subtypes() {
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=IFCWALL('a',$,$,$,$,$,$,$,$);\n",
            "#2=IFCWALLSTANDARDCASE('b',$,$,$,$,$,$,$,$);\n",
            "#3=IFCSLAB('c',$,$,$,$,$,$,$,$);\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        assert_eq!(model.count_of_type("IfcWall"), 2);
        assert_eq!(model.count_of_type("IfcSlab"), 1);
        assert_eq!(model.count_of_type("IfcBuildingElement"), 3);
        assert_eq!(model.count_of_type("IfcProduct"), 3);
        assert_eq!(model.count_of_type("IfcNotAClass"), 0);
    }

    #[test]
    fn unknown_classes_keep_their_name() {
        let src = concat!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
            "#1=IFCVENDOREXTENSION('x',1,2);\n",
            "ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let model = model_from(src);
        let entity = model.entity(1).unwrap();
        assert_eq!(entity.class(), CLASS_UNKNOWN);
        assert_eq!(entity.class_name(), "IFCVENDOREXTENSION");
        assert_eq!(entity.arity(), 3);
    }
}
