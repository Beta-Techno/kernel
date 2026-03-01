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
  "run_id": "conductor-smoke-001",
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

Conductor should always provide `work_unit.id` and treat it as the authoritative job attempt id.

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

Do not rely on `workspace.branch` as the authoritative continuation point when prior runs can end with uncommitted changes.

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
