// SPDX-License-Identifier: Apache-2.0
//! From a product to a mesh: which representation, and where it goes.
//!
//! `Body` is the solid; `Axis`, `FootPrint` and `Box` are not what a viewer wants.

use crate::context::EvalCtx;
use crate::error::codes;
use crate::placement::axis2_placement_3d;
use crate::registry::Registry;
use crate::style::Rgba;
use glam::{DMat4, DVec3};
use std::sync::Arc;
use tessifc_mesh::Mesh64;
use tessifc_model::{Entity, Model, Relation};

/// Upper bound on the cutters one representation part deduplicates.
const MAX_DEDUPED_CUTTERS: usize = 256;

/// Representation identifiers we will draw, best first.
///
/// `Box` is a last resort: a crate where a chair should be beats nothing.
const PREFERRED: [&str; 4] = ["Body", "Facetation", "Body-FallBack", "Box"];

/// Where one drawn mesh came from.
///
/// The specification asks every instance to say which representation and item
/// produced it, which evaluator ran, what was approximated and what happened to
/// its booleans. All of it is known while the product is being evaluated and
/// was, until now, thrown away.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    /// The `IfcShapeRepresentation` this came from.
    pub representation: u32,
    /// The representation item, which is the entity the evaluator was given.
    pub item: u32,
    /// The item's IFC class, which is the name the registry dispatches on and
    /// the vocabulary `tessifc coverage` publishes.
    pub evaluator: String,
    /// The boolean outcome for this part.
    pub boolean: BooleanStatus,
}

/// What became of a part's openings and boolean operators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BooleanStatus {
    /// Nothing was subtracted from it.
    #[default]
    None,
    /// Every cutter was subtracted exactly.
    Exact,
    /// Cut through the faces of a body with no usable inside.
    Surface,
    /// The cut was refused and the body is drawn whole.
    Refused,
}

impl BooleanStatus {
    /// Combine parts without losing a refusal or surface-only result.
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Refused, _) | (_, Self::Refused) => Self::Refused,
            (Self::Surface, _) | (_, Self::Surface) => Self::Surface,
            (Self::Exact, _) | (_, Self::Exact) => Self::Exact,
            _ => Self::None,
        }
    }

    /// The stable name a pack writes.
    pub fn name(self) -> &'static str {
        match self {
            BooleanStatus::None => "none",
            BooleanStatus::Exact => "exact",
            BooleanStatus::Surface => "surface",
            BooleanStatus::Refused => "refused",
        }
    }
}

/// Where a part came from: its representation, its item, and the item's class.
fn origin(ctx: &EvalCtx<'_>, representation: &Representation<'_>, item: Entity<'_>) -> Provenance {
    Provenance {
        representation: representation.entity.id(),
        item: item.id(),
        evaluator: item.class_name().to_string(),
        boolean: ctx.boolean_outcome(item.id()),
    }
}

/// A chosen representation and how good a choice it was.
#[derive(Clone, Debug)]
pub struct Representation<'a> {
    /// The `IfcShapeRepresentation`.
    pub entity: Entity<'a>,
    /// Its `RepresentationIdentifier`, for provenance.
    pub identifier: String,
    /// Its `RepresentationType`.
    pub kind: String,
    /// Index into [`PREFERRED`]; lower is better.
    pub rank: usize,
}

/// Pick the representation to draw for a product.
///
/// A product with only an `Axis` gets nothing: a centre line is not a wall.
pub fn representation_of<'a>(product: Entity<'a>) -> Option<Representation<'a>> {
    let definition = product.attr("Representation").as_entity()?;
    let list = definition.attr("Representations").as_list()?;

    let mut best: Option<Representation<'a>> = None;
    for value in list {
        let Some(shape) = value.as_entity() else {
            continue;
        };
        let identifier = shape
            .attr("RepresentationIdentifier")
            .as_string()
            .unwrap_or_default();
        let Some(rank) = PREFERRED
            .iter()
            .position(|name| name.eq_ignore_ascii_case(&identifier))
        else {
            continue;
        };
        if best
            .as_ref()
            .map(|current| rank < current.rank)
            .unwrap_or(true)
        {
            best = Some(Representation {
                entity: shape,
                identifier,
                kind: shape
                    .attr("RepresentationType")
                    .as_string()
                    .unwrap_or_default(),
                rank,
            });
        }
    }
    best
}

/// A product's role in a rendered building model.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProductCategory {
    /// Physical building or site geometry.
    Physical,
    /// A space, spatial zone or external spatial volume.
    Space,
    /// An opening or another subtractive feature.
    Opening,
    /// An annotation or grid.
    Annotation,
    /// Non-physical reference geometry such as a port or structural analysis item.
    Reference,
}

impl ProductCategory {
    /// Stable lower-case spelling used by bindings and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            ProductCategory::Physical => "physical",
            ProductCategory::Space => "space",
            ProductCategory::Opening => "opening",
            ProductCategory::Annotation => "annotation",
            ProductCategory::Reference => "reference",
        }
    }
}

/// Classify geometry that describes the model rather than a physical object.
pub fn product_category(product: Entity<'_>) -> ProductCategory {
    if product.is_a("IfcOpeningElement") || product.is_a("IfcVoidingFeature") {
        return ProductCategory::Opening;
    }
    if product.is_a("IfcSpace")
        || product.is_a("IfcSpatialZone")
        || product.is_a("IfcExternalSpatialElement")
    {
        return ProductCategory::Space;
    }
    if product.is_a("IfcAnnotation") || product.is_a("IfcGrid") {
        return ProductCategory::Annotation;
    }
    if product.is_a("IfcVirtualElement")
        || product.is_a("IfcStructuralItem")
        || product.is_a("IfcStructuralActivity")
        || product.is_a("IfcDistributionPort")
        || product.is_a("IfcPositioningElement")
    {
        return ProductCategory::Reference;
    }
    ProductCategory::Physical
}

/// Should this product be drawn at all, given the settings?
pub fn should_include(ctx: &EvalCtx<'_>, product: Entity<'_>) -> bool {
    match product_category(product) {
        ProductCategory::Physical => true,
        ProductCategory::Space => ctx.settings.include_spaces,
        ProductCategory::Opening => ctx.settings.include_openings,
        ProductCategory::Annotation => ctx.settings.include_annotations,
        ProductCategory::Reference => ctx.settings.include_references,
    }
}

/// Identifies a shared mesh: one set of items of one mapped representation.
///
/// Lets a packer write the mesh once and the placements many times.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SharedKey {
    /// The `IfcShapeRepresentation` an `IfcRepresentationMap` points at.
    pub representation: u32,
    /// Which of its items make up this mesh, one bit per item index.
    pub items: u64,
}

