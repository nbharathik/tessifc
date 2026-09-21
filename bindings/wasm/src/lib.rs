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

#[cfg(feature = "edit")]
mod edit;

use std::collections::{BTreeMap, BTreeSet};
use tessifc_engine::report::{self, GeometryStream, OpenOptions};
use tessifc_engine::{Engine, EvaluationResult, ProductOutcome, ProductState};
use tessifc_geom::Settings as Settings3d;
use tessifc_model::Model;
use wasm_bindgen::prelude::*;

#[cfg(feature = "edit")]
pub(crate) use tessifc_engine::report::diagnostics_json;
pub(crate) use tessifc_engine::report::{GeometrySettings, effective_settings};

/// Geometry settings from the host's JSON, or the JavaScript error naming the field.
pub(crate) fn parse_geometry_settings(text: Option<String>) -> Result<GeometrySettings, JsValue> {
    GeometrySettings::parse(text.as_deref()).map_err(|error| JsValue::from_str(&error))
}

/// Milliseconds on a monotonic-enough clock, for chunk budgets.
#[cfg(target_arch = "wasm32")]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// An engine whose time budget runs on the host clock, since the module has none.
fn engine_for(settings: Settings3d) -> Engine {
    Engine::with_settings(settings).with_clock(std::sync::Arc::new(now_ms))
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// Sub-millisecond time for stage reports: `performance.now()` where the host has it.
#[cfg(all(feature = "edit", target_arch = "wasm32"))]
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

#[cfg(all(feature = "edit", not(target_arch = "wasm32")))]
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

/// A coarse level of a mesh: the triangles that survive a quadric
/// simplification to about a quarter, as indices over the same `positions`
/// (three f32 per vertex), or `undefined` when the mesh is too small, the
/// input invalid, or too little could go within the tolerance. `settings` is
/// JSON with `chordToleranceM` (the kernel's default when absent) and
/// `level` (1 or 2, 2 being four times the tolerance); the tolerance is the
/// larger of twice the chord tolerance and a 256th of the mesh's diagonal,
/// the same rule the kernel's `lodLevels` setting applies when it packs.
#[wasm_bindgen(js_name = simplifyMesh)]
pub fn simplify_mesh(
    positions: &[f32],
    indices: &[u32],
    settings: Option<String>,
) -> Result<Option<Vec<u32>>, JsValue> {
    let value: serde_json::Value = match settings {
        Some(text) => serde_json::from_str(&text)
            .map_err(|error| JsValue::from_str(&format!("invalid simplify settings: {error}")))?,
        None => serde_json::Value::Null,
    };
    let chord = value
        .get("chordToleranceM")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(Settings3d::default().chord_tolerance_m);
    let level = value
        .get("level")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    if !(chord > 0.0 && chord.is_finite() && (1..=2).contains(&level)) {
        return Err(JsValue::from_str(
            "invalid simplify settings: chordToleranceM must be positive and level 1 or 2",
        ));
    }
    if indices.len() < tessifc_mesh::LOD_MIN_TRIANGLES * 3 || !positions.len().is_multiple_of(3) {
        return Ok(None);
    }
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for vertex in positions.chunks_exact(3) {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(vertex[axis]);
            hi[axis] = hi[axis].max(vertex[axis]);
        }
    }
    let diagonal = (0..3)
        .map(|axis| ((hi[axis] - lo[axis]) as f64).powi(2))
        .sum::<f64>()
        .sqrt();
    let tolerance = (2.0 * chord).max(diagonal / 256.0) * if level == 2 { 4.0 } else { 1.0 };
    if tolerance.is_nan() || tolerance <= 0.0 || !tolerance.is_finite() {
        return Ok(None);
    }
    let options = tessifc_mesh::DecimateOptions {
        target_ratio: tessifc_mesh::LOD_TARGET_RATIO,
        tolerance,
        max_triangles: tessifc_mesh::MAX_DECIMATE_TRIANGLES,
    };
    Ok(tessifc_mesh::decimate_f32(positions, indices, &options))
}

