// SPDX-License-Identifier: Apache-2.0
//! `tessifc info`: read a file and report what is in it. The JSON form is a
//! contract: add fields freely, never rename or repurpose one.

use crate::alloc;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;
use tessifc_step::{ParseOptions, SchemaId};

#[derive(Serialize)]
struct Report {
    /// Name and version of the tool that produced this report.
    ///
    /// Here so a consumer can record which build produced the numbers without
    /// spawning a second process to ask.
    tool: String,
    file: String,
    bytes: usize,
    schema: String,
    schema_declared: Vec<String>,
    schema_approximate: bool,
    entities: usize,
    products: BTreeMap<String, usize>,
    product_total: usize,
    classes: BTreeMap<String, usize>,
    unknown_classes: BTreeMap<String, usize>,
    parse_ms: f64,
    throughput_mb_s: f64,
    image_bytes: usize,
    peak_memory_bytes: usize,
    diagnostics: DiagnosticReport,
    header: HeaderReport,
}

#[derive(Serialize)]
struct DiagnosticReport {
    total: usize,
    errors: usize,
    warnings: usize,
    by_code: BTreeMap<String, usize>,
    items: Vec<DiagnosticItem>,
}

#[derive(Serialize)]
struct DiagnosticItem {
    code: String,
    severity: String,
    line: u32,
    express_id: Option<u32>,
    message: String,
}

#[derive(Serialize)]
struct HeaderReport {
    name: String,
    time_stamp: String,
    preprocessor_version: String,
    originating_system: String,
    author: Vec<String>,
    organization: Vec<String>,
    description: Vec<String>,
}

pub fn run(
    path: &Path,
    json: bool,
    show_diagnostics: bool,
    schema_override: Option<&str>,
    strict: bool,
) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("tessifc: cannot read {}: {e}", path.display());
            return ExitCode::from(2);
        }
    };

    let mut options = ParseOptions::default();
    if let Some(name) = schema_override {
        match SchemaId::detect(name) {
            Some((id, _)) => options.schema_override = Some(id),
            None => {
                eprintln!("tessifc: {name} is not a schema I have tables for");
                return ExitCode::from(2);
            }
        }
    }

    let started = Instant::now();
    let image = tessifc_step::open(&bytes, &options);
    let elapsed = started.elapsed();

    let parse_ms = elapsed.as_secs_f64() * 1000.0;
    let throughput = if elapsed.as_secs_f64() > 0.0 {
        (bytes.len() as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
    } else {
        0.0
    };

    let schema = image.schema_tables();
    let product = schema.class_by_name("IfcProduct");

    let mut classes = BTreeMap::new();
    let mut products = BTreeMap::new();
    let mut unknown_classes = BTreeMap::new();
    let mut product_total = 0usize;

    for (class_id, count) in image.populated_classes() {
        if class_id == tessifc_step::CLASS_UNKNOWN {
            continue;
        }
        let name = schema.class(class_id).name.to_string();
        if let Some(product) = product
            && schema.is_a(class_id, product)
        {
            products.insert(name.clone(), count);
            product_total += count;
        }
        classes.insert(name, count);
    }

    for &(express_id, string_id) in &image.unknown_class_names {
        let _ = express_id;
        *unknown_classes
            .entry(image.strings.decode(string_id))
            .or_insert(0usize) += 1;
    }

    let mut by_code: BTreeMap<String, usize> = BTreeMap::new();
    for d in image.diagnostics.items() {
        *by_code.entry(d.code.as_str().to_string()).or_insert(0) += 1;
    }
    let items = if show_diagnostics || json {
        image
            .diagnostics
            .items()
            .iter()
            .take(if show_diagnostics { usize::MAX } else { 50 })
            .map(|d| DiagnosticItem {
                code: d.code.as_str().to_string(),
                severity: d.severity.as_str().to_string(),
                line: d.line,
                express_id: d.express_id,
                message: d.message.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };

    let report = Report {
        tool: concat!("tessifc ", env!("CARGO_PKG_VERSION")).to_string(),
        file: path.display().to_string(),
        bytes: bytes.len(),
        schema: image.schema.as_str().to_string(),
        schema_declared: image.header.schema_identifiers.clone(),
        schema_approximate: image.schema_approximate,
        entities: image.len(),
        products,
        product_total,
        classes,
        unknown_classes,
        parse_ms,
        throughput_mb_s: throughput,
        image_bytes: image.memory_bytes(),
        peak_memory_bytes: alloc::peak(),
        diagnostics: DiagnosticReport {
            total: image.diagnostics.total(),
            errors: image.diagnostics.count_of(tessifc_step::Severity::Error),
            warnings: image.diagnostics.count_of(tessifc_step::Severity::Warning),
            by_code,
            items,
        },
        header: HeaderReport {
            name: image.header.name.clone(),
            time_stamp: image.header.time_stamp.clone(),
            preprocessor_version: image.header.preprocessor_version.clone(),
            originating_system: image.header.originating_system.clone(),
            author: image.header.author.clone(),
            organization: image.header.organization.clone(),
            description: image.header.description.clone(),
        },
    };

    let failed = strict && report.diagnostics.errors > 0;

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("tessifc: cannot serialise the report: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_human(&report, show_diagnostics);
    }

    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_human(report: &Report, show_diagnostics: bool) {
    println!("{}", report.file);
    println!("  size                {}", human_bytes(report.bytes));
    print!("  schema              {}", report.schema);
    if !report.schema_declared.is_empty() {
        print!(" (declared {})", report.schema_declared.join(", "));
    }
    if report.schema_approximate {
        print!(" [approximated]");
    }
    println!();
    if !report.header.originating_system.is_empty() {
        println!("  written by          {}", report.header.originating_system);
    }
    if !report.header.preprocessor_version.is_empty() {
        println!(
            "  preprocessor        {}",
            report.header.preprocessor_version
        );
    }
    println!("  entities            {}", report.entities);
    println!("  products            {}", report.product_total);
    println!(
        "  parse               {:.1} ms  ({:.0} MB/s)",
        report.parse_ms, report.throughput_mb_s
    );
    println!(
        "  model image         {}   peak process {}",
        human_bytes(report.image_bytes),
        human_bytes(report.peak_memory_bytes)
    );

    if !report.products.is_empty() {
        println!("\n  products by class");
        let mut rows: Vec<_> = report.products.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (name, count) in rows.iter().take(40) {
            println!("    {count:>8}  {name}");
        }
        if rows.len() > 40 {
            println!("    ... and {} more classes", rows.len() - 40);
        }
    }

    if !report.unknown_classes.is_empty() {
        println!("\n  classes not in this schema");
        for (name, count) in &report.unknown_classes {
            println!("    {count:>8}  {name}");
        }
    }

    println!(
        "\n  diagnostics         {} ({} errors, {} warnings)",
        report.diagnostics.total, report.diagnostics.errors, report.diagnostics.warnings
    );
    for (code, count) in &report.diagnostics.by_code {
        println!("    {count:>8}  {code}");
    }
    if show_diagnostics {
        for item in &report.diagnostics.items {
            let id = item
                .express_id
                .map(|i| format!(" #{i}"))
                .unwrap_or_default();
            println!(
                "    {} line {}{}: {} {}",
                item.severity, item.line, id, item.code, item.message
            );
        }
    } else if report.diagnostics.total > 0 {
        println!("    (run with --diagnostics to list them)");
    }
}

fn human_bytes(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