/// Where a part's triangles are.
#[derive(Clone, Debug)]
pub enum PartGeometry {
    /// A mesh in world coordinates and metres, unique to this product.
    Unique(Mesh64),
    /// A mesh shared with every use of the family, in source coordinates, placed by `transform`.
    Shared {
        /// What the mesh is, for a packer that writes each shared mesh once.
        key: SharedKey,
        /// The mesh, in source coordinates and metres.
        mesh: Arc<Mesh64>,
        /// Source coordinates to world coordinates.
        transform: DMat4,
    },
}

impl PartGeometry {
    /// The mesh as stored; pair it with [`PartGeometry::transform`].
    pub fn local_mesh(&self) -> &Mesh64 {
        match self {
            PartGeometry::Unique(mesh) => mesh,
            PartGeometry::Shared { mesh, .. } => mesh,
        }
    }

    /// The transform from [`PartGeometry::local_mesh`] to the world.
    pub fn transform(&self) -> DMat4 {
        match self {
            PartGeometry::Unique(_) => DMat4::IDENTITY,
            PartGeometry::Shared { transform, .. } => *transform,
        }
    }

    /// The shared key, if this part is shared.
    pub fn shared_key(&self) -> Option<SharedKey> {
        match self {
            PartGeometry::Unique(_) => None,
            PartGeometry::Shared { key, .. } => Some(*key),
        }
    }

    /// The mesh in world coordinates. A copy, for a shared part.
    pub fn world_mesh(&self) -> Mesh64 {
        match self {
            PartGeometry::Unique(mesh) => mesh.clone(),
            PartGeometry::Shared {
                mesh, transform, ..
            } => {
                let mut placed = (**mesh).clone();
                placed.transform(transform);
                placed
            }
        }
    }

    /// The mesh in world coordinates, moving a unique mesh out without a copy.
    pub fn into_world_mesh(self) -> Mesh64 {
        match self {
            PartGeometry::Unique(mesh) => mesh,
            shared => shared.world_mesh(),
        }
    }

    /// Triangles in the part.
    pub fn triangle_count(&self) -> usize {
        self.local_mesh().triangle_count()
    }

    /// True when there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.local_mesh().is_empty()
    }

    /// Axis-aligned bounds in world coordinates.
    pub fn bounds(&self) -> Option<(DVec3, DVec3)> {
        match self {
            PartGeometry::Unique(mesh) => mesh.bounds(),
            PartGeometry::Shared {
                mesh, transform, ..
            } => {
                let mut lo = DVec3::splat(f64::INFINITY);
                let mut hi = DVec3::splat(f64::NEG_INFINITY);
                for point in &mesh.positions {
                    let placed = transform.transform_point3(*point);
                    lo = lo.min(placed);
                    hi = hi.max(placed);
                }
                (lo.x <= hi.x).then_some((lo, hi))
            }
        }
    }
}

/// One drawable piece of a product: the geometry that shares one colour.
///
/// A window is two of these, so the pane can be transparent and the frame solid.
pub struct ProductPart {
    /// The triangles, unique to the product or shared with its family.
    pub geometry: PartGeometry,
    /// The colour every triangle of it is drawn in.
    pub color: Rgba,
    /// Where it came from and what happened to it.
    pub provenance: Provenance,
}

impl ProductPart {
    /// The mesh in world coordinates and metres.
    pub fn world_mesh(&self) -> Mesh64 {
        self.geometry.world_mesh()
    }
}

/// Evaluate a product's body into a world-space mesh, with its openings cut out.
///
/// Colours are dropped; use [`product_parts`] to keep them.
pub fn product_mesh(
    ctx: &EvalCtx<'_>,
    registry: &Registry,
    model: &Model,
    product: Entity<'_>,
) -> Option<Mesh64> {
    let mut mesh = Mesh64::new();
    for part in product_parts(ctx, registry, model, product)? {
        mesh.append(&part.geometry.into_world_mesh());
    }
    Some(mesh)
}

/// Shared items of one family use that landed in one colour.
struct SharedGroup {
    representation: u32,
    placement: DMat4,
    items: Vec<(u32, Arc<Mesh64>)>,
}

