// SPDX-License-Identifier: Apache-2.0
//! The inverse index: who points at me?
//! IFC stores relationships as separate objects, so the reverse direction is
//! built once, lazily, as compressed sparse rows per relation.

use crate::Model;
use tessifc_schema::ClassId;

/// Which relationship an index answers for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Relation {
    /// Element to the openings that cut it, through `IfcRelVoidsElement`.
    Voids = 0,
    /// Opening to what fills it, through `IfcRelFillsElement`.
    /// Fillings are separate products and must not be subtracted a second time.
    Fills = 1,
    /// Object to the materials associated with it, through `IfcRelAssociatesMaterial`.
    Material = 2,
    /// Representation item to the `IfcStyledItem` instances that style it.
    Styles = 3,
    /// Whole to its parts, through `IfcRelAggregates`.
    Aggregates = 4,
    /// Part to its whole, the other direction of `IfcRelAggregates`.
    AggregatedInto = 5,
    /// `IfcRepresentationMap` to the `IfcMappedItem` instances that use it.
    MapUsers = 6,
    /// Product to the spatial structures that contain it, through `IfcRelContainedInSpatialStructure`.
    ContainedIn = 7,
    /// Object to the type object that defines it, through `IfcRelDefinesByType`.
    DefinedByType = 8,
}

impl Relation {
    /// Every relation, for building them all at once.
    pub const ALL: [Relation; 9] = [
        Relation::Voids,
        Relation::Fills,
        Relation::Material,
        Relation::Styles,
        Relation::Aggregates,
        Relation::AggregatedInto,
        Relation::MapUsers,
        Relation::DefinedByType,
        Relation::ContainedIn,
    ];
}

/// One relation as compressed sparse rows.
#[derive(Debug, Default)]
struct Csr {
    /// Sorted, unique keys.
    keys: Vec<u32>,
    /// `offsets[i] .. offsets[i + 1]` is the target range of `keys[i]`.
    offsets: Vec<u32>,
    /// Target express ids, ascending within each row.
    targets: Vec<u32>,
}

impl Csr {
    fn build(mut pairs: Vec<(u32, u32)>) -> Self {
        pairs.sort_unstable();
        pairs.dedup();
        let mut keys = Vec::new();
        let mut offsets = vec![0u32];
        let mut targets = Vec::with_capacity(pairs.len());
        for (key, target) in pairs {
            if keys.last() != Some(&key) {
                keys.push(key);
                offsets.push(targets.len() as u32);
            }
            targets.push(target);
            let last = offsets.len() - 1;
            offsets[last] = targets.len() as u32;
        }
        Csr {
            keys,
            offsets,
            targets,
        }
    }

    fn get(&self, key: u32) -> &[u32] {
        match self.keys.binary_search(&key) {
            Ok(i) => {
                let start = self.offsets[i] as usize;
                let end = self.offsets[i + 1] as usize;
                self.targets.get(start..end).unwrap_or(&[])
            }
            Err(_) => &[],
        }
    }

    fn memory_bytes(&self) -> usize {
        (self.keys.capacity() + self.offsets.capacity() + self.targets.capacity()) * 4
    }
}

/// All the inverse relations for one model.
#[derive(Debug)]
pub struct Inverse {
    relations: Vec<Csr>,
    dropped: Vec<u32>,
}

/// Upper bound on the pairs one relationship record may contribute.
const MAX_PAIRS_PER_RELATIONSHIP: usize = 1 << 16;

impl Inverse {
    /// Targets of a relation for one key, ascending. Empty when there are none.
    pub fn get(&self, relation: Relation, key: u32) -> &[u32] {
        match self.relations.get(relation as usize) {
            Some(csr) => csr.get(key),
            None => &[],
        }
    }

    /// Express ids of relationship records whose links were too many to index,
    /// ascending. Their targets are missing from every relation.
    pub fn dropped_relationships(&self) -> &[u32] {
        &self.dropped
    }

    /// Approximate resident size, for reporting.
    pub fn memory_bytes(&self) -> usize {
        self.relations.iter().map(|c| c.memory_bytes()).sum()
    }

