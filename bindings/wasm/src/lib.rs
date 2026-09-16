// SPDX-License-Identifier: Apache-2.0
//! WebAssembly bindings for TessIFC. One [`Kernel`] holds any number of open
//! models; results cross as JSON strings and geometry as typed arrays, either
//! as one pack (`evaluateGeometry`, `takePack`) or streamed as IGP chunks.
//!
//! ```js
//! import init, { Kernel } from "@tessifc/core";
//! await init();
//! const kernel = new Kernel();
//! const id = kernel.openModel(new Uint8Array(await file.arrayBuffer()));
//! const info = JSON.parse(kernel.getModelInfo(id));
//! kernel.closeModel(id);
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};
use tessifc_engine::pack::{PackState, Packer};
use tessifc_engine::revision::{ChangeImpact, compare_revisions_with_sources};
use tessifc_engine::{Engine, EvaluationResult, ProductOutcome, ProductState, Session};
use tessifc_geom::{Settings as Settings3d, product_category};
use tessifc_model::Model;
use tessifc_pack::StreamPosition;
use tessifc_step::{
    AttributeEdit, EditValue, ParseOptions, SchemaId, apply_edits, argument_source,
    leaf_argument_source, parse,
};
use wasm_bindgen::prelude::*;

fn pack_evaluation(schema: &str, result: &EvaluationResult) -> Vec<u8> {
    let mut packer = Packer::new(schema, result.units.length_to_m, result.model_offset);
    packer.set_georef(result.georef.clone());
    for shape in &result.shapes {
        packer.add_shape_ref(shape);
    }
    packer.add_diagnostics(&result.diagnostics);
    packer.set_stat("products", result.shapes.len() as f64);
    packer.set_stat("triangles", packer.triangles() as f64);
    packer.finish()
}

fn pack_evaluation_owned(schema: &str, result: EvaluationResult) -> Vec<u8> {
    let EvaluationResult {
        shapes,
        diagnostics,
        units,
        model_offset,
        georef,
        ..
    } = result;
    let products = shapes.len();
    let mut packer = Packer::new(schema, units.length_to_m, model_offset);
    packer.set_georef(georef);
    for shape in shapes {
        packer.add_shape(shape);
    }
    packer.add_diagnostics(&diagnostics);
    packer.set_stat("products", products as f64);
    packer.set_stat("triangles", packer.triangles() as f64);
    packer.finish()
}

/// Settings accepted by geometry entry points; unknown fields are ignored.
#[derive(Default, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeometrySettings {
    #[serde(flatten)]
    geometry: Settings3d,
    model_offset: Option<[f64; 3]>,
    first_geometry_id: Option<u32>,
}

impl GeometrySettings {
    fn from_json(text: &str) -> Result<Self, String> {
        let settings: Self = serde_json::from_str(text)
            .map_err(|error| format!("invalid geometry settings: {error}"))?;
        settings
            .geometry
            .validate()
            .map_err(|error| error.to_string())?;
        // Ids are handed out upwards from here; leave room for a patch's meshes.
        if settings
            .first_geometry_id
            .is_some_and(|first| first > u32::MAX - (1 << 24))
        {
            return Err(
                "invalid geometry settings: firstGeometryId leaves no room for new ids".into(),
            );
        }
        Ok(settings)
    }

    fn parse(text: Option<String>) -> Result<Self, JsValue> {
        text.map(|text| Self::from_json(&text))
            .transpose()
            .map(|settings| settings.unwrap_or_default())
            .map_err(|error| JsValue::from_str(&error))
    }
}

fn effective_settings(settings: &Settings3d) -> serde_json::Value {
    serde_json::to_value(settings).expect("validated geometry settings are serialisable")
}

/// A finite f64 as JSON, since JSON has no NaN and no infinity.
fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".into()
    }
}

fn json_vector(vector: DVec3) -> String {
    format!(
        "[{},{},{}]",
        json_number(vector.x),
        json_number(vector.y),
        json_number(vector.z)
    )
}

/// Milliseconds on a monotonic-enough clock, for chunk budgets.
#[cfg(target_arch = "wasm32")]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// Sub-millisecond time for stage reports: `performance.now()` where the host has it.
#[cfg(target_arch = "wasm32")]
fn precise_now_ms() -> f64 {
    use std::cell::OnceCell;
    thread_local! {
        static CLOCK: OnceCell<Option<(js_sys::Object, js_sys::Function)>> = const { OnceCell::new() };
    }
    CLOCK.with(|clock| {
        let clock = clock.get_or_init(|| {
            let performance =
                js_sys::Reflect::get(&js_sys::global(), &"performance".into()).ok()?;
            let now = js_sys::Reflect::get(&performance, &"now".into()).ok()?;
            Some((performance.dyn_into().ok()?, now.dyn_into().ok()?))
        });
        clock
            .as_ref()
            .and_then(|(performance, now)| now.call0(performance).ok()?.as_f64())
            .unwrap_or_else(js_sys::Date::now)
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn precise_now_ms() -> f64 {
    now_ms()
}

/// Version of the TessIFC crates this module was built from.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Install a panic hook that forwards Rust panics to `console.error`.
#[wasm_bindgen(js_name = initPanicHook)]
pub fn init_panic_hook() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
}

/// Settings accepted by [`Kernel::open_model`], as JSON; unknown fields are ignored.
///
/// Kept per model, so an edit reparses the file exactly as it was opened.
#[derive(Clone, Default)]
struct OpenOptions {
    schema_override: Option<SchemaId>,
    max_entities: Option<usize>,
}

impl OpenOptions {
    /// The two fields the kernel understands; malformed JSON or an unknown schema name is an error.
    fn from_json(text: &str) -> Result<OpenOptions, String> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| format!("invalid open settings: {error}"))?;
        let mut options = OpenOptions::default();
        if let Some(name) = value.get("schemaOverride") {
            let name = name
                .as_str()
                .ok_or("invalid open settings: schemaOverride must be a string")?;
            options.schema_override = Some(
                SchemaId::detect(name)
                    .map(|(id, _)| id)
                    .ok_or_else(|| format!("invalid open settings: unknown schema {name}"))?,
            );
        }
        if let Some(max) = value.get("maxEntities") {
            let max = max
                .as_u64()
                .ok_or("invalid open settings: maxEntities must be a non-negative integer")?;
            options.max_entities = Some(usize::try_from(max).unwrap_or(usize::MAX));
        }
        Ok(options)
    }

    fn parse_options(&self) -> ParseOptions {
        let defaults = ParseOptions::default();
        ParseOptions {
            schema_override: self.schema_override,
            max_entities: self.max_entities.unwrap_or(defaults.max_entities),
            ..defaults
        }
    }
}

/// A geometry stream in progress for one model.
struct Stream {
    diagnostic_counts: [usize; 3],
    session: Session,
    state: PackState,
    chunk: u32,
}

/// Milliseconds spent in each stage of preparing and evaluating a revision.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionTimings {
    validate_source_ms: f64,
    parse_ms: f64,
    validate_model_ms: f64,
    compare_ms: f64,
    prepare_ms: f64,
    session_ms: f64,
    evaluate_ms: f64,
    outcomes_ms: f64,
    pack_ms: f64,
    baseline_ms: f64,
    evaluate_total_ms: f64,
}

/// An unpublished model and the result of checking its replacement geometry.
struct PreparedRevision {
    source: Vec<u8>,
    model: Model,
    impact: ChangeImpact,
    base_revision: u64,
    revision: u64,
    token: u64,
    evaluated: bool,
    accepted: bool,
    outcomes: Vec<ProductOutcome>,
    diagnostics: Vec<tessifc_step::Diagnostic>,
    evaluation_settings: Option<serde_json::Value>,
    model_offset: Option<[f64; 3]>,
    refused_boolean_products: Vec<u32>,
    timings: RevisionTimings,
    dangling: BTreeSet<(u32, u32)>,
}

struct GeometryBasis {
    settings: serde_json::Value,
    offset: DVec3,
}

impl PreparedRevision {
    fn report(&self) -> String {
        let mut report =
            serde_json::to_value(&self.impact).expect("revision impact is serialisable");
        report["baseRevision"] = self.base_revision.to_string().into();
        report["revision"] = self.revision.to_string().into();
        report["candidateToken"] = self.token.to_string().into();
        report["evaluated"] = self.evaluated.into();
        report["evaluationAccepted"] = self.accepted.into();
        report["productOutcomes"] = serde_json::to_value(&self.outcomes).unwrap();
        report["diagnostics"] = diagnostics_json(&self.diagnostics);
        report["effectiveSettings"] = self
            .evaluation_settings
            .clone()
            .unwrap_or(serde_json::Value::Null);
        report["modelOffset"] = serde_json::json!(self.model_offset);
        report["refusedBooleanProducts"] = serde_json::json!(self.refused_boolean_products);
        report["timings"] = serde_json::to_value(&self.timings).unwrap_or(serde_json::Value::Null);
        report.to_string()
    }
}

fn diagnostics_json(diagnostics: &[tessifc_step::Diagnostic]) -> serde_json::Value {
    serde_json::Value::Array(
        diagnostics
            .iter()
            .map(|d| {
                serde_json::json!({
                    "code": d.code.as_str(), "severity": d.severity.as_str(),
                    "line": d.line, "expressId": d.express_id, "message": d.message,
                })
            })
            .collect(),
    )
}

/// A TessIFC instance holding open models.
#[wasm_bindgen]
pub struct Kernel {
    models: BTreeMap<u32, Model>,
    /// Original or most recently edited IFC bytes, kept apart from the model image.
    sources: BTreeMap<u32, Vec<u8>>,
    /// What each model was opened with.
    options: BTreeMap<u32, OpenOptions>,
    revisions: BTreeMap<u32, u64>,
    prepared: BTreeMap<u32, PreparedRevision>,
    /// Dangling references of each committed model, found once and carried across revisions.
    dangling: BTreeMap<u32, BTreeSet<(u32, u32)>>,
    next_candidate: u64,
    geometry_basis: BTreeMap<u32, GeometryBasis>,
    published_outcomes: BTreeMap<u32, Vec<ProductOutcome>>,
    next_id: u32,
    /// The last evaluation per model, so geometry can be read in pieces.
    geometry: BTreeMap<u32, EvaluationResult>,
    /// Geometry streams in progress, or finished and kept for the hierarchy.
    streams: BTreeMap<u32, Stream>,
}

impl Default for Kernel {
    fn default() -> Self {
        Kernel::new()
    }
}

