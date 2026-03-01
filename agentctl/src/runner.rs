use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::artifacts::{self, ArtifactRefs, BudgetsUsed, RunRecord, Spec, Validation, Workspace};
use crate::events::EventWriter;
use crate::run_dir::RunPaths;
use crate::run_id;
use crate::work_unit::{CommandPolicy, WorkUnit, WorkspaceMode};

pub struct DriverResult {
    pub status: RunStatus,
}

struct AgentOutcome {
    status: RunStatus,
    commands_used: u64,
    tool_calls: u64,
    session_id: Option<String>,
    final_message_ref: Option<String>,
}

struct ValidationOutcome {
    status: String,
    details_ref: Option<String>,
    commands_used: u64,
}

struct GitArtifacts {
    changed_files: Vec<String>,
    diff_lines: u64,
    bytes_written: u64,
}

struct StatusSummary {
    changed_files: Vec<String>,
    untracked_files: Vec<String>,
    tracked_count: usize,
    untracked_count: usize,
}

#[derive(Default)]
struct ToolEventCounts {
    tool_calls: u64,
    command_calls: u64,
    session_id: Option<String>,
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
    let wall_budget_seconds = work_unit.budgets.wall_seconds;
    let mut emitted_budget_events = BTreeSet::new();
    let branch = workspace_branch(work_unit, run_id);

    prepare_workspace(work_unit, run_id, paths)?;

    events.emit(
        "workspace.created",
        &serde_json::json!({
            "path": paths.workspace_dir.display().to_string(),
            "mode": work_unit.target.workspace_mode.as_str(),
            "branch": branch,
        }),
    )?;

    let mut agent = match work_unit.agent.driver.as_str() {
        "noop" => run_noop(work_unit, paths, events)?,
        "codex_exec" => run_codex_exec(
            work_unit,
            paths,
            events,
            &timer,
            wall_budget_seconds,
            &mut emitted_budget_events,
        )?,
        other => bail!("unsupported driver: {other}"),
    };

    if remaining_wall_budget(&timer, wall_budget_seconds).is_none() {
        emit_budget_exceeded_once(
            events,
            "wall_seconds",
            wall_budget_seconds,
            &mut emitted_budget_events,
        )?;
        agent.status = RunStatus::Failed;
    }
    if enforce_budget_limit(
        events,
        "max_tool_calls",
        work_unit.budgets.max_tool_calls,
        agent.tool_calls,
        &mut emitted_budget_events,
    )? {
        agent.status = RunStatus::Failed;
    }
    if enforce_budget_limit(
        events,
        "max_commands",
        work_unit.budgets.max_commands,
        agent.commands_used,
        &mut emitted_budget_events,
    )? {
        agent.status = RunStatus::Failed;
    }

    let validation = if agent.status == RunStatus::Ok {
        run_validation(
            work_unit,
            paths,
            events,
            &timer,
            wall_budget_seconds,
            agent.commands_used,
            work_unit.budgets.max_commands,
            &mut emitted_budget_events,
        )?
    } else {
        emit_skipped_validation(events)?;
        ValidationOutcome {
            status: "skipped".to_string(),
            details_ref: None,
            commands_used: 0,
        }
    };

    let mut final_status = agent.status;
    if validation.status == "failed" && final_status == RunStatus::Ok {
        final_status = RunStatus::Failed;
    }
    let total_commands = agent.commands_used.saturating_add(validation.commands_used);
    if enforce_budget_limit(
        events,
        "max_commands",
        work_unit.budgets.max_commands,
        total_commands,
        &mut emitted_budget_events,
    )? {
        final_status = RunStatus::Failed;
    }

    let git_artifacts = write_git_artifacts(work_unit, paths, events)?;
    write_commits_artifact(work_unit, paths, events)?;
    if enforce_budget_limit(
        events,
        "max_diff_lines",
        work_unit.budgets.max_diff_lines,
        git_artifacts.diff_lines,
        &mut emitted_budget_events,
    )? {
        final_status = RunStatus::Failed;
    }
    if enforce_budget_limit(
        events,
        "max_bytes_written",
        work_unit.budgets.max_bytes_written,
        git_artifacts.bytes_written,
        &mut emitted_budget_events,
    )? {
        final_status = RunStatus::Failed;
    }
    if receipts_are_required(work_unit) {
        let receipt_count = count_receipt_files(&paths.receipts_dir)?;
        if receipt_count == 0 {
            events.emit(
                "policy.denied",
                &serde_json::json!({
                    "capability": "receipts.required",
                    "reason": "missing_receipts",
                }),
            )?;
            final_status = RunStatus::Failed;
        }
    }
    if enforce_output_action_policy(work_unit, events)? {
        final_status = RunStatus::Failed;
    }
    let finished_at = run_id::timestamp();
    let wall_seconds = timer.elapsed().as_secs();
    let diff_ref = "artifacts/diff.patch".to_string();
    let handoff_ref = "HANDOFF.md".to_string();

