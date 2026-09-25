// SPDX-License-Identifier: Apache-2.0
//! The inverse index: who points at me?
//! IFC stores relationships as separate objects, so the reverse direction is
//! built once, lazily, as compressed sparse rows per relation.

use crate::{Entity, Model, Value};
use tessifc_schema::{ClassId, Schema};

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

/// Upper bound on the pairs one relationship record may contribute when both
/// of its sides are lists.
const MAX_PAIRS_PER_RELATIONSHIP: usize = 1 << 16;

/// Pairs the whole index may hold for each instance in the model.
const PAIRS_PER_INSTANCE: usize = 8;

/// The pair budget of a small model.
const MIN_PAIR_BUDGET: usize = 1 << 20;

impl Inverse {
    /// Targets of a relation for one key, ascending. Empty when there are none.
    pub fn get(&self, relation: Relation, key: u32) -> &[u32] {
        match self.relations.get(relation as usize) {
            Some(csr) => csr.get(key),
            None => &[],
        }
    }

    /// Express ids of relationship records left out of the index, ascending:
    /// a list where the schema allows one reference, or more links than the
    /// index has room for. Their targets are missing from every relation.
    pub fn dropped_relationships(&self) -> &[u32] {
        &self.dropped
    }

    /// Approximate resident size, for reporting.
    pub fn memory_bytes(&self) -> usize {
        self.relations.iter().map(|c| c.memory_bytes()).sum()
    }

    /// Build every relation in one pass over the relationship instances.
    pub(crate) fn build(model: &Model) -> Inverse {
        let budget = model
            .len()
            .saturating_mul(PAIRS_PER_INSTANCE)
            .max(MIN_PAIR_BUDGET);
        Inverse::build_within(model, budget)
    }

    /// [`Inverse::build`] holding at most `budget` pairs across every relation.
    fn build_within(model: &Model, budget: usize) -> Inverse {
        let mut builder = Builder {
            model,
            pairs: vec![Vec::new(); Relation::ALL.len()],
            dropped: Vec::new(),
            budget,
        };
        builder.collect(
            "IfcRelVoidsElement",
            End::Attr("RelatingBuildingElement"),
            End::Attr("RelatedOpeningElement"),
            &[(Relation::Voids, false)],
        );
        builder.collect(
            "IfcRelFillsElement",
            End::Attr("RelatingOpeningElement"),
            End::Attr("RelatedBuildingElement"),
            &[(Relation::Fills, false)],
        );
        builder.collect(
            "IfcRelAssociatesMaterial",
            End::Attr("RelatedObjects"),
            End::Attr("RelatingMaterial"),
            &[(Relation::Material, false)],
        );
        // One pass feeds both directions, so a record is kept or dropped in both.
        builder.collect(
            "IfcRelAggregates",
            End::Attr("RelatingObject"),
            End::Attr("RelatedObjects"),
            &[
                (Relation::Aggregates, false),
                (Relation::AggregatedInto, true),
            ],
        );
        builder.collect(
            "IfcRelContainedInSpatialStructure",
            End::Attr("RelatedElements"),
            End::Attr("RelatingStructure"),
            &[(Relation::ContainedIn, false)],
        );
        builder.collect(
            "IfcRelDefinesByType",
            End::Attr("RelatedObjects"),
            End::Attr("RelatingType"),
            &[(Relation::DefinedByType, false)],
        );
        // Plain attributes rather than relationship objects.
        builder.collect(
            "IfcStyledItem",
            End::Attr("Item"),
            End::Record,
            &[(Relation::Styles, false)],
        );
        builder.collect(
            "IfcMappedItem",
            End::Attr("MappingSource"),
            End::Record,
            &[(Relation::MapUsers, false)],
        );

        let Builder {
            pairs, mut dropped, ..
        } = builder;
        dropped.sort_unstable();
        dropped.dedup();
        Inverse {
            relations: pairs.into_iter().map(Csr::build).collect(),
            dropped,
        }
    }

    /// Pairs held across every relation.
    #[cfg(test)]
    fn pair_count(&self) -> usize {
        self.relations.iter().map(|csr| csr.targets.len()).sum()
    }
}

/// One side of a relation: an attribute of the record, or the record itself.
#[derive(Copy, Clone)]
enum End {
    Attr(&'static str),
    Record,
}

/// A side resolved against the schema once, outside the loop over records.
#[derive(Copy, Clone)]
enum Side {
    Attr { index: usize, aggregate: bool },
    Record,
}

impl Side {
    fn resolve(schema: &Schema, class: ClassId, end: End) -> Option<Side> {
        match end {
            End::Record => Some(Side::Record),
            End::Attr(name) => {
                let index = schema.attr_index(class, name)?;
                let aggregate = schema.attr(class, index)?.agg_depth > 0;
                Some(Side::Attr { index, aggregate })
            }
        }
    }

