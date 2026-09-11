// SPDX-License-Identifier: Apache-2.0
//! Structural revision comparison and conservative invalidation for the built-in evaluators.
//! Both immutable models remain available while removed and newly added dependencies are followed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tessifc_model::{Entity, Model};
use tessifc_step::tape::{Cursor, RawValue};

const MAX_DEPENDENCY_EDGES: usize = 4_000_000;
const MAX_DEPENDENCY_VISITS: usize = 1_000_000;

/// Changes and product work needed to bring an existing scene to a candidate revision.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeImpact {
    /// New STEP entities, in ascending candidate Express ID order.
    pub created_entities: Vec<u32>,
    /// Entities whose class or decoded attribute values changed.
    pub modified_entities: Vec<u32>,
    /// Removed STEP entities, in ascending previous Express ID order.
    pub deleted_entities: Vec<u32>,
    /// Candidate product IDs to evaluate, including products currently without geometry.
    pub affected_products: Vec<u32>,
    /// Previous product IDs to remove, including identities replaced at the same ID.
    pub removed_products: Vec<u32>,
    /// Candidate product IDs whose inspection data may need refreshing.
    pub metadata_products: Vec<u32>,
    /// True when interpretation, identity or a traversal budget requires all candidate products.
    pub full_rebuild: bool,
    /// One deterministic explanation per affected product, including metadata-only work.
    pub reasons: Vec<ImpactReason>,
}

/// A changed entity that reaches a product through a dependency path.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactReason {
    /// The candidate product to refresh.
    pub express_id: u32,
    /// The previous or candidate entity whose change caused this work.
    pub entity_id: u32,
    /// The invalidation category or conservative fallback reason.
    pub reason: String,
}

/// Compare immutable snapshots and find affected products for the built-in geometry pipeline.
/// Caller settings and parse policy must agree; changing them requires a full evaluation.
/// Renumbered product identities and exhausted graph budgets conservatively rebuild the scene.
pub fn compare_revisions(old: &Model, new: &Model) -> ChangeImpact {
    compare_with_limits(old, new, MAX_DEPENDENCY_EDGES, MAX_DEPENDENCY_VISITS)
}

