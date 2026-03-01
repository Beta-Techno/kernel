use std::fs::{self, File};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::work_unit::WorkspaceMode;

pub struct RunPaths {
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub artifacts_dir: PathBuf,
    pub receipts_dir: PathBuf,
    pub workspace_dir: PathBuf,
    pub events_raw: PathBuf,
    pub events_norm: PathBuf,
    pub repos_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

pub fn provision(run_id: &str, workspace_mode: WorkspaceMode) -> Result<RunPaths> {
    provision_at(root(), run_id, workspace_mode)
}

pub(crate) fn provision_at(
    root: PathBuf,
    run_id: &str,
    _workspace_mode: WorkspaceMode,
) -> Result<RunPaths> {
    let runs_dir = root.join("runs");
    let worktrees_dir = root.join("worktrees");
    let repos_dir = root.join("repos");
    let cache_dir = root.join("cache");

    for dir in [&root, &runs_dir, &worktrees_dir, &repos_dir, &cache_dir] {
        ensure_dir(dir)?;
    }

    let run_dir = runs_dir.join(run_id);
    let logs_dir = run_dir.join("logs");
    let artifacts_dir = run_dir.join("artifacts");
    let receipts_dir = run_dir.join("receipts");
    let workspace_dir = worktrees_dir.join(run_id);

    ensure_new_dir(&run_dir)
        .with_context(|| format!("run id already exists or cannot be created: {}", run_id))?;
    if let Err(err) = ensure_new_dir(&workspace_dir) {
        let _ = fs::remove_dir_all(&run_dir);
        return Err(err).with_context(|| {
            format!(
                "workspace for run id already exists or cannot be created: {}",
                run_id
            )
        });
    }

    for dir in [&logs_dir, &artifacts_dir, &receipts_dir] {
        ensure_dir(dir)?;
    }

    let events_raw = run_dir.join("events.raw.jsonl");
    let events_norm = run_dir.join("events.norm.jsonl");
    ensure_file(&events_raw)?;
    ensure_file(&events_norm)?;

    Ok(RunPaths {
        root,
        run_dir,
        logs_dir,
        artifacts_dir,
        receipts_dir,
        workspace_dir,
        events_raw,
        events_norm,
        repos_dir,
        worktrees_dir,
    })
}

pub fn root() -> PathBuf {
    if let Ok(root) = std::env::var("AGENTD_ROOT") {
        return PathBuf::from(root);
    }

    if let Some(dir) = dirs::data_dir() {
        return dir.join("agentd");
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentd")
}

fn ensure_dir(path: &PathBuf) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create dir {:?}", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn ensure_new_dir(path: &PathBuf) -> Result<()> {
    fs::create_dir(path).with_context(|| format!("failed to create new dir {:?}", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn ensure_file(path: &PathBuf) -> Result<()> {
    File::create(path).with_context(|| format!("failed to create file {:?}", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_run_id() {
        let root = std::env::temp_dir().join(format!("agentd-run-dir-{}", uuid::Uuid::new_v4()));
        let first = provision_at(root.clone(), "dup-run", WorkspaceMode::Scratch);
        assert!(first.is_ok(), "first provision should succeed");

        let second = provision_at(root, "dup-run", WorkspaceMode::Scratch);
        assert!(
            second.is_err(),
            "second provision with same run_id must fail"
        );
    }
}