/// Evaluate a product's body into meshes, one per colour, with its openings cut out.
///
/// Uncut family geometry comes back [`PartGeometry::Shared`]; `None` means nothing to draw.
pub fn product_parts(
    ctx: &EvalCtx<'_>,
    registry: &Registry,
    model: &Model,
    product: Entity<'_>,
) -> Option<Vec<ProductPart>> {
    ctx.clear_boolean_cache();
    let mut parts = product_body_parts(ctx, registry, model, product, true)?;
    let openings = if ctx.settings.cut_openings {
        opening_parts(ctx, registry, model, product)
    } else {
        Vec::new()
    };

    // A shared mesh cannot be cut, so with openings the family geometry is copied first.
    if !openings.is_empty() {
        for part in &mut parts {
            if let BodyGeometry::Shared { .. } = part.geometry {
                let world =
                    std::mem::replace(&mut part.geometry, BodyGeometry::Unique(Mesh64::new()))
                        .into_world_mesh();
                part.geometry = BodyGeometry::Unique(world);
            }
        }
    }

    // One item at a time: cutting each convex body is exact where their union would be refused.
    let fallback = crate::style::fallback_colour(ctx, product);
    let mut colours: Vec<Rgba> = Vec::new();
    let mut meshes: Vec<Mesh64> = Vec::new();
    // One provenance per colour slot, from the first part that filled it.
    let mut origins: Vec<Provenance> = Vec::new();
    // Shared groups per colour slot, keyed by family use, in first-seen order.
    let mut shared: Vec<Vec<(u32, SharedGroup)>> = Vec::new();
    let mut refused = 0;
    let mut refusal_reasons: Vec<String> = Vec::new();
    let mut all_cutters = Vec::new();
    let mut all_planes: Vec<Option<Vec<tessifc_mesh::Plane>>> = Vec::new();
    for part in parts {
        // One mesh per colour in first-seen order; a linear search keeps this deterministic.
        let colour = part.colour.unwrap_or(fallback);
        let slot = match colours.iter().enumerate().position(|(index, known)| {
            *known == colour
                && origins[index].representation == part.provenance.representation
                && origins[index].item == part.provenance.item
        }) {
            Some(index) => {
                origins[index].boolean = origins[index].boolean.merge(part.provenance.boolean);
                index
            }
            None => {
                colours.push(colour);
                meshes.push(Mesh64::new());
                origins.push(part.provenance.clone());
                shared.push(Vec::new());
                colours.len() - 1
            }
        };
        let body = match part.geometry {
            BodyGeometry::Unique(mesh) => mesh,
            BodyGeometry::Shared {
                representation,
                item,
                use_index,
                mesh,
                placement,
            } => {
                let groups = &mut shared[slot];
                match groups.iter_mut().find(|(index, _)| *index == use_index) {
                    Some((_, group)) => group.items.push((item, mesh)),
                    None => groups.push((
                        use_index,
                        SharedGroup {
                            representation,
                            placement,
                            items: vec![(item, mesh)],
                        },
                    )),
                }
                continue;
            }
        };
        let mesh = &mut meshes[slot];
        let cutters: &[Mesh64] = if openings.is_empty() {
            &part.cutters
        } else {
            all_cutters.clear();
            all_planes.clear();
            // The dedup is quadratic and the cutter count is file-controlled, so cap it.
            let mut dedup = true;
            for cutter in part.cutters.iter().chain(&openings) {
                if all_cutters.len() >= MAX_DEDUPED_CUTTERS {
                    dedup = false;
                }
                if !dedup {
                    all_cutters.push(cutter.clone());
                    all_planes.push(None);
                    continue;
                }
                // Planes are found once per cutter; every containment test reuses them.
                let planes = convex_planes(cutter, ctx.tol.len);
                let enclosed = all_planes.iter().any(|known| {
                    known
                        .as_ref()
                        .is_some_and(|planes| encloses(planes, cutter, ctx.tol.len))
                });
                if !enclosed {
                    // A cutter inside another adds nothing but coincident faces.
                    let mut index = 0;
                    while index < all_cutters.len() {
                        let inside = planes.as_ref().is_some_and(|planes| {
                            encloses(planes, &all_cutters[index], ctx.tol.len)
                        });
                        if inside {
                            all_cutters.remove(index);
                            all_planes.remove(index);
                        } else {
                            index += 1;
                        }
                    }
                    all_cutters.push(cutter.clone());
                    all_planes.push(planes);
                }
            }
            &all_cutters
        };
        if cutters.is_empty() {
            mesh.append(&body);
            continue;
        }
        origins[slot].boolean = origins[slot].boolean.merge(BooleanStatus::Exact);
        let watch = crate::context::Stopwatch::start();
        let result = tessifc_mesh::difference_convex_many(&body, cutters, ctx.tol.len)
            .or_else(|| tessifc_mesh::difference_extrusion_many(&body, cutters, ctx.tol.len))
            .or_else(|| {
                match tessifc_mesh::difference_prismatic_many_or_reason(&body, cutters, ctx.tol.len)
                {
                    Ok(result) => Some(result),
                    Err(why) => {
                        // A body with no usable inside, whether a face-based cabinet
                        // or a shell whose cells cannot be proved, still gets its
                        // openings cut through its faces.
                        if why.starts_with("body:")
                            && let Ok(result) = tessifc_mesh::difference_surface_many_or_reason(
                                &body,
                                cutters,
                                ctx.tol.len,
                            )
                        {
                            ctx.diag.warn(
                                codes::OPENING_CUT_ON_SURFACE,
                                product.id(),
                                format!("openings cut through the faces only ({why})"),
                            );
                            origins[slot].boolean =
                                origins[slot].boolean.merge(BooleanStatus::Surface);
                            return Some(result);
                        }
                        if !refusal_reasons.contains(&why) {
                            refusal_reasons.push(why);
                        }
                        None
                    }
                }
            });
        ctx.time.add_openings(watch.ms());
        match result {
            Some(result) if result.is_empty() && !body.is_empty() => {
                // Openings that swallow the body leave nothing to draw; say so
                // rather than let the product disappear.
                ctx.diag.warn(
                    codes::DEGENERATE_GEOMETRY,
                    product.id(),
                    "the openings and cutters removed the whole body",
                );
            }
            Some(result) => mesh.append(&result),
            None => {
                mesh.append(&body);
                origins[slot].boolean = BooleanStatus::Refused;
                refused += 1;
            }
        }
    }

    for colour in &colours {
        if colour.0[3] == 0 {
            // Transparency 1.0 is what several exporters write for glass; say so.
            ctx.diag.warn(
                codes::FULLY_TRANSPARENT_STYLE,
                product.id(),
                "the file styles part of this product as fully transparent",
            );
        }
    }

    if refused > 0 {
        let reason = if refusal_reasons.is_empty() {
            format!("{refused} body solid(s) could not be cut")
        } else {
            refusal_reasons.join("; ")
        };
        ctx.diag.warn(
            codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE,
            product.id(),
            format!("openings not cut ({reason}); body emitted un-cut"),
        );
    }

    let mut out = Vec::with_capacity(colours.len());
    for (slot, colour) in colours.into_iter().enumerate() {
        let mesh = std::mem::take(&mut meshes[slot]);
        let groups = std::mem::take(&mut shared[slot]);
        // The family items that did not fit a shared mesh join the unique one.
        let mut unique = mesh;
        let mut shared_parts = Vec::new();
        for (_, group) in groups {
            match shared_geometry(ctx, &group) {
                Some(geometry) => shared_parts.push(ProductPart {
                    geometry,
                    color: colour,
                    provenance: origins[slot].clone(),
                }),
                None => {
                    for (_, item) in &group.items {
                        let mut placed = (**item).clone();
                        placed.transform(&group.placement);
                        unique.append(&placed);
                    }
                }
            }
        }
        if !unique.is_empty() {
            out.push(ProductPart {
                geometry: PartGeometry::Unique(unique),
                color: colour,
                provenance: origins[slot].clone(),
            });
        }
        out.extend(shared_parts);
    }
    // A coordinate that f32 cannot hold would vanish in the pack; say so instead.
    let representable = |part: &ProductPart| {
        part.geometry.bounds().is_some_and(|(low, high)| {
            low.is_finite()
                && high.is_finite()
                && low.abs().max_element() < 1e30
                && high.abs().max_element() < 1e30
        })
    };
    let before = out.len();
    out.retain(representable);
    if out.len() < before {
        ctx.diag.warn(
            codes::DEGENERATE_GEOMETRY,
            product.id(),
            format!(
                "{} part(s) with coordinates beyond the representable range were dropped",
                before - out.len()
            ),
        );
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// The shared mesh for a group of same-colour family items, or `None` to bake it instead.
fn shared_geometry(ctx: &EvalCtx<'_>, group: &SharedGroup) -> Option<PartGeometry> {
    // A mirroring placement is baked, so the mesh keeps a winding a renderer can light.
    if group.placement.determinant() < 0.0 {
        return None;
    }
    // One bit per item; a family with more items than bits is baked.
    if group.items.iter().any(|(item, _)| *item >= 64) {
        return None;
    }
    let items = group
        .items
        .iter()
        .fold(0u64, |mask, (item, _)| mask | (1 << item));
    let key = SharedKey {
        representation: group.representation,
        items,
    };
    let mesh = match group.items.as_slice() {
        [(_, only)] => only.clone(),
        many => ctx.merged_mapped(group.representation, items, || {
            let mut merged = Mesh64::new();
            let mut ordered: Vec<&(u32, Arc<Mesh64>)> = many.iter().collect();
            ordered.sort_by_key(|(item, _)| *item);
            for (_, item) in ordered {
                merged.append(item);
            }
            merged
        }),
    };
    Some(PartGeometry::Shared {
        key,
        mesh,
        transform: group.placement,
    })
}

/// The face planes of a convex solid, or `None` when it is not one.
fn convex_planes(mesh: &Mesh64, tolerance: f64) -> Option<Vec<tessifc_mesh::Plane>> {
    let planes = tessifc_mesh::face_planes(mesh, tolerance);
    (planes.len() >= 4 && tessifc_mesh::is_convex(mesh, &planes, tolerance)).then_some(planes)
}

/// Whether the convex solid with these planes encloses all of `contained`.
///
/// Exporters write an opening twice; subtracting both leaves coincident fragments.
fn encloses(planes: &[tessifc_mesh::Plane], contained: &Mesh64, tolerance: f64) -> bool {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        1e-9
    };
    contained
        .positions
        .iter()
        .all(|point| planes.iter().all(|plane| plane.distance(*point) <= tol))
}

/// Every opening solid that voids `product`, in world space, one per item.
///
/// Per item, not per opening: two convex boxes cut where their union would not.
fn opening_parts(
    ctx: &EvalCtx<'_>,
    registry: &Registry,
    model: &Model,
    product: Entity<'_>,
) -> Vec<Mesh64> {
    let voids = model.inverse().get(Relation::Voids, product.id());
    let mut cutters = Vec::with_capacity(voids.len());
    for opening in voids {
        let Some(entity) = model.entity(*opening) else {
            continue;
        };
        // The opening's own body; its openings are not cut in turn, which avoids a cycle.
        if let Some(parts) = product_body_parts(ctx, registry, model, entity, false) {
            // An opening's own cutters would only enlarge it, so the body alone is taken.
            cutters.extend(
                parts
                    .into_iter()
                    .map(|part| part.geometry.into_world_mesh())
                    .filter(|mesh| !mesh.is_empty()),
            );
        }
    }
    cutters
}

/// One representation item: its solid and the cutters a difference chain still owes it.
struct BodyPart {
    geometry: BodyGeometry,
    cutters: Vec<Mesh64>,
    /// The style the item carries, before the product's own fallback.
    colour: Option<Rgba>,
    /// Where it came from, carried through to the pack.
    provenance: Provenance,
}

/// A body item's mesh before the product decides whether it can stay shared.
enum BodyGeometry {
    /// In product space until the placement is applied, then in the world.
    Unique(Mesh64),
    /// One item of a family, by reference, with the transform that places it.
    Shared {
        representation: u32,
        item: u32,
        /// Which `IfcMappedItem` this came from, so two uses in one product stay apart.
        use_index: u32,
        mesh: Arc<Mesh64>,
        placement: DMat4,
    },
}

impl BodyGeometry {
    fn into_world_mesh(self) -> Mesh64 {
        match self {
            BodyGeometry::Unique(mesh) => mesh,
            BodyGeometry::Shared {
                mesh, placement, ..
            } => {
                let mut placed = (*mesh).clone();
                placed.transform(&placement);
                placed
            }
        }
    }
}

/// The product's geometry in world space, one mesh per representation item.
///
/// With `split_colours`, a face set with a colour map yields one mesh per colour.
fn product_body_parts(
    ctx: &EvalCtx<'_>,
    registry: &Registry,
    model: &Model,
    product: Entity<'_>,
    split_colours: bool,
) -> Option<Vec<BodyPart>> {
    let Some(representation) = representation_of(product) else {
        // The product was selected and then produced nothing, because every
        // representation it carries is one this kernel does not draw: a
        // FootPrint, an Axis, a 2D annotation, a grid's axes. Saying so is the
        // difference between "not supported" and a product silently missing.
        if product.attr("Representation").as_entity().is_some() {
            ctx.diag.warn(
                codes::NO_DRAWN_REPRESENTATION,
                product.id(),
                format!(
                    "{} carries only representations this kernel does not draw,                      such as 2D annotation or grid axes",
                    product.class_name()
                ),
            );
        }
        return None;
    };
    let items = representation.entity.attr("Items").as_list()?;

    let mut parts: Vec<BodyPart> = Vec::new();
    let mut failures = 0;
    let mut attempts = 0;
    let mut use_index = 0;
    let mut seen_items = std::collections::HashSet::new();
    for value in items {
        let Some(item) = value.as_entity() else {
            continue;
        };
        // Items is a SET, but some exporters repeat an entity and double the surface.
        if !seen_items.insert(item.id()) {
            continue;
        }
        attempts += 1;
        // A difference chain is kept open so its cutters and the openings cut together,
        // which stays exact where cutting a notched body afterwards would be refused.
        if let Some(difference) = crate::eval::solids::evaluate_difference(ctx, registry, item) {
            for reason in &difference.refused {
                ctx.diag.warn(
                    codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE,
                    item.id(),
                    reason.clone(),
                );
            }
            if !difference.body.is_empty() {
                parts.push(BodyPart {
                    geometry: BodyGeometry::Unique(difference.body),
                    cutters: difference.cutters,
                    colour: crate::style::item_style(ctx, item),
                    provenance: origin(ctx, &representation, item),
                });
            }
            continue;
        }
        // A mapped item is opened up so each source item keeps its style; the source stays shared.
        if item.is_a("IfcMappedItem") {
            match crate::eval::solids::mapped_item_use(ctx, item) {
                Ok(mapped) => {
                    for (index, part) in mapped.parts.iter().enumerate() {
                        if part.mesh.is_empty() {
                            continue;
                        }
                        parts.push(BodyPart {
                            geometry: BodyGeometry::Shared {
                                representation: mapped.representation_id,
                                item: index as u32,
                                use_index,
                                mesh: part.mesh.clone(),
                                placement: mapped.placement,
                            },
                            cutters: Vec::new(),
                            colour: part.colour,
                            provenance: origin(ctx, &representation, item),
                        });
                    }
                    use_index += 1;
                }
                Err(error) => {
                    failures += 1;
                    ctx.diag.push(
                        tessifc_step::Diagnostic::warning(error.code(), 0, error.to_string())
                            .with_id(item.id()),
                    );
                }
            }
            continue;
        }
        // A face set with a colour map becomes one mesh per colour; a cutter is
        // needed whole, so openings are never split.
        if split_colours
            && item.is_a("IfcTessellatedFaceSet")
            && let Some(groups) = crate::eval::tessellated::coloured_parts(ctx, item)
        {
            let own = crate::style::item_style(ctx, item);
            for (colour, mesh) in groups {
                parts.push(BodyPart {
                    geometry: BodyGeometry::Unique(mesh),
                    cutters: Vec::new(),
                    colour: colour.or(own),
                    provenance: origin(ctx, &representation, item),
                });
            }
            continue;
        }
        match registry.solid(ctx, item) {
            Ok(part) => {
                if !part.is_empty() {
                    parts.push(BodyPart {
                        geometry: BodyGeometry::Unique(part),
                        cutters: Vec::new(),
                        colour: crate::style::item_style(ctx, item),
                        provenance: origin(ctx, &representation, item),
                    });
                }
            }
            Err(error) => {
                failures += 1;
                ctx.diag.push(
                    tessifc_step::Diagnostic::warning(error.code(), 0, error.to_string())
                        .with_id(item.id()),
                );
            }
        }
    }

    if parts.is_empty() {
        if attempts > 0 {
            // Every item failed, or every item came back empty: either way the
            // product is not on screen and the file has to be told.
            let reason = if failures == attempts {
                format!("none of its {attempts} items could be evaluated")
            } else {
                format!("its {attempts} item(s) evaluated to no geometry")
            };
            ctx.diag.warn(
                codes::NO_USABLE_REPRESENTATION,
                product.id(),
                format!(
                    "{} has a {} representation but {reason}",
                    product.class_name(),
                    representation.identifier
                ),
            );
        }
        return None;
    }

    // The product placement takes everything above from local space into the world.
    let world = product_transform(ctx, model, product);
    for part in &mut parts {
        match &mut part.geometry {
            BodyGeometry::Unique(mesh) => mesh.transform(&world),
            BodyGeometry::Shared { placement, .. } => *placement = world * *placement,
        }
        for cutter in &mut part.cutters {
            cutter.transform(&world);
        }
    }
    Some(parts)
}
/// The colour of a product, from its styles, its material, or its class.
///
/// Always returns something; see [`crate::style`].
pub fn product_colour(ctx: &EvalCtx<'_>, product: Entity<'_>) -> crate::style::Rgba {
    if let Some(representation) = representation_of(product)
        && let Some(items) = representation.entity.attr("Items").as_list()
    {
        for value in items {
            let Some(item) = value.as_entity() else {
                continue;
            };
            if let Some(colour) = crate::style::item_colour(ctx, item, product) {
                if colour.0[3] == 0 {
                    // Transparency 1.0 is what several exporters write for windows; say so.
                    ctx.diag.warn(
                        codes::FULLY_TRANSPARENT_STYLE,
                        product.id(),
                        "the file styles this product as fully transparent",
                    );
                }
                return colour;
            }
        }
    }
    crate::style::class_colour(&product.class_name())
}

/// A product's local-to-world transform, without evaluating any geometry.
///
/// A grid placement that cannot be resolved is reported and leaves the product
/// at its grid's origin.
pub fn product_transform(ctx: &EvalCtx<'_>, model: &Model, product: Entity<'_>) -> DMat4 {
    let Some(placement) = product.attr("ObjectPlacement").as_entity() else {
        return DMat4::IDENTITY;
    };
    let resolver = |grid_placement: Entity<'_>| match crate::grid::grid_placement(
        ctx,
        model,
        grid_placement,
    ) {
        Ok(resolved) => Some(resolved),
        Err(error) => {
            ctx.diag.warn(
                codes::PLACEMENT_UNSUPPORTED,
                product.id(),
                format!("IfcGridPlacement could not be resolved ({error}); the product sits at its grid's origin"),
            );
            None
        }
    };
    ctx.placements
        .borrow_mut()
        .world_with(model, placement, &ctx.units, &resolver)
}