    let record = RunRecord {
        run_id: run_id.to_string(),
        version: "runfmt/0.1",
        kind: work_unit.kind.clone(),
        status: final_status.as_run_record_status().to_string(),
        driver: work_unit.agent.driver.clone(),
        agent_session_id: agent.session_id.clone(),
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
            tool_calls: agent.tool_calls,
            commands: total_commands,
        },
        changed_files: git_artifacts.changed_files,
        validation: Validation {
            status: validation.status.clone(),
            details_ref: validation.details_ref.clone(),
        },
        artifacts: ArtifactRefs {
            diff: diff_ref,
            handoff: handoff_ref.clone(),
            agent_final: agent.final_message_ref.clone(),
        },
    };

    artifacts::write_run_json(&paths.run_dir.join("RUN.json"), &record)?;
    let handoff_path = paths.run_dir.join(&handoff_ref);
    if work_unit.outputs.want_handoff {
        artifacts::write_handoff(
            &handoff_path,
            run_id,
            &work_unit.target.repo,
            record.workspace.branch.as_deref(),
            record.status.as_str(),
        )?;
    } else {
        events.emit(
            "artifact.skipped",
            &serde_json::json!({
                "artifact": handoff_ref,
                "reason": "outputs.want_handoff=false",
            }),
        )?;
        artifacts::write_handoff_disabled(&handoff_path, run_id, record.status.as_str())?;
    }
    artifacts::write_env_fingerprint(&paths.run_dir.join("env_fingerprint.json"))?;

    events.emit(
        "run.finished",
        &serde_json::json!({
            "status": final_status.as_run_finished_status(),
            "summary_ref": "RUN.json",
        }),
    )?;

    Ok(DriverResult {
        status: final_status,
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
            tool_calls: 0,
            session_id: None,
            final_message_ref: None,
        });
    }

    Ok(AgentOutcome {
        status: RunStatus::Ok,
        commands_used: 0,
        tool_calls: 0,
        session_id: None,
        final_message_ref: None,
    })
}

