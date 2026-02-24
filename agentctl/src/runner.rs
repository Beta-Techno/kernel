use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::artifacts::{self, ArtifactRefs, BudgetsUsed, RunRecord, Spec, Validation, Workspace};
use crate::events::EventWriter;
use crate::run_dir::RunPaths;
use crate::run_id;
use crate::work_unit::{WorkUnit, WorkspaceMode};

pub struct DriverResult {
    pub status: RunStatus,
}

struct AgentOutcome {
    status: RunStatus,
    commands_used: u64,
}

struct ValidationOutcome {
    status: String,
    details_ref: Option<String>,
    commands_used: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn as_run_record_status(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Failed => "failed",
            RunStatus::NeedsHuman => "needs_human",
        }
    }

    fn as_run_finished_status(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Failed => "failed",
            RunStatus::NeedsHuman => "canceled",
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
    let timer = Instant::now();
    let branch = workspace_branch(work_unit, run_id);

    if work_unit.agent.driver == "codex_exec" {
        prepare_workspace(work_unit, run_id, paths)?;
    }

    events.emit(
        "workspace.created",
        &serde_json::json!({
            "path": paths.workspace_dir.display().to_string(),
            "mode": work_unit.target.workspace_mode.as_str(),
            "branch": branch,
        }),
    )?;

    let agent = match work_unit.agent.driver.as_str() {
        "noop" => run_noop(work_unit, paths, events)?,
        "codex_exec" => run_codex_exec(work_unit, paths, events)?,
        other => bail!("unsupported driver: {other}"),
    };

    let validation = if agent.status == RunStatus::Ok {
        run_validation(work_unit, paths, events)?
    } else {
        emit_skipped_validation(events)?;
        ValidationOutcome {
            status: "skipped".to_string(),
            details_ref: None,
            commands_used: 0,
        }
    };

    let changed_files = write_git_artifacts(work_unit, paths, events)?;
    let finished_at = run_id::timestamp();
    let wall_seconds = timer.elapsed().as_secs();
    let diff_ref = "artifacts/diff.patch".to_string();
    let handoff_ref = "HANDOFF.md".to_string();

    let record = RunRecord {
        run_id: run_id.to_string(),
        version: "runfmt/0.1",
        kind: work_unit.kind.clone(),
        status: agent.status.as_run_record_status().to_string(),
        driver: work_unit.agent.driver.clone(),
        started_at,
        finished_at,
        spec: spec.clone(),
        workspace: Workspace {
            mode: work_unit.target.workspace_mode.as_str().to_string(),
            path: paths.workspace_dir.display().to_string(),
            branch,
            base_ref: Some(work_unit.target.base_ref.clone()),
        },
        budgets_used: BudgetsUsed {
            wall_seconds,
            tool_calls: 0,
            commands: agent.commands_used + validation.commands_used,
        },
        changed_files,
        validation: Validation {
            status: validation.status.clone(),
            details_ref: validation.details_ref.clone(),
        },
        artifacts: ArtifactRefs {
            diff: diff_ref,
            handoff: handoff_ref.clone(),
        },
    };

    artifacts::write_run_json(&paths.run_dir.join("RUN.json"), &record)?;
    artifacts::write_handoff(
        &paths.run_dir.join(&handoff_ref),
        run_id,
        work_unit,
        record.status.as_str(),
    )?;
    artifacts::write_env_fingerprint(&paths.run_dir.join("env_fingerprint.json"))?;

    events.emit(
        "run.finished",
        &serde_json::json!({
            "status": agent.status.as_run_finished_status(),
            "summary_ref": "RUN.json",
        }),
    )?;

    Ok(DriverResult {
        status: agent.status,
    })
}

fn run_noop(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
) -> Result<AgentOutcome> {
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

    touch_agent_logs(paths)?;

    // Keep runfmt parity even when no external process is involved.
    File::create(&paths.events_raw)?;

    if work_unit.acceptance.commands.is_empty() {
        return Ok(AgentOutcome {
            status: RunStatus::Ok,
            commands_used: 0,
        });
    }

    Ok(AgentOutcome {
        status: RunStatus::Ok,
        commands_used: 0,
    })
}

