// SPDX-License-Identifier: Apache-2.0
//! `tessifc convert`: an IFC file in, an IGP pack out.

use crate::alloc;
use serde::Serialize;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;
use tessifc_engine::Engine;
use tessifc_engine::pack::Packer;
use tessifc_geom::Settings;
use tessifc_model::Model;
use tessifc_step::ParseOptions;

#[derive(Serialize)]
struct Report {
    tool: String,
    file: String,
    bytes: usize,
    schema: String,
    output: String,
    output_bytes: usize,
    entities: usize,
    products_considered: usize,
    products_filtered: usize,
    products_with_geometry: usize,
    geometries: usize,
    instances: usize,
    shared_instances: usize,
    triangles: usize,
    threads: usize,
    parse_ms: f64,
    geometry_ms: f64,
    pack_ms: f64,
    boolean_cpu_ms: f64,
    openings_cpu_ms: f64,
    boolean_calls: u32,
    openings_calls: u32,
    total_ms: f64,
    peak_memory_bytes: usize,
    model_offset: [f64; 3],
    length_scale_to_m: f64,
    effective_settings: Settings,
    product_outcomes: Vec<tessifc_engine::ProductOutcome>,
    limit_reached: Option<tessifc_engine::LimitReached>,
    output_written: bool,
    product_classes: std::collections::BTreeMap<String, ProductClassSummary>,
    diagnostics: DiagnosticSummary,
}

#[derive(Serialize)]
struct ProductClassSummary {
    total: usize,
    with_representation: usize,
    expected_geometry: usize,
    with_geometry: usize,
}

#[derive(Serialize)]
struct DiagnosticSummary {
    total: usize,
    infos: usize,
    errors: usize,
    warnings: usize,
    by_code: std::collections::BTreeMap<String, usize>,
    by_class: std::collections::BTreeMap<String, usize>,
    by_code_and_class:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, usize>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    items: Vec<DiagnosticItem>,
}

#[derive(Serialize)]
struct DiagnosticItem {
    express_id: Option<u32>,
    class: String,
    severity: String,
    code: String,
    message: String,
}

/// Options a caller can set.
pub struct Options<'a> {
    /// Where to write the pack. `None` measures without writing.
    pub output: Option<&'a Path>,
    /// Machine-readable report.
    pub json: bool,
    /// Include `IfcSpace`.
    pub include_spaces: bool,
    /// Include `IfcOpeningElement`.
    pub include_openings: bool,
    /// Include annotations and grids.
    pub include_annotations: bool,
    /// Include non-physical reference products.
    pub include_references: bool,
    /// Fixed circle segments, or adaptive.
    pub circle_segments: Option<u32>,
    /// Geometry settings JSON shared with the WASM API.
    pub settings: Option<&'a str>,
    /// Chord tolerance override, in metres.
    pub chord_tolerance_m: Option<f64>,
    /// Refuse output that lost or approximated geometry.
    pub strict: bool,
    /// List every diagnostic rather than only counting them by code.
    pub diagnostics: bool,
    /// Threads for evaluation; `None` uses every core.
    pub jobs: Option<usize>,
}

