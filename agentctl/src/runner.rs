use std::fs::File;

use anyhow::Result;

use crate::artifacts::{self, ArtifactRefs, BudgetsUsed, RunRecord, Spec, Validation, Workspace};
use crate::events::EventWriter;
use crate::run_dir::RunPaths;
use crate::run_id;
use crate::work_unit::WorkUnit;

pub struct DriverResult {
    pub status: RunStatus,
}

#[derive(Debug, Clone, Copy)]
pub enum RunStatus {
    Ok,
    Failed,
    NeedsHuman,
}

impl RunStatus {
    pub fn exit_code(self) -> i32 {
        match self {
            RunStatus::Ok => 0,
            RunStatus::NeedsHuman => 10,
            RunStatus::Failed => 20,
        }
    }
}

pub fn execute(
    work_unit: &WorkUnit,
    run_id: &str,
    spec: &Spec,
    paths: &RunPaths,
    events: &mut EventWriter,
) -> Result<DriverResult> {
    let started_at = run_id::timestamp();
    let branch = workspace_branch(work_unit, run_id);
    events.emit(
        "workspace.created",
        &serde_json::json!({
            "path": paths.workspace_dir.display().to_string(),
            "mode": work_unit.target.workspace_mode.as_str(),
            "branch": branch,
        }),
    )?;

    let driver = work_unit.agent.driver.as_str();
    match driver {
        "noop" => run_noop(work_unit, run_id, spec, paths, events, &started_at),
        _ => anyhow::bail!("unsupported driver: {}", driver),
    }
}

fn run_noop(
    work_unit: &WorkUnit,
    run_id: &str,
    spec: &Spec,
    paths: &RunPaths,
    events: &mut EventWriter,
    started_at: &str,
) -> Result<DriverResult> {
    events.emit(
        "agent.started",
        &serde_json::json!({
            "driver": "noop",
            "cmd": ["noop"],
        }),
    )?;
    events.emit(
        "agent.exited",
        &serde_json::json!({
            "exit_code": 0,
            "reason": "completed",
        }),
    )?;

    // Touch logs for parity with contract
    File::create(paths.logs_dir.join("agent.stdout.log"))?;
    File::create(paths.logs_dir.join("agent.stderr.log"))?;
    File::create(paths.logs_dir.join("validate.stdout.log"))?;
    File::create(paths.logs_dir.join("validate.stderr.log"))?;
    File::create(paths.artifacts_dir.join("diff.patch"))?;
    std::fs::write(paths.artifacts_dir.join("changed_files.json"), b"[]\n")?;

    events.emit(
        "validation.started",
        &serde_json::json!({
            "commands": work_unit.acceptance.commands.clone(),
        }),
    )?;
    events.emit(
        "validation.finished",
        &serde_json::json!({
            "status": "skipped",
            "details_ref": null,
        }),
    )?;

    let finished_at = run_id::timestamp();
    let diff_ref = "artifacts/diff.patch".to_string();
    let handoff_ref = "HANDOFF.md".to_string();
    let branch = workspace_branch(work_unit, run_id);
    let record = RunRecord {
        run_id: run_id.to_string(),
        version: "runfmt/0.1",
        kind: work_unit.kind.clone(),
        status: "ok".into(),
        driver: "noop".into(),
        started_at: started_at.to_string(),
        finished_at,
        spec: spec.clone(),
        workspace: Workspace {
            mode: work_unit.target.workspace_mode.as_str().to_string(),
            path: paths.workspace_dir.display().to_string(),
            branch,
            base_ref: Some(work_unit.target.base_ref.clone()),
        },
        budgets_used: BudgetsUsed {
            wall_seconds: 0,
            tool_calls: 0,
            commands: 0,
        },
        changed_files: vec![],
        validation: Validation {
            status: "skipped".into(),
            details_ref: None,
        },
        artifacts: ArtifactRefs {
            diff: diff_ref.clone(),
            handoff: handoff_ref.clone(),
        },
    };

    artifacts::write_run_json(&paths.run_dir.join("RUN.json"), &record)?;
    artifacts::write_handoff(&paths.run_dir.join(&handoff_ref), run_id, work_unit, "ok")?;
    artifacts::write_env_fingerprint(&paths.run_dir.join("env_fingerprint.json"))?;

    events.emit(
        "run.finished",
        &serde_json::json!({
            "status": "ok",
            "summary_ref": "RUN.json",
        }),
    )?;
    Ok(DriverResult {
        status: RunStatus::Ok,
    })
}

fn workspace_branch(work_unit: &WorkUnit, run_id: &str) -> Option<String> {
    match work_unit.target.workspace_mode {
        crate::work_unit::WorkspaceMode::Scratch => None,
        _ => Some(work_unit.target.branch_slug(run_id)),
    }
}
