# Conductor Integration Contract (runfmt/0.1)

This document defines how orchestration wrappers (Conductor) should invoke `agentctl` and continue runs safely.

## Invocation

Run command:

```bash
AGENTD_ROOT=/var/lib/agentd \
agentctl run --spec /path/to/work-unit.json --json
```

`--json` is the machine contract for wrappers. It prints a single JSON object:

```json
{
  "run_id": "01JEXAMPLE2NN4Y8C9K3V0Q5M",
  "status": "ok|failed|needs_human",
  "exit_code": 0,
  "run_dir": "/var/lib/agentd/runs/conductor-smoke-001",
  "run_record": "/var/lib/agentd/runs/conductor-smoke-001/RUN.json"
}
```

Without `--json`, `agentctl` prints a stable human line:

```text
run <run_id> completed with status <ok|failed|needs_human>
```

## Run ID Rules

`work_unit.id` is the per-attempt bundle id. Conductor should usually omit it and let kernel generate a fresh run id.

Use `lineage.workflow_id` as the durable workflow identity and `lineage.parent_run_id` to link attempts.

- `work_unit.id` rules when supplied:
  - length `1..128`
  - characters `[A-Za-z0-9._-]`
  - not `"."` and not `".."`

If `work_unit.id` is omitted, kernel generates a UUID.

## Valid Runs

A run is valid for orchestration when all are true:

- process exits with `0`, `10`, or `20`
- `runs/<run_id>/RUN.json` exists and validates against `runfmt/run_record.schema.json`
- `runs/<run_id>/events.norm.jsonl` exists

If `RUN.json` is missing or invalid, wrappers must treat the run as failed even if process output looked successful.

## Continuation Semantics

For git-backed runs, `RUN.json.workspace` includes:

- `base_sha`: exact commit at run start
- `final_sha`: exact commit for final workspace state
- `continuation_ref`: kernel-managed ref pointing to `final_sha`

To continue code state exactly across turns:

- set next run `target.base_ref = previous RUN.json.workspace.final_sha`
- set next run `lineage.parent_run_id = previous RUN.json.run_id`
- preserve `lineage.workflow_id` across all turns in one workflow

Do not rely on `workspace.branch` as the authoritative continuation point when prior runs can end with uncommitted changes.

For same-agent continuation, set next run `agent.resume_session_id = previous RUN.json.agent_session_id` when present.

Current guarantee scope: exact continuation is guaranteed for runs chained under the same `AGENTD_ROOT` using `workspace_mode = "worktree"`. Clone-mode continuation may require additional object/ref import logic and should be treated as best-effort until explicitly hardened.

## Artifacts

Wrappers should consume:

- `runs/<run_id>/RUN.json`
- `runs/<run_id>/events.norm.jsonl`
- `runs/<run_id>/artifacts/diff.patch`
- `runs/<run_id>/artifacts/changed_files.json`
- `runs/<run_id>/artifacts/commits.json`
- `runs/<run_id>/spec/work_unit.json`

`diff.patch` and `changed_files.json` are computed from `workspace.base_sha..workspace.final_sha`.
`commits.json` contains user/agent commits only (kernel synthetic continuation snapshots are excluded).

For replay, wrappers should prefer `RUN.json.spec.snapshot_path` over `RUN.json.spec.path`.
