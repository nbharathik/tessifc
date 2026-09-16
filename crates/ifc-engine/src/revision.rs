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
    compare_with_limits(old, new, None, MAX_DEPENDENCY_EDGES, MAX_DEPENDENCY_VISITS)
}

/// [`compare_revisions`] with the two source files: a record whose bytes are
/// identical in both is unchanged without decoding it.
pub fn compare_revisions_with_sources(
    old: &Model,
    old_source: &[u8],
    new: &Model,
    new_source: &[u8],
) -> ChangeImpact {
    compare_with_limits(
        old,
        new,
        Some((old_source, new_source)),
        MAX_DEPENDENCY_EDGES,
        MAX_DEPENDENCY_VISITS,
    )
}

fn compare_with_limits(
    old: &Model,
    new: &Model,
    sources: Option<(&[u8], &[u8])>,
    edge_limit: usize,
    visit_limit: usize,
) -> ChangeImpact {
    compare_with_graph(old, new, sources, edge_limit, visit_limit, false)
}

/// The record's bytes in both files, when both sources are known and the spans are sound.
fn same_source_bytes(
    sources: Option<(&[u8], &[u8])>,
    before: &tessifc_step::image::IndexEntry,
    after: &tessifc_step::image::IndexEntry,
) -> bool {
    let Some((old_source, new_source)) = sources else {
        return false;
    };
    if before.source_len != after.source_len || before.class_id != after.class_id {
        return false;
    }
    fn span<'a>(source: &'a [u8], entry: &tessifc_step::image::IndexEntry) -> Option<&'a [u8]> {
        let start = entry.source_off as usize;
        let end = start.checked_add(entry.source_len as usize)?;
        source.get(start..end)
    }
    match (span(old_source, before), span(new_source, after)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn compare_with_graph(
    old: &Model,
    new: &Model,
    sources: Option<(&[u8], &[u8])>,
    edge_limit: usize,
    visit_limit: usize,
    whole_old_graph: bool,
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
                let unchanged = match (before.peek(), after.peek()) {
                    (Some(x), Some(y)) => same_source_bytes(sources, x, y),
                    _ => false,
                };
                if !unchanged && let Some(metadata_only) = record_change(old, new, a) {
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
    if product_identity_changed(old, new, &impact) {
        impact.removed_products = old_products.into_iter().collect();
        return full_impact(impact, &new_products, origin, "product identity changed");
    }

    // An unchanged entity references the same entities in both snapshots, so the
    // candidate graph already holds its edges; only the old versions of changed
    // and deleted entities add what the candidate no longer says.
    let mut kinds = Kinds::default();
    let mut graph = Dependencies::default();
    let old_changed: Vec<u32> = impact
        .deleted_entities
        .iter()
        .chain(&impact.modified_entities)
        .copied()
        .collect();
    let old_added = if whole_old_graph {
        graph.add_model(old, &mut kinds, edge_limit)
    } else {
        graph.add_entities(old, &old_changed, &mut kinds, edge_limit)
    };
    if !graph.add_model(new, &mut kinds, edge_limit) || !old_added {
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

/// An identity event is a product whose GlobalId changed in place, or a GlobalId that left
/// one Express ID and appeared at another. GlobalIds duplicated alike in both snapshots are
/// not events, so files that already carry duplicates still update selectively.
fn product_identity_changed(old: &Model, new: &Model, impact: &ChangeImpact) -> bool {
    fn product(model: &Model, id: u32) -> Option<Entity<'_>> {
        model.entity(id).filter(|e| e.is_a("IfcProduct"))
    }
    fn guid(entity: Entity<'_>) -> Option<String> {
        entity
            .attr("GlobalId")
            .as_string()
            .filter(|g| !g.is_empty())
    }
    let mut lost = BTreeSet::new();
    let mut gained = BTreeSet::new();
    for &id in &impact.deleted_entities {
        if let Some(g) = product(old, id).and_then(guid) {
            lost.insert(g);
        }
    }
    for &id in &impact.created_entities {
        if let Some(g) = product(new, id).and_then(guid) {
            gained.insert(g);
        }
    }
    for &id in &impact.modified_entities {
        match (product(old, id), product(new, id)) {
            (Some(before), Some(after)) => {
                if guid(before) != guid(after) {
                    return true;
                }
            }
            (Some(before), None) => {
                if let Some(g) = guid(before) {
                    lost.insert(g);
                }
            }
            (None, Some(after)) => {
                if let Some(g) = guid(after) {
                    gained.insert(g);
                }
            }
            (None, None) => {}
        }
    }
    lost.iter().any(|g| gained.contains(g))
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
    entity.is_a("IfcRoot") && metadata_attribute_name(name)
}

fn metadata_attribute_name(name: Option<&str>) -> bool {
    matches!(
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    None,
    Voids,
    Material,
    Type,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Inverse {
    None,
    StyledItem,
    ColourMap,
    MaterialRepresentation,
    Grid,
}

/// What the graph needs to know about an entity's class.
#[derive(Clone, Copy)]
struct ClassKind {
    global: bool,
    metadata: bool,
    relationship: bool,
    root: bool,
    relation: Relation,
    inverse: Inverse,
}

impl ClassKind {
    fn of(entity: Entity<'_>) -> Self {
        let relation = if entity.is_a("IfcRelVoidsElement") {
            Relation::Voids
        } else if entity.is_a("IfcRelAssociatesMaterial") {
            Relation::Material
        } else if entity.is_a("IfcRelDefinesByType") {
            Relation::Type
        } else {
            Relation::None
        };
        let inverse = if entity.is_a("IfcStyledItem") {
            Inverse::StyledItem
        } else if entity.is_a("IfcIndexedColourMap") {
            Inverse::ColourMap
        } else if entity.is_a("IfcMaterialDefinitionRepresentation") {
            Inverse::MaterialRepresentation
        } else if entity.is_a("IfcGrid") {
            Inverse::Grid
        } else {
            Inverse::None
        };
        ClassKind {
            global: global_entity(entity),
            metadata: metadata_entity(entity),
            relationship: entity.is_a("IfcRelationship"),
            root: entity.is_a("IfcRoot"),
            relation,
            inverse,
        }
    }
}

/// Class facts resolved once per class id rather than by name for every reference.
/// Both snapshots share one schema, so one cache serves both.
#[derive(Default)]
struct Kinds {
    cache: Vec<Option<ClassKind>>,
}

impl Kinds {
    fn of(&mut self, entity: Entity<'_>) -> ClassKind {
        if entity.is_complex() {
            return ClassKind::of(entity);
        }
        let index = usize::from(entity.class());
        if index >= self.cache.len() {
            self.cache.resize(index + 1, None);
        }
        match self.cache[index] {
            Some(kind) => kind,
            None => {
                let kind = ClassKind::of(entity);
                self.cache[index] = Some(kind);
                kind
            }
        }
    }
}

/// Directed edges `from -> to` packed as one integer each, sorted once by `finish`.
#[derive(Default)]
struct EdgeList {
    edges: Vec<u64>,
}

impl EdgeList {
    fn push(&mut self, from: u32, to: u32) {
        self.edges.push((u64::from(from) << 32) | u64::from(to));
    }

    fn finish(&mut self) {
        self.edges.sort_unstable();
        self.edges.dedup();
    }

    /// The entities `from` points at, after `finish`.
    fn users(&self, from: u32) -> impl Iterator<Item = u32> + '_ {
        let start = self
            .edges
            .partition_point(|&edge| edge >> 32 < u64::from(from));
        self.edges[start..]
            .iter()
            .take_while(move |&&edge| edge >> 32 == u64::from(from))
            .map(|&edge| edge as u32)
    }
}

#[derive(Default)]
struct Dependencies {
    geometry: EdgeList,
    metadata: EdgeList,
    global: BTreeSet<u32>,
    edges: usize,
}

impl Dependencies {
    fn add_model(&mut self, model: &Model, kinds: &mut Kinds, limit: usize) -> bool {
        for entry in &model.image().index {
            if !self.add_entry(model, entry, kinds, limit) {
                return false;
            }
        }
        true
    }

    fn add_entities(
        &mut self,
        model: &Model,
        ids: &[u32],
        kinds: &mut Kinds,
        limit: usize,
    ) -> bool {
        for &id in ids {
            if let Some(entry) = model.image().entry(id)
                && !self.add_entry(model, entry, kinds, limit)
            {
                return false;
            }
        }
        true
    }

    fn add_entry(
        &mut self,
        model: &Model,
        entry: &tessifc_step::image::IndexEntry,
        kinds: &mut Kinds,
        limit: usize,
    ) -> bool {
        let entity = model.entity_ref(entry.express_id);
        let kind = kinds.of(entity);
        if kind.global {
            self.global.insert(entity.id());
        }
        let complex = entity.is_complex();
        let class = entity.class();
        let mut arguments = model.image().args_of(entry);
        let mut index = 0;
        while !arguments.is_empty() {
            let mut cursor = arguments;
            if !arguments.skip_value() {
                return false;
            }
            let name = if complex {
                None
            } else {
                model.schema().attr_name(class, index)
            };
            while cursor.pos() < arguments.pos() {
                if let Some(RawValue::Ref(target)) = cursor.read() {
                    self.reference(kind, entity.id(), name, target);
                    if self.edges > limit {
                        return false;
                    }
                }
            }
            index += 1;
        }
        true
    }

    fn reference(&mut self, kind: ClassKind, id: u32, name: Option<&str>, target: u32) {
        self.metadata.push(target, id);
        self.edges += 1;
        if kind.relationship {
            if !matches!(name, Some("OwnerHistory")) {
                self.metadata.push(id, target);
                self.edges += 1;
            }
            let (input, output) = match kind.relation {
                Relation::Voids => (
                    Some("RelatedOpeningElement"),
                    Some("RelatingBuildingElement"),
                ),
                Relation::Material => (Some("RelatingMaterial"), Some("RelatedObjects")),
                Relation::Type => (Some("RelatingType"), Some("RelatedObjects")),
                Relation::None => return,
            };
            if name == input {
                self.geometry.push(target, id);
                self.edges += 1;
            } else if name == output {
                self.geometry.push(id, target);
                self.edges += 1;
            }
            return;
        }
        if kind.metadata || (kind.root && metadata_attribute_name(name)) {
            return;
        }
        self.geometry.push(target, id);
        self.edges += 1;
        let inverse = match kind.inverse {
            Inverse::StyledItem => name == Some("Item"),
            Inverse::ColourMap => name == Some("MappedTo"),
            Inverse::MaterialRepresentation => name == Some("RepresentedMaterial"),
            Inverse::Grid => matches!(name, Some("UAxes" | "VAxes" | "WAxes")),
            Inverse::None => false,
        };
        if inverse {
            self.geometry.push(id, target);
            self.edges += 1;
        }
    }

    fn finish(&mut self) {
        self.geometry.finish();
        self.metadata.finish();
    }
}

fn affected(
    graph: &EdgeList,
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
        for user in graph.users(id) {
            if visited.insert(user) {
                if visited.len() > limit {
                    return Err((origin, "dependency traversal budget exceeded"));
                }
                queue.push_back((user, origin));
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

    fn source(records: &str) -> Vec<u8> {
        format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{records}\nENDSEC;\nEND-ISO-10303-21;"
        )
        .into_bytes()
    }

    fn model(records: &str) -> Model {
        Model::new(parse(&source(records), &ParseOptions::default()))
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

    const DUPLICATED: &str = "
        #1=IFCCARTESIANPOINT((0.,0.,0.));
        #2=IFCAXIS2PLACEMENT3D(#1,$,$);
        #3=IFCDIRECTION((0.,0.,1.));
        #4=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,0.3);
        #5=IFCEXTRUDEDAREASOLID(#4,#2,#3,3.);
        #6=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#5));
        #7=IFCPRODUCTDEFINITIONSHAPE($,$,(#6));
        #8=IFCLOCALPLACEMENT($,#2);
        #10=IFCWALL('wall-a',$,'A',$,$,#8,#7,$,$);
        #11=IFCWALL('wall-a',$,'B',$,$,#8,#7,$,$);
        #12=IFCWALL('independent',$,'C',$,$,$,$,$,$);
    ";

    #[test]
    fn duplicated_guids_kept_in_place_update_selectively() {
        let old = model(DUPLICATED);
        let renamed = compare_revisions(&old, &model(&DUPLICATED.replace("'A'", "'Renamed'")));
        assert!(!renamed.full_rebuild);
        assert!(renamed.affected_products.is_empty());
        assert_eq!(renamed.metadata_products, [10]);

        let resized = compare_revisions(&old, &model(&DUPLICATED.replace("4.,0.3", "5.,0.3")));
        assert!(!resized.full_rebuild);
        assert_eq!(resized.affected_products, [10, 11]);

        let deleted = compare_revisions(
            &old,
            &model(&DUPLICATED.replace("#11=IFCWALL('wall-a',$,'B',$,$,#8,#7,$,$);", "")),
        );
        assert!(!deleted.full_rebuild);
        assert_eq!(deleted.removed_products, [11]);
        assert!(deleted.affected_products.is_empty());

        let created = compare_revisions(
            &old,
            &model(&format!(
                "{DUPLICATED}\n#13=IFCWALL('wall-a',$,'D',$,$,#8,#7,$,$);"
            )),
        );
        assert!(!created.full_rebuild);
        assert_eq!(created.affected_products, [13]);
    }

    #[test]
    fn a_changed_or_swapped_guid_is_an_identity_event() {
        let old = model(WALLS);
        let changed = compare_revisions(&old, &model(&WALLS.replace("'wall-a'", "'other'")));
        assert!(changed.full_rebuild);
        assert_eq!(changed.removed_products, [10, 11, 12]);

        let swapped = model(
            &WALLS
                .replace("#10=IFCWALL('wall-a'", "#10=IFCWALL('wall-b'")
                .replace("#11=IFCWALL('wall-b'", "#11=IFCWALL('wall-a'"),
        );
        assert!(compare_revisions(&old, &swapped).full_rebuild);
    }

    #[test]
    fn the_changed_entity_graph_matches_the_whole_old_graph() {
        let pairs: Vec<(String, String)> = vec![
            (WALLS.into(), WALLS.replace("4.,0.3", "5.,0.3")),
            (WALLS.into(), WALLS.replace("'A'", "'Renamed'")),
            (WALLS.into(), format!("{WALLS}{}", opening(10))),
            (format!("{WALLS}{}", opening(10)), WALLS.into()),
            (
                format!("{WALLS}{}", opening(10)),
                format!("{WALLS}{}", opening(11)),
            ),
            (
                WALLS.into(),
                WALLS.replace(
                    "#12=IFCWALL('independent',$,'C',$,$,$,$,$,$);",
                    "#13=IFCWALL('new',$,'D',$,$,#8,#7,$,$);",
                ),
            ),
            (
                WALLS.replace("#5=IFCEXTRUDEDAREASOLID(#4,#2,#3,3.);", ""),
                WALLS.into(),
            ),
            (DUPLICATED.into(), DUPLICATED.replace("4.,0.3", "5.,0.3")),
        ];
        for (before, after) in pairs {
            let (old, new) = (model(&before), model(&after));
            let sources = (source(&before), source(&after));
            let reduced = compare_with_graph(
                &old,
                &new,
                Some((&sources.0, &sources.1)),
                MAX_DEPENDENCY_EDGES,
                MAX_DEPENDENCY_VISITS,
                false,
            );
            let whole = compare_with_graph(
                &old,
                &new,
                None,
                MAX_DEPENDENCY_EDGES,
                MAX_DEPENDENCY_VISITS,
                true,
            );
            assert_eq!(reduced, whole, "{before} -> {after}");
        }
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
        assert!(compare_with_limits(&old, &new, None, 1, 10).full_rebuild);
        assert!(compare_with_limits(&old, &new, None, MAX_DEPENDENCY_EDGES, 1).full_rebuild);
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
