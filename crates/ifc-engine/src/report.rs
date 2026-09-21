// SPDX-License-Identifier: Apache-2.0
//! The JSON reports and packing steps every host binding shares: model info,
//! diagnostics, entity and class reports, the spatial hierarchy, geometry
//! summaries and the chunked geometry stream. One implementation, so the
//! WASM and the Python kernels answer the same questions the same way.

use std::collections::BTreeSet;

use glam::DVec3;
use serde_json::{Value, json};
use tessifc_geom::{Settings, product_category};
use tessifc_model::Model;
use tessifc_pack::StreamPosition;
use tessifc_step::{
    CLASS_UNKNOWN, Diagnostic, ParseOptions, SchemaId, Severity, argument_source,
    leaf_argument_source,
};

use crate::pack::{PackState, Packer};
use crate::{Batch, BatchProgress, EvaluationResult, LimitReached, Session};

/// Settings accepted by the geometry entry points, as JSON; unknown fields
/// are ignored. `modelOffset` and `firstGeometryId` place a patch into an
/// existing scene.
#[derive(Default, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeometrySettings {
    /// The kernel's own settings.
    #[serde(flatten)]
    pub geometry: Settings,
    /// The scene's model offset a patch must keep.
    pub model_offset: Option<[f64; 3]>,
    /// The first geometry id a patch may use.
    pub first_geometry_id: Option<u32>,
}

impl GeometrySettings {
    /// Parse and validate; the error text names the field at fault.
    pub fn from_json(text: &str) -> Result<Self, String> {
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

    /// `from_json` when text is given, the defaults otherwise.
    pub fn parse(text: Option<&str>) -> Result<Self, String> {
        text.map(Self::from_json)
            .transpose()
            .map(|settings| settings.unwrap_or_default())
    }
}

/// The settings a run used, as the JSON a summary reports.
pub fn effective_settings(settings: &Settings) -> Value {
    serde_json::to_value(settings).expect("validated geometry settings are serialisable")
}

/// Settings accepted when a model is opened, as JSON; unknown fields are ignored.
#[derive(Clone, Default, Debug)]
pub struct OpenOptions {
    /// Read the file as this schema whatever its header says.
    pub schema_override: Option<SchemaId>,
    /// Refuse a file with more entities than this.
    pub max_entities: Option<usize>,
    /// Refuse an IFCZIP archive that inflates past this many bytes.
    pub max_ifczip_bytes: Option<usize>,
}

impl OpenOptions {
    /// The fields the kernel understands; malformed JSON or an unknown schema name is an error.
    pub fn from_json(text: &str) -> Result<OpenOptions, String> {
        let value: Value = serde_json::from_str(text)
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
        if let Some(max) = value.get("maxIfczipBytes") {
            let max = max
                .as_u64()
                .ok_or("invalid open settings: maxIfczipBytes must be a non-negative integer")?;
            options.max_ifczip_bytes = Some(usize::try_from(max).unwrap_or(usize::MAX));
        }
        Ok(options)
    }

    /// `from_json` when text is given, the defaults otherwise.
    pub fn parse(text: Option<&str>) -> Result<OpenOptions, String> {
        text.map(Self::from_json)
            .transpose()
            .map(|options| options.unwrap_or_default())
    }