fn compare_with_limits(
    old: &Model,
    new: &Model,
    edge_limit: usize,
    visit_limit: usize,
) -> ChangeImpact {
    let mut impact = ChangeImpact::default();
    let mut geometry_seeds = BTreeSet::new();
    let mut all_seeds = BTreeSet::new();
    let mut before = old.image().index.iter().peekable();
    let mut after = new.image().index.iter().peekable();
    while before.peek().is_some() || after.peek().is_some() {
        let old_id = before.peek().map(|entry| entry.express_id);
        let new_id = after.peek().map(|entry| entry.express_id);
        match (old_id, new_id) {
            (Some(a), Some(b)) if a == b => {
                if let Some(metadata_only) = record_change(old, new, a) {
                    impact.modified_entities.push(a);
                    all_seeds.insert(a);
                    if !metadata_only {
                        geometry_seeds.insert(a);
                    }
                }
                before.next();
                after.next();
            }
            (Some(a), Some(b)) if a < b => {
                impact.deleted_entities.push(a);
                all_seeds.insert(a);
                if !old.entity(a).is_some_and(metadata_entity) {
                    geometry_seeds.insert(a);
                }
                before.next();
            }
            (Some(a), None) => {
                impact.deleted_entities.push(a);
                all_seeds.insert(a);
                if !old.entity(a).is_some_and(metadata_entity) {
                    geometry_seeds.insert(a);
                }
                before.next();
            }
            (_, Some(b)) => {
                impact.created_entities.push(b);
                all_seeds.insert(b);
                if !new.entity(b).is_some_and(metadata_entity) {
                    geometry_seeds.insert(b);
                }
                after.next();
            }
            (None, None) => break,
        }
    }

    let old_products = product_ids(old);
    let new_products = product_ids(new);
    impact.removed_products = old_products.difference(&new_products).copied().collect();
    let schema_changed = old.image().schema != new.image().schema
        || old.image().schema_approximate != new.image().schema_approximate;
    if all_seeds.is_empty() && !schema_changed {
        return impact;
    }
    let origin = all_seeds.first().copied().unwrap_or(0);
    if schema_changed {
        return full_impact(
            impact,
            &new_products,
            origin,
            "schema interpretation changed",
        );
    }
    if product_identity_changed(old, new) {
        impact.removed_products = old_products.into_iter().collect();
        return full_impact(impact, &new_products, origin, "product identity changed");
    }

    let mut graph = Dependencies::default();
    if !graph.add_model(old, edge_limit) || !graph.add_model(new, edge_limit) {
        return full_impact(
            impact,
            &new_products,
            origin,
            "dependency edge budget exceeded",
        );
    }
    graph.finish();
    let geometry = match affected(
        &graph.geometry,
        &geometry_seeds,
        &new_products,
        &graph.global,
        false,
        visit_limit,
    ) {
        Ok(products) => products,
        Err((id, reason)) => return full_impact(impact, &new_products, id, reason),
    };
    let metadata = match affected(
        &graph.metadata,
        &all_seeds,
        &new_products,
        &BTreeSet::new(),
        true,
        visit_limit,
    ) {
        Ok(products) => products,
        Err((id, reason)) => return full_impact(impact, &new_products, id, reason),
    };
    impact.affected_products = geometry.keys().copied().collect();
    impact.metadata_products = metadata.keys().chain(geometry.keys()).copied().collect();
    impact.metadata_products.sort_unstable();
    impact.metadata_products.dedup();
    for &id in &impact.metadata_products {
        let (entity_id, reason) = match geometry.get(&id) {
            Some(&source) => (source, "geometry dependency changed"),
            None => (metadata[&id], "metadata dependency changed"),
        };
        impact.reasons.push(ImpactReason {
            express_id: id,
            entity_id,
            reason: reason.into(),
        });
    }
    impact
}

fn product_ids(model: &Model) -> BTreeSet<u32> {
    model
        .entities_of_type("IfcProduct")
        .map(|entity| entity.id())
        .collect()
}

fn full_impact(
    mut impact: ChangeImpact,
    products: &BTreeSet<u32>,
    entity_id: u32,
    reason: &str,
) -> ChangeImpact {
    impact.full_rebuild = true;
    impact.affected_products = products.iter().copied().collect();
    impact.metadata_products = impact.affected_products.clone();
    impact.reasons = products
        .iter()
        .map(|&express_id| ImpactReason {
            express_id,
            entity_id,
            reason: reason.into(),
        })
        .collect();
    impact
}

fn product_identity_changed(old: &Model, new: &Model) -> bool {
    let mut old_guids = BTreeMap::new();
    let mut new_guids = BTreeMap::new();
    for (model, ids) in [(old, &mut old_guids), (new, &mut new_guids)] {
        for product in model.entities_of_type("IfcProduct") {
            if let Some(guid) = product
                .attr("GlobalId")
                .as_string()
                .filter(|g| !g.is_empty())
                && ids.insert(guid, product.id()).is_some()
            {
                return true;
            }
        }
    }
    if old_guids
        .iter()
        .any(|(guid, id)| new_guids.get(guid).is_some_and(|new_id| id != new_id))
    {
        return true;
    }
    old.entities_of_type("IfcProduct").any(|product| {
        new.entity(product.id())
            .filter(|entity| entity.is_a("IfcProduct"))
            .is_some_and(|next| {
                product.attr("GlobalId").as_string() != next.attr("GlobalId").as_string()
            })
    })
}