fn run_codex_exec(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
    timer: &Instant,
    wall_budget_seconds: u64,
    emitted_budget_events: &mut BTreeSet<&'static str>,
) -> Result<AgentOutcome> {
    touch_agent_logs(paths)?;

    let command_dir = command_dir(work_unit, paths).with_context(|| {
        format!(
            "invalid command directory for subdir {:?}",
            work_unit.target.subdir
        )
    })?;
    let mut cmd = Command::new("codex");
    cmd.arg("exec");
    if let Some(session_id) = &work_unit.agent.resume_session_id {
        cmd.arg("resume").arg(session_id);
    }
    cmd.arg("--ask-for-approval").arg("never");
    cmd.arg("--sandbox").arg(codex_sandbox(work_unit));
    cmd.arg("--json").arg("--cd").arg(&command_dir);
    let final_message_path = paths.artifacts_dir.join("agent_final.md");
    let final_message_ref = "artifacts/agent_final.md".to_string();
    cmd.arg("--output-last-message").arg(&final_message_path);

    if !is_git_context(&command_dir, &paths.workspace_dir) {
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

    let remaining = match remaining_wall_budget(timer, wall_budget_seconds) {
        Some(v) => v,
        None => {
            emit_budget_exceeded_once(
                events,
                "wall_seconds",
                wall_budget_seconds,
                emitted_budget_events,
            )?;
            fs::write(&final_message_path, [])?;
            events.emit(
                "agent.exited",
                &serde_json::json!({
                    "exit_code": -1,
                    "reason": "budget_exceeded",
                }),
            )?;
            return Ok(AgentOutcome {
                status: RunStatus::Failed,
                commands_used: 0,
                tool_calls: 0,
                session_id: None,
                final_message_ref: Some(final_message_ref),
            });
        }
    };

    let (output, timed_out) = run_command_with_timeout(
        &mut cmd,
        remaining,
        "failed to spawn codex exec; ensure codex CLI is installed and in PATH",
    )?;

    fs::write(paths.logs_dir.join("agent.stdout.log"), &output.stdout)?;
    fs::write(paths.logs_dir.join("agent.stderr.log"), &output.stderr)?;
    fs::write(&paths.events_raw, &output.stdout)?;
    if !final_message_path.exists() {
        fs::write(&final_message_path, [])?;
    }

    let counts = emit_tool_events_from_raw(&output.stdout, events)?;
    let (status, reason) = if timed_out {
        emit_budget_exceeded_once(
            events,
            "wall_seconds",
            wall_budget_seconds,
            emitted_budget_events,
        )?;
        (RunStatus::Failed, "budget_exceeded")
    } else {
        status_from_exit(output.status)
    };
    events.emit(
        "agent.exited",
        &serde_json::json!({
            "exit_code": if timed_out { -1 } else { output.status.code().unwrap_or(-1) },
            "reason": reason,
        }),
    )?;

    Ok(AgentOutcome {
        status,
        commands_used: counts.command_calls,
        tool_calls: counts.tool_calls,
        session_id: counts.session_id,
        final_message_ref: Some(final_message_ref),
    })
}

fn run_validation(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
    timer: &Instant,
    wall_budget_seconds: u64,
    already_used_commands: u64,
    max_commands_budget: Option<u64>,
    emitted_budget_events: &mut BTreeSet<&'static str>,
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
        let total_before = already_used_commands.saturating_add(commands_used);
        if let Some(limit) = command_budget_exhausted(max_commands_budget, total_before) {
            emit_budget_exceeded_once(events, "max_commands", limit, emitted_budget_events)?;
            all_passed = false;
            break;
        }

        let remaining = match remaining_wall_budget(timer, wall_budget_seconds) {
            Some(v) => v,
            None => {
                emit_budget_exceeded_once(
                    events,
                    "wall_seconds",
                    wall_budget_seconds,
                    emitted_budget_events,
                )?;
                all_passed = false;
                break;
            }
        };

        let command_id = format!("cmd-{:02}", idx + 1);
        events.emit(
            "command.exec",
            &serde_json::json!({
                "id": command_id,
                "cmd": shell_preview(command_text),
                "cwd": command_dir.display().to_string(),
            }),
        )?;

        let mut command = shell_command(command_text, &command_dir);
        let (output, timed_out) = run_command_with_timeout(
            &mut command,
            remaining,
            &format!("failed running acceptance command: {command_text}"),
        )?;
        commands_used += 1;

        append_bytes(&stdout_path, &output.stdout)?;
        append_bytes(&stderr_path, &output.stderr)?;

        let exit_code = if timed_out {
            emit_budget_exceeded_once(
                events,
                "wall_seconds",
                wall_budget_seconds,
                emitted_budget_events,
            )?;
            -1
        } else {
            output.status.code().unwrap_or(-1)
        };
        events.emit(
            "command.result",
            &serde_json::json!({
                "id": command_id,
                "exit_code": exit_code,
                "stdout_ref": "logs/validate.stdout.log",
                "stderr_ref": "logs/validate.stderr.log",
            }),
        )?;

        if timed_out || !output.status.success() {
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
) -> Result<GitArtifacts> {
    let diff_path = paths.artifacts_dir.join("diff.patch");
    let changed_files_path = paths.artifacts_dir.join("changed_files.json");

    if work_unit.target.workspace_mode == WorkspaceMode::Scratch
        || !is_git_repo_root(&paths.workspace_dir)
    {
        fs::write(diff_path, [])?;
        fs::write(changed_files_path, b"[]\n")?;
        return Ok(GitArtifacts {
            changed_files: vec![],
            diff_lines: 0,
            bytes_written: 0,
        });
    }

    if work_unit.outputs.want_patch {
        let diff = git_capture(&paths.workspace_dir, ["diff", "--binary"])
            .context("failed to compute git diff for artifact")?;
        fs::write(&diff_path, &diff.stdout)?;
    } else {
        fs::write(&diff_path, [])?;
        events.emit(
            "artifact.skipped",
            &serde_json::json!({
                "artifact": "artifacts/diff.patch",
                "reason": "outputs.want_patch=false",
            }),
        )?;
    }

    let status = git_capture(&paths.workspace_dir, ["status", "--porcelain"])?;
    let summary = parse_status_summary(&status.stdout);
    events.emit(
        "git.status",
        &serde_json::json!({
            "clean": summary.changed_files.is_empty(),
            "tracked": summary.tracked_count,
            "untracked": summary.untracked_count,
        }),
    )?;

    let numstat = git_capture(&paths.workspace_dir, ["diff", "--numstat"])?;
    let (files, insertions, deletions) = parse_numstat(&numstat.stdout);
    let untracked_lines = sum_file_lines(&paths.workspace_dir, &summary.untracked_files);
    let diff_lines = insertions
        .saturating_add(deletions)
        .saturating_add(untracked_lines);
    let bytes_written =
        emit_file_write_events(&paths.workspace_dir, &summary.changed_files, events)?;
    events.emit(
        "git.diff.stats",
        &serde_json::json!({
            "files": std::cmp::max(files, summary.changed_files.len()),
            "insertions": insertions,
            "deletions": deletions,
            "diff_lines": diff_lines,
        }),
    )?;

    let json = serde_json::to_vec_pretty(&summary.changed_files)?;
    fs::write(changed_files_path, json)?;

    Ok(GitArtifacts {
        changed_files: summary.changed_files,
        diff_lines,
        bytes_written,
    })
}

fn write_commits_artifact(
    work_unit: &WorkUnit,
    paths: &RunPaths,
    events: &mut EventWriter,
) -> Result<()> {
    let commits_path = paths.artifacts_dir.join("commits.json");
    if !work_unit.outputs.want_commits {
        fs::write(&commits_path, b"[]\n")?;
        events.emit(
            "artifact.skipped",
            &serde_json::json!({
                "artifact": "artifacts/commits.json",
                "reason": "outputs.want_commits=false",
            }),
        )?;
        return Ok(());
    }
    if work_unit.target.workspace_mode == WorkspaceMode::Scratch
        || !is_git_repo_root(&paths.workspace_dir)
    {
        fs::write(&commits_path, b"[]\n")?;
        return Ok(());
    }

    let range = format!("{}..HEAD", work_unit.target.base_ref);
    let out = git_capture(
        &paths.workspace_dir,
        ["log", "--reverse", "--format=%H%x09%s", &range],
    )
    .context("failed to collect git commits for artifact")?;
    let commits = parse_commit_log(&out.stdout);
    fs::write(&commits_path, serde_json::to_vec_pretty(&commits)?)?;
    events.emit(
        "git.commits",
        &serde_json::json!({
            "count": commits.len(),
            "artifact": "artifacts/commits.json",
        }),
    )?;
    Ok(())
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
    // Always clone into the cache, even for local repos, to avoid mutating user repos.
    let repo_path = Path::new(repo);
    let (source, name_hint) = if repo_path.exists() {
        let canonical = repo_path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize repo path {repo}"))?;
        let name = canonical
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("repo")
            .to_string();
        (canonical.to_string_lossy().to_string(), name)
    } else {
        let name = repo
            .rsplit('/')
            .next()
            .unwrap_or("repo")
            .trim_end_matches(".git")
            .to_string();
        (repo.to_string(), name)
    };

    let key = short_hash(&source);
    let name = name_hint
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
        Command::new("git").arg("clone").arg(&source).arg(&cached),
        "git clone source repo",
    )
    .with_context(|| format!("failed to clone source repo {source}"))?;
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

fn codex_sandbox(work_unit: &WorkUnit) -> &'static str {
    if work_unit.authority.mode >= 3
        && has_capability(
            work_unit,
            &[
                "sandbox.danger-full-access",
                "danger-full-access",
                "sandbox.full-access",
            ],
        )
    {
        return "danger-full-access";
    }

    if work_unit.authority.mode == 0
        || matches!(work_unit.tools.command_policy, CommandPolicy::Deny)
    {
        return "read-only";
    }

    "workspace-write"
}

fn has_capability(work_unit: &WorkUnit, allowed: &[&str]) -> bool {
    work_unit
        .authority
        .capabilities
        .iter()
        .any(|cap| allowed.iter().any(|name| cap.name == *name))
}

fn status_from_exit(status: ExitStatus) -> (RunStatus, &'static str) {
    match status.code() {
        Some(0) => (RunStatus::Ok, "completed"),
        Some(10) => (RunStatus::NeedsHuman, "needs_human"),
        _ => (RunStatus::Failed, "failed"),
    }
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: Duration,
    spawn_error_context: &str,
) -> Result<(Output, bool)> {
    let mut child = command
        .spawn()
        .with_context(|| spawn_error_context.to_string())?;
    let started = Instant::now();

    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            return Ok((output, false));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Ok((output, true));
        }
        sleep(Duration::from_millis(100));
    }
}