    /// The parser options these stand for, over the parser's defaults.
    pub fn parse_options(&self) -> ParseOptions {
        let defaults = ParseOptions::default();
        ParseOptions {
            schema_override: self.schema_override,
            max_entities: self.max_entities.unwrap_or(defaults.max_entities),
            max_ifczip_bytes: self.max_ifczip_bytes.unwrap_or(defaults.max_ifczip_bytes),
            ..defaults
        }
    }
}

/// A report about an open model: schema, counts per class, header and parse
/// diagnostic counts. `source_bytes` is what the host retains for edits.
pub fn model_info(model: &Model, source_bytes: usize) -> Value {
    let image = model.image();
    let schema = model.schema();
    let product = schema.class_by_name("IfcProduct");

    let mut products = serde_json::Map::new();
    let mut classes = serde_json::Map::new();
    let mut product_total = 0u64;
    for (class_id, count) in image.populated_classes() {
        if class_id == CLASS_UNKNOWN {
            continue;
        }
        let name = schema.class(class_id).name;
        classes.insert(name.to_string(), json!(count));
        if let Some(product) = product
            && schema.is_a(class_id, product)
        {
            products.insert(name.to_string(), json!(count));
            product_total += count as u64;
        }
    }

    json!({
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
            "errors": image.diagnostics.count_of(Severity::Error),
            "warnings": image.diagnostics.count_of(Severity::Warning),
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
    })
}

/// One diagnostic as the hosts report it.
pub fn diagnostic_json(diagnostic: &Diagnostic) -> Value {
    json!({
        "code": diagnostic.code.as_str(),
        "severity": diagnostic.severity.as_str(),
        "line": diagnostic.line,
        "expressId": diagnostic.express_id,
        "message": diagnostic.message,
    })
}

/// A list of diagnostics as the hosts report it.
pub fn diagnostics_json(diagnostics: &[Diagnostic]) -> Value {
    Value::Array(diagnostics.iter().map(diagnostic_json).collect())
}

/// The parse diagnostics of a model; geometry diagnostics travel in the packs.
pub fn parse_diagnostics(model: &Model) -> Value {
    diagnostics_json(model.image().diagnostics.items())
}

/// The class name of one instance, or `None` for an id the model has not.
pub fn class_name(model: &Model, express_id: u32) -> Option<String> {
    model.entity(express_id)?;
    Some(model.image().class_name_of(express_id))
}

/// The product category of an instance: physical, space, opening, annotation
/// or reference; `None` for anything that is not a product.
pub fn product_category_name(model: &Model, express_id: u32) -> Option<String> {
    let entity = model.entity(express_id)?;
    entity
        .is_a("IfcProduct")
        .then(|| product_category(entity).as_str().to_owned())
}

/// Express ids of every instance of a class or its subtypes; empty for an unknown class.
pub fn ids_of_type(model: &Model, class_name: &str) -> Vec<u32> {
    let Some(class) = model.schema().class_by_name(class_name) else {
        return Vec::new();
    };
    model.image().ids_of_type(class).collect()
}

/// The schema definition of a class: `abstract` and `attributes` in STEP
/// argument order with `name`, `type`, `base`, `aggDepth`, `optional` and
/// `derived`; `None` for a class the model's schema does not define.
pub fn class_attributes(model: &Model, class_name: &str) -> Option<Value> {
    let schema = model.schema();
    let class = schema.class_by_name(class_name)?;
    let definition = schema.class(class);
    let attributes: Vec<_> = definition
        .attrs
        .iter()
        .map(|attribute| {
            json!({
                "name": attribute.name,
                "type": attribute.type_name,
                "base": format!("{:?}", attribute.base).to_ascii_lowercase(),
                "aggDepth": attribute.agg_depth,
                "optional": attribute.optional,
                "derived": attribute.kind == tessifc_schema::AttrKind::DerivedOverride,
            })
        })
        .collect();
    Some(json!({
        "class": definition.name,
        "abstract": definition.is_abstract,
        "attributes": attributes,
    }))
}

/// The class and its supertypes up to the root, as names; `None` for a class
/// outside the model's schema.
pub fn class_supertypes(model: &Model, class_name: &str) -> Option<Value> {
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
    Some(json!(names))
}

/// The building hierarchy as a flat node list of products and their spatial
/// or aggregate ancestors. `rendered` names the products that produced
/// geometry; with `None`, before any evaluation, every product counts.
pub fn spatial_hierarchy(model: &Model, rendered: Option<&BTreeSet<u32>>) -> Value {
    let mut included = match rendered {
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
            Some(json!({
                "expressId": express_id,
                "class": entity.class_name(),
                "name": name,
                "globalId": entity.attr("GlobalId").as_string(),
                "parentExpressId": parent,
                "rendered": rendered.is_none_or(|set| set.contains(&express_id)),
            }))
        })
        .collect();

    json!({ "nodes": nodes })
}

