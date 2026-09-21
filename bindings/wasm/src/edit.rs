// SPDX-License-Identifier: Apache-2.0

//! Editing and revisions: attribute edits on the source text, prepared
//! candidate files, patch evaluation and commits. Behind the `edit` feature.

use crate::{
    GeometrySettings, Kernel, diagnostics_json, effective_settings, engine_for,
    parse_geometry_settings, precise_now_ms,
};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet};
use tessifc_engine::pack::{PackState, Packer};
use tessifc_engine::revision::{ChangeImpact, compare_revisions_with_sources};
use tessifc_engine::{ProductOutcome, ProductState};
use tessifc_model::Model;
use tessifc_pack::StreamPosition;
use tessifc_step::{AttributeEdit, EditValue, apply_edits, parse};
use wasm_bindgen::prelude::*;

/// Everything a kernel keeps for editing: revisions, candidates and the
/// geometry frame a patch must match.
#[derive(Default)]
pub(crate) struct EditState {
    pub(crate) revisions: BTreeMap<u32, u64>,
    pub(crate) prepared: BTreeMap<u32, PreparedRevision>,
    /// Dangling references of each committed model, found once and carried across revisions.
    pub(crate) dangling: BTreeMap<u32, BTreeSet<(u32, u32)>>,
    next_candidate: u64,
    geometry_basis: BTreeMap<u32, GeometryBasis>,
}

impl EditState {
    pub(crate) fn new() -> Self {
        EditState {
            next_candidate: 1,
            ..EditState::default()
        }
    }

    pub(crate) fn open(&mut self, model_id: u32) {
        self.revisions.insert(model_id, 0);
    }

    pub(crate) fn close(&mut self, model_id: u32) {
        self.geometry_basis.remove(&model_id);
        self.prepared.remove(&model_id);
        self.dangling.remove(&model_id);
        self.revisions.remove(&model_id);
    }

    pub(crate) fn close_all(&mut self) {
        self.geometry_basis.clear();
        self.prepared.clear();
        self.dangling.clear();
        self.revisions.clear();
    }

    /// A fresh whole-model evaluation fixes the frame every later patch must match.
    pub(crate) fn set_basis(&mut self, model_id: u32, settings: serde_json::Value, offset: DVec3) {
        self.prepared.remove(&model_id);
        self.geometry_basis
            .insert(model_id, GeometryBasis { settings, offset });
    }
}

/// Milliseconds spent in each stage of preparing and evaluating a revision.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RevisionTimings {
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
pub(crate) struct PreparedRevision {
    pub(crate) source: Vec<u8>,
    pub(crate) model: Model,
    pub(crate) impact: ChangeImpact,
    pub(crate) base_revision: u64,
    pub(crate) revision: u64,
    pub(crate) token: u64,
    pub(crate) evaluated: bool,
    pub(crate) accepted: bool,
    pub(crate) outcomes: Vec<ProductOutcome>,
    pub(crate) diagnostics: Vec<tessifc_step::Diagnostic>,
    pub(crate) evaluation_settings: Option<serde_json::Value>,
    pub(crate) model_offset: Option<[f64; 3]>,
    pub(crate) refused_boolean_products: Vec<u32>,
    pub(crate) timings: RevisionTimings,
    pub(crate) dangling: BTreeSet<(u32, u32)>,
}

pub(crate) struct GeometryBasis {
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

#[wasm_bindgen]
impl Kernel {
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
        self.edit.revisions.get(&model_id).map(u64::to_string)
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
        self.edit
            .prepared
            .get(&model_id)
            .map(PreparedRevision::report)
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
        let options = parse_geometry_settings(options)?;
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
        let options = parse_geometry_settings(options)?;
        Ok(Some(evaluate_subset(model, express_ids, &options).bytes))
    }
}

impl Kernel {
    pub(crate) fn check_candidate_token(&self, model_id: u32, token: &str) -> Result<(), String> {
        let candidate = self
            .edit
            .prepared
            .get(&model_id)
            .ok_or("no prepared revision")?;
        if token != candidate.token.to_string() {
            return Err("stale candidate token".into());
        }
        Ok(())
    }

    pub(crate) fn discard_revision_inner(
        &mut self,
        model_id: u32,
        candidate_token: &str,
    ) -> Result<bool, String> {
        if !self.edit.prepared.contains_key(&model_id) {
            return Ok(false);
        }
        self.check_candidate_token(model_id, candidate_token)?;
        Ok(self.edit.prepared.remove(&model_id).is_some())
    }
    pub(crate) fn check_base_revision(
        &self,
        model_id: u32,
        base_revision: &str,
    ) -> Result<u64, String> {
        let revision = *self
            .edit
            .revisions
            .get(&model_id)
            .ok_or("model is not open")?;
        if base_revision != revision.to_string() {
            return Err(format!("stale base revision: expected {revision}"));
        }
        Ok(revision)
    }

    pub(crate) fn prepare_revision_inner(
        &mut self,
        model_id: u32,
        source: Vec<u8>,
        base_revision: &str,
    ) -> Result<String, String> {
        let base = self.check_base_revision(model_id, base_revision)?;
        if self.edit.prepared.contains_key(&model_id) {
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
            .edit
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
        let token = self.edit.next_candidate;
        self.edit.next_candidate = token.checked_add(1).ok_or("candidate counter exhausted")?;
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
        self.edit.prepared.insert(model_id, prepared);
        Ok(report)
    }

    pub(crate) fn prepare_attribute_edits_inner(
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

    pub(crate) fn evaluate_prepared_revision_inner(
        &mut self,
        model_id: u32,
        mut options: GeometrySettings,
    ) -> Result<Vec<u8>, String> {
        if let Some(basis) = self.edit.geometry_basis.get(&model_id) {
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
            .edit
            .prepared
            .get_mut(&model_id)
            .ok_or("no prepared revision")?;
        if self.edit.revisions.get(&model_id) != Some(&prepared.base_revision) {
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

    pub(crate) fn commit_revision_inner(
        &mut self,
        model_id: u32,
        base_revision: &str,
    ) -> Result<String, String> {
        let base = self.check_base_revision(model_id, base_revision)?;
        let prepared = self
            .edit
            .prepared
            .get(&model_id)
            .ok_or("no prepared revision")?;
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
            .edit
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
            self.edit.geometry_basis.insert(
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
        self.edit.revisions.insert(model_id, prepared.revision);
        self.edit.dangling.insert(model_id, prepared.dangling);
        Ok(prepared.revision.to_string())
    }

    pub(crate) fn replace_argument(
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

    pub(crate) fn replace_edits(
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
            .edit
            .revisions
            .get(&model_id)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| JsValue::from_str("revision counter exhausted"))?;
        self.edit.prepared.remove(&model_id);
        self.edit.dangling.remove(&model_id);
        self.edit.revisions.insert(model_id, revision);
        self.geometry.remove(&model_id);
        // A finished stream still describes the pack the viewer holds; keep it.
        if let Some(stream) = self.streams.get(&model_id)
            && !stream.session().is_finished()
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
        ProductState::EmptyOrFailed
            | ProductState::Pending
            | ProductState::NoUsableRepresentation
            | ProductState::SkippedByLimit
    )
}

fn evaluate_subset(
    model: &Model,
    express_ids: &[u32],
    options: &GeometrySettings,
) -> SubsetEvaluation {
    let started = precise_now_ms();
    let mut session = engine_for(options.geometry.clone()).session(model);
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
            known_textures: Default::default(),
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