fn metadata_entity(entity: Entity<'_>) -> bool {
    [
        "IfcProperty",
        "IfcPropertyDefinition",
        "IfcPhysicalQuantity",
        "IfcOwnerHistory",
        "IfcPerson",
        "IfcOrganization",
        "IfcPersonAndOrganization",
        "IfcApplication",
        "IfcRelDefinesByProperties",
    ]
    .iter()
    .any(|class| entity.is_a(class))
}

fn metadata_attribute(entity: Entity<'_>, name: Option<&str>) -> bool {
    entity.is_a("IfcRoot")
        && matches!(
            name,
            Some("Name" | "Description" | "OwnerHistory" | "ObjectType" | "Tag")
        )
}

fn global_entity(entity: Entity<'_>) -> bool {
    [
        "IfcProject",
        "IfcUnitAssignment",
        "IfcNamedUnit",
        "IfcDerivedUnit",
        "IfcMeasureWithUnit",
        "IfcGeometricRepresentationContext",
        "IfcCoordinateOperation",
        "IfcCoordinateReferenceSystem",
    ]
    .iter()
    .any(|class| entity.is_a(class))
}

fn record_change(old: &Model, new: &Model, id: u32) -> Option<bool> {
    let a = old.entity_ref(id);
    let b = new.entity_ref(id);
    if !a.class_name().eq_ignore_ascii_case(&b.class_name()) || a.is_complex() != b.is_complex() {
        return Some(false);
    }
    let mut left = old.image().args(id)?;
    let mut right = new.image().args(id)?;
    let mut changed = false;
    let mut metadata_only = true;
    let mut index = 0;
    while !left.is_empty() || !right.is_empty() {
        let a_start = left;
        let b_start = right;
        let a_valid = left.skip_value();
        let b_valid = right.skip_value();
        if !a_valid
            || !b_valid
            || !tokens_equal(old, new, a_start, left.pos(), b_start, right.pos())
        {
            changed = true;
            metadata_only &= metadata_entity(a)
                || (!a.is_complex()
                    && metadata_attribute(a, old.schema().attr_name(a.class(), index)));
        }
        if !a_valid || !b_valid {
            return Some(false);
        }
        index += 1;
    }
    changed.then_some(metadata_only)
}

