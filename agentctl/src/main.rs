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
use run_dir::{provision, provision_at};
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
        /// Run ID to replay using its stored spec snapshot (fallback: spec.path).
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
    run_work_unit_value(
        spec_path.display().to_string(),
        work_unit_value,
        &buf,
        json_output,
        false,
        None,
    )
}

fn run_work_unit_value(
    spec_origin_path: String,
    mut work_unit_value: serde_json::Value,
    spec_hash_source: &[u8],
    json_output: bool,
    force_new_run_id: bool,
    root_override: Option<&Path>,
) -> Result<i32> {
    if force_new_run_id {
        strip_work_unit_id(&mut work_unit_value);
    }
    schema::validate_work_unit(&work_unit_value)?;
    let normalized_work_unit = serde_json::to_vec_pretty(&work_unit_value)
        .context("failed to serialize normalized WorkUnit snapshot")?;
    let work_unit: WorkUnit =
        serde_json::from_value(work_unit_value).context("failed to deserialize WorkUnit")?;

    let spec_hash = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        if force_new_run_id {
            hasher.update(&normalized_work_unit);
        } else {
            hasher.update(spec_hash_source);
        }
        hex::encode(hasher.finalize())
    };

    let run_id = if force_new_run_id {
        run_id::new_run_id()
    } else if let Some(id) = work_unit.id.clone() {
        run_id::validate_user_supplied(&id)?;
        id
    } else {
        run_id::new_run_id()
    };

    let paths = if let Some(root) = root_override {
        provision_at(root.to_path_buf(), &run_id, work_unit.target.workspace_mode)?
    } else {
        provision(&run_id, work_unit.target.workspace_mode)?
    };
    let spec_snapshot_rel = "spec/work_unit.json".to_string();
    let spec_snapshot_path = paths.run_dir.join(&spec_snapshot_rel);
    if let Some(parent) = spec_snapshot_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&spec_snapshot_path, &normalized_work_unit)
        .with_context(|| format!("failed to write {}", spec_snapshot_path.display()))?;

    let mut events = EventWriter::new(run_id.clone(), paths.events_norm.clone())?;
    events.emit(
        "run.created",
        &serde_json::json!({
            "kind": work_unit.kind.clone(),
            "submitted_by": whoami::username(),
        }),
    )?;

    let spec = Spec {
        path: spec_origin_path,
        hash: spec_hash,
        snapshot_path: Some(spec_snapshot_rel),
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
    let run_dir = run_json
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid run path: {}", run_json.display()))?;
    let spec_path = resolve_rerun_spec_path(run_dir, &record.spec);
    if !spec_path.exists() {
        anyhow::bail!(
            "spec path from RUN.json does not exist: {}",
            spec_path.display()
        );
    }
    let spec_bytes =
        fs::read(&spec_path).with_context(|| format!("failed to read {}", spec_path.display()))?;
    let work_unit_value = parse_work_unit_value(&spec_path, &spec_bytes)?;
    run_work_unit_value(
        spec_path.display().to_string(),
        work_unit_value,
        &spec_bytes,
        false,
        true,
        None,
    )
}

fn resolve_rerun_spec_path(run_dir: &Path, spec: &RunSpecPath) -> PathBuf {
    if let Some(snapshot_path) = &spec.snapshot_path {
        let snapshot = PathBuf::from(snapshot_path);
        if snapshot.is_absolute() {
            return snapshot;
        }
        return run_dir.join(snapshot);
    }
    PathBuf::from(&spec.path)
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

fn strip_work_unit_id(value: &mut serde_json::Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("id");
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
    snapshot_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn rerun_uses_relative_snapshot_path_when_present() {
        let run_dir = PathBuf::from("/tmp/agentd/runs/run-1");
        let spec = RunSpecPath {
            path: "/old/spec.json".to_string(),
            snapshot_path: Some("spec/work_unit.json".to_string()),
        };
        let resolved = resolve_rerun_spec_path(&run_dir, &spec);
        assert_eq!(resolved, run_dir.join("spec/work_unit.json"));
    }

    #[test]
    fn rerun_uses_absolute_snapshot_path_when_present() {
        let run_dir = PathBuf::from("/tmp/agentd/runs/run-1");
        let spec = RunSpecPath {
            path: "/old/spec.json".to_string(),
            snapshot_path: Some("/tmp/snapshots/work_unit.json".to_string()),
        };
        let resolved = resolve_rerun_spec_path(&run_dir, &spec);
        assert_eq!(resolved, PathBuf::from("/tmp/snapshots/work_unit.json"));
    }

    #[test]
    fn rerun_falls_back_to_original_spec_path() {
        let run_dir = PathBuf::from("/tmp/agentd/runs/run-1");
        let spec = RunSpecPath {
            path: "/old/spec.json".to_string(),
            snapshot_path: None,
        };
        let resolved = resolve_rerun_spec_path(&run_dir, &spec);
        assert_eq!(resolved, PathBuf::from("/old/spec.json"));
    }

    #[test]
    fn strip_work_unit_id_removes_id_field() {
        let mut value = serde_json::json!({
            "version": "runfmt/0.1",
            "id": "abc-123"
        });
        strip_work_unit_id(&mut value);
        assert!(value.get("id").is_none());
    }

    #[test]
    fn rerun_mode_generates_fresh_run_and_preserves_original_bundle() {
        let root = std::env::temp_dir().join(format!("agentctl-rerun-{}", uuid::Uuid::new_v4()));
        let mut spec: Value =
            serde_json::from_str(include_str!("../runfmt-example.json")).expect("valid sample");
        spec["id"] = Value::String("stable-run-id".to_string());
        let raw = serde_json::to_vec_pretty(&spec).expect("serialize spec");

        let first = run_work_unit_value(
            "inline-spec.json".to_string(),
            spec.clone(),
            &raw,
            false,
            false,
            Some(&root),
        )
        .expect("first run succeeds");
        assert_eq!(first, 0);

        let first_run_json_path = root.join("runs").join("stable-run-id").join("RUN.json");
        let first_run_json = fs::read(&first_run_json_path).expect("read first RUN.json");

        let replay = run_work_unit_value(
            "inline-spec.json".to_string(),
            spec,
            &raw,
            false,
            true,
            Some(&root),
        )
        .expect("rerun succeeds");
        assert_eq!(replay, 0);

        let runs_dir = root.join("runs");
        let run_names = fs::read_dir(&runs_dir)
            .expect("read runs dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(run_names.len(), 2);
        assert!(run_names.iter().any(|name| name == "stable-run-id"));

        let first_run_json_after =
            fs::read(&first_run_json_path).expect("read first RUN.json again");
        assert_eq!(
            first_run_json, first_run_json_after,
            "original run bundle must remain unchanged"
        );
    }
}