/// Resolve an `IfcAxis2Placement3D` value, for callers building their own pipelines.
pub fn placement_matrix(value: tessifc_model::Value<'_>, units: &crate::units::Units) -> DMat4 {
    axis2_placement_3d(value, units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{DiagnosticSink, Settings, Tolerances};
    use crate::eval::tests::model_of;

    /// The containment test as the dedup loop applies it, for the tests below.
    fn convex_solid_contains(container: &Mesh64, contained: &Mesh64, tolerance: f64) -> bool {
        convex_planes(container, tolerance)
            .is_some_and(|planes| encloses(&planes, contained, tolerance))
    }
    use crate::units::Units;

    fn box_mesh(lo: DVec3, hi: DVec3) -> Mesh64 {
        let positions = vec![
            DVec3::new(lo.x, lo.y, lo.z),
            DVec3::new(hi.x, lo.y, lo.z),
            DVec3::new(hi.x, hi.y, lo.z),
            DVec3::new(lo.x, hi.y, lo.z),
            DVec3::new(lo.x, lo.y, hi.z),
            DVec3::new(hi.x, lo.y, hi.z),
            DVec3::new(hi.x, hi.y, hi.z),
            DVec3::new(lo.x, hi.y, hi.z),
        ];
        let indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        Mesh64 {
            positions,
            indices,
            closed: Some(true),
        }
    }

    /// A wall with a 2 x 3 x 4 box body, placed at (10, 0, 0).
    fn wall_model(extra_representations: &str) -> String {
        format!(
            "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,3.);\n\
             #2=IFCDIRECTION((0.,0.,1.));\n\
             #3=IFCEXTRUDEDAREASOLID(#1,$,#2,4.);\n\
             #4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
             #5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4{extra_representations}));\n\
             #6=IFCCARTESIANPOINT((10.,0.,0.));\n\
             #7=IFCAXIS2PLACEMENT3D(#6,$,$);\n\
             #8=IFCLOCALPLACEMENT($,#7);\n\
             #9=IFCWALL('guid',$,'W',$,$,#8,#5,$,$);\n"
        )
    }

    fn evaluate_parts(source: &str, id: u32) -> (Vec<ProductPart>, Vec<tessifc_step::Diagnostic>) {
        let model = model_of(source);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::from_model(&model),
            Tolerances::default(),
            &settings,
            &sink,
        );
        let registry = Registry::defaults(model.image().schema);
        let parts = product_parts(&ctx, &registry, &model, model.entity(id).unwrap());
        (parts.unwrap_or_default(), sink.take())
    }

    fn evaluate(source: &str, id: u32) -> (Option<Mesh64>, Vec<tessifc_step::Diagnostic>) {
        let model = model_of(source);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::from_model(&model),
            Tolerances::default(),
            &settings,
            &sink,
        );
        let registry = Registry::defaults(model.image().schema);
        let mesh = product_mesh(&ctx, &registry, &model, model.entity(id).unwrap());
        (mesh, sink.take())
    }

    #[test]
    fn a_refused_difference_keeps_its_provenance() {
        let source = boolean_body_wall("#12=IFCBOOLEANRESULT(.DIFFERENCE.,#7,$);\n");
        let (parts, diagnostics) = evaluate_parts(&source, 15);
        assert!(!parts.is_empty());
        assert_eq!(parts[0].provenance.boolean, BooleanStatus::Refused);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE)
        );
    }

    #[test]
    fn same_colour_items_keep_distinct_source_ids() {
        let source = wall_model("").replace("(#3));", "(#3,#30));")
            + "#30=IFCEXTRUDEDAREASOLID(#1,$,#2,1.);";
        let (parts, _) = evaluate_parts(&source, 9);
        let mut ids: Vec<_> = parts.iter().map(|part| part.provenance.item).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![3, 30]);
    }

    #[test]
    fn a_wall_becomes_a_placed_box() {
        let (mesh, _) = evaluate(&wall_model(""), 9);
        let mesh = mesh.expect("the wall should produce geometry");
        assert!((mesh.signed_volume() - 24.0).abs() < 1e-9);
        let (lo, hi) = mesh.bounds().unwrap();
        let centre = (lo + hi) * 0.5;
        assert!(
            (centre - glam::DVec3::new(10.0, 0.0, 2.0)).length() < 1e-9,
            "the placement must have been applied, got {centre}"
        );
    }

    #[test]
    fn repeated_body_items_do_not_leave_coincident_triangles() {
        let source = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,3.);\n\
                      #2=IFCDIRECTION((0.,0.,1.));\n\
                      #3=IFCEXTRUDEDAREASOLID(#1,$,#2,4.);\n\
                      #4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3,#3));\n\
                      #5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));\n\
                      #6=IFCWALL('guid',$,'W',$,$,$,#5,$,$);\n";
        let (mesh, diagnostics) = evaluate(source, 6);
        let mesh = mesh.expect("the repeated body should still produce geometry");

        assert_eq!(mesh.positions.len(), 8);
        assert_eq!(mesh.triangle_count(), 12);
        assert!((mesh.signed_volume() - 24.0).abs() < 1e-9);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
    }

    #[test]
    fn convex_cutter_containment_is_independent_of_triangulation_order() {
        let a = box_mesh(DVec3::ZERO, DVec3::ONE);
        let mut b = a.clone();
        b.indices.reverse();
        for triangle in b.indices.chunks_exact_mut(3) {
            triangle.swap(0, 1);
        }
        assert!(convex_solid_contains(&a, &b, 1e-9));
        assert!(convex_solid_contains(&b, &a, 1e-9));

        let inner = box_mesh(DVec3::splat(0.25), DVec3::splat(0.75));
        assert!(convex_solid_contains(&a, &inner, 1e-9));
        assert!(!convex_solid_contains(&inner, &a, 1e-9));

        let overlapping = box_mesh(DVec3::new(0.01, 0.0, 0.0), DVec3::new(1.01, 1.0, 1.0));
        assert!(!convex_solid_contains(&a, &overlapping, 1e-9));
        assert!(!convex_solid_contains(&overlapping, &a, 1e-9));
    }

    #[test]
    fn a_contained_boolean_cutter_and_relvoid_opening_cut_only_their_union() {
        let source = "#1=IFCCARTESIANPOINT((0.,0.));\n\
                      #2=IFCAXIS2PLACEMENT2D(#1,$);\n\
                      #3=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,4.,0.2);\n\
                      #4=IFCCARTESIANPOINT((0.,0.,0.));\n\
                      #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
                      #6=IFCDIRECTION((0.,0.,1.));\n\
                      #7=IFCEXTRUDEDAREASOLID(#3,#5,#6,3.);\n\
                      #8=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,1.,0.2);\n\
                      #9=IFCCARTESIANPOINT((0.,0.,0.5));\n\
                      #10=IFCAXIS2PLACEMENT3D(#9,$,$);\n\
                      #11=IFCEXTRUDEDAREASOLID(#8,#10,#6,2.);\n\
                      #12=IFCBOOLEANRESULT(.DIFFERENCE.,#7,#11);\n\
                      #13=IFCSHAPEREPRESENTATION($,'Body','CSG',(#12));\n\
                      #14=IFCPRODUCTDEFINITIONSHAPE($,$,(#13));\n\
                      #15=IFCWALL('wall',$,$,$,$,$,#14,$,$);\n\
                      #16=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,1.,1.);\n\
                      #17=IFCEXTRUDEDAREASOLID(#16,#10,#6,2.);\n\
                      #18=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#17));\n\
                      #19=IFCPRODUCTDEFINITIONSHAPE($,$,(#18));\n\
                      #20=IFCOPENINGELEMENT('opening',$,$,$,$,$,#19,$,$);\n\
                      #21=IFCRELVOIDSELEMENT('void',$,$,$,#15,#20);\n";
        let (mesh, diagnostics) = evaluate(source, 15);
        let mesh = mesh.expect("the wall should survive the nested opening encoding");
        assert!(
            (mesh.signed_volume() - 2.0).abs() < 1e-9,
            "the cutter union should remove 1 x 0.2 x 2, got {}",
            mesh.signed_volume()
        );
        assert!(mesh.is_edge_manifold(), "the result should remain closed");
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
    }

    /// A wall whose Body is the CSG item `#12`, with `#7` a 4 by 0.2 by 3 box.
    fn boolean_body_wall(boolean: &str) -> String {
        format!(
            "#1=IFCCARTESIANPOINT((0.,0.));\n\
             #2=IFCAXIS2PLACEMENT2D(#1,$);\n\
             #3=IFCRECTANGLEPROFILEDEF(.AREA.,$,#2,4.,0.2);\n\
             #4=IFCCARTESIANPOINT((0.,0.,0.));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n\
             #6=IFCDIRECTION((0.,0.,1.));\n\
             #7=IFCEXTRUDEDAREASOLID(#3,#5,#6,3.);\n\
             {boolean}\
             #13=IFCSHAPEREPRESENTATION($,'Body','CSG',(#12));\n\
             #14=IFCPRODUCTDEFINITIONSHAPE($,$,(#13));\n\
             #15=IFCWALL('wall',$,$,$,$,$,#14,$,$);\n"
        )
    }

    fn says_the_chain_is_cyclic(diagnostics: &[tessifc_step::Diagnostic]) -> bool {
        diagnostics.iter().any(|d| {
            d.code == codes::BOOLEAN_UNSUPPORTED_IN_CLIP_MODE && d.message.contains("cycle")
        })
    }

    #[test]
    fn a_boolean_whose_first_operand_is_itself_terminates() {
        let source = boolean_body_wall(
            "#12=IFCBOOLEANRESULT(.DIFFERENCE.,#12,$);\n\
             ",
        );
        let (_, diagnostics) = evaluate(&source, 15);
        assert!(
            says_the_chain_is_cyclic(&diagnostics),
            "the cycle has to be named, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_self_cutting_boolean_terminates_without_collecting_operands() {
        let source = boolean_body_wall(
            "#12=IFCBOOLEANRESULT(.DIFFERENCE.,#12,#7);\n\
             ",
        );
        let (_, diagnostics) = evaluate(&source, 15);
        assert!(
            says_the_chain_is_cyclic(&diagnostics),
            "the cycle has to be named, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_two_step_boolean_cycle_terminates() {
        let source = boolean_body_wall(
            "#12=IFCBOOLEANRESULT(.DIFFERENCE.,#16,$);\n\
             #16=IFCBOOLEANRESULT(.DIFFERENCE.,#12,$);\n\
             ",
        );
        let (_, diagnostics) = evaluate(&source, 15);
        assert!(
            says_the_chain_is_cyclic(&diagnostics),
            "the cycle has to be named, got {diagnostics:?}"
        );
    }

    #[test]
    fn body_wins_over_axis_and_footprint() {
        // An Axis representation listed first must not be chosen over Body.
        let source = format!(
            "#20=IFCCARTESIANPOINT((0.,0.,0.));\n#21=IFCCARTESIANPOINT((5.,0.,0.));\n\
             #22=IFCPOLYLINE((#20,#21));\n\
             #23=IFCSHAPEREPRESENTATION($,'Axis','Curve2D',(#22));\n{}",
            wall_model(",#23")
        );
        let model = model_of(&source);
        let chosen = representation_of(model.entity(9).unwrap()).unwrap();
        assert_eq!(chosen.identifier, "Body");
    }

    #[test]
    fn a_product_with_only_an_axis_draws_nothing() {
        let source = "#20=IFCCARTESIANPOINT((0.,0.,0.));\n#21=IFCCARTESIANPOINT((5.,0.,0.));\n\
                      #22=IFCPOLYLINE((#20,#21));\n\
                      #23=IFCSHAPEREPRESENTATION($,'Axis','Curve2D',(#22));\n\
                      #24=IFCPRODUCTDEFINITIONSHAPE($,$,(#23));\n\
                      #25=IFCWALL('g',$,'W',$,$,$,#24,$,$);\n";
        let (mesh, _) = evaluate(source, 25);
        assert!(mesh.is_none(), "a centre line is not a wall");
    }

    #[test]
    fn a_product_with_no_representation_draws_nothing() {
        let source = "#1=IFCWALL('g',$,'W',$,$,$,$,$,$);\n";
        let (mesh, diagnostics) = evaluate(source, 1);
        assert!(mesh.is_none());
        assert!(
            diagnostics.is_empty(),
            "having no geometry is not a complaint"
        );
    }

    #[test]
    fn an_unevaluable_body_is_reported() {
        // A class with no evaluator and no evaluable ancestor.
        let source = "#5=IFCCARTESIANPOINT((0.,0.,0.));\n\
                      #6=IFCAXIS2PLACEMENT3D(#5,$,$);\n\
                      #1=IFCSPHERICALSURFACE(#6,1.);\n\
                      #2=IFCSHAPEREPRESENTATION($,'Body','AdvancedSweptSolid',(#1));\n\
                      #3=IFCPRODUCTDEFINITIONSHAPE($,$,(#2));\n\
                      #4=IFCWALL('g',$,'W',$,$,$,#3,$,$);\n";
        let (mesh, diagnostics) = evaluate(source, 4);
        assert!(mesh.is_none());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::NO_USABLE_REPRESENTATION),
            "a body that failed entirely must be reported: {diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == codes::UNSUPPORTED_ITEM),
            "and the specific item too"
        );
    }

    #[test]
    fn openings_are_excluded_by_default() {
        let model = model_of("#1=IFCOPENINGSTANDARDCASE('g',$,$,$,$,$,$,$,$);\n");
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );
        assert!(
            !should_include(&ctx, model.entity(1).unwrap()),
            "drawing an opening fills in the hole it was cutting"
        );
        assert_eq!(
            product_category(model.entity(1).unwrap()),
            ProductCategory::Opening,
            "opening subclasses use the category of their schema parent"
        );

        let including = Settings {
            include_openings: true,
            ..Settings::default()
        };
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &including,
            &sink,
        );
        assert!(should_include(&ctx, model.entity(1).unwrap()));
    }

    #[test]
    fn helper_geometry_has_independent_categories() {
        let model = model_of(
            "#1=IFCSPACE('g',$,$,$,$,$,$,$,$,$);\n\
             #2=IFCVIRTUALELEMENT('g',$,$,$,$,$,$,$);\n\
             #3=IFCANNOTATION('g',$,$,$,$,$,$);\n",
        );
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &settings,
            &sink,
        );
        assert!(should_include(&ctx, model.entity(1).unwrap()));
        assert!(!should_include(&ctx, model.entity(2).unwrap()));
        assert!(!should_include(&ctx, model.entity(3).unwrap()));
        assert_eq!(
            product_category(model.entity(1).unwrap()),
            ProductCategory::Space
        );
        assert_eq!(
            product_category(model.entity(2).unwrap()),
            ProductCategory::Reference
        );
        assert_eq!(
            product_category(model.entity(3).unwrap()),
            ProductCategory::Annotation
        );

        let including = Settings {
            include_annotations: true,
            include_references: true,
            ..Settings::default()
        };
        let ctx = EvalCtx::new(
            &model,
            Units::default(),
            Tolerances::default(),
            &including,
            &sink,
        );
        assert!(should_include(&ctx, model.entity(2).unwrap()));
        assert!(should_include(&ctx, model.entity(3).unwrap()));
    }
    /// A window as a mapped family whose frame and pane are separate styled items.
    const MAPPED_WINDOW: &str = "#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,3.);
         #2=IFCDIRECTION((0.,0.,1.));
         #3=IFCEXTRUDEDAREASOLID(#1,$,#2,4.);
         #10=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,1.,1.);
         #11=IFCEXTRUDEDAREASOLID(#10,$,#2,1.);
         #12=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3,#11));
         #13=IFCCARTESIANPOINT((0.,0.,0.));
         #14=IFCAXIS2PLACEMENT3D(#13,$,$);
         #15=IFCREPRESENTATIONMAP(#14,#12);
         #16=IFCMAPPEDITEM(#15,$);
         #17=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#16));
         #18=IFCPRODUCTDEFINITIONSHAPE($,$,(#17));
         #20=IFCCOLOURRGB($,1.,1.,1.);
         #21=IFCSURFACESTYLERENDERING(#20,0.,$,$,$,$,$,$,.NOTDEFINED.);
         #22=IFCSURFACESTYLE('Window Frame',.BOTH.,(#21));
         #23=IFCSTYLEDITEM(#3,(#22),$);
         #24=IFCCOLOURRGB($,0.,0.5,0.75);
         #25=IFCSURFACESTYLERENDERING(#24,0.75,$,$,$,$,$,$,.NOTDEFINED.);
         #26=IFCSURFACESTYLE('Glass',.BOTH.,(#25));
         #27=IFCSTYLEDITEM(#11,(#26),$);
         #30=IFCWINDOW('guid',$,'W',$,$,$,#18,$,$,$,.WINDOW.,.NOTDEFINED.,$);