fn tokens_equal(
    old: &Model,
    new: &Model,
    mut left: Cursor<'_>,
    left_end: usize,
    mut right: Cursor<'_>,
    right_end: usize,
) -> bool {
    while left.pos() < left_end && right.pos() < right_end {
        let equivalent = match (left.read(), right.read()) {
            (Some(RawValue::Str(a)), Some(RawValue::Str(b))) => {
                old.image().strings.decode(a) == new.image().strings.decode(b)
            }
            (Some(RawValue::Enum(a)), Some(RawValue::Enum(b)))
            | (Some(RawValue::Typed(a)), Some(RawValue::Typed(b)))
            | (Some(RawValue::Binary(a)), Some(RawValue::Binary(b))) => old
                .image()
                .strings
                .get(a)
                .eq_ignore_ascii_case(new.image().strings.get(b)),
            (Some(RawValue::Leaf(a)), Some(RawValue::Leaf(b))) => {
                old.schema().class(a).name == new.schema().class(b).name
            }
            (Some(RawValue::Real(a)), Some(RawValue::Real(b))) => {
                a == b || a.to_bits() == b.to_bits()
            }
            (Some(RawValue::Int(a)), Some(RawValue::Real(b)))
            | (Some(RawValue::Real(b)), Some(RawValue::Int(a))) => integer_equals_real(a, b),
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if !equivalent {
            return false;
        }
    }
    left.pos() == left_end && right.pos() == right_end
}

fn integer_equals_real(integer: i64, real: f64) -> bool {
    real.is_finite()
        && real.fract() == 0.0
        && real >= i64::MIN as f64
        && real < -(i64::MIN as f64)
        && real as i64 == integer
}

#[derive(Default)]
struct Dependencies {
    geometry: BTreeMap<u32, Vec<u32>>,
    metadata: BTreeMap<u32, Vec<u32>>,
    global: BTreeSet<u32>,
    edges: usize,
}

impl Dependencies {
    fn add_model(&mut self, model: &Model, limit: usize) -> bool {
        for entry in &model.image().index {
            let entity = model.entity_ref(entry.express_id);
            if global_entity(entity) {
                self.global.insert(entity.id());
            }
            let mut arguments = model.image().args_of(entry);
            let mut index = 0;
            while !arguments.is_empty() {
                let mut cursor = arguments;
                if !arguments.skip_value() {
                    return false;
                }
                let name = if entity.is_complex() {
                    None
                } else {
                    model.schema().attr_name(entity.class(), index)
                };
                while cursor.pos() < arguments.pos() {
                    if let Some(RawValue::Ref(target)) = cursor.read() {
                        self.reference(entity, name, target);
                        if self.edges > limit {
                            return false;
                        }
                    }
                }
                index += 1;
            }
        }
        true
    }

    fn reference(&mut self, entity: Entity<'_>, name: Option<&str>, target: u32) {
        let id = entity.id();
        self.metadata.entry(target).or_default().push(id);
        self.edges += 1;
        if entity.is_a("IfcRelationship") {
            if !matches!(name, Some("OwnerHistory")) {
                self.metadata.entry(id).or_default().push(target);
                self.edges += 1;
            }
            let (input, output) = if entity.is_a("IfcRelVoidsElement") {
                (
                    Some("RelatedOpeningElement"),
                    Some("RelatingBuildingElement"),
                )
            } else if entity.is_a("IfcRelAssociatesMaterial") {
                (Some("RelatingMaterial"), Some("RelatedObjects"))
            } else if entity.is_a("IfcRelDefinesByType") {
                (Some("RelatingType"), Some("RelatedObjects"))
            } else {
                return;
            };
            if name == input {
                self.geometry.entry(target).or_default().push(id);
                self.edges += 1;
            } else if name == output {
                self.geometry.entry(id).or_default().push(target);
                self.edges += 1;
            }
            return;
        }
        if metadata_entity(entity) || metadata_attribute(entity, name) {
            return;
        }
        self.geometry.entry(target).or_default().push(id);
        self.edges += 1;
        let inverse = (entity.is_a("IfcStyledItem") && name == Some("Item"))
            || (entity.is_a("IfcIndexedColourMap") && name == Some("MappedTo"))
            || (entity.is_a("IfcMaterialDefinitionRepresentation")
                && name == Some("RepresentedMaterial"))
            || (entity.is_a("IfcGrid") && matches!(name, Some("UAxes" | "VAxes" | "WAxes")));
        if inverse {
            self.geometry.entry(id).or_default().push(target);
            self.edges += 1;
        }
    }

    fn finish(&mut self) {
        for users in self.geometry.values_mut().chain(self.metadata.values_mut()) {
            users.sort_unstable();
            users.dedup();
        }
    }
}

fn affected(
    graph: &BTreeMap<u32, Vec<u32>>,
    seeds: &BTreeSet<u32>,
    products: &BTreeSet<u32>,
    globals: &BTreeSet<u32>,
    stop_at_product: bool,
    limit: usize,
) -> Result<BTreeMap<u32, u32>, (u32, &'static str)> {
    if seeds.len() > limit {
        return Err((
            seeds.first().copied().unwrap_or(0),
            "dependency traversal budget exceeded",
        ));
    }
    let mut queue: VecDeque<_> = seeds.iter().map(|&id| (id, id)).collect();
    let mut visited = seeds.clone();
    let mut found = BTreeMap::new();
    while let Some((id, origin)) = queue.pop_front() {
        if visited.len() > limit {
            return Err((origin, "dependency traversal budget exceeded"));
        }
        if globals.contains(&id) {
            return Err((origin, "global interpretation changed"));
        }
        if products.contains(&id) {
            found.insert(id, origin);
            if stop_at_product {
                continue;
            }
        }
        if let Some(users) = graph.get(&id) {
            for &user in users {
                if visited.insert(user) {
                    if visited.len() > limit {
                        return Err((origin, "dependency traversal budget exceeded"));
                    }
                    queue.push_back((user, origin));
                }
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use tessifc_step::{ParseOptions, parse};

    fn model(records: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{records}\nENDSEC;\nEND-ISO-10303-21;"
        );
        Model::new(parse(source.as_bytes(), &ParseOptions::default()))
    }

    const WALLS: &str = "
        #1=IFCCARTESIANPOINT((0.,0.,0.));
        #2=IFCAXIS2PLACEMENT3D(#1,$,$);
        #3=IFCDIRECTION((0.,0.,1.));
        #4=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,0.3);
        #5=IFCEXTRUDEDAREASOLID(#4,#2,#3,3.);
        #6=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#5));
        #7=IFCPRODUCTDEFINITIONSHAPE($,$,(#6));
        #8=IFCLOCALPLACEMENT($,#2);
        #10=IFCWALL('wall-a',$,'A',$,$,#8,#7,$,$);
        #11=IFCWALL('wall-b',$,'B',$,$,#8,#7,$,$);
        #12=IFCWALL('independent',$,'C',$,$,$,$,$,$);
    ";

    fn opening(host: u32) -> String {
        format!(
            "#20=IFCOPENINGELEMENT('opening',$,$,$,$,#8,#7,$,$);\n#21=IFCRELVOIDSELEMENT('void',$,$,$,#{host},#20);"
        )
    }

    #[test]
    fn shared_profile_reaches_both_consumers_only() {
        let old = model(WALLS);
        let new = model(&WALLS.replace("4.,0.3", "5.,0.3"));
        let impact = compare_revisions(&old, &new);
        assert_eq!(impact.modified_entities, [4]);
        assert_eq!(impact.affected_products, [10, 11]);
        assert!(!impact.full_rebuild);
        assert_eq!(impact.reasons[0].entity_id, 4);
    }

    #[test]
    fn rename_is_metadata_only_and_serializes_the_contract() {
        let old = model(WALLS);
        let new = model(&WALLS.replace("'A'", "'Renamed'"));
        let impact = compare_revisions(&old, &new);
        assert_eq!(impact.modified_entities, [10]);
        assert!(impact.affected_products.is_empty());
        assert_eq!(impact.metadata_products, [10]);
        let json = serde_json::to_value(impact).unwrap();
        assert_eq!(json["metadataProducts"], serde_json::json!([10]));
        assert_eq!(json["reasons"][0]["expressId"], 10);
    }

    #[test]
    fn equivalent_rewrite_does_not_compare_string_arena_ids() {
        let old = model(
            "#1=IFCWALL('w',$,'Ren\\X2\\00E9\\X0\\',$,$,$,$,$,$);\n#2=IFCCARTESIANPOINT((1.,-0.,3.));",
        );
        let new =
            model("#2 = IFCCARTESIANPOINT((1E0,0,3));\n#1=IFCWALL('w',$,'René',$,$,$,$,$,$);");
        assert_eq!(compare_revisions(&old, &new), ChangeImpact::default());
    }

    #[test]
    fn integers_beyond_float_precision_are_not_rounded_away() {
        assert!(integer_equals_real(4, 4.));
        assert!(!integer_equals_real(
            9_007_199_254_740_993,
            9_007_199_254_740_992.
        ));
        assert!(!integer_equals_real(i64::MAX, -(i64::MIN as f64)));
    }

    #[test]
    fn first_opening_relationship_invalidates_its_host() {
        let old = model(WALLS);
        let new = model(&format!("{WALLS}{}", opening(10)));
        let impact = compare_revisions(&old, &new);
        assert_eq!(impact.created_entities, [20, 21]);
        assert_eq!(impact.affected_products, [10, 20]);
        assert!(!impact.full_rebuild);
    }

    #[test]
    fn deleted_opening_uses_the_previous_host_dependency() {
        let old = model(&format!("{WALLS}{}", opening(10)));
        let new = model(WALLS);
        let impact = compare_revisions(&old, &new);
        assert_eq!(impact.deleted_entities, [20, 21]);
        assert_eq!(impact.removed_products, [20]);
        assert_eq!(impact.affected_products, [10]);
    }

    #[test]
    fn reassigned_opening_invalidates_old_and_new_hosts() {
        let old = model(&format!("{WALLS}{}", opening(10)));
        let new = model(&format!("{WALLS}{}", opening(11)));
        let impact = compare_revisions(&old, &new);
        assert_eq!(impact.modified_entities, [21]);
        assert_eq!(impact.affected_products, [10, 11]);
    }

    #[test]
    fn nested_placements_reach_children_without_spatial_guessing() {
        let records = format!(
            "{}\n#30=IFCLOCALPLACEMENT(#8,#2);\n#31=IFCWALL('child',$,$,$,$,#30,#7,$,$);",
            WALLS.replace("#8=IFCLOCALPLACEMENT($,#2)", "#8=IFCLOCALPLACEMENT($,#32)")
        );
        let before = format!(
            "{records}\n#32=IFCAXIS2PLACEMENT3D(#33,$,$);\n#33=IFCCARTESIANPOINT((0.,0.,0.));"
        );
        let after = before.replace(
            "#33=IFCCARTESIANPOINT((0.,0.,0.))",
            "#33=IFCCARTESIANPOINT((2.,0.,0.))",
        );
        assert_eq!(
            compare_revisions(&model(&before), &model(&after)).affected_products,
            [10, 11, 31]
        );
    }

    #[test]
    fn created_and_deleted_products_are_explicit() {
        let old = model(WALLS);
        let new = model(&WALLS.replace(
            "#12=IFCWALL('independent',$,'C',$,$,$,$,$,$);",
            "#13=IFCWALL('new',$,'D',$,$,#8,#7,$,$);",
        ));
        let impact = compare_revisions(&old, &new);
        assert_eq!(impact.created_entities, [13]);
        assert_eq!(impact.deleted_entities, [12]);
        assert_eq!(impact.removed_products, [12]);
        assert_eq!(impact.affected_products, [13]);
    }

    #[test]
    fn renumbering_a_product_is_an_explicit_full_fallback() {
        let old = model(WALLS);
        let new = model(&WALLS.replace("#10=IFCWALL", "#100=IFCWALL"));
        let impact = compare_revisions(&old, &new);
        assert!(impact.full_rebuild);
        assert_eq!(impact.affected_products, [11, 12, 100]);
        assert_eq!(impact.removed_products, [10, 11, 12]);
    }

    #[test]
    fn cycles_terminate_and_budget_exhaustion_falls_back() {
        let old = model(&format!(
            "{WALLS}\n#40=UNKNOWN((#41));\n#41=UNKNOWN((#40),1.);"
        ));
        let new = model(&format!(
            "{WALLS}\n#40=UNKNOWN((#41));\n#41=UNKNOWN((#40),2.);"
        ));
        let impact = compare_revisions(&old, &new);
        assert!(impact.affected_products.is_empty());
        assert!(!impact.full_rebuild);
        assert!(compare_with_limits(&old, &new, 1, 10).full_rebuild);
        assert!(compare_with_limits(&old, &new, MAX_DEPENDENCY_EDGES, 1).full_rebuild);
    }

    #[test]
    fn unit_name_is_geometry_even_though_product_name_is_metadata() {
        let records = format!("{WALLS}\n#40=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);");
        let impact = compare_revisions(
            &model(&records),
            &model(&records.replace(".METRE.", ".FOOT.")),
        );
        assert!(impact.full_rebuild);
        assert_eq!(impact.affected_products, [10, 11, 12]);
    }

    #[test]
    fn adding_a_style_finds_previously_unstyled_geometry() {
        let before = model(WALLS);
        let after = model(&format!(
            "{WALLS}\n#40=IFCSTYLEDITEM(#5,(#41),$);\n#41=IFCSURFACESTYLE($,.BOTH.,(#42));\n#42=IFCSURFACESTYLESHADING(#43,$);\n#43=IFCCOLOURRGB($,1.,0.,0.);"
        ));
        assert_eq!(
            compare_revisions(&before, &after).affected_products,
            [10, 11]
        );
    }

    #[test]
    fn material_style_and_removed_association_reach_consumers() {
        let records = format!(
            "{WALLS}
            #40=IFCMATERIAL('M',$,$);
            #41=IFCRELASSOCIATESMATERIAL('mat',$,$,$,(#10),#40);
            #42=IFCMATERIALDEFINITIONREPRESENTATION($,$,(#43),#40);
            #43=IFCSTYLEDREPRESENTATION($,'Style','Material',(#44));
            #44=IFCSTYLEDITEM($,(#45),$);
            #45=IFCSURFACESTYLE($,.BOTH.,(#46));
            #46=IFCSURFACESTYLESHADING(#47,$);
            #47=IFCCOLOURRGB($,1.,0.,0.);"
        );
        let new = records.replace("IFCCOLOURRGB($,1.,0.,0.)", "IFCCOLOURRGB($,0.,1.,0.)");
        assert_eq!(
            compare_revisions(&model(&records), &model(&new)).affected_products,
            [10]
        );
        let without = records.replace("#41=IFCRELASSOCIATESMATERIAL('mat',$,$,$,(#10),#40);", "");
        assert_eq!(
            compare_revisions(&model(&records), &model(&without)).affected_products,
            [10]
        );
    }

    #[test]
    fn nested_mapped_representations_propagate_shape_changes() {
        let records = format!(
            "{WALLS}
            #40=IFCREPRESENTATIONMAP(#2,#6);
            #41=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#1,1.,$);
            #42=IFCMAPPEDITEM(#40,#41);
            #43=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#42));
            #44=IFCREPRESENTATIONMAP(#2,#43);
            #45=IFCMAPPEDITEM(#44,#41);
            #46=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#45));
            #47=IFCPRODUCTDEFINITIONSHAPE($,$,(#46));
            #48=IFCWALL('mapped',$,$,$,$,#8,#47,$,$);"
        );
        let new = records.replace("4.,0.3", "5.,0.3");
        assert_eq!(
            compare_revisions(&model(&records), &model(&new)).affected_products,
            [10, 11, 48]
        );
    }

    #[test]
    fn grid_parent_placement_reaches_products_through_axis_membership() {
        let records = "
            #1=IFCCARTESIANPOINT((0.,0.,0.));
            #2=IFCAXIS2PLACEMENT3D(#1,$,$);
            #3=IFCLOCALPLACEMENT($,#2);
            #4=IFCGRIDAXIS('A',$,.T.);
            #5=IFCGRIDAXIS('B',$,.T.);
            #6=IFCGRID('grid',$,$,$,$,#3,$,(#4),(#5),$,$);
            #7=IFCVIRTUALGRIDINTERSECTION((#4,#5),(0.,0.));
            #8=IFCGRIDPLACEMENT(#7,$);
            #9=IFCCOLUMN('column',$,$,$,$,#8,$,$,$);
            #10=IFCWALL('other',$,$,$,$,$,$,$,$);
        ";
        let new = records.replace("(0.,0.,0.)", "(2.,0.,0.)");
        assert_eq!(
            compare_revisions(&model(records), &model(&new)).affected_products,
            [6, 9]
        );
    }

    #[test]
    fn adding_a_previously_missing_reference_invalidates_its_reader() {
        let records = WALLS.replace("#5=IFCEXTRUDEDAREASOLID(#4,#2,#3,3.);", "");
        assert_eq!(
            compare_revisions(&model(&records), &model(WALLS)).affected_products,
            [10, 11]
        );
    }

    #[test]
    fn complex_values_compare_decoded_names_and_nested_references() {
        let before = "#1=(IFCNAMEDUNIT(*)IFCSIUNIT(.LENGTHUNIT.,$,.METRE.));\n#2=UNKNOWN(IFCANY((#1,(#3))),1);";
        let same = "#2=UNKNOWN(ifcany((#1,(#3))),1.);\n#1=(IFCNAMEDUNIT(*)IFCSIUNIT(.LENGTHUNIT.,$,.METRE.));";
        assert_eq!(
            compare_revisions(&model(before), &model(same)),
            ChangeImpact::default()
        );
        let new = before.replace(".METRE.", ".FOOT.");
        assert!(compare_revisions(&model(before), &model(&new)).full_rebuild);
    }

    #[test]
    fn property_value_changes_refresh_inspection_without_meshing() {
        let before = format!(
            "{WALLS}\n#40=IFCPROPERTYSINGLEVALUE('Height',$,IFCLENGTHMEASURE(3.),$);\n#41=IFCPROPERTYSET('ps',$,'Pset_Test',$,(#40));\n#42=IFCRELDEFINESBYPROPERTIES('rel',$,$,$,(#10),#41);"
        );
        let after = before.replace("IFCLENGTHMEASURE(3.)", "IFCLENGTHMEASURE(4.)");
        let impact = compare_revisions(&model(&before), &model(&after));
        assert_eq!(impact.modified_entities, [40]);
        assert!(impact.affected_products.is_empty());
        assert_eq!(impact.metadata_products, [10]);
    }

    #[test]
    fn unrelated_products_do_not_fan_out_through_containment_or_material() {
        let records = format!(
            "{WALLS}\n#40=IFCMATERIAL('M',$,$);\n#41=IFCRELASSOCIATESMATERIAL('mat',$,$,$,(#10,#11,#12),#40);\n#42=IFCBUILDINGSTOREY('storey',$,$,$,$,$,$,$,.ELEMENT.,0.);\n#43=IFCRELCONTAINEDINSPATIALSTRUCTURE('contains',$,$,$,(#10,#11,#12),#42);"
        );
        let after = records.replace(
            "#10=IFCWALL('wall-a',$,'A',$,$,#8",
            "#10=IFCWALL('wall-a',$,'A',$,$,$",
        );
        assert_eq!(
            compare_revisions(&model(&records), &model(&after)).affected_products,
            [10]
        );
    }

    #[test]
    fn selective_replacement_matches_fresh_full_evaluation() {
        let old = model(WALLS);
        let new = model(&WALLS.replace("4.,0.3", "5.,0.3"));
        let engine = Engine::new();
        let mut scene: BTreeMap<_, _> = engine
            .evaluate(&old)
            .shapes
            .into_iter()
            .map(|shape| (shape.express_id, shape.mesh()))
            .collect();
        let impact = compare_revisions(&old, &new);
        let mut session = engine.session(&new);
        session.restrict(&impact.affected_products);
        let batch = session.next(&new, |_| false);
        for id in impact
            .removed_products
            .iter()
            .chain(&impact.affected_products)
        {
            scene.remove(id);
        }
        for shape in batch.shapes {
            scene.insert(shape.express_id, shape.mesh());
        }
        let expected = engine.evaluate(&new);
        assert_eq!(scene.len(), expected.shapes.len());
        for shape in expected.shapes {
            let actual = &scene[&shape.express_id];
            let mesh = shape.mesh();
            assert_eq!(actual.positions, mesh.positions);
            assert_eq!(actual.indices, mesh.indices);
        }
    }
}