/// Source-level attributes of one entity: `raw` is the exact STEP spelling
/// and `value` the decoded text where the value is string-like. `source` is
/// the file's bytes as the host retained them; `None` for an unknown id.
pub fn entity_info(model: &Model, source: &[u8], express_id: u32) -> Option<Value> {
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
            for (local_index, attribute) in definition.attrs.iter().skip(parent_count).enumerate() {
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
                fields.push(json!({
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
        return Some(json!({
            "expressId": express_id,
            "class": class,
            "complex": true,
            "fields": fields,
        }));
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
        fields.push(json!({
            "name": name,
            "index": index,
            "type": type_name,
            "kind": kind,
            "optional": definition.is_some_and(|attribute| attribute.optional),
            "raw": raw,
            "value": value,
        }));
    }

    Some(json!({
        "expressId": express_id,
        "class": class,
        "complex": false,
        "fields": fields,
    }))
}

/// Every schema entity and its direct or inherited evaluator routes.
pub fn geometry_capabilities(model: &Model) -> Option<String> {
    serde_json::to_string(&tessifc_geom::Registry::shared(model.image().schema).inventory()).ok()
}

/// A finite f64 as JSON text, since JSON has no NaN and no infinity.
pub fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".into()
    }
}

/// A vector as a JSON array of three finite numbers.
pub fn json_vector(vector: DVec3) -> String {
    format!(
        "[{},{},{}]",
        json_number(vector.x),
        json_number(vector.y),
        json_number(vector.z)
    )
}

/// A tripped budget as JSON, or `null`.
pub fn limit_json(limit: Option<&LimitReached>) -> Value {
    limit
        .and_then(|limit| serde_json::to_value(limit).ok())
        .unwrap_or(Value::Null)
}

/// Distinct materials and textures across an evaluation's parts.
pub fn material_counts(result: &EvaluationResult) -> (usize, usize) {
    let mut materials = std::collections::HashSet::new();
    let mut textures = std::collections::HashSet::new();
    for shape in &result.shapes {
        for part in &shape.parts {
            if let Some(material) = &part.material {
                materials.insert(material.style);
                if let Some(texture) = &material.texture {
                    textures.insert(texture.id);
                }
            }
        }
    }
    (materials.len(), textures.len())
}

/// The summary of a whole-model evaluation, as the JSON text the hosts return.
pub fn evaluation_summary(result: &EvaluationResult, effective: &Value) -> String {
    let count = |severity: Severity| {
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == severity)
            .count()
    };
    let (materials, textures) = material_counts(result);
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
    summary.push_str(&count(Severity::Info).to_string());
    summary.push_str(",\"diagnosticWarnings\":");
    summary.push_str(&count(Severity::Warning).to_string());
    summary.push_str(",\"diagnosticErrors\":");
    summary.push_str(&count(Severity::Error).to_string());
    summary.push_str(",\"effectiveSettings\":");
    summary.push_str(&effective.to_string());
    summary.push_str(",\"limitReached\":");
    summary.push_str(&limit_json(result.limit_reached.as_ref()).to_string());
    summary.push_str(",\"materials\":");
    summary.push_str(&materials.to_string());
    summary.push_str(",\"textures\":");
    summary.push_str(&textures.to_string());
    summary.push('}');
    summary
}

/// The summary a stream starts with, as JSON.
pub fn stream_summary(session: &Session, effective: Value) -> Value {
    let summary = format!(
        "{{\"products\":{},\"productsConsidered\":{},\"productsFiltered\":{},\
         \"lengthScaleToM\":{},\"modelOffset\":{}}}",
        session.total(),
        session.products_considered(),
        session.products_filtered(),
        json_number(session.units().length_to_m),
        json_vector(session.model_offset()),
    );
    let mut summary: Value = serde_json::from_str(&summary).expect("finite geometry summary");
    summary["effectiveSettings"] = effective;
    summary
}

/// A whole evaluation as one IGP pack, the geometry borrowed.
pub fn pack_evaluation(schema: &str, result: &EvaluationResult) -> Vec<u8> {
    let mut packer = Packer::new(schema, result.units.length_to_m, result.model_offset);
    packer.set_lod_levels(
        result.settings.lod_levels,
        result.settings.chord_tolerance_m,
    );
    packer.set_georef(result.georef.clone());
    for shape in &result.shapes {
        packer.add_shape_ref(shape);
    }
    packer.add_diagnostics(&result.diagnostics);
    packer.set_stat("products", result.shapes.len() as f64);
    packer.set_stat("triangles", packer.triangles() as f64);
    if let Some(limit) = &result.limit_reached {
        packer.set_stat("limit_reached", 1.0);
        packer.set_stat("products_skipped", limit.products_skipped as f64);
    }
    packer.finish()
}

