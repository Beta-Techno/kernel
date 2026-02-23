# runfmt/0.1 Artifact Contract

Every run produces `runs/<run_id>/` containing the following structure. Missing files indicate an invalid run record.

```
runs/<run_id>/
├── RUN.json               # machine summary (mirrors schema below)
├── HANDOFF.md             # human summary (template driven)
├── events.raw.jsonl       # raw agent stream from driver
├── events.norm.jsonl      # normalized stream (per events.md)
├── env_fingerprint.json   # toolchain snapshot (versions, container IDs)
├── artifacts/
│   ├── diff.patch         # unified diff against base_ref
│   ├── commits.json       # list of commit SHAs (if any)
│   └── changed_files.json # structured file stats
├── logs/
│   ├── agent.stdout.log
│   ├── agent.stderr.log
│   ├── validate.stdout.log
│   └── validate.stderr.log
└── receipts/
    └── <source>/<timestamp>_<id>.json  # populated for ops receipts
```

## RUN.json Schema

```json
{
  "run_id": "01J...",
  "version": "runfmt/0.1",
  "kind": "code_pr",
  "status": "ok|failed|canceled",
  "started_at": "2026-02-23T18:25:43.511Z",
  "finished_at": "2026-02-23T18:30:12.100Z",
  "budgets_used": {
    "wall_seconds": 180,
    "tool_calls": 12,
    "commands": 2
  },
  "workspace": {
    "path": "/runner/worktrees/01J...",
    "branch": "runs/01J.../builder",
    "base_ref": "main"
  },
  "validation": {
    "status": "passed",
    "details_ref": "logs/validate.stdout.log"
  },
  "artifact_refs": {
    "diff": "artifacts/diff.patch",
    "handoff": "HANDOFF.md"
  }
}
```

## HANDOFF.md Template

```
# Summary
- **Run:** <run_id>
- **Status:** ok|failed|needs_human
- **Branch:** runs/<run_id>/...

## Intent
<One paragraph describing the goal.>

## Changes
<List key files touched + rationale.>

## Validation
- lint: pass/fail
- tests: pass/fail (link to logs)

## Risks / Next Steps
- Bullet list of open questions, manual verifications, or follow-on tasks.
```

## Receipts

Each receipt file is JSON with:

```json
{
  "source": "github",
  "kind": "pull_request",
  "id": "123",
  "url": "https://github.com/...",
  "description": "Opened PR for branch runs/01J.../builder",
  "timestamp": "2026-02-23T18:31:00Z",
  "idempotency_key": "sha256-..."
}
```

Receipts are mandatory when `kind = ops` or `acceptance.receipts_required = true`.