    /// Build every relation in one pass over the relationship instances.
    pub(crate) fn build(model: &Model) -> Inverse {
        let schema = model.schema();
        let mut pairs: Vec<Vec<(u32, u32)>> = vec![Vec::new(); Relation::ALL.len()];
        let mut dropped: Vec<u32> = Vec::new();

        // Class ids and attribute slots are resolved once, outside the loop.
        let collect_rel = |class_name: &str,
                           from_attr: &str,
                           to_attr: &str,
                           relation: Relation,
                           reverse: bool,
                           pairs: &mut Vec<Vec<(u32, u32)>>,
                           dropped: &mut Vec<u32>| {
            let Some(class) = schema.class_by_name(class_name) else {
                return;
            };
            let Some(from_slot) = schema.attr_index(class, from_attr) else {
                return;
            };
            let Some(to_slot) = schema.attr_index(class, to_attr) else {
                return;
            };
            let out = &mut pairs[relation as usize];
            for id in model.image().ids_of_type(class) {
                let Some(entity) = model.entity(id) else {
                    continue;
                };
                let froms = ids_of(entity.attr_at(from_slot));
                let tos = ids_of(entity.attr_at(to_slot));
                // Only a list on both sides can multiply out. A well-formed
                // one-to-many record is accepted whole, however long it is.
                if froms.len() > 1
                    && tos.len() > 1
                    && froms.len().saturating_mul(tos.len()) > MAX_PAIRS_PER_RELATIONSHIP
                {
                    dropped.push(id);
                    continue;
                }
                for &f in &froms {
                    for &t in &tos {
                        if reverse {
                            out.push((t, f))
                        } else {
                            out.push((f, t))
                        }
                    }
                }
            }
        };

        collect_rel(
            "IfcRelVoidsElement",
            "RelatingBuildingElement",
            "RelatedOpeningElement",
            Relation::Voids,
            false,
            &mut pairs,
            &mut dropped,
        );
        collect_rel(
            "IfcRelFillsElement",
            "RelatingOpeningElement",
            "RelatedBuildingElement",
            Relation::Fills,
            false,
            &mut pairs,
            &mut dropped,
        );
        collect_rel(
            "IfcRelAssociatesMaterial",
            "RelatedObjects",
            "RelatingMaterial",
            Relation::Material,
            false,
            &mut pairs,
            &mut dropped,
        );
        collect_rel(
            "IfcRelAggregates",
            "RelatingObject",
            "RelatedObjects",
            Relation::Aggregates,
            false,
            &mut pairs,
            &mut dropped,
        );
        collect_rel(
            "IfcRelAggregates",
            "RelatingObject",
            "RelatedObjects",
            Relation::AggregatedInto,
            true,
            &mut pairs,
            &mut dropped,
        );
        collect_rel(
            "IfcRelContainedInSpatialStructure",
            "RelatedElements",
            "RelatingStructure",
            Relation::ContainedIn,
            false,
            &mut pairs,
            &mut dropped,
        );
        collect_rel(
            "IfcRelDefinesByType",
            "RelatedObjects",
            "RelatingType",
            Relation::DefinedByType,
            false,
            &mut pairs,
            &mut dropped,
        );

        // IfcStyledItem.Item is a plain attribute, not a relationship object.
        if let Some(class) = schema.class_by_name("IfcStyledItem")
            && let Some(slot) = schema.attr_index(class, "Item")
        {
            let out = &mut pairs[Relation::Styles as usize];
            for id in model.image().ids_of_type(class) {
                let Some(entity) = model.entity(id) else {
                    continue;
                };
                for item in ids_of(entity.attr_at(slot)) {
                    out.push((item, id));
                }
            }
        }

        // IfcMappedItem.MappingSource points at the representation map.
        if let Some(class) = schema.class_by_name("IfcMappedItem")
            && let Some(slot) = schema.attr_index(class, "MappingSource")
        {
            let out = &mut pairs[Relation::MapUsers as usize];
            for id in model.image().ids_of_type(class) {
                let Some(entity) = model.entity(id) else {
                    continue;
                };
                for source in ids_of(entity.attr_at(slot)) {
                    out.push((source, id));
                }
            }
        }

        // IfcRelAggregates is collected twice, so the same record can be
        // recorded twice.
        dropped.sort_unstable();
        dropped.dedup();
        Inverse {
            relations: pairs.into_iter().map(Csr::build).collect(),
            dropped,
        }
    }
}

