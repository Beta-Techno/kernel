mod artifacts;
mod events;
mod run_dir;
mod run_id;
mod runner;
mod schema;
mod work_unit;

use std::cmp::Reverse;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use artifacts::Spec;
use clap::{Parser, Subcommand};
use events::EventWriter;
use run_dir::provision;
use runner::execute;
use serde::Deserialize;
use work_unit::WorkUnit;

/// CLI for running and inspecting WorkUnits defined by runfmt/0.1.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Submit and execute a WorkUnit spec.
    Run {
        /// Path to a WorkUnit file (JSON or TOML).
        #[arg(long, value_name = "FILE")]
        spec: PathBuf,
        /// Emit machine-readable run result JSON.
        #[arg(long)]
        json: bool,
    },
    /// List recent completed runs.
    List {
        /// Number of runs to print.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show RUN.json for a run ID.
    Show {
        /// Run ID (folder name under runs/).
        run_id: String,
    },
    /// Re-execute the original spec from a previous run.
    Rerun {
        /// Run ID to replay using its stored spec.path.
        run_id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let code = match cli.command {
        Commands::Run { spec, json } => run_spec(&spec, json)?,
        Commands::List { limit } => {
            list_runs(limit)?;
            0
        }
        Commands::Show { run_id } => {
            show_run(&run_id)?;
            0
        }
        Commands::Rerun { run_id } => rerun(&run_id)?,
    };

    process::exit(code);
}

fn run_spec(spec_path: &Path, json_output: bool) -> Result<i32> {
    let mut buf = Vec::new();
    File::open(spec_path)
        .with_context(|| format!("unable to open spec {:?}", spec_path))?
        .read_to_end(&mut buf)?;

    let work_unit_value = parse_work_unit_value(spec_path, &buf)?;
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
    let paths = provision(&run_id, work_unit.target.workspace_mode)?;

    let mut events = EventWriter::new(run_id.clone(), paths.events_norm.clone())?;
    events.emit(
        "run.created",
        &serde_json::json!({
            "kind": work_unit.kind.clone(),
            "submitted_by": whoami::username(),
        }),
    )?;

    let spec = Spec {
        path: spec_path.display().to_string(),
        hash: spec_hash,
    };
    let driver_result = execute(&work_unit, &run_id, &spec, &paths, &mut events)?;
    let exit_code = driver_result.status.exit_code();
    let status = match driver_result.status {
        runner::RunStatus::Ok => "ok",
        runner::RunStatus::NeedsHuman => "needs_human",
        runner::RunStatus::Failed => "failed",
    };

    if json_output {
        let payload = serde_json::json!({
            "run_id": run_id,
            "status": status,
            "exit_code": exit_code,
            "run_dir": paths.run_dir.display().to_string(),
            "run_record": paths.run_dir.join("RUN.json").display().to_string(),
        });
        println!("{}", serde_json::to_string(&payload)?);
    } else {
        println!("run {} completed with status {}", run_id, status);
    }
    Ok(exit_code)
}

fn list_runs(limit: usize) -> Result<()> {
    let runs_dir = run_dir::root().join("runs");
    if !runs_dir.exists() {
        println!("no runs found at {}", runs_dir.display());
        return Ok(());
    }

    let mut records = Vec::new();
    for entry in fs::read_dir(&runs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_json = entry.path().join("RUN.json");
        if !run_json.exists() {
            continue;
        }
        let bytes = fs::read(&run_json)?;
        if let Ok(record) = serde_json::from_slice::<RunSummary>(&bytes) {
            records.push(record);
        }
    }

    records.sort_by_key(|r| Reverse(r.finished_at.clone()));
    println!("RUN_ID\tSTATUS\tKIND\tDRIVER\tFINISHED_AT");
    for record in records.into_iter().take(limit) {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            record.run_id, record.status, record.kind, record.driver, record.finished_at
        );
    }
    Ok(())
}

fn show_run(run_id: &str) -> Result<()> {
    let run_json = run_dir::root().join("runs").join(run_id).join("RUN.json");
    let content = fs::read_to_string(&run_json)
        .with_context(|| format!("failed to read {}", run_json.display()))?;
    println!("{content}");
    Ok(())
}

fn rerun(run_id: &str) -> Result<i32> {
    let run_json = run_dir::root().join("runs").join(run_id).join("RUN.json");
    let bytes =
        fs::read(&run_json).with_context(|| format!("failed to read {}", run_json.display()))?;
    let record: RunSpecRef =
        serde_json::from_slice(&bytes).context("failed to parse RUN.json for rerun")?;
    let spec_path = PathBuf::from(&record.spec.path);
    if !spec_path.exists() {
        anyhow::bail!(
            "spec path from RUN.json does not exist: {}",
            spec_path.display()
        );
    }
    run_spec(&spec_path, false)
}

fn parse_work_unit_value(path: &Path, bytes: &[u8]) -> Result<serde_json::Value> {
    if path.extension().map(|ext| ext == "toml").unwrap_or(false) {
        let toml_value: toml::Value =
            toml::from_str(std::str::from_utf8(bytes)?).context("failed to parse WorkUnit TOML")?;
        serde_json::to_value(toml_value).context("failed to convert TOML WorkUnit into JSON value")
    } else {
        serde_json::from_slice(bytes).context("failed to parse WorkUnit JSON")
    }
}

#[derive(Debug, Deserialize)]
struct RunSummary {
    run_id: String,
    status: String,
    kind: String,
    driver: String,
    finished_at: String,
}

#[derive(Debug, Deserialize)]
struct RunSpecRef {
    spec: RunSpecPath,
}

#[derive(Debug, Deserialize)]
struct RunSpecPath {
    path: String,
}