    /// Referenced express ids, ascending and unique; `None` for a list where
    /// the schema allows a single reference.
    fn ids(self, entity: Entity<'_>) -> Option<Vec<u32>> {
        let (index, aggregate) = match self {
            Side::Record => return Some(vec![entity.id()]),
            Side::Attr { index, aggregate } => (index, aggregate),
        };
        let mut ids = match entity.attr_at(index) {
            Value::Ref(e) => vec![e.id()],
            Value::List(list) if aggregate => {
                list.filter_map(|v| v.as_entity().map(|e| e.id())).collect()
            }
            Value::List(_) => return None,
            _ => Vec::new(),
        };
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }
}

/// The pairs gathered so far, and what is left of the budget.
struct Builder<'m> {
    model: &'m Model,
    pairs: Vec<Vec<(u32, u32)>>,
    dropped: Vec<u32>,
    budget: usize,
}

impl Builder<'_> {
    /// Index every record of a class into each `(relation, reverse)` target.
    fn collect(&mut self, class_name: &str, from: End, to: End, targets: &[(Relation, bool)]) {
        let schema = self.model.schema();
        let Some(class) = schema.class_by_name(class_name) else {
            return;
        };
        let (Some(from), Some(to)) = (
            Side::resolve(schema, class, from),
            Side::resolve(schema, class, to),
        ) else {
            return;
        };
        for id in self.model.image().ids_of_type(class) {
            let Some(entity) = self.model.entity(id) else {
                continue;
            };
            let (Some(froms), Some(tos)) = (from.ids(entity), to.ids(entity)) else {
                self.dropped.push(id);
                continue;
            };
            let per_target = froms.len().saturating_mul(tos.len());
            let cost = per_target.saturating_mul(targets.len());
            // Only a list on both sides can multiply out. A well-formed
            // one-to-many record is accepted whole while the budget lasts.
            let crossed =
                froms.len() > 1 && tos.len() > 1 && per_target > MAX_PAIRS_PER_RELATIONSHIP;
            if crossed || cost > self.budget {
                self.dropped.push(id);
                continue;
            }
            self.budget -= cost;
            for &(relation, reverse) in targets {
                let out = &mut self.pairs[relation as usize];
                for &f in &froms {
                    for &t in &tos {
                        out.push(if reverse { (t, f) } else { (f, t) });
                    }
                }
            }
        }
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
        // A list in the single-reference RelatingObject slot is malformed.
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

    /// `count` aggregation records, each `IFCRELAGGREGATES` with the given
    /// RelatingObject and RelatedObjects text, after one wall `#1`.
    fn aggregates(count: u32, relating: &str, related: &str) -> String {
        let mut src = String::from(HEADER);
        src.push_str("#1=IFCWALL('w',$,$,$,$,$,$,$,$);\n");
        for id in 2..count + 2 {
            src.push_str(&format!(
                "#{id}=IFCRELAGGREGATES('r',$,$,$,{relating},{related});\n"
            ));
        }
        src.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        src
    }

    #[test]
    fn a_list_in_a_single_reference_slot_is_dropped_not_multiplied() {
        // Each record would be 256 by 256 pairs if RelatingObject took a list.
        let refs = vec!["#1"; 256].join(",");
        let list = format!("({refs})");
        let count = 64;
        let model = model_from(&aggregates(count, &list, &list));
        let inverse = model.inverse();
        assert_eq!(inverse.pair_count(), 0);
        let dropped: Vec<u32> = (2..count + 2).collect();
        assert_eq!(inverse.dropped_relationships(), dropped.as_slice());
    }

    #[test]
    fn repeated_references_in_one_record_count_once() {
        let refs = vec!["#1"; 256].join(",");
        let model = model_from(&aggregates(1, "#1", &format!("({refs})")));
        assert_eq!(model.parts_of(1), &[1]);
        assert_eq!(model.inverse().pair_count(), 2, "one pair each way");
        assert!(model.inverse().dropped_relationships().is_empty());
    }

    #[test]
    fn records_past_the_whole_index_budget_are_dropped() {
        let mut src = String::from(HEADER);
        src.push_str("#1=IFCBUILDINGSTOREY('s',$,$,$,$,$,$,$,$,$);\n");
        for id in 2..=11 {
            src.push_str(&format!("#{id}=IFCWALL('w',$,$,$,$,$,$,$,$);\n"));
        }
        let walls = (2..=11)
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(",");
        for id in 20..23 {
            src.push_str(&format!(
                "#{id}=IFCRELCONTAINEDINSPATIALSTRUCTURE('r',$,$,$,({walls}),#1);\n"
            ));
        }
        src.push_str("ENDSEC;\nEND-ISO-10303-21;\n");

        let model = model_from(&src);
        // Ten pairs a record: two fit in 25, the third does not.
        let inverse = Inverse::build_within(&model, 25);
        assert_eq!(
            inverse.pair_count(),
            10,
            "the first two records are identical"
        );
        assert_eq!(inverse.dropped_relationships(), &[22]);
        assert_eq!(inverse.get(Relation::ContainedIn, 2), &[1]);
        assert!(model.inverse().dropped_relationships().is_empty());
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
