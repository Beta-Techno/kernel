mod artifacts;
mod events;
mod run_dir;
mod run_id;
mod runner;
mod schema;
mod work_unit;

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process;

use anyhow::{Context, Result};
use artifacts::Spec;
use clap::Parser;
use events::EventWriter;
use run_dir::provision;
use runner::execute;
use work_unit::WorkUnit;

/// CLI for submitting WorkUnits defined by runfmt/0.1.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Path to a WorkUnit file (JSON or TOML).
    #[arg(long, value_name = "FILE")]
    spec: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut buf = Vec::new();
    File::open(&cli.spec)
        .with_context(|| format!("unable to open spec {:?}", cli.spec))?
        .read_to_end(&mut buf)?;

    let work_unit_value = parse_work_unit_value(&cli.spec, &buf)?;
    schema::validate_work_unit(&work_unit_value)?;
    let work_unit: WorkUnit =
        serde_json::from_value(work_unit_value).context("failed to deserialize WorkUnit")?;

    let spec_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&buf);
        hex::encode(hasher.finalize())
    };

    let run_id = work_unit.id.clone().unwrap_or_else(run_id::new_run_id);

    let workspace_mode = work_unit.target.workspace_mode;
    let paths = provision(&run_id, workspace_mode)?;

    let mut events = EventWriter::new(run_id.clone(), paths.events_norm.clone())?;
    events.emit(
        "run.created",
        &serde_json::json!({
            "kind": work_unit.kind.clone(),
            "submitted_by": whoami::username(),
        }),
    )?;

    let spec = Spec {
        path: cli.spec.display().to_string(),
        hash: spec_hash,
    };

    let driver_result = execute(&work_unit, &run_id, &spec, &paths, &mut events)?;

    println!(
        "run {} completed with status {:?}",
        run_id, driver_result.status
    );

    process::exit(driver_result.status.exit_code());
}

fn parse_work_unit_value(path: &PathBuf, bytes: &[u8]) -> Result<serde_json::Value> {
    if path.extension().map(|ext| ext == "toml").unwrap_or(false) {
        let toml_value: toml::Value =
            toml::from_str(std::str::from_utf8(bytes)?).context("failed to parse WorkUnit TOML")?;
        serde_json::to_value(toml_value).context("failed to convert TOML WorkUnit into JSON value")
    } else {
        serde_json::from_slice(bytes).context("failed to parse WorkUnit JSON")
    }
}
