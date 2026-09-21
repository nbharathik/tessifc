// SPDX-License-Identifier: Apache-2.0
//! Python bindings for TessIFC. One [`Kernel`] holds any number of open
//! models; reports cross as JSON strings and geometry as `bytes`, either one
//! IGP pack or streamed chunks. The `tessifc` package wraps this module and
//! turns the JSON into dictionaries.
//!
//! ```python
//! import tessifc
//! kernel = tessifc.Kernel()
//! model = kernel.open_model(open("model.ifc", "rb").read())
//! print(kernel.get_model_info(model)["entities"])
//! kernel.close_model(model)
//! ```

#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tessifc_engine::report::{self, GeometrySettings, GeometryStream, OpenOptions};
use tessifc_engine::{Engine, EvaluationResult, ProductOutcome, ProductState};
use tessifc_geom::Settings;
use tessifc_model::Model;

pyo3::create_exception!(
    _core,
    KernelError,
    PyException,
    "An error raised by the TessIFC kernel: invalid settings or a request it cannot answer."
);

/// Version of the TessIFC crates this module was built from.
#[pyfunction]
fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn kernel_error(message: String) -> PyErr {
    KernelError::new_err(message)
}

/// Milliseconds on a monotonic clock, for chunk budgets and time limits.
fn now_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// An engine whose time budget runs on the host clock; the crate's `parallel`
/// feature spreads a whole-model evaluation across every core.
fn engine_for(settings: Settings) -> Engine {
    Engine::with_settings(settings).with_clock(std::sync::Arc::new(now_ms))
}

/// A TessIFC instance holding open models. Not thread-safe: use it from one
/// thread, or one kernel per thread.
#[pyclass(unsendable, module = "tessifc._core")]
pub struct Kernel {
    models: BTreeMap<u32, Model>,
    /// The file's bytes, kept for the source-level entity report.
    sources: BTreeMap<u32, Vec<u8>>,
    /// Outcomes kept after an evaluation or a stream was released.
    published_outcomes: BTreeMap<u32, Vec<ProductOutcome>>,
    next_id: u32,
    /// The last evaluation per model, until it is packed or released.
    geometry: BTreeMap<u32, EvaluationResult>,
    /// Geometry streams in progress, or finished and kept for the hierarchy.
    streams: BTreeMap<u32, GeometryStream>,
}

impl Default for Kernel {
    fn default() -> Self {
        Kernel::new()
    }
}

#[pymethods]
impl Kernel {
    /// A kernel with no models open.
    #[new]
    pub fn new() -> Kernel {
        Kernel {
            models: BTreeMap::new(),
            sources: BTreeMap::new(),
            published_outcomes: BTreeMap::new(),
            next_id: 1,
            geometry: BTreeMap::new(),
            streams: BTreeMap::new(),
        }
    }

    /// Parse a file and keep it open. Returns the model id; an unreadable file
    /// yields zero entities and diagnostics rather than an error. `options` is
    /// the JSON of `schemaOverride`, `maxEntities` and `maxIfczipBytes`.
    #[pyo3(signature = (data, options = None))]
    pub fn open_model(
        &mut self,
        py: Python<'_>,
        data: &[u8],
        options: Option<&str>,
    ) -> PyResult<u32> {
        let options = OpenOptions::parse(options).map_err(kernel_error)?;
        let bytes = data.to_vec();
        let (image, source) =
            py.detach(|| tessifc_step::open_source(bytes, &options.parse_options()));
        let model = Model::new(image);
        // Skip live ids so a wrapped counter cannot rebind another caller's model.
        let mut id = self.next_id;
        while self.models.contains_key(&id) {
            id = id.wrapping_add(1).max(1);
            if id == self.next_id {
                return Err(kernel_error("no free model id".into()));
            }
        }
        self.next_id = id.wrapping_add(1).max(1);
        self.models.insert(id, model);
        self.sources.insert(id, source);
        Ok(id)
    }

    /// A JSON report about an open model, or `None` for an unknown id.
    pub fn get_model_info(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let source_bytes = self.sources.get(&model_id).map_or(0, Vec::len);
        Some(report::model_info(model, source_bytes).to_string())
    }