pub fn run(path: &Path, options: Options<'_>) -> ExitCode {
    let started = Instant::now();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("tessifc: cannot read {}: {error}", path.display());
            return ExitCode::from(2);
        }
    };

    let parse_started = Instant::now();
    let image = tessifc_step::open(&bytes, &ParseOptions::default());
    let schema = image.schema.as_str().to_string();
    let entities = image.len();
    let model = Model::new(image);
    let parse_ms = parse_started.elapsed().as_secs_f64() * 1000.0;

    let decoded = options.settings.map(|json| {
        let value: serde_json::Value = serde_json::from_str(json)?;
        if !value.is_object() {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "expected a JSON object",
            ));
        }
        serde_json::from_value::<Settings>(value)
    });
    let mut settings: Settings = match decoded.transpose() {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => {
            eprintln!("tessifc: invalid geometry settings: {error}");
            return ExitCode::from(2);
        }
    };
    if !options.include_spaces {
        settings.include_spaces = false;
    }
    if options.include_openings {
        settings.include_openings = true;
    }
    if options.include_annotations {
        settings.include_annotations = true;
    }
    if options.include_references {
        settings.include_references = true;
    }
    if let Some(value) = options.circle_segments {
        settings.circle_segments = Some(value);
    }
    if let Some(value) = options.chord_tolerance_m {
        settings.chord_tolerance_m = value;
    }
    if let Err(error) = settings.validate() {
        eprintln!("tessifc: {error}");
        return ExitCode::from(2);
    }

    // The pool is process-wide and built once; a thread per core times a few is all the work can use.
    let threads = options
        .jobs
        .unwrap_or_else(rayon::current_num_threads)
        .clamp(1, 512);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global();
    let threads = rayon::current_num_threads();

    let geometry_started = Instant::now();
    let engine = Engine::with_settings(settings.clone());
    let selected_products: std::collections::HashSet<u32> =
        engine.products(&model).into_iter().collect();
    let result = engine.evaluate(&model);
    let geometry_ms = geometry_started.elapsed().as_secs_f64() * 1000.0;
    let timings = result.timings;

    let pack_started = Instant::now();
    let mut packer = Packer::new(&schema, result.units.length_to_m, result.model_offset);
    packer.set_georef(result.georef.clone());
    let shape_classes: Vec<String> = result
        .shapes
        .iter()
        .map(|shape| shape.class.clone())
        .collect();
    let products_with_geometry = result.shapes.len();
    // One record per colour: a window's frame and glass are two records sharing an express id.
    for shape in &result.shapes {
        packer.add_shape_ref(shape);
    }
    let triangles = packer.triangles();

    let mut by_code: std::collections::BTreeMap<String, usize> = Default::default();
    let mut by_class: std::collections::BTreeMap<String, usize> = Default::default();
    let mut by_code_and_class: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, usize>,
    > = Default::default();
    let mut diagnostic_items = Vec::new();
    let mut diagnostic_infos = 0;
    let mut diagnostic_errors = 0;
    let mut diagnostic_warnings = 0;
    for diagnostic in &result.diagnostics {
        let code = diagnostic.code.as_str().to_string();
        let class = match diagnostic.express_id {
            Some(id) => model.image().class_name_of(id),
            None => "<model>".to_owned(),
        };
        let class_name = class.clone();
        *by_code.entry(code.clone()).or_insert(0) += 1;
        *by_class.entry(class.clone()).or_insert(0) += 1;
        *by_code_and_class
            .entry(code)
            .or_default()
            .entry(class)
            .or_insert(0) += 1;
        match diagnostic.severity {
            tessifc_step::Severity::Info => diagnostic_infos += 1,
            tessifc_step::Severity::Error => diagnostic_errors += 1,
            tessifc_step::Severity::Warning => diagnostic_warnings += 1,
        }
        if options.diagnostics {
            diagnostic_items.push(DiagnosticItem {
                express_id: diagnostic.express_id,
                class: class_name,
                severity: diagnostic.severity.as_str().to_string(),
                code: diagnostic.code.as_str().to_string(),
                message: diagnostic.message.clone(),
            });
        }
    }
    packer.add_diagnostics(&result.diagnostics);

    packer.set_stat("products", products_with_geometry as f64);
    packer.set_stat("triangles", triangles as f64);
    packer.set_stat("parse_ms", parse_ms);
    packer.set_stat("geometry_ms", geometry_ms);
    if let Some(limit) = &result.limit_reached {
        packer.set_stat("limit_reached", 1.0);
        packer.set_stat("products_skipped", limit.products_skipped as f64);
    }

    let geometries = packer.geometry_count();
    let instances = packer.instance_count();
    let shared_instances = packer.shared_instances();
    let pack = packer.finish();
    let pack_ms = pack_started.elapsed().as_secs_f64() * 1000.0;

    let mut product_classes = std::collections::BTreeMap::new();
    if let Some(product) = model.schema().class_by_name("IfcProduct") {
        for (class, count) in model.image().populated_classes() {
            if class != tessifc_step::CLASS_UNKNOWN && model.schema().is_a(class, product) {
                product_classes.insert(
                    model.schema().class(class).name.to_owned(),
                    ProductClassSummary {
                        total: count,
                        with_representation: model
                            .entities_of_class(class)
                            .filter(|product| product.attr("Representation").as_entity().is_some())
                            .count(),
                        expected_geometry: model
                            .entities_of_class(class)
                            .filter(|product| {
                                selected_products.contains(&product.id())
                                    && tessifc_geom::product::representation_of(*product).is_some()
                            })
                            .count(),
                        with_geometry: 0,
                    },
                );
            }
        }
    }
    for class in shape_classes {
        product_classes
            .entry(class)
            .or_insert(ProductClassSummary {
                total: 0,
                with_representation: 0,
                expected_geometry: 0,
                with_geometry: 0,
            })
            .with_geometry += 1;
    }

    let refused = options.strict
        && result.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == tessifc_step::Severity::Error
                || diagnostic.code.as_str().starts_with("E_")
                || matches!(
                    diagnostic.code.as_str(),
                    "W_PCURVE_DOMAIN_RECOVERED"
                        | "W_PCURVE_REFERENCE_RECOVERED"
                        | "W_PCURVE_3D_FALLBACK"
                        | "W_EDGE_ORIENTATION_RECOVERED"
                        | "W_TESSELLATION_TOLERANCE_UNMET"
                        | "W_PROFILE_DETAIL_APPROXIMATED"
                        | "W_TRIM_IGNORED"
                        | "W_SWEEP_PARAMETERS_APPROXIMATED"
                        | "W_SWEEP_FILLET_IGNORED"
                        | "W_BOUNDING_BOX_SUBSTITUTED"
                        | "W_NON_MANIFOLD_INPUT"
                        | "W_OPENING_CUT_ON_SURFACE"
                        | "W_BOOLEAN_REFUSED"
                        | "W_PLACEMENT_UNSUPPORTED"
                        | "W_ALIGNMENT_SEGMENT_GAP"
                        | "W_CANT_APPROXIMATED"
                        | "W_LINEAR_PLACEMENT_MISMATCH"
                        | "W_NO_DRAWN_REPRESENTATION"
                        | "W_TEXTURE_DROPPED_BY_BOOLEAN"
                        | "W_TEXTURE_OMITTED"
                )
        });
    if !refused
        && let Some(target) = options.output
        && let Err(error) = write_whole(target, &pack)
    {
        eprintln!("tessifc: cannot write {}: {error}", target.display());
        return ExitCode::from(2);
    }

    let report = Report {
        tool: concat!("tessifc ", env!("CARGO_PKG_VERSION")).to_string(),
        file: path.display().to_string(),
        bytes: bytes.len(),
        schema,
        output: options
            .output
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        output_bytes: pack.len(),
        entities,
        products_considered: result.products_considered,
        products_filtered: result.products_filtered,
        products_with_geometry,
        geometries,
        instances,
        shared_instances,
        triangles,
        threads,
        parse_ms,
        geometry_ms,
        pack_ms,
        // Processor time, summed over threads: on a parallel run these exceed
        // geometry_ms, which is wall time.
        boolean_cpu_ms: timings.boolean_ms,
        openings_cpu_ms: timings.openings_ms,
        boolean_calls: timings.boolean_calls,
        openings_calls: timings.openings_calls,
        total_ms: started.elapsed().as_secs_f64() * 1000.0,
        peak_memory_bytes: alloc::peak(),
        model_offset: result.model_offset.to_array(),
        length_scale_to_m: result.units.length_to_m,
        effective_settings: settings,
        product_outcomes: engine.outcomes(&model, &result),
        limit_reached: result.limit_reached.clone(),
        output_written: options.output.is_some() && !refused,
        product_classes,
        diagnostics: DiagnosticSummary {
            total: result.diagnostics.len(),
            infos: diagnostic_infos,
            errors: diagnostic_errors,
            warnings: diagnostic_warnings,
            by_code,
            by_class,
            by_code_and_class,
            items: diagnostic_items,
        },
    };

    if options.json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("tessifc: cannot serialise the report: {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        println!("{}", report.file);
        println!("  schema              {}", report.schema);
        println!("  entities            {}", report.entities);
        println!(
            "  products            {} of {} considered, {} filtered out",
            report.products_with_geometry, report.products_considered, report.products_filtered
        );
        println!(
            "  geometries          {} unique, {} records, {} placed from shared families",
            report.geometries, report.instances, report.shared_instances
        );
        println!("  triangles           {}", report.triangles);
        if let Some(limit) = &report.limit_reached {
            println!(
                "  stopped             {} reached {} of {}; {} products skipped",
                limit.which.as_str(),
                limit.reached,
                limit.limit,
                limit.products_skipped
            );
        }
        println!(
            "  parse               {:.1} ms      geometry {:.1} ms on {} threads      pack {:.1} ms",
            report.parse_ms, report.geometry_ms, report.threads, report.pack_ms
        );
        if report.boolean_calls > 0 || report.openings_calls > 0 {
            println!(
                "  booleans            {:.1} ms cpu over {} operators, {:.1} ms cpu cutting {} products' openings",
                report.boolean_cpu_ms,
                report.boolean_calls,
                report.openings_cpu_ms,
                report.openings_calls
            );
        }
        println!(
            "  peak memory         {:.1} MB",
            report.peak_memory_bytes as f64 / 1048576.0
        );
        if report.output_written {
            println!(
                "  wrote               {} ({:.1} kB)",
                report.output,
                report.output_bytes as f64 / 1024.0
            );
        }
        if report.diagnostics.total > 0 {
            println!("\n  diagnostics         {}", report.diagnostics.total);
            for (code, count) in &report.diagnostics.by_code {
                println!("    {count:>8}  {code}");
            }
            for item in &report.diagnostics.items {
                let at = match item.express_id {
                    Some(id) => format!("#{id} {}", item.class),
                    None => "<model>".to_owned(),
                };
                println!("    {:<8} {:<28} {at}", item.severity, item.code);
                println!("             {}", item.message);
            }
            if report.diagnostics.items.is_empty() {
                println!("    (run with --diagnostics to list them)");
            }
        }
    }
    if refused {
        eprintln!(
            "tessifc: strict conversion refused incomplete or degraded geometry; output was not written"
        );
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Write through a sibling file and rename, so a failure leaves no half-written pack.
fn write_whole(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut partial = target.as_os_str().to_owned();
    partial.push(".partial");
    let partial = std::path::PathBuf::from(partial);
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}