fn run_codex_exec(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
) -> Result<AgentOutcome> {
    touch_agent_logs(paths)?;

    let command_dir = command_dir(work_unit, paths).with_context(|| {
        format!(
            "invalid command directory for subdir {:?}",
            work_unit.target.subdir
        )
    })?;
    let mut cmd = Command::new("codex");
    cmd.arg("exec").arg("--json").arg("--cd").arg(&command_dir);

    if !is_git_repo(&command_dir) {
        cmd.arg("--skip-git-repo-check");
    }

    if let Some(model) = &work_unit.agent.model_hint {
        cmd.arg("--model").arg(model);
    }

    cmd.arg(&work_unit.agent.prompt);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let cmd_vec = command_preview(&cmd);
    events.emit(
        "agent.started",
        &serde_json::json!({
            "driver": "codex_exec",
            "cmd": cmd_vec,
        }),
    )?;

    let output = cmd
        .output()
        .context("failed to spawn codex exec; ensure codex CLI is installed and in PATH")?;

    fs::write(paths.logs_dir.join("agent.stdout.log"), &output.stdout)?;
    fs::write(paths.logs_dir.join("agent.stderr.log"), &output.stderr)?;
    fs::write(&paths.events_raw, &output.stdout)?;

    let (status, reason) = status_from_exit(output.status);
    events.emit(
        "agent.exited",
        &serde_json::json!({
            "exit_code": output.status.code().unwrap_or(-1),
            "reason": reason,
        }),
    )?;

    Ok(AgentOutcome {
        status,
        commands_used: 0,
    })
}

fn run_validation(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
) -> Result<ValidationOutcome> {
    let commands = work_unit.acceptance.commands.clone();
    events.emit(
        "validation.started",
        &serde_json::json!({
            "commands": commands,
        }),
    )?;

    if work_unit.acceptance.commands.is_empty() {
        events.emit(
            "validation.finished",
            &serde_json::json!({
                "status": "skipped",
                "details_ref": null,
            }),
        )?;
        return Ok(ValidationOutcome {
            status: "skipped".to_string(),
            details_ref: None,
            commands_used: 0,
        });
    }

    let stdout_path = paths.logs_dir.join("validate.stdout.log");
    let stderr_path = paths.logs_dir.join("validate.stderr.log");
    fs::write(&stdout_path, [])?;
    fs::write(&stderr_path, [])?;
    let command_dir = command_dir(work_unit, paths)?;

    let mut all_passed = true;
    let mut commands_used = 0_u64;

    for (idx, command_text) in work_unit.acceptance.commands.iter().enumerate() {
        let command_id = format!("cmd-{:02}", idx + 1);
        events.emit(
            "command.exec",
            &serde_json::json!({
                "id": command_id,
                "cmd": shell_preview(command_text),
                "cwd": command_dir.display().to_string(),
            }),
        )?;

        let output = shell_command(command_text, &command_dir)
            .output()
            .with_context(|| format!("failed running acceptance command: {command_text}"))?;
        commands_used += 1;

        append_bytes(&stdout_path, &output.stdout)?;
        append_bytes(&stderr_path, &output.stderr)?;

        let exit_code = output.status.code().unwrap_or(-1);
        events.emit(
            "command.result",
            &serde_json::json!({
                "id": command_id,
                "exit_code": exit_code,
                "stdout_ref": "logs/validate.stdout.log",
                "stderr_ref": "logs/validate.stderr.log",
            }),
        )?;

        if !output.status.success() {
            all_passed = false;
            break;
        }
    }

    let status = if all_passed { "passed" } else { "failed" };
    events.emit(
        "validation.finished",
        &serde_json::json!({
            "status": status,
            "details_ref": "logs/validate.stdout.log",
        }),
    )?;

    Ok(ValidationOutcome {
        status: status.to_string(),
        details_ref: Some("logs/validate.stdout.log".to_string()),
        commands_used,
    })
}

fn emit_skipped_validation(events: &mut EventWriter) -> Result<()> {
    events.emit(
        "validation.started",
        &serde_json::json!({
            "commands": [],
        }),
    )?;
    events.emit(
        "validation.finished",
        &serde_json::json!({
            "status": "skipped",
            "details_ref": null,
        }),
    )?;
    Ok(())
}