    /// Parse diagnostics as JSON. Geometry diagnostics are carried by IGP packs.
    pub fn get_diagnostics(&self, model_id: u32) -> Option<String> {
        Some(report::parse_diagnostics(self.models.get(&model_id)?).to_string())
    }

    /// The class name of one instance, or `None`.
    pub fn get_class_name(&self, model_id: u32, express_id: u32) -> Option<String> {
        report::class_name(self.models.get(&model_id)?, express_id)
    }

    /// The product category: physical, space, opening, annotation or reference.
    pub fn get_product_category(&self, model_id: u32, express_id: u32) -> Option<String> {
        report::product_category_name(self.models.get(&model_id)?, express_id)
    }

    /// Express ids of every instance of a class or its subtypes.
    pub fn get_ids_of_type(&self, model_id: u32, class_name: &str) -> Vec<u32> {
        self.models
            .get(&model_id)
            .map(|model| report::ids_of_type(model, class_name))
            .unwrap_or_default()
    }

    /// Source-level attributes of one entity, as JSON; `None` for an unknown id.
    pub fn get_entity_info(&self, model_id: u32, express_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let source = self.sources.get(&model_id)?;
        report::entity_info(model, source, express_id).map(|value| value.to_string())
    }

    /// The building hierarchy as a flat JSON node list; before an evaluation
    /// every product counts as rendered.
    pub fn get_spatial_hierarchy(&self, model_id: u32) -> Option<String> {
        let model = self.models.get(&model_id)?;
        let rendered = self.rendered_products(model_id);
        Some(report::spatial_hierarchy(model, rendered.as_ref()).to_string())
    }

    /// The schema definition of a class as JSON, or `None` for a class the
    /// model's schema does not define.
    pub fn get_class_attributes(&self, model_id: u32, class_name: &str) -> Option<String> {
        report::class_attributes(self.models.get(&model_id)?, class_name)
            .map(|value| value.to_string())
    }

    /// The class and its supertypes up to the root, as a JSON array of names.
    pub fn get_class_supertypes(&self, model_id: u32, class_name: &str) -> Option<String> {
        report::class_supertypes(self.models.get(&model_id)?, class_name)
            .map(|value| value.to_string())
    }

    /// Every schema entity and its direct or inherited evaluator routes, as JSON.
    pub fn get_geometry_capabilities(&self, model_id: u32) -> Option<String> {
        report::geometry_capabilities(self.models.get(&model_id)?)
    }

    /// Evaluate every product, across every core, and keep the result for
    /// `take_pack`. Returns a JSON summary, or `None` for an unknown id.
    /// Invalid `settings` raise `KernelError`.
    #[pyo3(signature = (model_id, settings = None))]
    pub fn evaluate_geometry(
        &mut self,
        py: Python<'_>,
        model_id: u32,
        settings: Option<&str>,
    ) -> PyResult<Option<String>> {
        let Some(model) = self.models.get(&model_id) else {
            return Ok(None);
        };
        let settings = GeometrySettings::parse(settings)
            .map_err(kernel_error)?
            .geometry;
        let effective = report::effective_settings(&settings);
        let engine = engine_for(settings);
        let result = py.detach(|| engine.evaluate(model));
        let summary = report::evaluation_summary(&result, &effective);
        self.streams.remove(&model_id);
        self.published_outcomes.insert(
            model_id,
            Engine::with_settings(result.settings.clone()).outcomes(model, &result),
        );
        self.geometry.insert(model_id, result);
        Ok(Some(summary))
    }

    /// Per-product evaluation outcomes as JSON; emitted products may still have diagnostics.
    pub fn get_product_outcomes(&self, model_id: u32) -> Option<String> {
        let outcomes = self.current_product_outcomes(model_id)?;
        serde_json::to_string(&outcomes).ok()
    }

