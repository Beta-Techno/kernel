use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct RunPaths {
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub artifacts_dir: PathBuf,
    pub receipts_dir: PathBuf,
    pub workspace_dir: PathBuf,
}

pub fn provision(run_id: &str, workspace_mode: &str) -> Result<RunPaths> {
    let root = default_root();
    let runs_dir = root.join("runs");
    let worktrees_dir = root.join("worktrees");
    fs::create_dir_all(&runs_dir)?;
    fs::create_dir_all(&worktrees_dir)?;

    let run_dir = runs_dir.join(run_id);
    let logs_dir = run_dir.join("logs");
    let artifacts_dir = run_dir.join("artifacts");
    let receipts_dir = run_dir.join("receipts");
    let workspace_dir = worktrees_dir.join(run_id);

    for dir in [&run_dir, &logs_dir, &artifacts_dir, &receipts_dir] {
        fs::create_dir_all(dir).with_context(|| format!("failed to create dir {:?}", dir))?;
    }

    if workspace_mode == "scratch" {
        fs::create_dir_all(&workspace_dir)?;
    }

    Ok(RunPaths {
        root,
        run_dir,
        logs_dir,
        artifacts_dir,
        receipts_dir,
        workspace_dir,
    })
}

fn default_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agentd")
}