fn remaining_wall_budget(started: &Instant, limit_seconds: u64) -> Option<Duration> {
    let elapsed = started.elapsed();
    let limit = Duration::from_secs(limit_seconds);
    if elapsed >= limit {
        return None;
    }
    Some(limit - elapsed)
}

fn emit_budget_exceeded_once(
    events: &mut EventWriter,
    budget: &'static str,
    limit: u64,
    emitted: &mut BTreeSet<&'static str>,
) -> Result<()> {
    if !emitted.insert(budget) {
        return Ok(());
    }
    events.emit(
        "budget.exceeded",
        &serde_json::json!({
            "budget": budget,
            "limit": limit,
        }),
    )
}

fn enforce_budget_limit(
    events: &mut EventWriter,
    budget: &'static str,
    limit: Option<u64>,
    used: u64,
    emitted: &mut BTreeSet<&'static str>,
) -> Result<bool> {
    let Some(limit) = limit else {
        return Ok(false);
    };
    if used <= limit {
        return Ok(false);
    }
    emit_budget_exceeded_once(events, budget, limit, emitted)?;
    Ok(true)
}

fn command_budget_exhausted(limit: Option<u64>, used: u64) -> Option<u64> {
    let limit = limit?;
    if used >= limit {
        return Some(limit);
    }
    None
}

fn emit_tool_events_from_raw(
    raw_jsonl: &[u8],
    events: &mut EventWriter,
) -> Result<ToolEventCounts> {
    let text = String::from_utf8_lossy(raw_jsonl);
    let mut counts = ToolEventCounts::default();
    let mut command_execution_ids = BTreeSet::new();

    for line in text.lines() {
        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if counts.session_id.is_none() {
            counts.session_id = thread_id_from_event(&parsed);
        }
        if let Some(cmd_event) = classify_command_execution_event(&parsed) {
            if command_execution_ids.insert(cmd_event.item_id().to_string()) {
                counts.command_calls += 1;
            }
            emit_agent_command_event(events, cmd_event)?;
        }
        if let Some(tool_event) = classify_tool_event(&parsed) {
            match tool_event {
                ParsedToolEvent::Call { tool, args_hash } => {
                    counts.tool_calls += 1;
                    if command_execution_ids.is_empty() && is_command_tool_name(&tool) {
                        counts.command_calls += 1;
                    }
                    events.emit(
                        "tool.call",
                        &serde_json::json!({
                            "tool": tool,
                            "args_hash": args_hash,
                            "capability": null,
                        }),
                    )?;
                }
                ParsedToolEvent::Result {
                    tool,
                    status,
                    receipt_ref,
                } => {
                    events.emit(
                        "tool.result",
                        &serde_json::json!({
                            "tool": tool,
                            "status": status,
                            "receipt_ref": receipt_ref,
                        }),
                    )?;
                }
            }
        }
    }

    if !command_execution_ids.is_empty() {
        counts.command_calls = command_execution_ids.len() as u64;
    }

    Ok(counts)
}