/// A whole evaluation as one IGP pack, the geometry moved in rather than cloned.
pub fn pack_evaluation_owned(schema: &str, result: EvaluationResult) -> Vec<u8> {
    let EvaluationResult {
        shapes,
        diagnostics,
        units,
        model_offset,
        georef,
        limit_reached,
        settings,
        ..
    } = result;
    let products = shapes.len();
    let mut packer = Packer::new(schema, units.length_to_m, model_offset);
    packer.set_lod_levels(settings.lod_levels, settings.chord_tolerance_m);
    packer.set_georef(georef);
    for shape in shapes {
        packer.add_shape(shape);
    }
    packer.add_diagnostics(&diagnostics);
    if let Some(limit) = limit_reached {
        packer.set_stat("limit_reached", 1.0);
        packer.set_stat("products_skipped", limit.products_skipped as f64);
    }
    packer.set_stat("products", products as f64);
    packer.set_stat("triangles", packer.triangles() as f64);
    packer.finish()
}

/// A geometry stream in progress for one model: the session, the packer's
/// carried state and the chunk count.
pub struct GeometryStream {
    session: Session,
    state: PackState,
    chunk: u32,
    diagnostic_counts: [usize; 3],
}

impl GeometryStream {
    /// Wrap a session the engine started.
    pub fn new(session: Session) -> Self {
        GeometryStream {
            session,
            state: PackState::default(),
            chunk: 0,
            diagnostic_counts: [0; 3],
        }
    }

    /// The session underneath, for outcomes and the emitted products.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// True once the last chunk has been handed out.
    pub fn is_finished(&self) -> bool {
        self.session.is_finished() && self.chunk > 0
    }

    /// The next IGP chunk, or `None` once finished. `stop` is asked after every
    /// product whether the batch is large enough; at least one product goes in.
    pub fn next_chunk(
        &mut self,
        model: &Model,
        stop: impl FnMut(BatchProgress) -> bool,
    ) -> Option<Vec<u8>> {
        if self.is_finished() {
            return None;
        }
        let batch = self.session.next(model, stop);
        Some(self.pack_batch(model, batch))
    }

    fn pack_batch(&mut self, model: &Model, batch: Batch) -> Vec<u8> {
        let session = &self.session;
        let mut packer = Packer::continue_stream(
            model.image().schema.as_str(),
            session.units().length_to_m,
            session.model_offset(),
            std::mem::take(&mut self.state),
        );
        let settings = session.settings();
        packer.set_lod_levels(settings.lod_levels, settings.chord_tolerance_m);
        packer.set_georef(session.georef().map(str::to_string));
        packer.set_stream(StreamPosition {
            chunk: self.chunk,
            is_final: batch.is_final,
            products_done: session.done(),
            products_total: session.total(),
        });
        for shape in batch.shapes {
            packer.add_shape(shape);
        }
        for diagnostic in &batch.diagnostics {
            let index = match diagnostic.severity {
                Severity::Info => 0,
                Severity::Warning => 1,
                Severity::Error => 2,
            };
            self.diagnostic_counts[index] += 1;
        }
        packer.add_diagnostics(&batch.diagnostics);
        if batch.is_final {
            packer.set_stat("products", session.emitted().len() as f64);
            packer.set_stat("triangles", session.triangles() as f64);
            if let Some(limit) = session.limit_reached() {
                packer.set_stat("limit_reached", 1.0);
                packer.set_stat("products_skipped", limit.products_skipped as f64);
            }
        }
        let (bytes, state) = packer.finish_chunk();
        self.state = state;
        self.chunk += 1;
        bytes
    }

    /// How far the stream has come, as JSON.
    pub fn progress(&self) -> Value {
        let session = &self.session;
        json!({
            "done": session.done(), "total": session.total(), "emitted": session.emitted().len(),
            "triangles": session.triangles(), "chunks": self.chunk,
            "finished": self.is_finished(),
            "diagnostics": self.diagnostic_counts.iter().sum::<usize>(),
            "diagnosticInfos": self.diagnostic_counts[0],
            "diagnosticWarnings": self.diagnostic_counts[1],
            "diagnosticErrors": self.diagnostic_counts[2],
            "limitReached": limit_json(session.limit_reached()),
        })
    }
}