#[wasm_bindgen]
impl Kernel {
    /// A kernel with no models open.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Kernel {
        init_panic_hook();
        Kernel {
            models: BTreeMap::new(),
            options: BTreeMap::new(),
            sources: BTreeMap::new(),
            revisions: BTreeMap::new(),
            prepared: BTreeMap::new(),
            dangling: BTreeMap::new(),
            next_candidate: 1,
            geometry_basis: BTreeMap::new(),
            published_outcomes: BTreeMap::new(),
            next_id: 1,
            geometry: BTreeMap::new(),
            streams: BTreeMap::new(),
        }
    }

    /// Parse a file and keep it open. Returns the model id; an unreadable file
    /// yields zero entities and diagnostics rather than an error. The bytes are
    /// taken over, so the file is copied into the kernel once. Invalid
    /// `settings` throw.
    #[wasm_bindgen(js_name = openModel)]
    pub fn open_model(&mut self, bytes: Vec<u8>, settings: Option<String>) -> Result<u32, JsValue> {
        let options = settings
            .as_deref()
            .map(OpenOptions::from_json)
            .transpose()
            .map_err(|error| JsValue::from_str(&error))?
            .unwrap_or_default();

        let source = bytes;
        let model = Model::new(parse(&source, &options.parse_options()));
        // Skip live ids so a wrapped counter cannot rebind another caller's model.
        let mut id = self.next_id;
        while self.models.contains_key(&id) {
            id = id.wrapping_add(1).max(1);
            if id == self.next_id {
                return Ok(0);
            }
        }
        self.next_id = id.wrapping_add(1).max(1);
        self.models.insert(id, model);
        self.sources.insert(id, source);
        self.options.insert(id, options);
        self.revisions.insert(id, 0);
        Ok(id)
    }

    /// A JSON report about an open model, or `null` for an unknown id.
    #[wasm_bindgen(js_name = getModelInfo)]
    pub fn get_model_info(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let source_bytes = self.sources.get(&model_id).map_or(0, Vec::len);
        let image = model.image();
        let schema = model.schema();
        let product = schema.class_by_name("IfcProduct");

        let mut products = serde_json::Map::new();
        let mut classes = serde_json::Map::new();
        let mut product_total = 0u64;
        for (class_id, count) in image.populated_classes() {
            if class_id == tessifc_step::CLASS_UNKNOWN {
                continue;
            }
            let name = schema.class(class_id).name;
            classes.insert(name.to_string(), serde_json::json!(count));
            if let Some(product) = product
                && schema.is_a(class_id, product)
            {
                products.insert(name.to_string(), serde_json::json!(count));
                product_total += count as u64;
            }
        }

        let report = serde_json::json!({
            "schema": image.schema.as_str(),
            "schemaDeclared": image.header.schema_identifiers,
            "schemaApproximate": image.schema_approximate,
            "bytes": image.source_len,
            "entities": image.len(),
            "products": products,
            "productTotal": product_total,
            "classes": classes,
            "imageBytes": image.memory_bytes(),
            "sourceRetainedBytes": source_bytes,
            "diagnostics": {
                "total": image.diagnostics.total(),
                "errors": image.diagnostics.count_of(tessifc_step::Severity::Error),
                "warnings": image.diagnostics.count_of(tessifc_step::Severity::Warning),
            },
            "header": {
                "name": image.header.name,
                "timeStamp": image.header.time_stamp,
                "preprocessorVersion": image.header.preprocessor_version,
                "originatingSystem": image.header.originating_system,
                "author": image.header.author,
                "organization": image.header.organization,
                "description": image.header.description,
            },
        });
        Some(report.to_string())
    }

    /// Parse diagnostics as JSON. Geometry diagnostics are carried by IGP packs.
    #[wasm_bindgen(js_name = getDiagnostics)]
    pub fn get_diagnostics(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let items: Vec<_> = model
            .image()
            .diagnostics
            .items()
            .iter()
            .map(|d| {
                serde_json::json!({
                    "code": d.code.as_str(),
                    "severity": d.severity.as_str(),
                    "line": d.line,
                    "expressId": d.express_id,
                    "message": d.message,
                })
            })
            .collect();
        Some(serde_json::Value::Array(items).to_string())
    }

    /// The class name of one instance, or `null`.
    #[wasm_bindgen(js_name = getClassName)]
    pub fn get_class_name(&self, model_id: u32, express_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        model.entity(express_id)?;
        Some(model.image().class_name_of(express_id))
    }

    /// The product category: physical, space, opening, annotation or reference.
    #[wasm_bindgen(js_name = getProductCategory)]
    pub fn get_product_category(&self, model_id: u32, express_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let entity = model.entity(express_id)?;
        entity
            .is_a("IfcProduct")
            .then(|| product_category(entity).as_str().to_owned())
    }

    /// Express ids of every instance of a class or its subtypes.
    #[wasm_bindgen(js_name = getIdsOfType)]
    pub fn get_ids_of_type(&self, model_id: u32, class_name: &str) -> Vec<u32> {
        let Some(model) = self.models.get(&model_id) else {
            return Vec::new();
        };
        let Some(class) = model.schema().class_by_name(class_name) else {
            return Vec::new();
        };
        model.image().ids_of_type(class).collect()
    }

    /// The schema definition of a class as JSON: `abstract` and `attributes` in
    /// STEP argument order with `name`, `type`, `base`, `aggDepth`, `optional`
    /// and `derived`; `null` for a class the model's schema does not define.
    #[wasm_bindgen(js_name = getClassAttributes)]
    pub fn get_class_attributes(&self, model_id: u32, class_name: &str) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let schema = model.schema();
        let class = schema.class_by_name(class_name)?;
        let definition = schema.class(class);
        let attributes: Vec<_> = definition
            .attrs
            .iter()
            .map(|attribute| {
                serde_json::json!({
                    "name": attribute.name,
                    "type": attribute.type_name,
                    "base": format!("{:?}", attribute.base).to_ascii_lowercase(),
                    "aggDepth": attribute.agg_depth,
                    "optional": attribute.optional,
                    "derived": attribute.kind == tessifc_schema::AttrKind::DerivedOverride,
                })
            })
            .collect();
        Some(
            serde_json::json!({
                "class": definition.name,
                "abstract": definition.is_abstract,
                "attributes": attributes,
            })
            .to_string(),
        )
    }

    /// The class and its supertypes up to the root, as a JSON array of names;
    /// `undefined` for an unknown model or a class outside its schema.
    #[wasm_bindgen(js_name = getClassSupertypes)]
    pub fn get_class_supertypes(&self, model_id: u32, class_name: &str) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let schema = model.schema();
        let mut names = Vec::new();
        let mut next = Some(schema.class_by_name(class_name)?);
        while let Some(class) = next
            && names.len() < schema.class_count()
        {
            let definition = schema.class(class);
            names.push(definition.name);
            next = definition.parent;
        }
        Some(serde_json::json!(names).to_string())
    }

    /// The rendered building hierarchy as a flat JSON node list of products and
    /// their spatial or aggregate ancestors; before evaluation every product counts.
    #[wasm_bindgen(js_name = getSpatialHierarchy)]
    pub fn get_spatial_hierarchy(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let rendered: Option<BTreeSet<u32>> =
            match (self.geometry.get(&model_id), self.streams.get(&model_id)) {
                (Some(result), _) => {
                    Some(result.shapes.iter().map(|shape| shape.express_id).collect())
                }
                (None, Some(stream)) => Some(stream.session.emitted().iter().copied().collect()),
                (None, None) => self.published_outcomes.get(&model_id).map(|outcomes| {
                    outcomes
                        .iter()
                        .filter(|outcome| outcome.state == ProductState::Emitted)
                        .map(|outcome| outcome.express_id)
                        .collect()
                }),
            };

        let mut included = match &rendered {
            Some(rendered) => rendered.clone(),
            None => model
                .entities_of_type("IfcProduct")
                .map(|entity| entity.id())
                .collect(),
        };
        let mut pending: Vec<u32> = included.iter().copied().collect();
        while let Some(express_id) = pending.pop() {
            let parents = model
                .spatial_containers_of(express_id)
                .iter()
                .chain(model.aggregate_parents_of(express_id));
            for &parent in parents {
                if included.insert(parent) {
                    pending.push(parent);
                }
            }
        }

        let nodes: Vec<_> = included
            .iter()
            .filter_map(|&express_id| {
                let entity = model.entity(express_id)?;
                let parent = model
                    .spatial_containers_of(express_id)
                    .first()
                    .or_else(|| model.aggregate_parents_of(express_id).first())
                    .copied()
                    .filter(|candidate| included.contains(candidate));
                let name = entity
                    .attr("Name")
                    .as_string()
                    .filter(|value| !value.trim().is_empty());
                Some(serde_json::json!({
                    "expressId": express_id,
                    "class": entity.class_name(),
                    "name": name,
                    "globalId": entity.attr("GlobalId").as_string(),
                    "parentExpressId": parent,
                    "rendered": rendered.as_ref().is_none_or(|set| set.contains(&express_id)),
                }))
            })
            .collect();

        Some(serde_json::json!({ "nodes": nodes }).to_string())
    }

    /// Source-level attributes for one entity, as JSON; `raw` is the exact STEP
    /// spelling and `value` the decoded text where the value is string-like.
    #[wasm_bindgen(js_name = getEntityInfo)]
    pub fn get_entity_info(&self, model_id: u32, express_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let source = self.sources.get(&model_id)?;
        let entity = model.entity(express_id)?;
        let class = entity.class_name();
        let mut fields = Vec::new();

        if entity.is_complex() {
            for leaf in entity.leaves() {
                let definition = model.schema().class(leaf);
                let parent_count = definition
                    .parent
                    .map(|parent| model.schema().arity(parent))
                    .unwrap_or(0);
                for (local_index, attribute) in
                    definition.attrs.iter().skip(parent_count).enumerate()
                {
                    let raw = leaf_argument_source(
                        source,
                        model.image(),
                        express_id,
                        definition.name,
                        local_index,
                    )
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .unwrap_or_default();
                    fields.push(serde_json::json!({
                        "name": attribute.name,
                        "index": local_index,
                        "leaf": definition.name,
                        "type": attribute.type_name,
                        "kind": format!("{:?}", attribute.base).to_ascii_lowercase(),
                        "optional": attribute.optional,
                        "raw": raw,
                        "value": entity.attr(attribute.name).as_string(),
                    }));
                }
            }
            return Some(
                serde_json::json!({
                    "expressId": express_id,
                    "class": class,
                    "complex": true,
                    "fields": fields,
                })
                .to_string(),
            );
        }

        for index in 0..entity.arity() {
            let definition = model.schema().attr(entity.class(), index);
            let name = definition
                .map(|attribute| attribute.name.to_owned())
                .unwrap_or_else(|| format!("Argument {index}"));
            let type_name = definition.map_or("unknown", |attribute| attribute.type_name);
            let kind = definition
                .map(|attribute| format!("{:?}", attribute.base).to_ascii_lowercase())
                .unwrap_or_else(|| "unknown".to_owned());
            let raw = argument_source(source, model.image(), express_id, index)
                .ok()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            let value = entity.attr_at(index).as_string();
            fields.push(serde_json::json!({
                "name": name,
                "index": index,
                "type": type_name,
                "kind": kind,
                "optional": definition.is_some_and(|attribute| attribute.optional),
                "raw": raw,
                "value": value,
            }));
        }

        Some(
            serde_json::json!({
                "expressId": express_id,
                "class": class,
                "complex": false,
                "fields": fields,
            })
            .to_string(),
        )
    }

    /// Replace one named IFC attribute and reparse; `raw` passes a complete STEP value.
    #[wasm_bindgen(js_name = setAttribute)]
    pub fn set_attribute(
        &mut self,
        model_id: u32,
        express_id: u32,
        attribute: &str,
        value: &str,
        raw: bool,
    ) -> Result<String, JsValue> {
        let index = {
            let model = self
                .models
                .get(&model_id)
                .ok_or_else(|| JsValue::from_str("model is not open"))?;
            let entity = model
                .entity(express_id)
                .ok_or_else(|| JsValue::from_str("IFC entity does not exist"))?;
            entity
                .attribute_location(attribute)
                .ok_or_else(|| JsValue::from_str("the entity has no attribute with that name"))?
        };
        self.replace_argument(
            model_id,
            express_id,
            index.argument_index,
            index.leaf_class.map(str::to_owned),
            value,
            raw,
        )?;
        self.get_entity_info(model_id, express_id)
            .ok_or_else(|| JsValue::from_str("edited entity could not be read back"))
    }

    /// Replace several named attributes with one source rewrite and one reparse;
    /// `edits` is a JSON array of `{ "attribute", "value", "raw" }` objects.
    #[wasm_bindgen(js_name = setAttributes)]
    pub fn set_attributes(
        &mut self,
        model_id: u32,
        express_id: u32,
        edits: &str,
    ) -> Result<String, JsValue> {
        let requests = serde_json::from_str::<serde_json::Value>(edits)
            .map_err(|_| JsValue::from_str("edits must be a JSON array"))?;
        let requests = requests
            .as_array()
            .ok_or_else(|| JsValue::from_str("edits must be a JSON array"))?;
        if requests.is_empty() {
            return self
                .get_entity_info(model_id, express_id)
                .ok_or_else(|| JsValue::from_str("IFC entity does not exist"));
        }

        let replacements = {
            let model = self
                .models
                .get(&model_id)
                .ok_or_else(|| JsValue::from_str("model is not open"))?;
            let entity = model
                .entity(express_id)
                .ok_or_else(|| JsValue::from_str("IFC entity does not exist"))?;
            let mut replacements = Vec::with_capacity(requests.len());
            for request in requests {
                let attribute = request
                    .get("attribute")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| JsValue::from_str("every edit needs an attribute name"))?;
                let value = request
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| JsValue::from_str("every edit needs a string value"))?;
                let location = entity
                    .attribute_location(attribute)
                    .ok_or_else(|| JsValue::from_str("the entity has no requested attribute"))?;
                let value = if request
                    .get("raw")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    EditValue::Raw(value.to_owned())
                } else {
                    EditValue::String(value.to_owned())
                };
                replacements.push(AttributeEdit {
                    express_id,
                    argument_index: location.argument_index,
                    leaf_class: location.leaf_class.map(str::to_owned),
                    value,
                });
            }
            replacements
        };

        self.replace_edits(model_id, &replacements)?;
        self.get_entity_info(model_id, express_id)
            .ok_or_else(|| JsValue::from_str("edited entity could not be read back"))
    }

    /// Replace one argument by its zero-based STEP position, for vendor extension classes.
    #[wasm_bindgen(js_name = setArgument)]
    pub fn set_argument(
        &mut self,
        model_id: u32,
        express_id: u32,
        argument: usize,
        value: &str,
        raw: bool,
    ) -> Result<String, JsValue> {
        self.replace_argument(model_id, express_id, argument, None, value, raw)?;
        self.get_entity_info(model_id, express_id)
            .ok_or_else(|| JsValue::from_str("edited entity could not be read back"))
    }

    /// The current IFC source, including every accepted edit.
    #[wasm_bindgen(js_name = exportModel)]
    pub fn export_model(&self, model_id: u32) -> Option<Vec<u8>> {
        self.sources.get(&model_id).cloned()
    }

    /// The committed source revision, as a decimal string; `null` if not open.
    #[wasm_bindgen(js_name = getModelRevision)]
    pub fn get_model_revision(&self, model_id: u32) -> Option<String> {
        self.revisions.get(&model_id).map(u64::to_string)
    }

    /// Validate and compare a complete replacement IFC without publishing it.
    /// The base revision must match and no other candidate may be pending.
    #[wasm_bindgen(js_name = prepareRevision)]
    pub fn prepare_revision(
        &mut self,
        model_id: u32,
        bytes: Vec<u8>,
        base_revision: &str,
    ) -> Result<String, JsValue> {
        self.prepare_revision_inner(model_id, bytes, base_revision)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Stage source-preserving named edits across multiple entities. Every edit
    /// is `{expressId, attribute, value, raw?}`; values are strings.
    #[wasm_bindgen(js_name = prepareAttributeEdits)]
    pub fn prepare_attribute_edits(
        &mut self,
        model_id: u32,
        edits: &str,
        base_revision: &str,
    ) -> Result<String, JsValue> {
        self.prepare_attribute_edits_inner(model_id, edits, base_revision)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Candidate impact and explicit per-product outcomes. Nothing is committed.
    #[wasm_bindgen(js_name = getPreparedRevisionInfo)]
    pub fn get_prepared_revision_info(&self, model_id: u32) -> Option<String> {
        self.prepared.get(&model_id).map(PreparedRevision::report)
    }

    /// Evaluate every affected candidate product into a self-contained patch.
    /// Pass the displayed pack's `modelOffset` and its next `firstGeometryId`.
    /// Inspect `getPreparedRevisionInfo().evaluationAccepted` before publication.
    #[wasm_bindgen(js_name = evaluatePreparedRevision)]
    pub fn evaluate_prepared_revision(
        &mut self,
        model_id: u32,
        candidate_token: &str,
        options: Option<String>,
    ) -> Result<Vec<u8>, JsValue> {
        self.check_candidate_token(model_id, candidate_token)
            .map_err(|error| JsValue::from_str(&error))?;
        let options = GeometrySettings::parse(options)?;
        self.evaluate_prepared_revision_inner(model_id, options)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Publish an accepted candidate and return the new decimal revision string.
    /// Geometry changes must have an accepted staged evaluation first.
    #[wasm_bindgen(js_name = commitRevision)]
    pub fn commit_revision(
        &mut self,
        model_id: u32,
        base_revision: &str,
        candidate_token: &str,
    ) -> Result<String, JsValue> {
        self.check_candidate_token(model_id, candidate_token)
            .map_err(|error| JsValue::from_str(&error))?;
        self.commit_revision_inner(model_id, base_revision)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Discard the named unpublished candidate, leaving the source untouched.
    /// A stale token cannot discard a newer candidate; no candidate returns false.
    #[wasm_bindgen(js_name = discardRevision)]
    pub fn discard_revision(
        &mut self,
        model_id: u32,
        candidate_token: &str,
    ) -> Result<bool, JsValue> {
        self.discard_revision_inner(model_id, candidate_token)
            .map_err(|error| JsValue::from_str(&error))
    }

    /// Every schema entity and its direct or inherited evaluator routes, as JSON.
    #[wasm_bindgen(js_name = getGeometryCapabilities)]
    pub fn get_geometry_capabilities(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        serde_json::to_string(&tessifc_geom::Registry::shared(model.image().schema).inventory())
            .ok()
    }

    /// Per-product evaluation outcomes; emitted products may still have diagnostics.
    #[wasm_bindgen(js_name = getProductOutcomes)]
    pub fn get_product_outcomes(&self, model_id: u32) -> Option<String> {
        let outcomes = self.current_product_outcomes(model_id)?;
        serde_json::to_string(&outcomes).ok()
    }

    /// Stop and release a geometry stream while keeping the parsed model open.
    #[wasm_bindgen(js_name = cancelGeometryStream)]
    pub fn cancel_geometry_stream(&mut self, model_id: u32) -> bool {
        if let Some(outcomes) = self.current_product_outcomes(model_id) {
            self.published_outcomes.insert(model_id, outcomes);
        }
        self.streams.remove(&model_id).is_some()
    }

    /// Evaluate every product and keep the result for [`Kernel::take_pack`] or
    /// [`Kernel::shape_positions`]. Returns a JSON summary, `null` if unknown.
    #[wasm_bindgen(js_name = evaluateGeometry)]
    pub fn evaluate_geometry(
        &mut self,
        model_id: u32,
        settings: Option<String>,
    ) -> Result<Option<String>, JsValue> {
        let Some(model) = self.models.get(&model_id) else {
            return Ok(None);
        };
        let settings = GeometrySettings::parse(settings)?.geometry;
        let effective = effective_settings(&settings);

        let result = Engine::with_settings(settings).evaluate(model);
        let diagnostic_infos = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == tessifc_step::Severity::Info)
            .count();
        let diagnostic_warnings = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == tessifc_step::Severity::Warning)
            .count();
        let diagnostic_errors = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == tessifc_step::Severity::Error)
            .count();
        let mut summary = String::from("{\"products\":");
        summary.push_str(&result.shapes.len().to_string());
        summary.push_str(",\"triangles\":");
        summary.push_str(&result.triangles().to_string());
        summary.push_str(",\"productsConsidered\":");
        summary.push_str(&result.products_considered.to_string());
        summary.push_str(",\"productsFiltered\":");
        summary.push_str(&result.products_filtered.to_string());
        summary.push_str(",\"lengthScaleToM\":");
        summary.push_str(&json_number(result.units.length_to_m));
        summary.push_str(",\"modelOffset\":");
        summary.push_str(&json_vector(result.model_offset));
        summary.push_str(",\"diagnostics\":");
        summary.push_str(&result.diagnostics.len().to_string());
        summary.push_str(",\"diagnosticInfos\":");
        summary.push_str(&diagnostic_infos.to_string());
        summary.push_str(",\"diagnosticWarnings\":");
        summary.push_str(&diagnostic_warnings.to_string());
        summary.push_str(",\"diagnosticErrors\":");
        summary.push_str(&diagnostic_errors.to_string());
        summary.push_str(",\"effectiveSettings\":");
        summary.push_str(&effective.to_string());
        summary.push('}');

        self.streams.remove(&model_id);
        self.prepared.remove(&model_id);
        self.geometry_basis.insert(
            model_id,
            GeometryBasis {
                settings: effective_settings(&result.settings),
                offset: result.model_offset,
            },
        );
        self.published_outcomes.insert(
            model_id,
            Engine::with_settings(result.settings.clone()).outcomes(model, &result),
        );
        self.geometry.insert(model_id, result);
        Ok(Some(summary))
    }

    /// Start streaming a model's geometry as IGP chunks; returns a JSON summary or
    /// `null`. Follow with [`Kernel::next_geometry_chunk`] until it returns `null`.
    #[wasm_bindgen(js_name = beginGeometryStream)]
    pub fn begin_geometry_stream(
        &mut self,
        model_id: u32,
        settings: Option<String>,
    ) -> Result<Option<String>, JsValue> {
        let Some(model) = self.models.get(&model_id) else {
            return Ok(None);
        };
        let settings = GeometrySettings::parse(settings)?.geometry;
        let effective = effective_settings(&settings);
        let session = Engine::with_settings(settings).session(model);
        let summary = format!(
            "{{\"products\":{},\"productsConsidered\":{},\"productsFiltered\":{},\
             \"lengthScaleToM\":{},\"modelOffset\":{}}}",
            session.total(),
            session.products_considered(),
            session.products_filtered(),
            json_number(session.units().length_to_m),
            json_vector(session.model_offset()),
        );
        let mut summary: serde_json::Value =
            serde_json::from_str(&summary).expect("finite geometry summary");
        self.geometry_basis.insert(
            model_id,
            GeometryBasis {
                settings: effective.clone(),
                offset: session.model_offset(),
            },
        );
        self.prepared.remove(&model_id);
        summary["effectiveSettings"] = effective;
        let summary = summary.to_string();
        self.geometry.remove(&model_id);
        self.streams.insert(
            model_id,
            Stream {
                diagnostic_counts: [0; 3],
                session,
                state: PackState::default(),
                chunk: 0,
            },
        );
        Ok(Some(summary))
    }

    /// The next IGP chunk, or `null` once finished. Stops after `budget_ms`,
    /// `max_products` or `max_triangles` (zero is no limit), holding at least one product.
    #[wasm_bindgen(js_name = nextGeometryChunk)]
    pub fn next_geometry_chunk(
        &mut self,
        model_id: u32,
        budget_ms: f64,
        max_products: u32,
        max_triangles: u32,
    ) -> Option<Vec<u8>> {
        let model = self.models.get(&model_id)?;
        let stream = self.streams.get_mut(&model_id)?;
        if stream.session.is_finished() && stream.chunk > 0 {
            return None;
        }
        let started = now_ms();
        let batch = stream.session.next(model, |progress| {
            (budget_ms > 0.0 && now_ms() - started >= budget_ms)
                || (max_products > 0 && progress.products >= max_products as usize)
                || (max_triangles > 0 && progress.triangles >= max_triangles as usize)
        });
        let session = &stream.session;
        let mut packer = Packer::continue_stream(
            model.image().schema.as_str(),
            session.units().length_to_m,
            session.model_offset(),
            std::mem::take(&mut stream.state),
        );
        packer.set_georef(session.georef().map(str::to_string));
        packer.set_stream(StreamPosition {
            chunk: stream.chunk,
            is_final: batch.is_final,
            products_done: session.done(),
            products_total: session.total(),
        });
        for shape in batch.shapes {
            packer.add_shape(shape);
        }
        for diagnostic in &batch.diagnostics {
            let index = match diagnostic.severity {
                tessifc_step::Severity::Info => 0,
                tessifc_step::Severity::Warning => 1,
                tessifc_step::Severity::Error => 2,
            };
            stream.diagnostic_counts[index] += 1;
        }
        packer.add_diagnostics(&batch.diagnostics);
        if batch.is_final {
            packer.set_stat("products", session.emitted().len() as f64);
            packer.set_stat("triangles", session.triangles() as f64);
        }
        let (bytes, state) = packer.finish_chunk();
        stream.state = state;
        stream.chunk += 1;
        Some(bytes)
    }

    /// How far a stream has come, as JSON, or `null` if there is none.
    #[wasm_bindgen(js_name = streamProgress)]
    pub fn stream_progress(&self, model_id: u32) -> Option<String> {
        let stream = self.streams.get(&model_id)?;
        Some(serde_json::json!({
            "done":stream.session.done(),"total":stream.session.total(),"emitted":stream.session.emitted().len(),
            "triangles":stream.session.triangles(),"chunks":stream.chunk,
            "finished":stream.session.is_finished() && stream.chunk > 0,
            "diagnostics":stream.diagnostic_counts.iter().sum::<usize>(),
            "diagnosticInfos":stream.diagnostic_counts[0],"diagnosticWarnings":stream.diagnostic_counts[1],"diagnosticErrors":stream.diagnostic_counts[2],
        }).to_string())
    }

    /// Re-evaluate a few products into one self-contained IGP chunk for patching
    /// a pack; `options` adds `modelOffset` and `firstGeometryId`. `null` if unknown.
    #[wasm_bindgen(js_name = evaluateProducts)]
    pub fn evaluate_products(
        &self,
        model_id: u32,
        express_ids: &[u32],
        options: Option<String>,
    ) -> Result<Option<Vec<u8>>, JsValue> {
        let Some(model) = self.models.get(&model_id) else {
            return Ok(None);
        };
        let options = GeometrySettings::parse(options)?;
        Ok(Some(evaluate_subset(model, express_ids, &options).bytes))
    }

    /// How many shapes the last evaluation produced. `0` if there was none.
    #[wasm_bindgen(js_name = shapeCount)]
    pub fn shape_count(&self, model_id: u32) -> usize {
        self.geometry
            .get(&model_id)
            .map(|result| result.shapes.len())
            .unwrap_or(0)
    }

    /// The express id of shape `index`, in ascending order.
    #[wasm_bindgen(js_name = shapeExpressId)]
    pub fn shape_express_id(&self, model_id: u32, index: usize) -> Option<u32> {
        Some(self.geometry.get(&model_id)?.shapes.get(index)?.express_id)
    }

    /// How many separately coloured parts shape `index` has; a window has two.
    #[wasm_bindgen(js_name = shapePartCount)]
    pub fn shape_part_count(&self, model_id: u32, index: usize) -> Option<usize> {
        Some(self.geometry.get(&model_id)?.shapes.get(index)?.parts.len())
    }

    /// The colour of shape `index` as RGBA bytes, from the file's styles or a
    /// class palette; `part` selects one part, omitted gives the product's own.
    #[wasm_bindgen(js_name = shapeColor)]
    pub fn shape_color(&self, model_id: u32, index: usize, part: Option<usize>) -> Option<Vec<u8>> {
        let shape = self.geometry.get(&model_id)?.shapes.get(index)?;
        match part {
            Some(part) => Some(shape.parts.get(part)?.color.to_vec()),
            None => Some(shape.color.to_vec()),
        }
    }

    /// The IFC class of shape `index`.
    #[wasm_bindgen(js_name = shapeClass)]
    pub fn shape_class(&self, model_id: u32, index: usize) -> Option<String> {
        Some(
            self.geometry
                .get(&model_id)?
                .shapes
                .get(index)?
                .class
                .clone(),
        )
    }

    /// Vertex positions of shape `index` as f32 triples with the model offset
    /// removed; `part` selects one coloured part, omitted merges them all.
    #[wasm_bindgen(js_name = shapePositions)]
    pub fn shape_positions(
        &self,
        model_id: u32,
        index: usize,
        part: Option<usize>,
    ) -> Option<Vec<f32>> {
        let result = self.geometry.get(&model_id)?;
        let shape = result.shapes.get(index)?;
        let offset = result.model_offset;
        let mesh = match part {
            Some(part) => shape.parts.get(part)?.mesh(),
            None => shape.mesh(),
        };
        Some(
            mesh.positions
                .iter()
                .flat_map(|point| {
                    let shifted = *point - offset;
                    [shifted.x as f32, shifted.y as f32, shifted.z as f32]
                })
                .collect(),
        )
    }

    /// Triangle indices of shape `index`; `part` selects one coloured part, omitted merges them.
    #[wasm_bindgen(js_name = shapeIndices)]
    pub fn shape_indices(
        &self,
        model_id: u32,
        index: usize,
        part: Option<usize>,
    ) -> Option<Vec<u32>> {
        let shape = self.geometry.get(&model_id)?.shapes.get(index)?;
        match part {
            Some(part) => Some(shape.parts.get(part)?.geometry.local_mesh().indices.clone()),
            None => Some(shape.mesh().indices),
        }
    }

    /// The whole evaluation as one IGP v0 pack.
    #[wasm_bindgen(js_name = getPack)]
    pub fn get_pack(&self, model_id: u32) -> Option<Vec<u8>> {
        let model = self.models.get(&model_id)?;
        let result = self.geometry.get(&model_id)?;
        Some(pack_evaluation(model.image().schema.as_str(), result))
    }

    /// The whole evaluation as one IGP v0 pack, releasing the geometry without cloning it.
    #[wasm_bindgen(js_name = takePack)]
    pub fn take_pack(&mut self, model_id: u32) -> Option<Vec<u8>> {
        let schema = self
            .models
            .get(&model_id)?
            .image()
            .schema
            .as_str()
            .to_owned();
        let result = self.geometry.remove(&model_id)?;
        Some(pack_evaluation_owned(&schema, result))
    }

    /// Drop the geometry for a model, keeping the model itself open.
    #[wasm_bindgen(js_name = releaseGeometry)]
    pub fn release_geometry(&mut self, model_id: u32) -> bool {
        if let Some(outcomes) = self.current_product_outcomes(model_id) {
            self.published_outcomes.insert(model_id, outcomes);
        }
        self.streams.remove(&model_id);
        self.geometry.remove(&model_id).is_some()
    }

    /// Close a model and free it. `false` if the id was not open.
    #[wasm_bindgen(js_name = closeModel)]
    pub fn close_model(&mut self, model_id: u32) -> bool {
        self.geometry_basis.remove(&model_id);
        self.published_outcomes.remove(&model_id);
        self.prepared.remove(&model_id);
        self.dangling.remove(&model_id);
        self.revisions.remove(&model_id);
        self.geometry.remove(&model_id);
        self.streams.remove(&model_id);
        self.sources.remove(&model_id);
        self.options.remove(&model_id);
        self.models.remove(&model_id).is_some()
    }

    /// How many models are open.
    #[wasm_bindgen(js_name = modelCount)]
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Close every open model.
    #[wasm_bindgen(js_name = closeAll)]
    pub fn close_all(&mut self) {
        self.geometry_basis.clear();
        self.published_outcomes.clear();
        self.prepared.clear();
        self.dangling.clear();
        self.revisions.clear();
        self.geometry.clear();
        self.streams.clear();
        self.sources.clear();
        self.options.clear();
        self.models.clear();
    }
}