fn write_git_artifacts(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
) -> Result<Vec<String>> {
    let diff_path = paths.artifacts_dir.join("diff.patch");
    let changed_files_path = paths.artifacts_dir.join("changed_files.json");

    if work_unit.target.workspace_mode == WorkspaceMode::Scratch
        || !is_git_repo(&paths.workspace_dir)
    {
        fs::write(diff_path, [])?;
        fs::write(changed_files_path, b"[]\n")?;
        return Ok(vec![]);
    }

    let diff = git_capture(&paths.workspace_dir, ["diff", "--binary"])
        .context("failed to compute git diff for artifact")?;
    fs::write(&diff_path, &diff.stdout)?;

    let status = git_capture(&paths.workspace_dir, ["status", "--porcelain"])?;
    let changed_files = parse_status_paths(&status.stdout);
    let status_lines = String::from_utf8_lossy(&status.stdout);
    let tracked = status_lines
        .lines()
        .filter(|line| !line.starts_with("??"))
        .count();
    let untracked = status_lines
        .lines()
        .filter(|line| line.starts_with("??"))
        .count();
    events.emit(
        "git.status",
        &serde_json::json!({
            "clean": changed_files.is_empty(),
            "tracked": tracked,
            "untracked": untracked,
        }),
    )?;

    let numstat = git_capture(&paths.workspace_dir, ["diff", "--numstat"])?;
    let (files, insertions, deletions) = parse_numstat(&numstat.stdout);
    events.emit(
        "git.diff.stats",
        &serde_json::json!({
            "files": files,
            "insertions": insertions,
            "deletions": deletions,
        }),
    )?;

    let json = serde_json::to_vec_pretty(&changed_files)?;
    fs::write(changed_files_path, json)?;

    Ok(changed_files)
}

fn prepare_workspace(work_unit: &WorkUnit, run_id: &str, paths: &RunPaths) -> Result<()> {
    match work_unit.target.workspace_mode {
        WorkspaceMode::Scratch => Ok(()),
        WorkspaceMode::Worktree => prepare_worktree(work_unit, run_id, paths),
        WorkspaceMode::Clone => prepare_clone(work_unit, run_id, paths),
    }
}

fn prepare_worktree(work_unit: &WorkUnit, run_id: &str, paths: &RunPaths) -> Result<()> {
    let branch =
        workspace_branch(work_unit, run_id).context("worktree mode requires a branch name")?;
    let source_repo = resolved_repo_source(&work_unit.target.repo, &paths.repos_dir)?;
    reset_workspace_dir(&paths.workspace_dir)?;
    git_ok(
        &source_repo,
        [
            "worktree",
            "add",
            "-B",
            branch.as_str(),
            &paths.workspace_dir.display().to_string(),
            work_unit.target.base_ref.as_str(),
        ],
    )
    .context("failed to create git worktree for run")?;
    Ok(())
}

fn prepare_clone(work_unit: &WorkUnit, run_id: &str, paths: &RunPaths) -> Result<()> {
    let branch =
        workspace_branch(work_unit, run_id).context("clone mode requires a branch name")?;
    let source_repo = resolved_repo_source(&work_unit.target.repo, &paths.repos_dir)?;
    reset_workspace_dir(&paths.workspace_dir)?;

    command_ok(
        Command::new("git")
            .arg("clone")
            .arg(source_repo)
            .arg(&paths.workspace_dir),
        "git clone",
    )?;
    git_ok(
        &paths.workspace_dir,
        ["checkout", work_unit.target.base_ref.as_str()],
    )
    .context("failed to checkout base_ref in clone mode")?;
    git_ok(&paths.workspace_dir, ["checkout", "-B", branch.as_str()])
        .context("failed to create run branch in clone mode")?;
    Ok(())
}

fn resolved_repo_source(repo: &str, repos_dir: &Path) -> Result<PathBuf> {
    let repo_path = Path::new(repo);
    if repo_path.exists() {
        return repo_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize repo path {repo}"));
    }

    let key = short_hash(repo);
    let name = repo
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let cached = repos_dir.join(format!("{name}-{key}"));

    if cached.exists() {
        git_ok(&cached, ["fetch", "--all", "--prune"])
            .with_context(|| format!("failed to refresh cached repo {:?}", cached))?;
        return Ok(cached);
    }

    command_ok(
        Command::new("git").arg("clone").arg(repo).arg(&cached),
        "git clone source repo",
    )
    .with_context(|| format!("failed to clone source repo {repo}"))?;
    Ok(cached)
}