/// A TessIFC instance holding open models.
#[wasm_bindgen]
pub struct Kernel {
    models: BTreeMap<u32, Model>,
    /// Original or most recently edited IFC bytes, kept apart from the model image.
    sources: BTreeMap<u32, Vec<u8>>,
    /// What each model was opened with.
    options: BTreeMap<u32, OpenOptions>,
    #[cfg(feature = "edit")]
    edit: edit::EditState,
    published_outcomes: BTreeMap<u32, Vec<ProductOutcome>>,
    next_id: u32,
    /// The last evaluation per model, so geometry can be read in pieces.
    geometry: BTreeMap<u32, EvaluationResult>,
    /// Geometry streams in progress, or finished and kept for the hierarchy.
    streams: BTreeMap<u32, GeometryStream>,
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
            #[cfg(feature = "edit")]
            edit: edit::EditState::new(),
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
        let options =
            OpenOptions::parse(settings.as_deref()).map_err(|error| JsValue::from_str(&error))?;

        // An archive is kept as the text inside it, so edits and exports are plain IFC.
        let (image, source) = tessifc_step::open_source(bytes, &options.parse_options());
        let model = Model::new(image);
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
        #[cfg(feature = "edit")]
        self.edit.open(id);
        Ok(id)
    }

    /// A JSON report about an open model, or `null` for an unknown id.
    #[wasm_bindgen(js_name = getModelInfo)]
    pub fn get_model_info(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let source_bytes = self.sources.get(&model_id).map_or(0, Vec::len);
        Some(report::model_info(model, source_bytes).to_string())
    }

    /// Parse diagnostics as JSON. Geometry diagnostics are carried by IGP packs.
    #[wasm_bindgen(js_name = getDiagnostics)]
    pub fn get_diagnostics(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        Some(report::parse_diagnostics(model).to_string())
    }

    /// The class name of one instance, or `null`.
    #[wasm_bindgen(js_name = getClassName)]
    pub fn get_class_name(&self, model_id: u32, express_id: u32) -> Option<String> {
        report::class_name(self.models.get(&model_id)?, express_id)
    }

    /// The product category: physical, space, opening, annotation or reference.
    #[wasm_bindgen(js_name = getProductCategory)]
    pub fn get_product_category(&self, model_id: u32, express_id: u32) -> Option<String> {
        report::product_category_name(self.models.get(&model_id)?, express_id)
    }

    /// Express ids of every instance of a class or its subtypes.
    #[wasm_bindgen(js_name = getIdsOfType)]
    pub fn get_ids_of_type(&self, model_id: u32, class_name: &str) -> Vec<u32> {
        self.models
            .get(&model_id)
            .map(|model| report::ids_of_type(model, class_name))
            .unwrap_or_default()
    }

    /// The schema definition of a class as JSON: `abstract` and `attributes` in
    /// STEP argument order with `name`, `type`, `base`, `aggDepth`, `optional`
    /// and `derived`; `null` for a class the model's schema does not define.
    #[wasm_bindgen(js_name = getClassAttributes)]
    pub fn get_class_attributes(&self, model_id: u32, class_name: &str) -> Option<String> {
        report::class_attributes(self.models.get(&model_id)?, class_name)
            .map(|value| value.to_string())
    }

    /// The class and its supertypes up to the root, as a JSON array of names;
    /// `undefined` for an unknown model or a class outside its schema.
    #[wasm_bindgen(js_name = getClassSupertypes)]
    pub fn get_class_supertypes(&self, model_id: u32, class_name: &str) -> Option<String> {
        report::class_supertypes(self.models.get(&model_id)?, class_name)
            .map(|value| value.to_string())
    }

    /// The rendered building hierarchy as a flat JSON node list of products and
    /// their spatial or aggregate ancestors; before evaluation every product counts.
    #[wasm_bindgen(js_name = getSpatialHierarchy)]
    pub fn get_spatial_hierarchy(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let rendered = self.rendered_products(model_id);
        Some(report::spatial_hierarchy(model, rendered.as_ref()).to_string())
    }

    /// Source-level attributes for one entity, as JSON; `raw` is the exact STEP
    /// spelling and `value` the decoded text where the value is string-like.
    #[wasm_bindgen(js_name = getEntityInfo)]
    pub fn get_entity_info(&self, model_id: u32, express_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let source = self.sources.get(&model_id)?;
        report::entity_info(model, source, express_id).map(|value| value.to_string())
    }

    /// Every schema entity and its direct or inherited evaluator routes, as JSON.
    #[wasm_bindgen(js_name = getGeometryCapabilities)]
    pub fn get_geometry_capabilities(&self, model_id: u32) -> Option<String> {
        report::geometry_capabilities(self.models.get(&model_id)?)
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
        let settings = parse_geometry_settings(settings)?.geometry;
        let effective = effective_settings(&settings);

        let result = engine_for(settings).evaluate(model);
        let summary = report::evaluation_summary(&result, &effective);

        self.streams.remove(&model_id);
        #[cfg(feature = "edit")]
        self.edit.set_basis(
            model_id,
            effective_settings(&result.settings),
            result.model_offset,
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
        let settings = parse_geometry_settings(settings)?.geometry;
        let effective = effective_settings(&settings);
        let session = engine_for(settings).session(model);
        #[cfg(feature = "edit")]
        self.edit
            .set_basis(model_id, effective.clone(), session.model_offset());
        let summary = report::stream_summary(&session, effective).to_string();
        self.geometry.remove(&model_id);
        self.streams.insert(model_id, GeometryStream::new(session));
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
        let started = now_ms();
        stream.next_chunk(model, |progress| {
            (budget_ms > 0.0 && now_ms() - started >= budget_ms)
                || (max_products > 0 && progress.products >= max_products as usize)
                || (max_triangles > 0 && progress.triangles >= max_triangles as usize)
        })
    }

    /// How far a stream has come, as JSON, or `null` if there is none.
    #[wasm_bindgen(js_name = streamProgress)]
    pub fn stream_progress(&self, model_id: u32) -> Option<String> {
        Some(self.streams.get(&model_id)?.progress().to_string())
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

    /// Texture coordinates of shape `index` as f32 pairs, one per vertex of
    /// `shapePositions` for the same `part`; `None` when the part carries none
    /// or when `part` is omitted, since merged parts do not share a mapping.
    #[wasm_bindgen(js_name = shapeUv)]
    pub fn shape_uv(&self, model_id: u32, index: usize, part: Option<usize>) -> Option<Vec<f32>> {
        let shape = self.geometry.get(&model_id)?.shapes.get(index)?;
        let mesh = shape.parts.get(part?)?.geometry.local_mesh();
        if !mesh.has_uvs() {
            return None;
        }
        Some(mesh.uvs.iter().flat_map(|uv| [uv[0], uv[1]]).collect())
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
        Some(report::pack_evaluation(
            model.image().schema.as_str(),
            result,
        ))
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
        Some(report::pack_evaluation_owned(&schema, result))
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
        #[cfg(feature = "edit")]
        self.edit.close(model_id);
        self.published_outcomes.remove(&model_id);
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
        #[cfg(feature = "edit")]
        self.edit.close_all();
        self.published_outcomes.clear();
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
            Some(stream.session().outcomes(model))
        } else if let Some(result) = self.geometry.get(&model_id) {
            Some(Engine::with_settings(result.settings.clone()).outcomes(model, result))
        } else {
            self.published_outcomes.get(&model_id).cloned()
        }
    }

    /// The products that produced geometry, from the evaluation, the stream or
    /// the outcomes kept after either was released; `None` before any of them.
    fn rendered_products(&self, model_id: u32) -> Option<BTreeSet<u32>> {
        match (self.geometry.get(&model_id), self.streams.get(&model_id)) {
            (Some(result), _) => Some(result.shapes.iter().map(|shape| shape.express_id).collect()),
            (None, Some(stream)) => Some(stream.session().emitted().iter().copied().collect()),
            (None, None) => self.published_outcomes.get(&model_id).map(|outcomes| {
                outcomes
                    .iter()
                    .filter(|outcome| outcome.state == ProductState::Emitted)
                    .map(|outcome| outcome.express_id)
                    .collect()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessifc_engine::pack::instance_flags;
    use tessifc_pack::{INSTANCE_OPENING, INSTANCE_SPACE, INSTANCE_TRANSPARENT};
    #[cfg(feature = "edit")]
    use tessifc_step::SchemaId;

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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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
        assert!(kernel.edit.dangling.contains_key(&id));
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
        assert!(!kernel.edit.dangling.contains_key(&id));
        let again = String::from_utf8_lossy(&kernel.export_model(id).unwrap())
            .replace("'W3'", "'W4'")
            .into_bytes();
        kernel.prepare_revision_inner(id, again, "2").unwrap();
        assert!(kernel.edit.dangling.contains_key(&id));
    }

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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
        let staged = &kernel.edit.prepared[&id].source;
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

    #[cfg(feature = "edit")]
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

    #[cfg(feature = "edit")]
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
        assert_eq!(
            kernel.edit.prepared[&id].model.image().schema,
            effective_schema
        );
        assert_eq!(kernel.options[&id].schema_override, Some(SchemaId::Ifc2x3));
        assert_eq!(kernel.edit.prepared[&id].impact.created_entities, vec![2]);
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
        assert_eq!(kernel.edit.prepared[&id].impact.deleted_entities, vec![2]);
        kernel.commit_revision_inner(id, "1").unwrap();
    }

    #[cfg(feature = "edit")]
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
        assert!(!kernel.edit.prepared[&id].accepted);
        assert!(kernel.commit_revision_inner(id, "0").is_err());
        assert_eq!(kernel.export_model(id).unwrap(), BOX);
        let token = kernel.edit.prepared[&id].token.to_string();
        assert!(kernel.discard_revision_inner(id, &token).unwrap());
        let unusable = String::from_utf8_lossy(BOX).replace("'Body'", "'Axis'");
        kernel
            .prepare_revision_inner(id, unusable.into_bytes(), "0")
            .unwrap();
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        assert!(!kernel.edit.prepared[&id].accepted);
        assert!(kernel.commit_revision_inner(id, "0").is_err());
        kernel.close_model(id);
        assert!(kernel.get_model_revision(id).is_none());
        assert!(kernel.get_prepared_revision_info(id).is_none());
    }

    #[cfg(feature = "edit")]
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
        assert!(kernel.edit.prepared[&id].impact.full_rebuild);
        kernel
            .evaluate_prepared_revision_inner(id, GeometrySettings::default())
            .unwrap();
        assert!(
            kernel.edit.prepared[&id].accepted,
            "{:?}",
            kernel.edit.prepared[&id].diagnostics
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
        assert!(kernel.edit.prepared[&id].accepted);
        assert_eq!(
            kernel.edit.prepared[&id].outcomes[0].state,
            ProductState::NoRepresentation
        );
        assert_eq!(kernel.commit_revision_inner(id, "0").unwrap(), "1");
    }

    #[cfg(feature = "edit")]
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
        let token = kernel.edit.prepared[&id].token.to_string();
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
        assert!(!kernel.edit.prepared[&id].accepted);
        assert!(
            kernel.edit.prepared[&id]
                .diagnostics
                .iter()
                .any(|d| d.code.as_str() == "E_REVISION_PACK_FAILED")
        );
        assert!(kernel.commit_revision_inner(id, "0").is_err());
    }
}
