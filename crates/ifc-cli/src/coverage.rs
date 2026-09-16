// SPDX-License-Identifier: Apache-2.0
//! `tessifc coverage`: what the evaluator registry handles, generated from the
//! registry so `docs/coverage.md` cannot claim an evaluator that does not exist.

use serde::Serialize;
use std::process::ExitCode;
use tessifc_geom::Registry;
use tessifc_step::SchemaId;

#[derive(Serialize)]
struct Row {
    class: String,
    kind: String,
    schemas: Vec<String>,
}

const SCHEMAS: [SchemaId; 3] = [SchemaId::Ifc2x3, SchemaId::Ifc4, SchemaId::Ifc4x3];

pub fn run(json: bool, markdown: bool, inventory: bool) -> ExitCode {
    if inventory {
        let schemas: Vec<_> = SchemaId::all().iter().map(|&schema| {
            serde_json::json!({"schema":schema.as_str(),"entities":Registry::defaults(schema).inventory()})
        }).collect();
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "meaning":"Dispatch inventory only; routes are not conformance or complete geometry support claims.",
            "schemas":schemas
        })).expect("static inventory serialises"));
        return ExitCode::SUCCESS;
    }
    // A class handled in one schema and absent from another is worth seeing,
    // so the schemas are merged rather than reported one at a time.
    let mut rows: std::collections::BTreeMap<(String, String), Vec<String>> = Default::default();
    for schema in SCHEMAS {
        let registry = Registry::defaults(schema);
        for (class, kind) in registry.coverage() {
            rows.entry((class.to_string(), kind.to_string()))
                .or_default()
                .push(schema.as_str().to_string());
        }
    }

    let rows: Vec<Row> = rows
        .into_iter()
        .map(|((class, kind), schemas)| Row {
            class,
            kind,
            schemas,
        })
        .collect();

    if json {
        match serde_json::to_string_pretty(&rows) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("tessifc: cannot serialise coverage: {error}");
                return ExitCode::from(2);
            }
        }
        return ExitCode::SUCCESS;
    }

    if markdown {
        println!("| Item | Kind | Schemas |");
        println!("|---|---|---|");
        for row in &rows {
            println!(
                "| `{}` | {} | {} |",
                row.class,
                row.kind,
                row.schemas.join(", ")
            );
        }
        return ExitCode::SUCCESS;
    }

    let mut by_kind: std::collections::BTreeMap<&str, Vec<&Row>> = Default::default();
    for row in &rows {
        by_kind.entry(row.kind.as_str()).or_default().push(row);
    }
    for (kind, items) in &by_kind {
        println!("{kind} ({} classes)", items.len());
        for row in items {
            let all = row.schemas.len() == SCHEMAS.len();
            println!(
                "  {:<44} {}",
                row.class,
                if all {
                    "all schemas".to_string()
                } else {
                    row.schemas.join(", ")
                }
            );
        }
        println!();
    }
    println!("{} classes in total", rows.len());
    ExitCode::SUCCESS
}