/// Express ids referenced by a value: one for a reference, many for a list.
fn ids_of(value: crate::Value<'_>) -> Vec<u32> {
    match value {
        crate::Value::Ref(e) => vec![e.id()],
        crate::Value::List(list) => list.filter_map(|v| v.as_entity().map(|e| e.id())).collect(),
        _ => Vec::new(),
    }
}

/// Class ids of the relationship classes, so callers can skip them when enumerating products.
pub fn relationship_classes(model: &Model) -> Vec<ClassId> {
    let schema = model.schema();
    ["IfcRelationship", "IfcStyledItem"]
        .iter()
        .filter_map(|name| schema.class_by_name(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_step::{ParseOptions, parse};

    fn model_from(src: &str) -> Model {
        Model::new(parse(src.as_bytes(), &ParseOptions::default()))
    }

    const HEADER: &str = "ISO-10303-21;
HEADER;
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
";

    #[test]
    fn a_long_one_to_many_relationship_is_kept_whole() {
        let count = MAX_PAIRS_PER_RELATIONSHIP + 1;
        let mut src = String::from(HEADER);
        src.push_str(
            "#1=IFCBUILDINGSTOREY('s',$,$,$,$,$,$,$,$,$);
",
        );
        for index in 0..count {
            src.push_str(&format!(
                "#{}=IFCWALL('w',$,$,$,$,$,$,$,$);
",
                index + 2
            ));
        }
        src.push_str(&format!(
            "#{}=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,(",
            count + 2
        ));
        for index in 0..count {
            if index > 0 {
                src.push(',');
            }
            src.push_str(&format!("#{}", index + 2));
        }
        src.push_str(
            "),#1);
ENDSEC;
END-ISO-10303-21;
",
        );

        let model = model_from(&src);
        let last = (count + 1) as u32;
        assert_eq!(
            model.spatial_containers_of(last),
            &[1],
            "a storey with more elements than the cap still contains them"
        );
        assert!(model.inverse().dropped_relationships().is_empty());
    }

    #[test]
    fn a_cross_product_blow_up_is_recorded_not_silently_dropped() {
        let side = 300u32;
        let mut src = String::from(HEADER);
        for id in 1..=side * 2 {
            src.push_str(&format!(
                "#{id}=IFCWALL('w',$,$,$,$,$,$,$,$);
"
            ));
        }
        let join = |range: std::ops::RangeInclusive<u32>| {
            range
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let rel = side * 2 + 1;
        // A list in the single-reference RelatingObject slot is the malformed
        // shape the cap exists for.
        src.push_str(&format!(
            "#{rel}=IFCRELAGGREGATES('r',$,$,$,({}),({}));
ENDSEC;
END-ISO-10303-21;
",
            join(1..=side),
            join(side + 1..=side * 2)
        ));

        let model = model_from(&src);
        assert!(model.parts_of(1).is_empty());
        assert_eq!(model.inverse().dropped_relationships(), &[rel]);
    }

    #[test]
    fn csr_rows_are_isolated() {
        let csr = Csr::build(vec![(5, 50), (1, 10), (5, 51), (1, 11), (9, 90)]);
        assert_eq!(csr.get(1), &[10, 11]);
        assert_eq!(csr.get(5), &[50, 51]);
        assert_eq!(csr.get(9), &[90]);
        assert_eq!(csr.get(2), &[] as &[u32]);
        assert_eq!(csr.get(u32::MAX), &[] as &[u32]);
    }

    #[test]
    fn csr_deduplicates() {
        let csr = Csr::build(vec![(1, 10), (1, 10), (1, 11)]);
        assert_eq!(csr.get(1), &[10, 11]);
    }

    #[test]
    fn an_empty_csr_answers_nothing() {
        let csr = Csr::build(Vec::new());
        assert_eq!(csr.get(0), &[] as &[u32]);
    }
}