fn thread_id_from_event(value: &Value) -> Option<String> {
    let top_type = value.get("type")?.as_str()?;
    if top_type == "thread.started" {
        return value
            .get("thread_id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }
    if top_type == "item.completed" {
        let item = value.get("item")?;
        if item.get("type").and_then(Value::as_str) == Some("thread.started") {
            return item
                .get("thread_id")
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
    }
    None
}

enum CommandExecutionEvent {
    Started { item_id: String, command: String },
    Completed { item_id: String, exit_code: i64 },
}

impl CommandExecutionEvent {
    fn item_id(&self) -> &str {
        match self {
            CommandExecutionEvent::Started { item_id, .. } => item_id,
            CommandExecutionEvent::Completed { item_id, .. } => item_id,
        }
    }
}

fn classify_command_execution_event(value: &Value) -> Option<CommandExecutionEvent> {
    let top_type = value.get("type")?.as_str()?;
    if top_type != "item.started" && top_type != "item.completed" {
        return None;
    }
    let item = value.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("command_execution") {
        return None;
    }

    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| hash_json_value(Some(item)));

    if top_type == "item.started" {
        let command = item
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Some(CommandExecutionEvent::Started { item_id, command });
    }

    let exit_code = item
        .get("exit_code")
        .and_then(Value::as_i64)
        .or_else(|| {
            item.get("result")
                .and_then(|v| v.get("exit_code"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(-1);
    Some(CommandExecutionEvent::Completed { item_id, exit_code })
}

fn emit_agent_command_event(events: &mut EventWriter, event: CommandExecutionEvent) -> Result<()> {
    match event {
        CommandExecutionEvent::Started { item_id, command } => events.emit(
            "command.exec",
            &serde_json::json!({
                "id": format!("agent-{item_id}"),
                "cmd": [command],
                "cwd": null,
            }),
        ),
        CommandExecutionEvent::Completed { item_id, exit_code } => events.emit(
            "command.result",
            &serde_json::json!({
                "id": format!("agent-{item_id}"),
                "exit_code": exit_code,
                "stdout_ref": null,
                "stderr_ref": null,
            }),
        ),
    }
}

enum ParsedToolEvent {
    Call {
        tool: String,
        args_hash: String,
    },
    Result {
        tool: String,
        status: String,
        receipt_ref: Option<String>,
    },
}

fn classify_tool_event(value: &Value) -> Option<ParsedToolEvent> {
    let top_type = value.get("type")?.as_str()?;

    if top_type == "item.completed" {
        let item = value.get("item")?;
        return classify_tool_item(item);
    }

    if top_type.contains("tool_call") || top_type.contains("mcp_tool") {
        let tool = tool_name(value);
        let args_hash = hash_json_value(
            value
                .get("arguments")
                .or_else(|| value.get("args"))
                .or_else(|| value.get("input")),
        );

        if top_type.contains("fail") {
            return Some(ParsedToolEvent::Result {
                tool,
                status: "error".to_string(),
                receipt_ref: None,
            });
        }
        if top_type.contains("complete") || top_type.contains("result") {
            return Some(ParsedToolEvent::Result {
                tool,
                status: "ok".to_string(),
                receipt_ref: None,
            });
        }
        if top_type.contains("start") || top_type.contains("call") {
            return Some(ParsedToolEvent::Call { tool, args_hash });
        }
    }

    None
}

fn classify_tool_item(item: &Value) -> Option<ParsedToolEvent> {
    let item_type = item.get("type")?.as_str()?;
    let tool = tool_name(item);

    if matches!(
        item_type,
        "tool_call" | "mcp_tool_call" | "function_call" | "tool_use"
    ) {
        return Some(ParsedToolEvent::Call {
            tool,
            args_hash: hash_json_value(
                item.get("arguments")
                    .or_else(|| item.get("args"))
                    .or_else(|| item.get("input")),
            ),
        });
    }

    if matches!(
        item_type,
        "tool_result" | "mcp_tool_result" | "function_result"
    ) {
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("ok")
            .to_string();
        return Some(ParsedToolEvent::Result {
            tool,
            status,
            receipt_ref: None,
        });
    }

    None
}

fn tool_name(value: &Value) -> String {
    value
        .get("tool")
        .or_else(|| value.get("name"))
        .or_else(|| value.get("tool_name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown_tool")
        .to_string()
}

fn hash_json_value(value: Option<&Value>) -> String {
    use sha2::{Digest, Sha256};
    let data = value.cloned().unwrap_or(Value::Null);
    let encoded = serde_json::to_vec(&data).unwrap_or_else(|_| b"null".to_vec());
    let mut hasher = Sha256::new();
    hasher.update(encoded);
    hex::encode(hasher.finalize())
}

fn is_command_tool_name(tool: &str) -> bool {
    let lower = tool.to_ascii_lowercase();
    lower == "exec_command"
        || lower.ends_with(".exec_command")
        || lower.contains("exec_command")
        || lower == "shell.exec"
}

fn emit_file_write_events(
    workspace_dir: &Path,
    changed_files: &[String],
    events: &mut EventWriter,
) -> Result<u64> {
    let mut total = 0_u64;

    for rel_path in changed_files {
        let path = workspace_dir.join(rel_path);
        let bytes = match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => meta.len(),
            _ => 0,
        };
        total = total.saturating_add(bytes);
        events.emit(
            "file.write",
            &serde_json::json!({
                "path": rel_path,
                "bytes": bytes,
            }),
        )?;
    }

    Ok(total)
}

fn is_git_repo_root(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn is_git_context(command_dir: &Path, workspace_root: &Path) -> bool {
    command_dir.starts_with(workspace_root) && is_git_repo_root(workspace_root)
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

fn parse_status_summary(status_output: &[u8]) -> StatusSummary {
    let text = String::from_utf8_lossy(status_output);
    let mut changed_paths = BTreeSet::new();
    let mut untracked_paths = BTreeSet::new();
    let mut tracked_count = 0_usize;
    let mut untracked_count = 0_usize;

    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let is_untracked = line.starts_with("??");
        let mut path = line[3..].trim().to_string();
        if let Some((_, renamed_to)) = path.rsplit_once(" -> ") {
            path = renamed_to.to_string();
        }
        if !path.is_empty() {
            if is_untracked {
                untracked_paths.insert(path.clone());
                untracked_count += 1;
            } else {
                tracked_count += 1;
            }
            changed_paths.insert(path);
        }
    }

    StatusSummary {
        changed_files: changed_paths.into_iter().collect(),
        untracked_files: untracked_paths.into_iter().collect(),
        tracked_count,
        untracked_count,
    }
}

fn append_untracked_file_diffs(
    workspace_dir: &Path,
    mut tracked_diff: Vec<u8>,
    untracked_files: &[String],
) -> Result<Vec<u8>> {
    let null_path = if cfg!(windows) { "NUL" } else { "/dev/null" };

    for rel_path in untracked_files {
        let path = workspace_dir.join(rel_path);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if metadata.is_dir() {
            continue;
        }

        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(workspace_dir)
            .arg("-c")
            .arg("core.quotePath=false")
            .arg("diff")
            .arg("--no-index")
            .arg("--binary")
            .arg("--")
            .arg(null_path)
            .arg(rel_path);
        let output = command_capture(&mut cmd, "git diff --no-index")
            .with_context(|| format!("failed to compute untracked diff for {rel_path}"))?;
        match output.status.code() {
            Some(0) | Some(1) => {
                tracked_diff.extend_from_slice(&output.stdout);
            }
            _ => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("git diff --no-index failed for {rel_path}: {stderr}");
            }
        }
    }

    Ok(tracked_diff)
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

fn sum_file_lines(workspace_dir: &Path, rel_paths: &[String]) -> u64 {
    rel_paths
        .iter()
        .filter_map(|rel| fs::read(workspace_dir.join(rel)).ok())
        .map(|content| String::from_utf8_lossy(&content).lines().count() as u64)
        .fold(0_u64, u64::saturating_add)
}

fn receipts_are_required(work_unit: &WorkUnit) -> bool {
    work_unit.kind == "ops" || work_unit.acceptance.receipts_required
}

fn count_receipt_files(receipts_dir: &Path) -> Result<u64> {
    let mut count = 0_u64;
    let mut stack = vec![receipts_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read receipts dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                count = count.saturating_add(1);
            }
        }
    }

    Ok(count)
}

fn enforce_output_action_policy(work_unit: &WorkUnit, events: &mut EventWriter) -> Result<bool> {
    let mut denied = false;
    if work_unit.outputs.push_branch {
        events.emit(
            "policy.denied",
            &serde_json::json!({
                "capability": "outputs.push_branch",
                "reason": "not_implemented",
            }),
        )?;
        denied = true;
    }
    if work_unit.outputs.open_pr {
        events.emit(
            "policy.denied",
            &serde_json::json!({
                "capability": "outputs.open_pr",
                "reason": "not_implemented",
            }),
        )?;
        denied = true;
    }
    Ok(denied)
}

fn parse_commit_log(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let (sha, subject) = line.split_once('\t')?;
            Some(serde_json::json!({
                "sha": sha,
                "subject": subject,
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_unit::{
        Acceptance, Agent, Authority, Budgets, Capability, CommandPolicy, NetworkPolicy, Outputs,
        Target, Tools, WorkUnit, WorkspaceMode,
    };
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn parses_status_paths_with_rename_and_untracked() {
        let status = b" M src/main.rs\nR  old.txt -> new.txt\n?? notes.md\n";
        let summary = parse_status_summary(status);
        assert_eq!(
            summary.changed_files,
            vec!["new.txt", "notes.md", "src/main.rs"]
        );
    }

    #[test]
    fn parses_status_summary_counts() {
        let status = b" M src/main.rs\nR  old.txt -> new.txt\n?? notes.md\n";
        let summary = parse_status_summary(status);
        assert_eq!(
            summary.changed_files,
            vec!["new.txt", "notes.md", "src/main.rs"]
        );
        assert_eq!(summary.untracked_files, vec!["notes.md"]);
        assert_eq!(summary.tracked_count, 2);
        assert_eq!(summary.untracked_count, 1);
    }

    #[test]
    fn parses_numstat_totals() {
        let numstat = b"10\t2\tsrc/main.rs\n3\t0\tREADME.md\n";
        let (files, insertions, deletions) = parse_numstat(numstat);
        assert_eq!(files, 2);
        assert_eq!(insertions, 13);
        assert_eq!(deletions, 2);
    }

    #[test]
    fn classifies_item_tool_call_and_result() {
        let call: Value = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "tool_call",
                "tool": "github.search",
                "arguments": {"q":"abc"}
            }
        });
        let result: Value = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "tool_result",
                "tool": "github.search",
                "status": "ok"
            }
        });
        match classify_tool_event(&call) {
            Some(ParsedToolEvent::Call { tool, .. }) => assert_eq!(tool, "github.search"),
            _ => panic!("expected tool call"),
        }
        match classify_tool_event(&result) {
            Some(ParsedToolEvent::Result { tool, status, .. }) => {
                assert_eq!(tool, "github.search");
                assert_eq!(status, "ok");
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn classifies_completed_top_level_tool_call_as_result() {
        let event: Value = serde_json::json!({
            "type": "mcp_tool_call.completed",
            "tool": "netbox.get",
            "args": {"id": 1}
        });
        match classify_tool_event(&event) {
            Some(ParsedToolEvent::Result { tool, status, .. }) => {
                assert_eq!(tool, "netbox.get");
                assert_eq!(status, "ok");
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn command_tool_name_detection_is_specific() {
        assert!(is_command_tool_name("exec_command"));
        assert!(is_command_tool_name("foo.exec_command"));
        assert!(!is_command_tool_name("github.search"));
    }

    #[test]
    fn remaining_budget_none_when_elapsed_exceeds_limit() {
        let started = Instant::now() - Duration::from_secs(2);
        assert!(remaining_wall_budget(&started, 1).is_none());
    }

    #[test]
    fn command_budget_exhausted_blocks_at_limit() {
        assert_eq!(command_budget_exhausted(Some(0), 0), Some(0));
        assert_eq!(command_budget_exhausted(Some(1), 1), Some(1));
        assert_eq!(command_budget_exhausted(Some(2), 1), None);
        assert_eq!(command_budget_exhausted(None, 100), None);
    }

    #[test]
    fn counts_receipts_recursively() {
        let root = std::env::temp_dir().join(format!("agentctl-receipts-{}", uuid::Uuid::new_v4()));
        let nested = root.join("github");
        fs::create_dir_all(&nested).expect("create nested receipt dir");
        fs::write(root.join("one.json"), b"{}").expect("write receipt");
        fs::write(nested.join("two.json"), b"{}").expect("write nested receipt");
        let count = count_receipt_files(&root).expect("count receipts");
        assert_eq!(count, 2);
    }

    #[test]
    fn parses_commit_log_lines() {
        let lines = b"abc123\tfirst commit\ndef456\tsecond commit\n";
        let commits = parse_commit_log(lines);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0]["sha"], "abc123");
        assert_eq!(commits[0]["subject"], "first commit");
        assert_eq!(commits[1]["sha"], "def456");
    }

    #[test]
    fn tool_events_count_command_calls() {
        let raw = br#"{"type":"item.completed","item":{"type":"tool_call","tool":"exec_command","arguments":{"cmd":"echo hi"}}}
{"type":"item.completed","item":{"type":"tool_call","tool":"github.search","arguments":{"q":"x"}}}
"#;
        let dir = std::env::temp_dir().join(format!("agentctl-tests-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let events_path = dir.join("events.norm.jsonl");
        let mut events =
            EventWriter::new("run-test".to_string(), events_path).expect("event writer");
        let counts = emit_tool_events_from_raw(raw, &mut events).expect("parse raw events");
        assert_eq!(counts.tool_calls, 2);
        assert_eq!(counts.command_calls, 1);
        assert!(counts.session_id.is_none());
    }

    #[test]
    fn tool_events_capture_thread_id() {
        let raw = br#"{"type":"thread.started","thread_id":"thread_123"}
{"type":"item.completed","item":{"type":"tool_call","tool":"exec_command","arguments":{"cmd":"echo hi"}}}
"#;
        let dir = std::env::temp_dir().join(format!("agentctl-tests-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let events_path = dir.join("events.norm.jsonl");
        let mut events =
            EventWriter::new("run-test".to_string(), events_path).expect("event writer");
        let counts = emit_tool_events_from_raw(raw, &mut events).expect("parse raw events");
        assert_eq!(counts.session_id.as_deref(), Some("thread_123"));
    }

    #[test]
    fn command_execution_items_drive_command_count() {
        let raw = br#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","exit_code":0}}
{"type":"item.completed","item":{"type":"tool_call","tool":"exec_command","arguments":{"cmd":"echo hi"}}}
"#;
        let dir = std::env::temp_dir().join(format!("agentctl-tests-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let events_path = dir.join("events.norm.jsonl");
        let mut events =
            EventWriter::new("run-test".to_string(), events_path).expect("event writer");
        let counts = emit_tool_events_from_raw(raw, &mut events).expect("parse raw events");
        assert_eq!(counts.command_calls, 1);
        assert_eq!(counts.tool_calls, 1);
    }

    #[test]
    fn codex_sandbox_maps_authority_and_policy() {
        let mut wu = minimal_work_unit();
        wu.authority.mode = 0;
        assert_eq!(codex_sandbox(&wu), "read-only");

        wu.authority.mode = 1;
        wu.tools.command_policy = CommandPolicy::SafeDefault;
        assert_eq!(codex_sandbox(&wu), "workspace-write");

        wu.authority.mode = 3;
        wu.authority.capabilities.push(Capability {
            name: "sandbox.danger-full-access".to_string(),
            scope: None,
            ttl_seconds: None,
            metadata: HashMap::new(),
        });
        assert_eq!(codex_sandbox(&wu), "danger-full-access");
    }

    fn new_test_paths(root: &Path, run_id: &str) -> RunPaths {
        let runs_dir = root.join("runs");
        let worktrees_dir = root.join("worktrees");
        let repos_dir = root.join("repos");
        fs::create_dir_all(&runs_dir).expect("create runs dir");
        fs::create_dir_all(&worktrees_dir).expect("create worktrees dir");
        fs::create_dir_all(&repos_dir).expect("create repos dir");

        let run_dir = runs_dir.join(run_id);
        let logs_dir = run_dir.join("logs");
        let artifacts_dir = run_dir.join("artifacts");
        let receipts_dir = run_dir.join("receipts");
        fs::create_dir_all(&logs_dir).expect("create logs dir");
        fs::create_dir_all(&artifacts_dir).expect("create artifacts dir");
        fs::create_dir_all(&receipts_dir).expect("create receipts dir");

        let workspace_dir = worktrees_dir.join(run_id);
        fs::create_dir_all(&workspace_dir).expect("create workspace dir");

        let events_raw = run_dir.join("events.raw.jsonl");
        let events_norm = run_dir.join("events.norm.jsonl");
        fs::write(&events_raw, []).expect("create raw events");
        fs::write(&events_norm, []).expect("create norm events");

        RunPaths {
            root: root.to_path_buf(),
            run_dir,
            logs_dir,
            artifacts_dir,
            receipts_dir,
            workspace_dir,
            events_raw,
            events_norm,
            repos_dir,
            worktrees_dir,
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn branch_slug_is_slash_free_and_git_safe() {
        let target = Target {
            repo: ".".to_string(),
            base_ref: "main".to_string(),
            subdir: Some("a/b c:d".to_string()),
            workspace_mode: WorkspaceMode::Worktree,
        };
        let branch = target.branch_slug("id:with:colon/and/slash");
        assert!(branch.starts_with("agentctl-run-"));
        assert!(!branch.contains('/'));
        assert!(!branch.contains(':'));
        assert!(!branch.contains(".."));
        assert!(!branch.ends_with(".lock"));
        assert!(branch.len() <= 200);
    }

    #[cfg(unix)]
    #[test]
    fn branch_slug_passes_git_check_ref_format_for_edge_inputs() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }

        let cases = [
            ("id..with..dots", Some("sub..dir")),
            ("id:with:colon/and/slash", Some("ends.lock")),
            ("--weird--..--", Some("...")),
        ];

        for (run_id, subdir) in cases {
            let target = Target {
                repo: ".".to_string(),
                base_ref: "main".to_string(),
                subdir: subdir.map(|v| v.to_string()),
                workspace_mode: WorkspaceMode::Worktree,
            };
            let branch = target.branch_slug(run_id);
            let out = Command::new("git")
                .arg("check-ref-format")
                .arg("--branch")
                .arg(&branch)
                .output()
                .expect("spawn git check-ref-format");
            assert!(
                out.status.success(),
                "git rejected branch {:?} (run_id={:?}, subdir={:?}): {}",
                branch,
                run_id,
                subdir,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn worktree_does_not_mutate_source_repo_and_survives_runs_leaf_ref() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "agentctl-worktree-collision-{}",
            uuid::Uuid::new_v4()
        ));
        let source = root.join("source");
        fs::create_dir_all(&source).expect("create source repo dir");

        git(&source, &["init"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "test"]);
        fs::write(source.join("README.md"), "hi\n").expect("write file");
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "init"]);
        git(&source, &["branch", "-M", "main"]);
        git(&source, &["branch", "runs"]);

        let run_id = "run-test";
        let paths = new_test_paths(&root.join("agentd"), run_id);
        let mut wu = minimal_work_unit();
        wu.target.repo = source.to_string_lossy().to_string();
        wu.target.base_ref = "main".to_string();
        wu.target.workspace_mode = WorkspaceMode::Worktree;

        prepare_worktree(&wu, run_id, &paths).expect("prepare_worktree");
        assert!(paths.workspace_dir.join(".git").exists());

        let branch = wu.target.branch_slug(run_id);
        let source_has_branch = Command::new("git")
            .arg("-C")
            .arg(&source)
            .arg("show-ref")
            .arg("--verify")
            .arg("--quiet")
            .arg(format!("refs/heads/{branch}"))
            .status()
            .expect("git show-ref source");
        assert!(!source_has_branch.success());

        let cached = resolved_repo_source(&wu.target.repo, &paths.repos_dir).expect("cached repo");
        let cached_has_branch = Command::new("git")
            .arg("-C")
            .arg(&cached)
            .arg("show-ref")
            .arg("--verify")
            .arg("--quiet")
            .arg(format!("refs/heads/{branch}"))
            .status()
            .expect("git show-ref cached");
        assert!(cached_has_branch.success());
    }

    #[test]
    fn codex_exec_early_budget_still_writes_agent_final_artifact() {
        let root = std::env::temp_dir().join(format!(
            "agentctl-agent-final-budget-{}",
            uuid::Uuid::new_v4()
        ));
        let run_id = "run-test";
        let paths = new_test_paths(&root, run_id);
        let mut events =
            EventWriter::new(run_id.to_string(), paths.events_norm.clone()).expect("event writer");

        let mut wu = minimal_work_unit();
        wu.agent.driver = "codex_exec".to_string();
        wu.target.workspace_mode = WorkspaceMode::Scratch;

        let timer = Instant::now() - Duration::from_secs(2);
        let out = run_codex_exec(&wu, &paths, &mut events, &timer, 1, &mut BTreeSet::new())
            .expect("run_codex_exec");
        assert_eq!(out.status, RunStatus::Failed);
        assert_eq!(
            out.final_message_ref.as_deref(),
            Some("artifacts/agent_final.md")
        );
        assert!(paths.artifacts_dir.join("agent_final.md").exists());
    }

    fn minimal_work_unit() -> WorkUnit {
        WorkUnit {
            version: "runfmt/0.1".to_string(),
            id: None,
            kind: "code_pr".to_string(),
            target: Target {
                repo: ".".to_string(),
                base_ref: "main".to_string(),
                subdir: None,
                workspace_mode: WorkspaceMode::Scratch,
            },
            agent: Agent {
                driver: "noop".to_string(),
                model_hint: None,
                prompt: "test".to_string(),
                context_files: vec![],
                personality: None,
                resume_session_id: None,
            },
            env: crate::work_unit::Env {
                profile: crate::work_unit::EnvProfile::Auto,
                setup: vec![],
            },
            authority: Authority {
                mode: 1,
                capabilities: vec![],
            },
            tools: Tools {
                mcp_profile: "docs".to_string(),
                command_policy: CommandPolicy::SafeDefault,
                network: NetworkPolicy::Deny,
            },
            budgets: Budgets {
                wall_seconds: 60,
                max_tool_calls: None,
                max_commands: None,
                max_bytes_written: None,
                max_diff_lines: None,
            },
            acceptance: Acceptance {
                commands: vec![],
                receipts_required: false,
            },
            outputs: Outputs {
                want_patch: true,
                want_commits: true,
                want_handoff: true,
                push_branch: false,
                open_pr: false,
            },
        }
    }

    #[cfg(unix)]
    #[test]
    fn times_out_command() {
        let mut cmd = Command::new("sh");
        cmd.arg("-lc").arg("sleep 1");
        let (_, timed_out) = run_command_with_timeout(&mut cmd, Duration::from_millis(50), "spawn")
            .expect("command should run");
        assert!(timed_out);
    }
}