    /// The whole evaluation as one IGP pack, the geometry kept.
    pub fn get_pack<'py>(&self, py: Python<'py>, model_id: u32) -> Option<Bound<'py, PyBytes>> {
        let model = self.models.get(&model_id)?;
        let result = self.geometry.get(&model_id)?;
        let bytes = report::pack_evaluation(model.image().schema.as_str(), result);
        Some(PyBytes::new(py, &bytes))
    }

    /// The whole evaluation as one IGP pack, releasing the geometry without copying it.
    pub fn take_pack<'py>(
        &mut self,
        py: Python<'py>,
        model_id: u32,
    ) -> Option<Bound<'py, PyBytes>> {
        let schema = self
            .models
            .get(&model_id)?
            .image()
            .schema
            .as_str()
            .to_owned();
        let result = self.geometry.remove(&model_id)?;
        let bytes = py.detach(|| report::pack_evaluation_owned(&schema, result));
        Some(PyBytes::new(py, &bytes))
    }

    /// Start streaming a model's geometry as IGP chunks; returns a JSON summary
    /// or `None`. Follow with `next_geometry_chunk` until it returns `None`.
    #[pyo3(signature = (model_id, settings = None))]
    pub fn begin_geometry_stream(
        &mut self,
        model_id: u32,
        settings: Option<&str>,
    ) -> PyResult<Option<String>> {
        let Some(model) = self.models.get(&model_id) else {
            return Ok(None);
        };
        let settings = GeometrySettings::parse(settings)
            .map_err(kernel_error)?
            .geometry;
        let effective = report::effective_settings(&settings);
        let session = engine_for(settings).session(model);
        let summary = report::stream_summary(&session, effective).to_string();
        self.geometry.remove(&model_id);
        self.streams.insert(model_id, GeometryStream::new(session));
        Ok(Some(summary))
    }

    /// The next IGP chunk, or `None` once finished. Stops after `budget_ms`,
    /// `max_products` or `max_triangles` (zero is no limit), holding at least one product.
    #[pyo3(signature = (model_id, budget_ms = 0.0, max_products = 0, max_triangles = 0))]
    pub fn next_geometry_chunk<'py>(
        &mut self,
        py: Python<'py>,
        model_id: u32,
        budget_ms: f64,
        max_products: u32,
        max_triangles: u32,
    ) -> Option<Bound<'py, PyBytes>> {
        let model = self.models.get(&model_id)?;
        let stream = self.streams.get_mut(&model_id)?;
        let bytes = py.detach(|| {
            let started = now_ms();
            stream.next_chunk(model, |progress| {
                (budget_ms > 0.0 && now_ms() - started >= budget_ms)
                    || (max_products > 0 && progress.products >= max_products as usize)
                    || (max_triangles > 0 && progress.triangles >= max_triangles as usize)
            })
        })?;
        Some(PyBytes::new(py, &bytes))
    }

    /// How far a stream has come, as JSON, or `None` if there is none.
    pub fn stream_progress(&self, model_id: u32) -> Option<String> {
        Some(self.streams.get(&model_id)?.progress().to_string())
    }

    /// Stop and release a geometry stream while keeping the parsed model open.
    pub fn cancel_geometry_stream(&mut self, model_id: u32) -> bool {
        if let Some(outcomes) = self.current_product_outcomes(model_id) {
            self.published_outcomes.insert(model_id, outcomes);
        }
        self.streams.remove(&model_id).is_some()
    }

    /// Drop the geometry for a model, keeping the model itself open.
    pub fn release_geometry(&mut self, model_id: u32) -> bool {
        if let Some(outcomes) = self.current_product_outcomes(model_id) {
            self.published_outcomes.insert(model_id, outcomes);
        }
        self.streams.remove(&model_id);
        self.geometry.remove(&model_id).is_some()
    }

    /// Close a model and free it. `False` if the id was not open.
    pub fn close_model(&mut self, model_id: u32) -> bool {
        self.published_outcomes.remove(&model_id);
        self.geometry.remove(&model_id);
        self.streams.remove(&model_id);
        self.sources.remove(&model_id);
        self.models.remove(&model_id).is_some()
    }

    /// How many models are open.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Close every open model.
    pub fn close_all(&mut self) {
        self.published_outcomes.clear();
        self.geometry.clear();
        self.streams.clear();
        self.sources.clear();
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

    /// The products that produced geometry; `None` before any evaluation.
    fn rendered_products(&self, model_id: u32) -> Option<std::collections::BTreeSet<u32>> {
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

/// The extension module `tessifc._core`.
#[pymodule]
fn _core(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Kernel>()?;
    module.add_function(wrap_pyfunction!(version, module)?)?;
    module.add("KernelError", module.py().get_type::<KernelError>())?;
    Ok(())
}