impl Kernel {
    fn current_product_outcomes(&self, model_id: u32) -> Option<Vec<ProductOutcome>> {
        let model = self.models.get(&model_id)?;
        if let Some(stream) = self.streams.get(&model_id) {
            Some(stream.session.outcomes(model))
        } else if let Some(result) = self.geometry.get(&model_id) {
            Some(Engine::with_settings(result.settings.clone()).outcomes(model, result))
        } else {
            self.published_outcomes.get(&model_id).cloned()
        }
    }

    fn check_candidate_token(&self, model_id: u32, token: &str) -> Result<(), String> {
        let candidate = self.prepared.get(&model_id).ok_or("no prepared revision")?;
        if token != candidate.token.to_string() {
            return Err("stale candidate token".into());
        }
        Ok(())
    }

    fn discard_revision_inner(
        &mut self,
        model_id: u32,
        candidate_token: &str,
    ) -> Result<bool, String> {
        if !self.prepared.contains_key(&model_id) {
            return Ok(false);
        }
        self.check_candidate_token(model_id, candidate_token)?;
        Ok(self.prepared.remove(&model_id).is_some())
    }
    fn check_base_revision(&self, model_id: u32, base_revision: &str) -> Result<u64, String> {
        let revision = *self.revisions.get(&model_id).ok_or("model is not open")?;
        if base_revision != revision.to_string() {
            return Err(format!("stale base revision: expected {revision}"));
        }
        Ok(revision)
    }

