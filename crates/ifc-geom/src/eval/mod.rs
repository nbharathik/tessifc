// SPDX-License-Identifier: Apache-2.0
//! The evaluators, one module per family.
//!
//! A new class is a new evaluator, a test, and a line in [`register_defaults`].

use crate::registry::Registry;

pub mod alignment;
pub mod curves;
mod pcurves;
pub mod primitives;
pub mod profiles;
pub mod sectioned;
pub mod solids;
mod surface_regions;
pub mod surfaces;
pub mod sweeps;
pub mod tessellated;
pub(crate) mod uv;

/// Register everything TessIFC implements.
pub fn register_defaults(registry: &mut Registry) {
    curves::register(registry);
    alignment::register(registry);
    surfaces::register(registry);
    primitives::register(registry);
    profiles::register(registry);
    solids::register(registry);
    sweeps::register(registry);
    sectioned::register(registry);
    tessellated::register(registry);
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::context::{DiagnosticSink, EvalCtx, Settings, Tolerances};
    use crate::error::GeomError;
    use crate::registry::{Polyline3, Profile2D, Registry, Surface};
    use crate::units::Units;
    use tessifc_mesh::Mesh64;
    use tessifc_model::Model;
    use tessifc_step::{Diagnostic, ParseOptions, parse};

    /// Build an IFC4 model from a DATA section.
    pub(crate) fn model_of(data: &str) -> Model {
        model_of_schema("IFC4", data)
    }

    /// Build a model from a DATA section under the named schema, such as `IFC4X3_ADD2`.
    pub(crate) fn model_of_schema(schema: &str, data: &str) -> Model {
        let source = format!(
            "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('{schema}'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n"
        );
        let image = parse(source.as_bytes(), &ParseOptions::default());
        assert!(
            !image.diagnostics.has_errors(),
            "the fixture itself does not parse: {:?}",
            image.diagnostics.items()
        );
        Model::new(image)
    }

    fn with_ctx<T>(
        model: &Model,
        body: impl FnOnce(&Registry, &EvalCtx<'_>) -> T,
    ) -> (T, Vec<Diagnostic>) {
        let units = Units::from_model(model);
        let settings = Settings::default();
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(model, units, Tolerances::default(), &settings, &sink);
        let registry = Registry::defaults(model.image().schema);
        let value = body(&registry, &ctx);
        (value, sink.take())
    }

    pub(crate) fn eval_solid(model: &Model, id: u32) -> Result<Mesh64, GeomError> {
        with_ctx(model, |registry, ctx| {
            registry.solid(ctx, model.entity(id).expect("no such instance"))
        })
        .0
    }

    pub(crate) fn eval_solid_with_diagnostics(
        model: &Model,
        id: u32,
    ) -> (Result<Mesh64, GeomError>, Vec<Diagnostic>) {
        with_ctx(model, |registry, ctx| {
            registry.solid(ctx, model.entity(id).expect("no such instance"))
        })
    }

    /// Evaluate a solid with the `textures` setting on.
    pub(crate) fn eval_textured_solid(
        model: &Model,
        id: u32,
    ) -> (Result<Mesh64, GeomError>, Vec<Diagnostic>) {
        let units = Units::from_model(model);
        let settings = Settings {
            textures: true,
            ..Settings::default()
        };
        let sink = DiagnosticSink::default();
        let ctx = EvalCtx::new(model, units, Tolerances::default(), &settings, &sink);
        let registry = Registry::defaults(model.image().schema);
        let value = registry.solid(&ctx, model.entity(id).expect("no such instance"));
        (value, sink.take())
    }

    pub(crate) fn eval_profile(model: &Model, id: u32) -> Result<Profile2D, GeomError> {
        with_ctx(model, |registry, ctx| {
            registry.profile(ctx, model.entity(id).expect("no such instance"))
        })
        .0
    }

    pub(crate) fn eval_profile_with_diagnostics(
        model: &Model,
        id: u32,
    ) -> (Result<Profile2D, GeomError>, Vec<Diagnostic>) {
        with_ctx(model, |registry, ctx| {
            registry.profile(ctx, model.entity(id).expect("no such instance"))
        })
    }

    pub(crate) fn eval_surface(model: &Model, id: u32) -> Result<Surface, GeomError> {
        with_ctx(model, |registry, ctx| {
            registry.surface(ctx, model.entity(id).expect("no such instance"))
        })
        .0
    }

    pub(crate) fn eval_curve(model: &Model, id: u32) -> Result<Polyline3, GeomError> {
        with_ctx(model, |registry, ctx| {
            registry.curve(ctx, model.entity(id).expect("no such instance"))
        })
        .0
    }

    pub(crate) fn eval_curve_with_diagnostics(
        model: &Model,
        id: u32,
    ) -> (Result<Polyline3, GeomError>, Vec<Diagnostic>) {
        with_ctx(model, |registry, ctx| {
            registry.curve(ctx, model.entity(id).expect("no such instance"))
        })
    }
}