";

    #[test]
    fn a_window_keeps_its_frame_and_its_glass_apart() {
        let (parts, _) = evaluate_parts(MAPPED_WINDOW, 30);
        assert_eq!(
            parts.len(),
            2,
            "one colour per part: taking the first for the whole window paints              the glass in the frame's white and it stops being see-through"
        );
        assert_eq!(parts[0].color, Rgba([255, 255, 255, 255]), "the frame");
        assert_eq!(parts[1].color, Rgba([0, 128, 191, 64]), "the glass");
        assert!((parts[0].world_mesh().signed_volume() - 24.0).abs() < 1e-9);
        assert!((parts[1].world_mesh().signed_volume() - 1.0).abs() < 1e-9);
    }

    const COLOURED_CUBE: &str = "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.),(0.,0.,1.),(1.,0.,1.),(1.,1.,1.),(0.,1.,1.)));
         #2=IFCINDEXEDPOLYGONALFACE((5,6,7,8));
         #3=IFCINDEXEDPOLYGONALFACE((1,4,3,2));
         #4=IFCINDEXEDPOLYGONALFACE((1,2,6,5));
         #5=IFCINDEXEDPOLYGONALFACE((2,3,7,6));
         #6=IFCINDEXEDPOLYGONALFACE((3,4,8,7));
         #7=IFCINDEXEDPOLYGONALFACE((4,1,5,8));
         #8=IFCPOLYGONALFACESET(#1,.T.,(#2,#3,#4,#5,#6,#7),$);
         #9=IFCCOLOURRGBLIST(((0.8,0.1,0.1),(0.6,0.6,0.6)));
         #10=IFCINDEXEDCOLOURMAP(#8,$,#9,(1,2,2,2,2,2));
         #11=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#8));
         #12=IFCPRODUCTDEFINITIONSHAPE($,$,(#11));
         #13=IFCBUILDINGELEMENTPROXY('guid',$,'P',$,$,$,#12,$,$);