    fn prepare_revision_inner(
        &mut self,
        model_id: u32,
        source: Vec<u8>,
        base_revision: &str,
    ) -> Result<String, String> {
        let base = self.check_base_revision(model_id, base_revision)?;
        if self.prepared.contains_key(&model_id) {
            return Err("discard the existing prepared revision first".into());
        }
        let started = precise_now_ms();
        validate_complete_source(&source)?;
        let validated = precise_now_ms();
        let old = self.models.get(&model_id).ok_or("model is not open")?;
        let options = self.options.get(&model_id).cloned().unwrap_or_default();
        let model = Model::new(parse(&source, &options.parse_options()));
        let parsed = precise_now_ms();
        let old_dangling = self
            .dangling
            .entry(model_id)
            .or_insert_with(|| dangling_references(old));
        let dangling = validate_revision_model(old, &model, old_dangling)?;
        let checked = precise_now_ms();
        let old_source = self
            .sources
            .get(&model_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let impact = compare_revisions_with_sources(old, old_source, &model, &source);
        let compared = precise_now_ms();
        let timings = RevisionTimings {
            validate_source_ms: validated - started,
            parse_ms: parsed - validated,
            validate_model_ms: checked - parsed,
            compare_ms: compared - checked,
            prepare_ms: compared - started,
            ..RevisionTimings::default()
        };
        let revision = base.checked_add(1).ok_or("revision counter exhausted")?;
        let token = self.next_candidate;
        self.next_candidate = token.checked_add(1).ok_or("candidate counter exhausted")?;
        let prepared = PreparedRevision {
            source,
            model,
            impact,
            base_revision: base,
            revision,
            token,
            evaluated: false,
            accepted: false,
            outcomes: Vec::new(),
            diagnostics: Vec::new(),
            evaluation_settings: None,
            model_offset: None,
            refused_boolean_products: Vec::new(),
            timings,
            dangling,
        };
        let report = prepared.report();
        self.prepared.insert(model_id, prepared);
        Ok(report)
    }

    fn prepare_attribute_edits_inner(
        &mut self,
        model_id: u32,
        edits: &str,
        base_revision: &str,
    ) -> Result<String, String> {
        self.check_base_revision(model_id, base_revision)?;
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Request {
            express_id: u32,
            attribute: String,
            value: String,
            #[serde(default)]
            raw: bool,
        }
        let requests: Vec<Request> = serde_json::from_str(edits)
            .map_err(|error| format!("invalid attribute edits: {error}"))?;
        let model = self.models.get(&model_id).ok_or("model is not open")?;
        let source = self
            .sources
            .get(&model_id)
            .ok_or("model source is not retained")?;
        let mut replacements = Vec::with_capacity(requests.len());
        for request in requests {
            let entity = model
                .entity(request.express_id)
                .ok_or("IFC entity does not exist")?;
            let location = entity
                .attribute_location(&request.attribute)
                .ok_or("the entity has no requested attribute")?;
            replacements.push(AttributeEdit {
                express_id: request.express_id,
                argument_index: location.argument_index,
                leaf_class: location.leaf_class.map(str::to_owned),
                value: if request.raw {
                    EditValue::Raw(request.value)
                } else {
                    EditValue::String(request.value)
                },
            });
        }
        let edited =
            apply_edits(source, model.image(), &replacements).map_err(|error| error.to_string())?;
        self.prepare_revision_inner(model_id, edited, base_revision)
    }

    fn evaluate_prepared_revision_inner(
        &mut self,
        model_id: u32,
        mut options: GeometrySettings,
    ) -> Result<Vec<u8>, String> {
        if let Some(basis) = self.geometry_basis.get(&model_id) {
            if effective_settings(&options.geometry) != basis.settings {
                return Err("geometry settings differ from the displayed model; start a new full geometry evaluation first".into());
            }
            if options
                .model_offset
                .is_some_and(|offset| DVec3::from_array(offset) != basis.offset)
            {
                return Err("modelOffset differs from the displayed model frame".into());
            }
            options.model_offset = Some(basis.offset.to_array());
        }
        let prepared = self
            .prepared
            .get_mut(&model_id)
            .ok_or("no prepared revision")?;
        if self.revisions.get(&model_id) != Some(&prepared.base_revision) {
            return Err("stale prepared revision".into());
        }
        let started = precise_now_ms();
        let result = evaluate_subset(
            &prepared.model,
            &prepared.impact.affected_products,
            &options,
        );
        let evaluated = precise_now_ms();
        // A broad invalidation must not fail merely because an unchanged object
        // was already unsupported. Verify those exceptions against the old model.
        let old = self.models.get(&model_id).ok_or("model is not open")?;
        let unchanged_failures: Vec<u32> = result
            .outcomes
            .iter()
            .filter(|_| prepared.impact.full_rebuild)
            .filter(|outcome| {
                failed_outcome(outcome.state)
                    || result
                        .refused_boolean_products
                        .contains(&outcome.express_id)
            })
            .filter(|outcome| unchanged_forward_graph(old, &prepared.model, outcome.express_id))
            .map(|outcome| outcome.express_id)
            .collect();
        let mut preserved = BTreeSet::new();
        let mut baseline_errors = BTreeSet::new();
        if !unchanged_failures.is_empty() {
            let baseline = evaluate_subset(old, &unchanged_failures, &options);
            baseline_errors.extend(
                baseline
                    .diagnostics
                    .iter()
                    .filter(|d| d.severity == tessifc_step::Severity::Error)
                    .map(|d| (d.code.as_str(), d.express_id, d.message.clone())),
            );
            for before in baseline.outcomes {
                if (failed_outcome(before.state)
                    || baseline
                        .refused_boolean_products
                        .contains(&before.express_id))
                    && result.outcomes.iter().any(|after| {
                        after.express_id == before.express_id
                            && after.state == before.state
                            && (!result.refused_boolean_products.contains(&after.express_id)
                                || baseline
                                    .refused_boolean_products
                                    .contains(&after.express_id))
                    })
                {
                    preserved.insert(before.express_id);
                }
            }
        }
        prepared.accepted = result.outcomes.iter().all(|outcome| {
            (!failed_outcome(outcome.state)
                && !result
                    .refused_boolean_products
                    .contains(&outcome.express_id))
                || preserved.contains(&outcome.express_id)
        }) && !result.diagnostics.iter().any(|d| {
            d.severity == tessifc_step::Severity::Error
                && (d.code.as_str() == "E_REVISION_PACK_FAILED"
                    || preserved.is_empty()
                    || !baseline_errors.contains(&(
                        d.code.as_str(),
                        d.express_id,
                        d.message.clone(),
                    )))
        });
        prepared.evaluated = true;
        prepared.outcomes = result.outcomes;
        prepared.diagnostics = result.diagnostics;
        prepared.refused_boolean_products = result.refused_boolean_products;
        prepared.evaluation_settings = Some(effective_settings(&options.geometry));
        prepared.model_offset = Some(result.model_offset);
        let finished = precise_now_ms();
        prepared.timings.session_ms = result.stages.session_ms;
        prepared.timings.evaluate_ms = result.stages.evaluate_ms;
        prepared.timings.outcomes_ms = result.stages.outcomes_ms;
        prepared.timings.pack_ms = result.stages.pack_ms;
        prepared.timings.baseline_ms = finished - evaluated;
        prepared.timings.evaluate_total_ms = finished - started;
        Ok(result.bytes)
    }

    fn commit_revision_inner(
        &mut self,
        model_id: u32,
        base_revision: &str,
    ) -> Result<String, String> {
        let base = self.check_base_revision(model_id, base_revision)?;
        let prepared = self.prepared.get(&model_id).ok_or("no prepared revision")?;
        if prepared.base_revision != base {
            return Err("stale prepared revision".into());
        }
        if (!prepared.impact.affected_products.is_empty() && !prepared.evaluated)
            || (prepared.evaluated && !prepared.accepted)
        {
            return Err(
                "candidate geometry has not passed evaluation; revision was not committed".into(),
            );
        }
        let mut outcomes: BTreeMap<u32, ProductOutcome> = self
            .current_product_outcomes(model_id)
            .unwrap_or_default()
            .into_iter()
            .map(|outcome| (outcome.express_id, outcome))
            .collect();
        let prepared = self
            .prepared
            .remove(&model_id)
            .expect("candidate checked above");
        outcomes.retain(|id, _| {
            prepared
                .model
                .entity(*id)
                .is_some_and(|entity| entity.is_a("IfcProduct"))
        });
        for outcome in prepared.outcomes {
            outcomes.insert(outcome.express_id, outcome);
        }
        for outcome in outcomes.values_mut() {
            outcome.class = prepared.model.image().class_name_of(outcome.express_id);
        }
        self.published_outcomes
            .insert(model_id, outcomes.into_values().collect());
        if let (Some(settings), Some(offset)) =
            (prepared.evaluation_settings, prepared.model_offset)
        {
            self.geometry_basis.insert(
                model_id,
                GeometryBasis {
                    settings,
                    offset: DVec3::from_array(offset),
                },
            );
        }
        self.geometry.remove(&model_id);
        self.streams.remove(&model_id);
        self.sources.insert(model_id, prepared.source);
        self.models.insert(model_id, prepared.model);
        self.revisions.insert(model_id, prepared.revision);
        self.dangling.insert(model_id, prepared.dangling);
        Ok(prepared.revision.to_string())
    }

    fn replace_argument(
        &mut self,
        model_id: u32,
        express_id: u32,
        argument: usize,
        leaf_class: Option<String>,
        value: &str,
        raw: bool,
    ) -> Result<(), JsValue> {
        let replacement = if raw {
            EditValue::Raw(value.to_owned())
        } else {
            EditValue::String(value.to_owned())
        };
        self.replace_edits(
            model_id,
            &[AttributeEdit {
                express_id,
                argument_index: argument,
                leaf_class,
                value: replacement,
            }],
        )
    }

    fn replace_edits(
        &mut self,
        model_id: u32,
        replacements: &[AttributeEdit],
    ) -> Result<(), JsValue> {
        let source = self
            .sources
            .get(&model_id)
            .ok_or_else(|| JsValue::from_str("model source is not retained"))?;
        let model = self
            .models
            .get(&model_id)
            .ok_or_else(|| JsValue::from_str("model is not open"))?;
        let edited = apply_edits(source, model.image(), replacements)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        // Reparsed the way it was opened, or a limited or overridden model would not verify.
        let options = self
            .options
            .get(&model_id)
            .cloned()
            .unwrap_or_default()
            .parse_options();
        let reparsed = Model::new(parse(&edited, &options));
        if reparsed.len() != model.len()
            || replacements
                .iter()
                .any(|edit| reparsed.entity(edit.express_id).is_none())
        {
            return Err(JsValue::from_str(
                "edited IFC failed structural verification; the edit was not applied",
            ));
        }
        let revision = self
            .revisions
            .get(&model_id)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| JsValue::from_str("revision counter exhausted"))?;
        self.prepared.remove(&model_id);
        self.dangling.remove(&model_id);
        self.revisions.insert(model_id, revision);
        self.geometry.remove(&model_id);
        // A finished stream still describes the pack the viewer holds; keep it.
        if let Some(stream) = self.streams.get(&model_id)
            && !stream.session.is_finished()
        {
            self.streams.remove(&model_id);
        }
        self.sources.insert(model_id, edited);
        self.models.insert(model_id, reparsed);
        Ok(())
    }
}

