use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::run_id;
use crate::schema;

#[derive(Serialize)]
pub struct RunRecord {
    pub run_id: String,
    pub version: &'static str,
    pub kind: String,
    pub status: String,
    pub driver: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub started_at: String,
    pub finished_at: String,
    pub spec: Spec,
    pub workspace: Workspace,
    pub budgets_used: BudgetsUsed,
    pub changed_files: Vec<String>,
    pub validation: Validation,
    pub artifacts: ArtifactRefs,
}

#[derive(Clone, Serialize)]
pub struct Spec {
    pub path: String,
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<String>,
}

#[derive(Serialize)]
pub struct Workspace {
    pub mode: String,
    pub path: String,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub base_sha: Option<String>,
    pub final_sha: Option<String>,
    pub continuation_ref: Option<String>,
}

#[derive(Serialize)]
pub struct BudgetsUsed {
    pub wall_seconds: u64,
    pub tool_calls: u64,
    pub commands: u64,
}

#[derive(Serialize)]
pub struct Validation {
    pub status: String,
    pub details_ref: Option<String>,
}

#[derive(Serialize)]
pub struct ArtifactRefs {
    pub diff: String,
    pub handoff: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_final: Option<String>,
}

pub fn write_run_json(path: &Path, record: &RunRecord) -> Result<()> {
    let value = serde_json::to_value(record)?;
    schema::validate_run_record(&value)?;
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, &value)?;
    Ok(())
}

pub fn write_handoff(
    path: &Path,
    run_id: &str,
    repo: &str,
    branch: Option<&str>,
    status: &str,
) -> Result<()> {
    let mut file = File::create(path)?;
    writeln!(file, "# Summary")?;
    writeln!(file, "- **Run:** {}", run_id)?;
    writeln!(file, "- **Status:** {}", status)?;
    writeln!(file, "- **Repo:** {}", repo)?;
    writeln!(file, "- **Branch:** {}", branch.unwrap_or("(none)"))?;
    writeln!(file, "\n## Intent\nDescribe the goal.")?;
    writeln!(file, "\n## Changes\n- TBD")?;
    writeln!(file, "\n## Validation\n- lint: pending\n- tests: pending")?;
    writeln!(file, "\n## Risks / Next Steps\n- TBD")?;
    Ok(())
}

pub fn write_handoff_disabled(path: &Path, run_id: &str, status: &str) -> Result<()> {
    let mut file = File::create(path)?;
    writeln!(file, "# Handoff Disabled")?;
    writeln!(file, "- **Run:** {}", run_id)?;
    writeln!(file, "- **Status:** {}", status)?;
    writeln!(file, "- **Reason:** outputs.want_handoff is false")?;
    Ok(())
}

pub fn write_env_fingerprint(path: &Path) -> Result<()> {
    let hostname = whoami::fallible::hostname().unwrap_or_else(|_| "unknown".to_string());
    let info = serde_json::json!({
        "timestamp": run_id::timestamp(),
        "hostname": hostname,
        "username": whoami::username(),
        "os": whoami::platform().to_string(),
    });
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, &info)?;
    Ok(())
}
