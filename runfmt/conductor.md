# Conductor Integration Contract (runfmt/0.1)

This document defines how Conductor should invoke `agentctl` and consume outputs for one WorkUnit execution.

## Invocation

Run command:

```bash
AGENTD_ROOT=/var/lib/agentd \
cargo run --manifest-path agentctl/Cargo.toml -- \
run --spec /path/to/work-unit.json --json
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

Conductor may provide `work_unit.id`; otherwise kernel generates a UUID.

Accepted IDs:

- length `1..128`
- characters `[A-Za-z0-9._-]`
- not `"."` and not `".."`

Rejected IDs fail fast before workspace creation.

## Run ID Reuse Policy

Kernel rejects reusing an existing run id:

- if `runs/<id>/` already exists, run start fails
- if `worktrees/<id>/` already exists, run start fails

Conductor should treat run IDs as immutable job IDs and generate a new one for retries.

## Canonical Artifacts

Conductor should consume:

- `runs/<run_id>/RUN.json`
- `runs/<run_id>/events.norm.jsonl`
- `runs/<run_id>/events.raw.jsonl`
- `runs/<run_id>/artifacts/diff.patch`
- `runs/<run_id>/artifacts/changed_files.json`
- `runs/<run_id>/artifacts/commits.json`
- `runs/<run_id>/logs/agent.stdout.log`
- `runs/<run_id>/logs/agent.stderr.log`
- `runs/<run_id>/logs/validate.stdout.log`
- `runs/<run_id>/logs/validate.stderr.log`

`artifacts/diff.patch` and `changed_files.json` are computed relative to `target.base_ref`, plus untracked additions.

## Valid vs Invalid Runs

A run is valid for orchestration when all are true:

- process exits with `0`, `10`, or `20`
- `runs/<run_id>/RUN.json` exists and validates against `runfmt/run_record.schema.json`
- `runs/<run_id>/events.norm.jsonl` exists

If `RUN.json` is missing or invalid, Conductor should treat the run as kernel-internal failure and not infer success from stdout alone.

## Exit Codes and Meaning

- `0` => `ok`
- `10` => `needs_human`
- `20` => `failed`

Conductor policy guidance:

- `ok`: proceed to next step
- `needs_human`: pause and surface intervention request
- `failed`: retry/escalate by policy with artifact links

## Event Signals

Conductor should parse `events.norm.jsonl` for:

- `run.created`
- `workspace.created`
- `agent.started`
- `agent.exited`
- `budget.exceeded`
- `policy.denied`
- `git.status`
- `git.diff.stats`
- `validation.finished`
- `run.finished`

## Notes

- `outputs.push_branch` and `outputs.open_pr` are currently denied by kernel policy.
- `kind="ops"` or `acceptance.receipts_required=true` requires at least one receipt file under `runs/<id>/receipts/`.