struct SubsetEvaluation {
    bytes: Vec<u8>,
    outcomes: Vec<ProductOutcome>,
    diagnostics: Vec<tessifc_step::Diagnostic>,
    model_offset: [f64; 3],
    refused_boolean_products: Vec<u32>,
    stages: SubsetStages,
}

#[derive(Default)]
struct SubsetStages {
    session_ms: f64,
    evaluate_ms: f64,
    outcomes_ms: f64,
    pack_ms: f64,
}

fn failed_outcome(state: ProductState) -> bool {
    matches!(
        state,
        ProductState::EmptyOrFailed | ProductState::Pending | ProductState::NoUsableRepresentation
    )
}

fn evaluate_subset(
    model: &Model,
    express_ids: &[u32],
    options: &GeometrySettings,
) -> SubsetEvaluation {
    let started = precise_now_ms();
    let mut session = Engine::with_settings(options.geometry.clone()).session(model);
    session.restrict(express_ids);
    if let Some(offset) = options.model_offset {
        session.set_model_offset(DVec3::from_array(offset));
    }
    let total = session.total();
    let prepared = precise_now_ms();
    let mut batch = session.next(model, |_| false);
    let evaluated = precise_now_ms();
    let ids: BTreeSet<u32> = express_ids.iter().copied().collect();
    let mut outcomes: Vec<ProductOutcome> = session
        .outcomes(model)
        .into_iter()
        .filter(|outcome| ids.contains(&outcome.express_id))
        .collect();
    let classified = precise_now_ms();
    let state = PackState {
        stream: tessifc_pack::StreamState {
            known: Default::default(),
            next_geometry_id: options.first_geometry_id.unwrap_or(0),
        },
        shared: Default::default(),
    };
    let mut packer = Packer::continue_stream(
        model.image().schema.as_str(),
        session.units().length_to_m,
        session.model_offset(),
        state,
    );
    packer.set_georef(session.georef().map(str::to_string));
    packer.set_stream(StreamPosition {
        chunk: 0,
        is_final: true,
        products_done: total,
        products_total: total,
    });
    let mut refused_boolean_products = Vec::new();
    let products = batch.shapes.len();
    for shape in batch.shapes {
        let product_id = shape.express_id;
        let expected_parts = shape.parts.len();
        let before_parts = packer.instance_count();
        if shape
            .parts
            .iter()
            .any(|part| part.provenance.boolean == tessifc_geom::BooleanStatus::Refused)
        {
            refused_boolean_products.push(product_id);
        }
        // The packer starts with no known families, so a shared mesh is written once
        // inside this patch and placed by its transforms, like the initial stream.
        packer.add_shape(shape);
        if packer.instance_count() - before_parts != expected_parts {
            batch.diagnostics.push(tessifc_step::Diagnostic::error(
                tessifc_step::DiagCode("E_REVISION_PACK_FAILED"), 0,
                "candidate geometry could not be completely encoded in the viewer coordinate frame",
            ).with_id(product_id));
            if let Some(outcome) = outcomes.iter_mut().find(|o| o.express_id == product_id) {
                outcome.state = ProductState::EmptyOrFailed;
            }
        }
    }
    packer.add_diagnostics(&batch.diagnostics);
    packer.set_stat("products", products as f64);
    packer.set_stat("triangles", packer.triangles() as f64);
    let bytes = packer.finish();
    SubsetEvaluation {
        bytes,
        outcomes,
        diagnostics: batch.diagnostics,
        model_offset: session.model_offset().to_array(),
        refused_boolean_products,
        stages: SubsetStages {
            session_ms: prepared - started,
            evaluate_ms: evaluated - prepared,
            outcomes_ms: classified - evaluated,
            pack_ms: precise_now_ms() - classified,
        },
    }
}

fn references(model: &Model, id: u32) -> Vec<u32> {
    let Some(entry) = model.image().entry(id) else {
        return Vec::new();
    };
    let mut cursor = model.image().args_of(entry);
    let mut refs = Vec::new();
    while let Some(value) = cursor.read() {
        if let tessifc_step::RawValue::Ref(id) = value {
            refs.push(id);
        }
    }
    refs
}

fn dangling_references(model: &Model) -> BTreeSet<(u32, u32)> {
    let mut result = BTreeSet::new();
    for entry in &model.image().index {
        let mut cursor = model.image().args_of(entry);
        while let Some(value) = cursor.read() {
            if let tessifc_step::RawValue::Ref(target) = value
                && model.entity(target).is_none()
            {
                result.insert((entry.express_id, target));
            }
        }
    }
    result
}

fn unchanged_forward_graph(old: &Model, new: &Model, product: u32) -> bool {
    let mut pending = vec![product];
    let mut seen = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        match (old.image().entry(id), new.image().entry(id)) {
            (Some(before), Some(after)) if before.source_hash == after.source_hash => {
                pending.extend(references(new, id));
            }
            (None, None) => {}
            _ => return false,
        }
    }
    true
}

/// Reject what the permissive parser accepted but a revision must not introduce;
/// returns the candidate's dangling references for the next comparison.
fn validate_revision_model(
    old: &Model,
    new: &Model,
    old_dangling: &BTreeSet<(u32, u32)>,
) -> Result<BTreeSet<(u32, u32)>, String> {
    use tessifc_step::{DiagCode, Severity};
    let diagnostics = &new.image().diagnostics;
    if diagnostics.dropped() != 0 {
        return Err("candidate diagnostics exceeded the validation limit".into());
    }
    if let Some(diagnostic) = diagnostics.items().iter().find(|d| {
        d.severity == Severity::Error
            || matches!(
                d.code,
                DiagCode::DUPLICATE_ID
                    | DiagCode::MISSING_SEMICOLON
                    | DiagCode::BAD_HEADER
                    | DiagCode::NUMBER_OUT_OF_RANGE
            )
    }) {
        return Err(format!("candidate IFC failed validation: {diagnostic}"));
    }
    // Existing vendor arity mismatches remain readable, but edits cannot add new ones.
    for d in diagnostics
        .items()
        .iter()
        .filter(|d| d.code == DiagCode::ARITY_MISMATCH)
    {
        if !old.image().diagnostics.items().iter().any(|prior| {
            prior.code == d.code && prior.express_id == d.express_id && prior.message == d.message
        }) {
            return Err(format!("candidate IFC introduced an arity mismatch: {d}"));
        }
    }
    let dangling = dangling_references(new);
    if let Some((owner, target)) = dangling.difference(old_dangling).next() {
        return Err(format!(
            "candidate IFC introduced dangling reference #{owner} -> #{target}"
        ));
    }
    Ok(dangling)
}

