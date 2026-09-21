// SPDX-License-Identifier: Apache-2.0
//! Source-preserving IFC edits.

use std::path::Path;
use std::process::ExitCode;
use tessifc_model::Model;
use tessifc_step::{AttributeEdit, EditValue, ParseOptions, apply_edits, parse};

pub(crate) struct Options<'a> {
    pub id: u32,
    pub attribute: Option<&'a str>,
    pub argument: Option<usize>,
    pub value: &'a str,
    pub raw: bool,
}

pub(crate) fn run(input: &Path, output: &Path, options: Options<'_>) -> ExitCode {
    if same_path(input, output) {
        eprintln!(
            "refusing to overwrite the input IFC; choose a different --output so the original remains recoverable"
        );
        return ExitCode::from(2);
    }

    let source = match std::fs::read(input) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not read {}: {error}", input.display());
            return ExitCode::from(2);
        }
    };
    // An archive is edited as the text inside it and written back as plain IFC.
    let (image, source) = tessifc_step::open_source(source, &ParseOptions::default());
    let model = Model::new(image);
    let Some(entity) = model.entity(options.id) else {
        eprintln!("IFC entity #{} does not exist", options.id);
        return ExitCode::from(2);
    };

    let (argument, leaf_class) = match (options.attribute, options.argument) {
        (Some(name), None) => match entity.attribute_location(name) {
            Some(location) => (
                location.argument_index,
                location.leaf_class.map(str::to_owned),
            ),
            None => {
                eprintln!(
                    "{} #{} has no attribute named {name}",
                    entity.class_name(),
                    options.id
                );
                return ExitCode::from(2);
            }
        },
        (None, Some(index)) if !entity.is_complex() => (index, None),
        (None, Some(_)) => {
            eprintln!("complex IFC entities require --attribute so the leaf class is unambiguous");
            return ExitCode::from(2);
        }
        _ => {
            eprintln!("provide exactly one of --attribute or --argument");
            return ExitCode::from(2);
        }
    };

    let value = if options.raw {
        EditValue::Raw(options.value.to_owned())
    } else {
        EditValue::String(options.value.to_owned())
    };
    let edited = match apply_edits(
        &source,
        model.image(),
        &[AttributeEdit {
            express_id: options.id,
            argument_index: argument,
            leaf_class,
            value,
        }],
    ) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not edit {}: {error}", input.display());
            return ExitCode::from(2);
        }
    };

    // Reparse before writing. A source edit must never turn a readable model
    // into an empty or structurally different one without being noticed.
    let verification = parse(&edited, &ParseOptions::default());
    if verification.len() != model.len() || verification.entry(options.id).is_none() {
        eprintln!("edited IFC failed structural verification; no file was written");
        return ExitCode::from(1);
    }

    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("could not create {}: {error}", parent.display());
        return ExitCode::from(2);
    }
    if let Err(error) = std::fs::write(output, &edited) {
        eprintln!("could not write {}: {error}", output.display());
        return ExitCode::from(2);
    }

    let field = options
        .attribute
        .map(str::to_owned)
        .unwrap_or_else(|| format!("argument {argument}"));
    println!(
        "edited {} #{} {field}; wrote {} bytes to {}",
        entity.class_name(),
        options.id,
        edited.len(),
        output.display()
    );
    ExitCode::SUCCESS
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}
