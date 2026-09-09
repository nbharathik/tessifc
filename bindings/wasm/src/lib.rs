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
use tessifc_engine::{Engine, EvaluationResult, Session};
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

/// A TessIFC instance holding open models.
#[wasm_bindgen]
pub struct Kernel {
    models: BTreeMap<u32, Model>,
    /// Original or most recently edited IFC bytes, kept apart from the model image.
    sources: BTreeMap<u32, Vec<u8>>,
    /// What each model was opened with.
    options: BTreeMap<u32, OpenOptions>,
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
                (None, None) => None,
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
        let model = self.models.get(&model_id)?;
        let outcomes = if let Some(stream) = self.streams.get(&model_id) {
            stream.session.outcomes(model)
        } else {
            let result = self.geometry.get(&model_id)?;
            Engine::with_settings(result.settings.clone()).outcomes(model, result)
        };
        serde_json::to_string(&outcomes).ok()
    }

    /// Stop and release a geometry stream while keeping the parsed model open.
    #[wasm_bindgen(js_name = cancelGeometryStream)]
    pub fn cancel_geometry_stream(&mut self, model_id: u32) -> bool {
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
        let settings = options.geometry.clone();
        let mut session = Engine::with_settings(settings).session(model);
        session.restrict(express_ids);
        if let Some(offset) = options.model_offset {
            session.set_model_offset(DVec3::from_array(offset));
        }
        let total = session.total();
        let batch = if total > 0 {
            session.next(model, |_| false)
        } else {
            tessifc_engine::Batch {
                shapes: Vec::new(),
                diagnostics: Vec::new(),
                is_final: true,
                timings: Default::default(),
            }
        };
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
        for shape in batch.shapes {
            // Baked, so the patch never refers to family meshes the pack may lack.
            let baked: Vec<_> = shape
                .parts
                .into_iter()
                .map(|part| tessifc_engine::ShapePart {
                    geometry: tessifc_engine::PartGeometry::Unique(part.mesh()),
                    color: part.color,
                    provenance: part.provenance.clone(),
                })
                .collect();
            packer.add_shape(tessifc_engine::Shape {
                express_id: shape.express_id,
                class: shape.class,
                category: shape.category,
                color: shape.color,
                parts: baked,
            });
        }
        packer.add_diagnostics(&batch.diagnostics);
        Ok(Some(packer.finish()))
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
        self.streams.remove(&model_id);
        self.geometry.remove(&model_id).is_some()
    }

    /// Close a model and free it. `false` if the id was not open.
    #[wasm_bindgen(js_name = closeModel)]
    pub fn close_model(&mut self, model_id: u32) -> bool {
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
        self.geometry.clear();
        self.streams.clear();
        self.sources.clear();
        self.options.clear();
        self.models.clear();
    }
}

impl Kernel {
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
    fn a_patch_id_without_headroom_is_refused() {
        assert!(GeometrySettings::from_json(r#"{"firstGeometryId":4294967295}"#).is_err());
        assert!(GeometrySettings::from_json(r#"{"firstGeometryId":1000}"#).is_ok());
    }
}