/// The permissive parser accepts partial input for viewing. Revision publication
/// additionally requires complete sections and an actual end marker outside literals.
fn validate_complete_source(source: &[u8]) -> Result<(), String> {
    let mut pos = if source.starts_with(&[0xef, 0xbb, 0xbf]) {
        3
    } else {
        0
    };
    let mut depth = 0usize;
    let mut first = String::new();
    let mut started = false;
    let mut section = false;
    let mut header = false;
    let mut data = false;
    let mut ended = false;
    while pos < source.len() {
        let byte = source[pos];
        if byte.is_ascii_whitespace() {
            pos += 1;
            continue;
        }
        if source[pos..].starts_with(b"/*") {
            pos += 2;
            let Some(end) = source[pos..].windows(2).position(|w| w == b"*/") else {
                return Err("candidate IFC contains an unclosed comment".into());
            };
            pos += end + 2;
            continue;
        }
        if ended {
            return Err("candidate IFC has content after its end marker".into());
        }
        if byte == b'\'' || byte == b'"' {
            if first.is_empty() {
                first.push('?');
            }
            let quote = byte;
            pos += 1;
            loop {
                if pos >= source.len() {
                    return Err("candidate IFC contains an unclosed literal".into());
                }
                if source[pos] == quote {
                    pos += 1;
                    if quote == b'\'' && source.get(pos) == Some(&quote) {
                        pos += 1;
                    } else {
                        break;
                    }
                } else {
                    pos += 1;
                }
            }
            continue;
        }
        if byte.is_ascii_alphabetic() {
            let begin = pos;
            while pos < source.len()
                && (source[pos].is_ascii_alphanumeric() || matches!(source[pos], b'_' | b'-'))
            {
                pos += 1;
            }
            if first.is_empty() {
                first = String::from_utf8_lossy(&source[begin..pos]).to_ascii_uppercase();
            }
            continue;
        }
        match byte {
            b'(' => {
                depth += 1;
            }
            b')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("candidate IFC has unmatched parentheses")?;
            }
            b';' if depth == 0 => {
                if !started {
                    if first != "ISO-10303-21" {
                        return Err("candidate IFC has no STEP signature".into());
                    }
                    started = true;
                } else {
                    match first.as_str() {
                        "HEADER" | "DATA" | "ANCHOR" | "REFERENCE" | "SIGNATURE" => {
                            if section {
                                return Err("candidate IFC has an unclosed section".into());
                            }
                            header |= first == "HEADER";
                            data |= first == "DATA";
                            section = true;
                        }
                        "ENDSEC" => {
                            if !section {
                                return Err("candidate IFC has an unexpected ENDSEC".into());
                            }
                            section = false;
                        }
                        "END-ISO-10303-21" => {
                            if section || !header || !data {
                                return Err("candidate IFC has incomplete sections".into());
                            }
                            ended = true;
                        }
                        _ if !section => {
                            return Err("candidate IFC has a record outside a section".into());
                        }
                        _ => {}
                    }
                }
                first.clear();
                pos += 1;
                continue;
            }
            _ => {}
        }
        if first.is_empty() {
            first.push('?');
        }
        pos += 1;
    }
    if !ended {
        return Err("candidate IFC is incomplete: missing final END-ISO-10303-21;".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_engine::pack::instance_flags;
    use tessifc_pack::{INSTANCE_OPENING, INSTANCE_SPACE, INSTANCE_TRANSPARENT};

    const TINY: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
                          #1=IFCWALL('g',$,'W',$,$,$,$,$,$);\nENDSEC;\nEND-ISO-10303-21;\n";

    const HIERARCHY: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCPROJECT('p',$,'Project',$,$,$,$,(),$);\n\
#2=IFCSITE('s',$,'Site',$,$,$,$,$,.ELEMENT.,$,$,$,$,$);\n\
#3=IFCBUILDING('b',$,'Building',$,$,$,$,'Building',.ELEMENT.,$,$,$);\n\
#4=IFCBUILDINGSTOREY('l',$,'Ground floor',$,$,$,$,'Ground floor',.ELEMENT.,0.);\n\
#5=IFCWALLSTANDARDCASE('w',$,'Wall A',$,$,$,$,$,$);\n\
#10=IFCRELAGGREGATES('r1',$,$,$,#1,(#2));\n\
#11=IFCRELAGGREGATES('r2',$,$,$,#2,(#3));\n\
#12=IFCRELAGGREGATES('r3',$,$,$,#3,(#4));\n\
#13=IFCRELCONTAINEDINSPATIALSTRUCTURE('r4',$,$,$,(#5),#4);\n\
ENDSEC;\nEND-ISO-10303-21;\n";

    const BOX: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
#2=IFCDIRECTION((0.,0.,1.));\n\
#3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n\
#4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
#5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));\n\
#6=IFCCARTESIANPOINT((0.,0.,0.));\n\
#7=IFCAXIS2PLACEMENT3D(#6,$,$);\n\
#8=IFCLOCALPLACEMENT($,#7);\n\
#9=IFCWALL('a',$,'W1',$,$,#8,#5,$,$);\n\
ENDSEC;\nEND-ISO-10303-21;\n";

    /// Two walls and a family placed twice, for streaming.
    const FOUR_PRODUCTS: &[u8] =
        b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
#2=IFCDIRECTION((0.,0.,1.));\n\
#3=IFCEXTRUDEDAREASOLID(#1,$,#2,3.);\n\
#4=IFCSHAPEREPRESENTATION($,'Body','SweptSolid',(#3));\n\
#5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));\n\
#6=IFCCARTESIANPOINT((0.,0.,0.));\n\
#7=IFCAXIS2PLACEMENT3D(#6,$,$);\n\
#8=IFCLOCALPLACEMENT($,#7);\n\
#9=IFCWALL('a',$,'W1',$,$,#8,#5,$,$);\n\
#10=IFCCARTESIANPOINT((10.,0.,0.));\n\
#11=IFCAXIS2PLACEMENT3D(#10,$,$);\n\
#12=IFCLOCALPLACEMENT($,#11);\n\
#13=IFCWALL('b',$,'W2',$,$,#12,#5,$,$);\n\
#20=IFCREPRESENTATIONMAP(#7,#4);\n\
#21=IFCMAPPEDITEM(#20,$);\n\
#22=IFCSHAPEREPRESENTATION($,'Body','MappedRepresentation',(#21));\n\
#23=IFCPRODUCTDEFINITIONSHAPE($,$,(#22));\n\
#30=IFCCARTESIANPOINT((20.,0.,0.));\n\
#31=IFCAXIS2PLACEMENT3D(#30,$,$);\n\
#32=IFCLOCALPLACEMENT($,#31);\n\
#33=IFCFURNISHINGELEMENT('c1',$,$,$,$,#32,#23,$);\n\
#40=IFCCARTESIANPOINT((30.,0.,0.));\n\
#41=IFCAXIS2PLACEMENT3D(#40,$,$);\n\
#42=IFCLOCALPLACEMENT($,#41);\n\
#43=IFCFURNISHINGELEMENT('c2',$,$,$,$,#42,#23,$);\n\
ENDSEC;\nEND-ISO-10303-21;\n";

    fn json_of(pack: &[u8]) -> serde_json::Value {
        let json_len = u32::from_le_bytes(pack[8..12].try_into().unwrap()) as usize;
        serde_json::from_str(std::str::from_utf8(&pack[24..24 + json_len]).unwrap()).unwrap()
    }

    #[test]
    fn malformed_geometry_options_are_rejected_without_integer_wrapping() {
        for json in [
            "{",
            "null",
            "[]",
            r#"{"circleSegments":4294967297}"#,
            r#"{"circleSegments":2}"#,
            r#"{"chordToleranceM":0}"#,
            r#"{"chordToleranceM":-1}"#,
            r#"{"maxDepth":0}"#,
            r#"{"includeSpaces":"yes"}"#,
            r#"{"modelOffset":[1,"bad",3]}"#,
            r#"{"firstGeometryId":4294967296}"#,
        ] {
            assert!(
                GeometrySettings::from_json(json).is_err(),
                "accepted {json}"
            );
        }
        let settings = GeometrySettings::from_json(
            r#"{"chordToleranceM":0.0005,"weld":false,"maxDepth":32,"futureOption":true}"#,
        )
        .unwrap();
        assert_eq!(settings.geometry.chord_tolerance_m, 0.0005);
        assert!(!settings.geometry.weld);
        assert_eq!(settings.geometry.max_depth, 32);
    }

    #[test]
    fn an_empty_stream_emits_one_final_pack_with_parse_diagnostics() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(b"not IFC".to_vec(), None).unwrap();
        kernel.begin_geometry_stream(id, None).unwrap().unwrap();
        let chunk = kernel
            .next_geometry_chunk(id, 0.0, 1, 0)
            .expect("final metadata chunk");
        let pack = json_of(&chunk);
        assert_eq!(pack["stream"]["final"], true);
        assert!(
            pack["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "E_NOT_A_STEP_FILE")
        );
        assert!(kernel.next_geometry_chunk(id, 0.0, 1, 0).is_none());
        assert!(kernel.cancel_geometry_stream(id));
        assert!(kernel.get_model_info(id).is_some());
        assert!(!kernel.cancel_geometry_stream(id));
    }

    #[test]
    fn pack_flags_preserve_semantic_volumes_and_transparency() {
        assert_eq!(instance_flags("IfcWall", [1, 2, 3, 255]), 0);
        assert_eq!(
            instance_flags("IfcSpace", [1, 2, 3, 120]),
            INSTANCE_SPACE | INSTANCE_TRANSPARENT
        );
        assert_eq!(
            instance_flags("IfcOpeningElement", [1, 2, 3, 255]),
            INSTANCE_OPENING
        );
    }

    #[test]
    fn open_report_close() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(TINY.to_vec(), None).unwrap();
        assert_eq!(kernel.model_count(), 1);

        let info: serde_json::Value =
            serde_json::from_str(&kernel.get_model_info(id).unwrap()).unwrap();
        assert_eq!(info["schema"], "IFC4");
        assert_eq!(info["entities"], 1);
        assert_eq!(info["products"]["IfcWall"], 1);
        assert_eq!(info["productTotal"], 1);

        assert!(kernel.close_model(id));
        assert_eq!(kernel.model_count(), 0);
        assert!(kernel.get_model_info(id).is_none());
        assert!(!kernel.close_model(id));
    }

    #[test]
    fn geometry_summary_reports_diagnostic_severity() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(TINY.to_vec(), None).unwrap();
        let summary: serde_json::Value = serde_json::from_str(
            &kernel
                .evaluate_geometry(id, None)
                .unwrap()
                .expect("an open model should evaluate"),
        )
        .unwrap();
        assert_eq!(summary["diagnostics"], 1);
        assert_eq!(summary["diagnosticInfos"], 0);
        assert_eq!(summary["diagnosticWarnings"], 1);
        assert_eq!(summary["diagnosticErrors"], 0);
    }

    #[test]
    fn taking_a_pack_matches_borrowed_output_and_releases_only_geometry() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        kernel
            .evaluate_geometry(id, None)
            .unwrap()
            .expect("an open model should evaluate");
        assert_eq!(kernel.shape_count(id), 1);

        let borrowed = kernel.get_pack(id).expect("evaluated geometry has a pack");
        let owned = kernel
            .take_pack(id)
            .expect("evaluated geometry can be transferred");
        assert_eq!(owned, borrowed);
        assert_eq!(kernel.shape_count(id), 0);
        assert!(kernel.get_model_info(id).is_some());
        assert!(kernel.take_pack(id).is_none());
    }

    #[test]
    fn a_stream_delivers_the_same_records_as_one_pack() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(FOUR_PRODUCTS.to_vec(), None).unwrap();
        kernel.evaluate_geometry(id, None).unwrap().unwrap();
        let whole = json_of(&kernel.take_pack(id).unwrap());

        let summary: serde_json::Value =
            serde_json::from_str(&kernel.begin_geometry_stream(id, None).unwrap().unwrap())
                .unwrap();
        assert_eq!(summary["products"], 4);
        assert_eq!(summary["modelOffset"], whole["model_offset"]);

        let mut chunks = Vec::new();
        while let Some(chunk) = kernel.next_geometry_chunk(id, 0.0, 1, 0) {
            chunks.push(json_of(&chunk));
        }
        assert_eq!(chunks.len(), 4, "one product per chunk was asked for");
        assert_eq!(chunks[0]["stream"]["final"], false);
        assert_eq!(chunks[3]["stream"]["final"], true);
        assert_eq!(chunks[3]["stream"]["products_done"], 4);
        let progress: serde_json::Value =
            serde_json::from_str(&kernel.stream_progress(id).unwrap()).unwrap();
        assert_eq!(progress["finished"], true);
        assert_eq!(progress["emitted"], 4);

        // Identical bytes share one mesh, so the stream must also reach two.
        assert_eq!(whole["geometries"].as_array().unwrap().len(), 2);
        let streamed_geometries: usize = chunks
            .iter()
            .map(|chunk| chunk["geometries"].as_array().unwrap().len())
            .sum();
        assert_eq!(streamed_geometries, 2);
        let streamed_records: u64 = chunks
            .iter()
            .map(|chunk| chunk["instances"]["count"].as_u64().unwrap())
            .sum();
        assert_eq!(
            streamed_records,
            whole["instances"]["count"].as_u64().unwrap()
        );
        assert_eq!(chunks[3]["stats"]["products"], 4);

        // The hierarchy knows what the stream drew.
        let hierarchy: serde_json::Value =
            serde_json::from_str(&kernel.get_spatial_hierarchy(id).unwrap()).unwrap();
        let rendered = hierarchy["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["rendered"] == true)
            .count();
        assert_eq!(rendered, 4);
    }

    #[test]
    fn a_few_products_can_be_re_evaluated_into_a_patch() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(FOUR_PRODUCTS.to_vec(), None).unwrap();
        let options = r#"{"modelOffset":[1.0,2.0,3.0],"firstGeometryId":40}"#;
        let patch = json_of(
            &kernel
                .evaluate_products(id, &[13, 43, 999], Some(options.into()))
                .unwrap()
                .unwrap(),
        );
        assert_eq!(patch["instances"]["count"], 2);
        let offset: Vec<f64> = patch["model_offset"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_f64().unwrap())
            .collect();
        assert_eq!(offset, vec![1.0, 2.0, 3.0]);
        assert_eq!(patch["geometries"][0]["id"], 40);
        assert_eq!(patch["stream"]["final"], true);
        let none = json_of(&kernel.evaluate_products(id, &[999], None).unwrap().unwrap());
        assert_eq!(none["instances"]["count"], 0);
    }

    #[test]
    fn class_supertypes_walk_to_the_root() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(TINY.to_vec(), None).unwrap();
        let chain: Vec<String> = serde_json::from_str(
            &kernel
                .get_class_supertypes(id, "IfcWallStandardCase")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(chain[0], "IfcWallStandardCase");
        assert_eq!(chain[1], "IfcWall");
        assert!(chain.iter().any(|name| name == "IfcProduct"));
        assert_eq!(chain.last().map(String::as_str), Some("IfcRoot"));
        assert!(kernel.get_class_supertypes(id, "IfcSpaceship").is_none());
        assert!(kernel.get_class_supertypes(99, "IfcWall").is_none());
    }

    #[test]
    fn spatial_hierarchy_follows_aggregation_and_containment() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(HIERARCHY.to_vec(), None).unwrap();
        let hierarchy: serde_json::Value =
            serde_json::from_str(&kernel.get_spatial_hierarchy(id).unwrap()).unwrap();
        let nodes = hierarchy["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 5);

        let by_id: BTreeMap<u64, &serde_json::Value> = nodes
            .iter()
            .map(|node| (node["expressId"].as_u64().unwrap(), node))
            .collect();
        assert_eq!(by_id[&1]["parentExpressId"], serde_json::Value::Null);
        assert_eq!(by_id[&2]["parentExpressId"], 1);
        assert_eq!(by_id[&3]["parentExpressId"], 2);
        assert_eq!(by_id[&4]["parentExpressId"], 3);
        assert_eq!(by_id[&5]["parentExpressId"], 4);
        assert_eq!(by_id[&4]["name"], "Ground floor");
        assert_eq!(by_id[&5]["class"], "IfcWallStandardCase");
        assert_eq!(
            by_id[&5]["rendered"], true,
            "before any evaluation every product counts as rendered"
        );
    }

    #[test]
    fn text_edits_reparse_and_export_without_rewriting_the_file() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(TINY.to_vec(), None).unwrap();
        let before = kernel.export_model(id).unwrap();
        let info = kernel
            .set_attribute(id, 1, "Name", "O'Brien wall", false)
            .unwrap();
        let info: serde_json::Value = serde_json::from_str(&info).unwrap();
        assert_eq!(info["fields"][2]["value"], "O'Brien wall");

        let after = kernel.export_model(id).unwrap();
        assert_ne!(after, before);
        let text = String::from_utf8(after).unwrap();
        assert!(text.contains("'O''Brien wall'"));
        assert!(text.contains("FILE_SCHEMA(('IFC4'))"));
        assert_eq!(kernel.model_count(), 1);
    }

    #[test]
    fn complex_leaf_edits_use_named_schema_locations() {
        let source = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n\
#1=(IFCNAMEDUNIT(*,.LENGTHUNIT.)IFCSIUNIT($,.METRE.));\nENDSEC;\n";
        let mut kernel = Kernel::new();
        let id = kernel.open_model(source.to_vec(), None).unwrap();
        let before: serde_json::Value =
            serde_json::from_str(&kernel.get_entity_info(id, 1).unwrap()).unwrap();
        assert_eq!(before["complex"], true);
        assert_eq!(before["fields"].as_array().unwrap().len(), 4);
        kernel.set_attribute(id, 1, "Name", ".FOOT.", true).unwrap();
        let exported = String::from_utf8(kernel.export_model(id).unwrap()).unwrap();
        assert!(exported.contains("IFCSIUNIT($,.FOOT.)"));
    }

    #[test]
    fn several_models_are_independent() {
        let mut kernel = Kernel::new();
        let a = kernel.open_model(TINY.to_vec(), None).unwrap();
        let b = kernel
            .open_model(b"not a step file at all".to_vec(), None)
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(kernel.model_count(), 2);

        let info_b: serde_json::Value =
            serde_json::from_str(&kernel.get_model_info(b).unwrap()).unwrap();
        assert_eq!(info_b["entities"], 0);
        assert!(info_b["diagnostics"]["errors"].as_u64().unwrap() > 0);

        kernel.close_model(a);
        assert!(kernel.get_model_info(b).is_some());
        kernel.close_all();
        assert_eq!(kernel.model_count(), 0);
    }

    /// Needs a second schema to switch to, so it only runs when one is built in.
    #[test]
    #[cfg(feature = "schema-ifc2x3")]
    fn settings_override_the_schema() {
        let mut kernel = Kernel::new();
        let id = kernel
            .open_model(TINY.to_vec(), Some(r#"{"schemaOverride":"IFC2X3"}"#.into()))
            .unwrap();
        let info: serde_json::Value =
            serde_json::from_str(&kernel.get_model_info(id).unwrap()).unwrap();
        assert_eq!(info["schema"], "IFC2X3");
    }

    #[test]
    fn ids_of_type_walks_subtypes() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(TINY.to_vec(), None).unwrap();
        assert_eq!(kernel.get_ids_of_type(id, "IfcProduct"), vec![1]);
        assert_eq!(kernel.get_ids_of_type(id, "IfcSlab"), Vec::<u32>::new());
        assert_eq!(kernel.get_ids_of_type(id, "NotAClass"), Vec::<u32>::new());
        assert_eq!(kernel.get_class_name(id, 1).as_deref(), Some("IfcWall"));
        assert_eq!(kernel.get_class_name(id, 99), None);
    }

    #[test]
    fn a_limited_model_is_edited_with_the_options_it_was_opened_with() {
        let mut kernel = Kernel::new();
        let id = kernel
            .open_model(FOUR_PRODUCTS.to_vec(), Some(r#"{"maxEntities":3}"#.into()))
            .unwrap();
        let info: serde_json::Value =
            serde_json::from_str(&kernel.get_model_info(id).unwrap()).unwrap();
        assert_eq!(info["entities"], 3);
        kernel
            .set_attribute(id, 1, "ProfileName", "kept", false)
            .expect("the edit reparses with the same limit");
    }

    #[test]
    fn invalid_open_settings_are_refused() {
        // The JS error value cannot be built outside a browser, so the parser is checked directly.
        assert!(OpenOptions::from_json("{not json").is_err());
        assert!(OpenOptions::from_json(r#"{"schemaOverride":"IFC9"}"#).is_err());
        assert!(OpenOptions::from_json(r#"{"maxEntities":-1}"#).is_err());
        let options = OpenOptions::from_json(r#"{"maxEntities":7,"other":true}"#).unwrap();
        assert_eq!(options.max_entities, Some(7));
    }

    #[test]
    fn a_patch_keeps_shared_families_and_carries_stats() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(FOUR_PRODUCTS.to_vec(), None).unwrap();
        let options = r#"{"firstGeometryId":40}"#;
        let patch = json_of(
            &kernel
                .evaluate_products(id, &[33, 43], Some(options.into()))
                .unwrap()
                .unwrap(),
        );
        assert_eq!(patch["instances"]["count"], 2);
        assert_eq!(patch["geometries"].as_array().unwrap().len(), 1);
        assert_eq!(patch["geometries"][0]["id"], 40);
        assert_eq!(patch["stats"]["products"], 2.0);
        assert!(patch["stats"]["triangles"].as_f64().unwrap() > 0.0);

        // A full rebuild through the revision path shares what the stream shares.
        kernel.evaluate_geometry(id, None).unwrap();
        let whole = json_of(&kernel.take_pack(id).unwrap());
        let renamed_guid = String::from_utf8_lossy(FOUR_PRODUCTS)
            .replace("IFCWALL('a'", "IFCWALL('z'")
            .into_bytes();
        let report: serde_json::Value = serde_json::from_str(
            &kernel
                .prepare_revision_inner(id, renamed_guid, "0")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(report["fullRebuild"], true);
        let settings = GeometrySettings::from_json(r#"{"firstGeometryId":100}"#).unwrap();
        let rebuilt = json_of(
            &kernel
                .evaluate_prepared_revision_inner(id, settings)
                .unwrap(),
        );
        assert_eq!(rebuilt["instances"]["count"], whole["instances"]["count"]);
        assert_eq!(
            rebuilt["geometries"].as_array().unwrap().len(),
            whole["geometries"].as_array().unwrap().len()
        );
    }

    #[test]
    fn dangling_references_are_checked_against_the_committed_model() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        let renamed = String::from_utf8_lossy(BOX)
            .replace("'W1'", "'W2'")
            .into_bytes();
        kernel
            .prepare_revision_inner(id, renamed.clone(), "0")
            .unwrap();
        assert_eq!(kernel.commit_revision_inner(id, "0").unwrap(), "1");
        assert!(kernel.dangling.contains_key(&id));
        let dangling = String::from_utf8_lossy(&renamed)
            .replace("#8=IFCLOCALPLACEMENT($,#7)", "#8=IFCLOCALPLACEMENT($,#77)")
            .into_bytes();
        let refused = kernel
            .prepare_revision_inner(id, dangling, "1")
            .unwrap_err();
        assert!(
            refused.contains("dangling reference #8 -> #77"),
            "{refused}"
        );
        // A legacy edit replaces the model; the next comparison starts from a fresh scan.
        kernel.set_attribute(id, 9, "Name", "W3", false).unwrap();
        assert!(!kernel.dangling.contains_key(&id));
        let again = String::from_utf8_lossy(&kernel.export_model(id).unwrap())
            .replace("'W3'", "'W4'")
            .into_bytes();
        kernel.prepare_revision_inner(id, again, "2").unwrap();
        assert!(kernel.dangling.contains_key(&id));
    }

    #[test]
    fn the_prepared_report_carries_stage_timings() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        let edited = String::from_utf8_lossy(BOX)
            .replace("#2,3.)", "#2,5.)")
            .into_bytes();
        let report: serde_json::Value =
            serde_json::from_str(&kernel.prepare_revision_inner(id, edited, "0").unwrap()).unwrap();
        let timings = &report["timings"];
        let stage = |name: &str| timings[name].as_f64().unwrap();
        assert!(stage("prepareMs") >= stage("parseMs") && stage("parseMs") >= 0.0);
        assert_eq!(stage("evaluateTotalMs"), 0.0);
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::from_json("{}").unwrap())
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_str(&kernel.get_prepared_revision_info(id).unwrap()).unwrap();
        let after = &report["timings"];
        assert!(
            after["evaluateTotalMs"].as_f64().unwrap() >= after["evaluateMs"].as_f64().unwrap()
        );
        assert!(
            after["sessionMs"].as_f64().unwrap() >= 0.0 && after["packMs"].as_f64().unwrap() >= 0.0
        );
    }

    #[test]
    fn a_patch_id_without_headroom_is_refused() {
        assert!(GeometrySettings::from_json(r#"{"firstGeometryId":4294967295}"#).is_err());
        assert!(GeometrySettings::from_json(r#"{"firstGeometryId":1000}"#).is_ok());
    }

    #[test]
    fn revisions_stage_geometry_before_publishing_and_enforce_the_base() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        let edited = String::from_utf8_lossy(BOX)
            .replace("#2,3.)", "#2,5.)")
            .into_bytes();
        let report: serde_json::Value = serde_json::from_str(
            &kernel
                .prepare_revision_inner(id, edited.clone(), "0")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(report["affectedProducts"], serde_json::json!([9]));
        assert_eq!(report["revision"], "1");
        assert_eq!(kernel.export_model(id).unwrap(), BOX);
        assert!(kernel.commit_revision_inner(id, "0").is_err());
        assert!(
            kernel
                .prepare_revision_inner(id, edited.clone(), "0")
                .is_err()
        );
        let options =
            GeometrySettings::from_json(r#"{"modelOffset":[100,200,300],"firstGeometryId":400}"#)
                .unwrap();
        let pack = json_of(
            &kernel
                .evaluate_prepared_revision_inner(id, options)
                .unwrap(),
        );
        assert_eq!(pack["model_offset"], serde_json::json!([100, 200, 300]));
        assert_eq!(pack["geometries"][0]["id"], 400);
        let report: serde_json::Value =
            serde_json::from_str(&kernel.get_prepared_revision_info(id).unwrap()).unwrap();
        assert_eq!(report["evaluationAccepted"], true);
        assert_eq!(report["productOutcomes"][0]["state"], "emitted");
        assert_eq!(kernel.commit_revision_inner(id, "0").unwrap(), "1");
        assert_eq!(kernel.export_model(id).unwrap(), edited);
        assert_eq!(kernel.get_model_revision(id).as_deref(), Some("1"));
        assert!(
            kernel
                .prepare_revision_inner(id, BOX.to_vec(), "0")
                .is_err()
        );
        assert!(kernel.get_prepared_revision_info(id).is_none());
    }

    #[test]
    fn revision_attribute_batches_are_source_preserving_and_legacy_edits_invalidate_them() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        kernel
            .prepare_attribute_edits_inner(
                id,
                r#"[
            {"expressId":9,"attribute":"Name","value":"O'Brien"},
            {"expressId":3,"attribute":"Depth","value":"4.","raw":true}
        ]"#,
                "0",
            )
            .unwrap();
        let staged = &kernel.prepared[&id].source;
        assert_eq!(
            String::from_utf8_lossy(staged),
            String::from_utf8_lossy(BOX)
                .replace("'W1'", "'O''Brien'")
                .replace("#2,3.)", "#2,4.)")
        );
        assert_eq!(kernel.export_model(id).unwrap(), BOX);
        kernel
            .set_attribute(id, 9, "Name", "legacy", false)
            .unwrap();
        assert_eq!(kernel.get_model_revision(id).as_deref(), Some("1"));
        assert!(kernel.get_prepared_revision_info(id).is_none());
        assert!(kernel.commit_revision_inner(id, "0").is_err());
    }

    #[test]
    fn candidate_source_validation_rejects_truncation_hidden_markers_and_duplicate_ids() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        let text = String::from_utf8_lossy(BOX);
        let malformed = [
            text.replace("END-ISO-10303-21;", ""),
            text.replace("ENDSEC;\nEND-ISO", "END-ISO"),
            text.replace("END-ISO-10303-21;", "/* END-ISO-10303-21; */"),
            text.replace("END-ISO-10303-21;", "'END-ISO-10303-21;'"),
            text.replace("END-ISO-10303-21;", "END-ISO-10303-21"),
            text.replace("#2,3.)", "#999,3.)"),
            text.replace("#2,3.)", "#2,3.,4.)"),
            text.replace("#2,3.)", "#2,1.e999)"),
            text.replace("#1=IFCRECTANGLE", "#2=IFCRECTANGLE"),
            format!("{text}#99=IFCWALL($);"),
        ];
        for candidate in malformed {
            assert!(
                kernel
                    .prepare_revision_inner(id, candidate.into_bytes(), "0")
                    .is_err()
            );
            assert_eq!(kernel.export_model(id).unwrap(), BOX);
            assert!(kernel.get_prepared_revision_info(id).is_none());
        }
        let commented = format!("/* leading */ {text} /* final */");
        kernel
            .prepare_revision_inner(id, commented.into_bytes(), "0")
            .unwrap();
        assert_eq!(kernel.commit_revision_inner(id, "0").unwrap(), "1");
    }

    #[test]
    fn revisions_accept_creation_and_deletion_and_keep_open_options() {
        let mut kernel = Kernel::new();
        let id = kernel
            .open_model(
                TINY.to_vec(),
                Some(r#"{"schemaOverride":"IFC2X3","maxEntities":2}"#.into()),
            )
            .unwrap();
        let effective_schema = kernel.models[&id].image().schema;
        let schema = kernel.models[&id].schema();
        let wall_arity = schema.arity(schema.class_by_name("IfcWall").unwrap());
        let wall_record = |id, guid: &str, name: &str| {
            let mut arguments = vec![format!("'{guid}'"), "$".into(), format!("'{name}'")];
            arguments.resize(wall_arity, "$".into());
            format!("#{id}=IFCWALL({});\nENDSEC;\nEND-ISO", arguments.join(","))
        };
        let added =
            String::from_utf8_lossy(TINY).replace("ENDSEC;\nEND-ISO", &wall_record(2, "h", "New"));
        // Single-schema builds may approximate the override. New records must
        // match the effective schema; the original record remains unchanged.
        kernel
            .prepare_revision_inner(id, added.clone().into_bytes(), "0")
            .unwrap();
        assert_eq!(kernel.prepared[&id].model.image().schema, effective_schema);
        assert_eq!(kernel.options[&id].schema_override, Some(SchemaId::Ifc2x3));
        assert_eq!(kernel.prepared[&id].impact.created_entities, vec![2]);
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        kernel.commit_revision_inner(id, "0").unwrap();
        let too_many = added.replace("ENDSEC;\nEND-ISO", &wall_record(3, "i", "Extra"));
        assert!(
            kernel
                .prepare_revision_inner(id, too_many.into_bytes(), "1")
                .is_err()
        );
        kernel
            .prepare_revision_inner(id, TINY.to_vec(), "1")
            .unwrap();
        assert_eq!(kernel.prepared[&id].impact.deleted_entities, vec![2]);
        kernel.commit_revision_inner(id, "1").unwrap();
    }

    #[test]
    fn failed_candidate_geometry_cannot_replace_a_renderable_product() {
        let mut kernel = Kernel::new();
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        let broken = String::from_utf8_lossy(BOX).replace("#1,$,#2,3.", "$,$,#2,3.");
        kernel
            .prepare_revision_inner(id, broken.into_bytes(), "0")
            .unwrap();
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        assert!(!kernel.prepared[&id].accepted);
        assert!(kernel.commit_revision_inner(id, "0").is_err());
        assert_eq!(kernel.export_model(id).unwrap(), BOX);
        let token = kernel.prepared[&id].token.to_string();
        assert!(kernel.discard_revision_inner(id, &token).unwrap());
        let unusable = String::from_utf8_lossy(BOX).replace("'Body'", "'Axis'");
        kernel
            .prepare_revision_inner(id, unusable.into_bytes(), "0")
            .unwrap();
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        assert!(!kernel.prepared[&id].accepted);
        assert!(kernel.commit_revision_inner(id, "0").is_err());
        kernel.close_model(id);
        assert!(kernel.get_model_revision(id).is_none());
        assert!(kernel.get_prepared_revision_info(id).is_none());
    }

    #[test]
    fn broad_revisions_preserve_unchanged_failed_products_and_allow_representation_removal() {
        let mut kernel = Kernel::new();
        let context = "#100=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,0.00001,#7,$);\n";
        let broken = String::from_utf8_lossy(BOX)
            .replace("#1,$,#2,3.", "$,$,#2,3.")
            .replace("ENDSEC;\nEND-ISO", &format!("{context}ENDSEC;\nEND-ISO"));
        let id = kernel.open_model(broken.as_bytes().to_vec(), None).unwrap();
        let revised = broken.replace("0.00001", "0.00002");
        kernel
            .prepare_revision_inner(id, revised.into_bytes(), "0")
            .unwrap();
        assert!(kernel.prepared[&id].impact.full_rebuild);
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        assert!(
            kernel.prepared[&id].accepted,
            "{:?}",
            kernel.prepared[&id].diagnostics
        );
        kernel.commit_revision_inner(id, "0").unwrap();

        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        kernel
            .prepare_attribute_edits_inner(
                id,
                r#"[{"expressId":9,"attribute":"Representation","value":"$","raw":true}]"#,
                "0",
            )
            .unwrap();
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        assert!(kernel.prepared[&id].accepted);
        assert_eq!(
            kernel.prepared[&id].outcomes[0].state,
            ProductState::NoRepresentation
        );
        assert_eq!(kernel.commit_revision_inner(id, "0").unwrap(), "1");
    }

    #[test]
    fn candidate_handles_settings_frames_and_postcommit_outcomes_remain_consistent() {
        let mut kernel = Kernel::new();
        let with_empty = String::from_utf8_lossy(BOX).replace(
            "ENDSEC;\nEND-ISO",
            "#20=IFCWALL('empty',$,'Empty',$,$,$,$,$,$);\nENDSEC;\nEND-ISO",
        );
        let id = kernel.open_model(with_empty.into_bytes(), None).unwrap();
        kernel
            .evaluate_geometry(id, Some(r#"{"circleSegments":40}"#.into()))
            .unwrap();
        kernel.take_pack(id);
        kernel
            .prepare_attribute_edits_inner(
                id,
                r#"[{"expressId":9,"attribute":"Name","value":"New name"}]"#,
                "0",
            )
            .unwrap();
        let token = kernel.prepared[&id].token.to_string();
        assert!(
            kernel
                .evaluate_prepared_revision_inner(id, GeometrySettings::default())
                .is_err()
        );
        assert!(
            kernel
                .evaluate_prepared_revision_inner(
                    id,
                    GeometrySettings::from_json(
                        r#"{"circleSegments":40,"modelOffset":[1e300,0,0]}"#
                    )
                    .unwrap()
                )
                .is_err()
        );
        assert!(kernel.discard_revision_inner(id, &token).unwrap());
        kernel
            .prepare_attribute_edits_inner(
                id,
                r#"[{"expressId":9,"attribute":"Name","value":"New name"}]"#,
                "0",
            )
            .unwrap();
        assert!(kernel.check_candidate_token(id, &token).is_err());
        assert!(kernel.discard_revision_inner(id, &token).is_err());
        assert!(kernel.get_prepared_revision_info(id).is_some());
        kernel
            .evaluate_prepared_revision_inner(
                id,
                GeometrySettings::from_json(r#"{"circleSegments":40}"#).unwrap(),
            )
            .unwrap();
        kernel.commit_revision_inner(id, "0").unwrap();
        assert!(!kernel.discard_revision_inner(id, &token).unwrap());
        let hierarchy: serde_json::Value =
            serde_json::from_str(&kernel.get_spatial_hierarchy(id).unwrap()).unwrap();
        assert!(
            hierarchy["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["expressId"] == 9 && n["rendered"] == true && n["globalId"] == "a")
        );
        assert!(
            !hierarchy["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["expressId"] == 20 && n["rendered"] == true)
        );
        let outcomes = kernel.current_product_outcomes(id).unwrap();
        assert_eq!(
            outcomes.iter().find(|o| o.express_id == 9).unwrap().state,
            ProductState::Emitted
        );
        assert_eq!(
            outcomes.iter().find(|o| o.express_id == 20).unwrap().state,
            ProductState::NoRepresentation
        );

        // Without an existing frame, packing must still reject f32 overflow.
        let id = kernel.open_model(BOX.to_vec(), None).unwrap();
        kernel
            .prepare_attribute_edits_inner(
                id,
                r#"[{"expressId":3,"attribute":"Depth","value":"4.","raw":true}]"#,
                "0",
            )
            .unwrap();
        kernel
            .evaluate_prepared_revision_inner(
                id,
                GeometrySettings::from_json(r#"{"modelOffset":[1e300,0,0]}"#).unwrap(),
            )
            .unwrap();
        assert!(!kernel.prepared[&id].accepted);
        assert!(
            kernel.prepared[&id]
                .diagnostics
                .iter()
                .any(|d| d.code.as_str() == "E_REVISION_PACK_FAILED")
        );
        assert!(kernel.commit_revision_inner(id, "0").is_err());
    }
}