";

    #[test]
    fn a_colour_map_splits_a_face_set_by_colour() {
        let (parts, diagnostics) = evaluate_parts(COLOURED_CUBE, 13);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(parts.len(), 2, "the red top and the grey rest");
        assert_eq!(parts[0].color, Rgba([204, 26, 26, 255]));
        assert_eq!(parts[1].color, Rgba([153, 153, 153, 255]));
        assert!((parts[0].world_mesh().surface_area() - 1.0).abs() < 1e-9);
        assert!((parts[1].world_mesh().surface_area() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_colour_map_inside_a_family_splits_too() {
        // The same cube, placed through a mapped representation.
        let source = "#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(1.,0.,0.),(1.,1.,0.),(0.,1.,0.),(0.,0.,1.),(1.,0.,1.),(1.,1.,1.),(0.,1.,1.)));
         #2=IFCINDEXEDPOLYGONALFACE((5,6,7,8));
         #3=IFCINDEXEDPOLYGONALFACE((1,4,3,2));
         #4=IFCINDEXEDPOLYGONALFACE((1,2,6,5));
         #5=IFCINDEXEDPOLYGONALFACE((2,3,7,6));
         #6=IFCINDEXEDPOLYGONALFACE((3,4,8,7));
         #7=IFCINDEXEDPOLYGONALFACE((4,1,5,8));
         #8=IFCPOLYGONALFACESET(#1,.T.,(#2,#3,#4,#5,#6,#7),$);
         #9=IFCCOLOURRGBLIST(((0.8,0.1,0.1),(0.6,0.6,0.6)));
         #10=IFCINDEXEDCOLOURMAP(#8,$,#9,(1,2,2,2,2,2));
         #11=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#8));
         #12=IFCCARTESIANPOINT((0.,0.,0.));
         #13=IFCAXIS2PLACEMENT3D(#12,$,$);
         #14=IFCREPRESENTATIONMAP(#13,#11);
         #15=IFCMAPPEDITEM(#14,$);
         #16=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#15));
         #17=IFCPRODUCTDEFINITIONSHAPE($,$,(#16));
         #18=IFCBUILDINGELEMENTPROXY('guid',$,'P',$,$,$,#17,$,$);
";
        let (parts, diagnostics) = evaluate_parts(source, 18);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(parts.len(), 2, "one part per colour, through the family");
        assert_eq!(parts[0].color, Rgba([204, 26, 26, 255]));
        assert!((parts[0].world_mesh().surface_area() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_mirrored_family_is_baked_rather_than_shared() {
        let (plain, _) = evaluate_parts(MAPPED_WINDOW, 30);
        assert!(
            plain[0].geometry.shared_key().is_some(),
            "an ordinary family is shared"
        );

        let mirrored = format!(
            "{}#5=IFCDIRECTION((1.,0.,0.));\n\
             #6=IFCDIRECTION((0.,-1.,0.));\n\
             #19=IFCCARTESIANTRANSFORMATIONOPERATOR3D(#5,#6,#13,$,$);\n",
            MAPPED_WINDOW.replace("#16=IFCMAPPEDITEM(#15,$);", "#16=IFCMAPPEDITEM(#15,#19);")
        );
        let (parts, _) = evaluate_parts(&mirrored, 30);
        for part in &parts {
            assert!(
                part.geometry.shared_key().is_none(),
                "a mirrored instance transform would light the family from inside"
            );
        }
        assert!(parts[0].world_mesh().signed_volume() > 0.0);
    }

    #[test]
    fn one_material_stays_one_part() {
        // The split is per colour, not per item.
        let (parts, _) = evaluate_parts(&wall_model(""), 9);
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0].color,
            crate::style::class_colour("IfcWall"),
            "an unstyled product falls back to the class palette"
        );
    }
}