fn short_hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let full = hex::encode(hasher.finalize());
    full[..12].to_string()
}

fn reset_workspace_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to clear workspace {:?}", path))?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create workspace parent {:?}", parent))?;
    }
    Ok(())
}

fn touch_agent_logs(paths: &RunPaths) -> Result<()> {
    File::create(paths.logs_dir.join("agent.stdout.log"))?;
    File::create(paths.logs_dir.join("agent.stderr.log"))?;
    File::create(paths.logs_dir.join("validate.stdout.log"))?;
    File::create(paths.logs_dir.join("validate.stderr.log"))?;
    Ok(())
}

fn command_dir(work_unit: &WorkUnit, paths: &RunPaths) -> Result<PathBuf> {
    let mut dir = paths.workspace_dir.clone();
    if let Some(subdir) = &work_unit.target.subdir {
        dir = dir.join(subdir);
    }
    if !dir.exists() {
        bail!("workspace subdir does not exist: {}", dir.display());
    }
    Ok(dir)
}

fn workspace_branch(work_unit: &WorkUnit, run_id: &str) -> Option<String> {
    match work_unit.target.workspace_mode {
        WorkspaceMode::Scratch => None,
        _ => Some(work_unit.target.branch_slug(run_id)),
    }
}

fn status_from_exit(status: ExitStatus) -> (RunStatus, &'static str) {
    match status.code() {
        Some(0) => (RunStatus::Ok, "completed"),
        Some(10) => (RunStatus::NeedsHuman, "needs_human"),
        _ => (RunStatus::Failed, "failed"),
    }
}

fn is_git_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn git_ok<const N: usize>(dir: &Path, args: [&str; N]) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd.args(args);
    command_ok(&mut cmd, "git command")
}

fn git_capture<const N: usize>(dir: &Path, args: [&str; N]) -> Result<Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd.args(args);
    command_capture(&mut cmd, "git command")
}

fn command_ok(cmd: &mut Command, label: &str) -> Result<()> {
    let output = command_capture(cmd, label)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("{label} failed: {stderr}");
}

fn command_capture(cmd: &mut Command, label: &str) -> Result<Output> {
    cmd.output()
        .with_context(|| format!("{label}: failed to spawn process"))
}

fn append_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(unix)]
fn shell_command(command: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-lc").arg(command).current_dir(cwd);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command).current_dir(cwd);
    cmd
}

fn command_preview(cmd: &Command) -> Vec<String> {
    let mut preview = Vec::new();
    preview.push(cmd.get_program().to_string_lossy().to_string());
    preview.extend(cmd.get_args().map(|a| a.to_string_lossy().to_string()));
    preview
}

fn shell_preview(command: &str) -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["sh".to_string(), "-lc".to_string(), command.to_string()]
    }
    #[cfg(windows)]
    {
        vec!["cmd".to_string(), "/C".to_string(), command.to_string()]
    }
}

fn parse_status_paths(status_output: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(status_output);
    let mut paths = BTreeSet::new();

    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let mut path = line[3..].trim().to_string();
        if let Some((_, renamed_to)) = path.rsplit_once(" -> ") {
            path = renamed_to.to_string();
        }
        if !path.is_empty() {
            paths.insert(path);
        }
    }

    paths.into_iter().collect()
}

fn parse_numstat(numstat_output: &[u8]) -> (usize, u64, u64) {
    let text = String::from_utf8_lossy(numstat_output);
    let mut files = 0_usize;
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;

    for line in text.lines() {
        let mut parts = line.split('\t');
        let ins = parts.next().unwrap_or("0");
        let del = parts.next().unwrap_or("0");
        if parts.next().is_none() {
            continue;
        }
        files += 1;
        if let Ok(v) = ins.parse::<u64>() {
            insertions += v;
        }
        if let Ok(v) = del.parse::<u64>() {
            deletions += v;
        }
    }

    (files, insertions, deletions)
}
