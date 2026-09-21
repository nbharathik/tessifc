// SPDX-License-Identifier: Apache-2.0
//! The `tessifc` command line tool.
//!
//! ```text
//! tessifc info model.ifc            what is in this file
//! tessifc info model.ifc --json     the same, machine readable
//! tessifc convert model.ifc -o out.igp
//! tessifc edit model.ifc --id 42 --attribute Name --value "Wall" -o edited.ifc
//! tessifc coverage                  what the evaluator registry handles
//! ```

mod alloc;
mod convert;
mod coverage;
mod edit;
mod info;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[global_allocator]
static ALLOCATOR: alloc::Counting = alloc::Counting::new();

#[derive(Parser)]
#[command(
    name = "tessifc",
    version,
    about = "An Apache-2.0 IFC geometry kernel",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read a file and report what is in it.
    Info {
        /// The .ifc or .ifczip file to read.
        file: PathBuf,
        /// Machine-readable JSON output.
        #[arg(long)]
        json: bool,
        /// Print every diagnostic rather than a summary.
        #[arg(long)]
        diagnostics: bool,
        /// Read with this schema regardless of what FILE_SCHEMA says.
        #[arg(long, value_name = "IFC2X3|IFC4|IFC4X3")]
        schema: Option<String>,
        /// Exit non-zero if any diagnostic has error severity.
        #[arg(long)]
        strict: bool,
    },

    /// Evaluate geometry and write an IGP pack.
    Convert {
        /// The .ifc or .ifczip file to read.
        file: PathBuf,
        /// Where to write the pack. Omit to measure without writing.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Machine-readable JSON report.
        #[arg(long)]
        json: bool,
        /// Leave IfcSpace products out.
        #[arg(long)]
        no_spaces: bool,
        /// Include IfcOpeningElement products, which are normally holes.
        #[arg(long)]
        openings: bool,
        /// Include annotations and grids.
        #[arg(long)]
        annotations: bool,
        /// Include ports, virtual elements and structural analysis geometry.
        #[arg(long)]
        references: bool,
        /// Fixed segments per circle instead of the adaptive default.
        #[arg(long)]
        circle_segments: Option<u32>,
        /// Geometry settings as a JSON object; uses the same names as the WASM API.
        #[arg(long, value_name = "JSON")]
        settings: Option<String>,
        /// Maximum chord sagitta, in metres; overrides the JSON setting.
        #[arg(long)]
        chord_tolerance_m: Option<f64>,
        /// Refuse to write output when evaluation reports errors or degraded geometry.
        #[arg(long)]
        strict: bool,
        /// List every conversion diagnostic, not just the counts by code.
        #[arg(long)]
        diagnostics: bool,
        /// Threads for geometry evaluation. Defaults to every core; 1 is serial.
        #[arg(short, long)]
        jobs: Option<usize>,
    },

    /// Change one entity attribute without rewriting the rest of the IFC.
    Edit {
        /// The .ifc or .ifczip file to read.
        file: PathBuf,
        /// Express id, written without the leading #.
        #[arg(long)]
        id: u32,
        /// IFC attribute name, such as Name or Description.
        #[arg(long, conflicts_with = "argument")]
        attribute: Option<String>,
        /// Zero-based STEP argument index, including for unknown vendor classes.
        #[arg(long, conflicts_with = "attribute")]
        argument: Option<usize>,
        /// New text, or one complete STEP value when --raw is present.
        #[arg(long)]
        value: String,
        /// Treat --value as STEP syntax instead of encoding it as a string.
        #[arg(long)]
        raw: bool,
        /// New IFC file to write. The input is never overwritten.
        #[arg(short, long)]
        output: PathBuf,
    },

    /// What the evaluator registry handles, read out of the registry itself.
    Coverage {
        /// Machine-readable.
        #[arg(long)]
        json: bool,
        /// Include all schema entities and inherited dispatch routes as JSON.
        #[arg(long, conflicts_with = "markdown")]
        inventory: bool,
        /// A markdown table, for docs/coverage.md.
        #[arg(long)]
        markdown: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Info {
            file,
            json,
            diagnostics,
            schema,
            strict,
        } => info::run(&file, json, diagnostics, schema.as_deref(), strict),
        Command::Convert {
            file,
            output,
            json,
            no_spaces,
            openings,
            annotations,
            references,
            circle_segments,
            settings,
            chord_tolerance_m,
            strict,
            diagnostics,
            jobs,
        } => convert::run(
            &file,
            convert::Options {
                output: output.as_deref(),
                json,
                include_spaces: !no_spaces,
                include_openings: openings,
                include_annotations: annotations,
                include_references: references,
                circle_segments,
                settings: settings.as_deref(),
                chord_tolerance_m,
                strict,
                diagnostics,
                jobs,
            },
        ),
        Command::Edit {
            file,
            id,
            attribute,
            argument,
            value,
            raw,
            output,
        } => edit::run(
            &file,
            &output,
            edit::Options {
                id,
                attribute: attribute.as_deref(),
                argument,
                value: &value,
                raw,
            },
        ),
        Command::Coverage {
            json,
            markdown,
            inventory,
        } => coverage::run(json, markdown, inventory),
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    fn subcommands() -> Vec<String> {
        Cli::command()
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .filter(|name| name != "help")
            .collect()
    }

    /// The description is what crates.io shows and cannot be edited after a
    /// release, so it must list exactly the commands that exist.
    #[test]
    fn the_published_description_names_every_subcommand_and_no_other() {
        let description = env!("CARGO_PKG_DESCRIPTION");
        let mut names = subcommands();
        names.sort();
        assert_eq!(names, ["convert", "coverage", "edit", "info"]);

        let listed = description.rsplit(':').next().unwrap_or_default();
        let mut listed: Vec<&str> = listed
            .split(',')
            .map(|word| word.trim().trim_end_matches('.'))
            .collect();
        listed.sort();
        assert_eq!(listed, names, "description: {description}");
    }

    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }
}
