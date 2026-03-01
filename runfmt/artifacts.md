# runfmt/0.1 Artifact Contract

Every run produces `runs/<run_id>/` with the following structure. Entries marked **optional** may be absent; everything else is mandatory (files may be empty). Missing mandatory files indicate the run is invalid.

```
runs/<run_id>/
├── RUN.json               # machine-readable record (see schema)
├── HANDOFF.md             # human summary (template below)
├── events.raw.jsonl       # raw driver stream (Codex JSONL, etc.)
├── events.norm.jsonl      # normalized kernel events (event registry)
├── env_fingerprint.json   # toolchain snapshot
├── artifacts/
│   ├── diff.patch         # unified diff for workspace.base_sha..workspace.final_sha (may be empty)
│   ├── changed_files.json # JSON array of changed file paths
│   ├── commits.json       # list of commits created (may be [] when none or want_commits=false)
│   └── agent_final.md     # optional final assistant message from driver
├── logs/
│   ├── agent.stdout.log
│   ├── agent.stderr.log
│   ├── validate.stdout.log
│   └── validate.stderr.log
├── receipts/              # side-effect receipts (required for ops)
│   └── <source>/<timestamp>_<id>.json
└── workspace/             # optional symlink pointing to worktrees/<run_id>/
```

Workspaces live under `{root}/worktrees/<run_id>/`. For scratch runs the workspace is an empty directory; for worktree mode it is a git worktree checked out from the repo cache.

## Output Flags

WorkUnit `outputs.*` flags control artifact intent, but files remain schema-stable:

- `want_patch = false` still creates `artifacts/diff.patch` as an empty file.
- `want_commits = false` still creates `artifacts/commits.json` as `[]`.
- `want_handoff = false` still creates `HANDOFF.md` with a disabled marker.

Skipped artifacts are recorded in `events.norm.jsonl` as `artifact.skipped`.

## RUN.json (schema excerpt)

`runfmt/run_record.schema.json` formalizes the structure. Key fields:

```json
{
  "run_id": "01J0EXAMPLE",
  "version": "runfmt/0.1",
  "kind": "code_pr",
  "status": "ok | failed | needs_human",
  "driver": "noop | codex_exec | ...",
  "agent_session_id": "thread_...",
  "started_at": "2026-02-23T18:25:43.511Z",
  "finished_at": "2026-02-23T18:30:12.100Z",
  "spec": {
    "path": "runfmt/examples/noop.toml",
    "hash": "<sha256 hex of raw spec bytes>"
  },
  "workspace": {
    "mode": "scratch | worktree | clone",
    "path": "/runner/worktrees/01J...",
    "branch": "agentctl-run-01J...-builder",
    "base_ref": "main",
    "base_sha": "6f1a...",
    "final_sha": "d4c9...",
    "continuation_ref": "refs/agentctl/continuations/01J0EXAMPLE"
  },
  "budgets_used": {
    "wall_seconds": 12,
    "tool_calls": 0,
    "commands": 0
  },
  "changed_files": [],
  "validation": {
    "status": "skipped | passed | failed",
    "details_ref": "logs/validate.stdout.log"
  },
  "artifacts": {
    "diff": "artifacts/diff.patch",
    "handoff": "HANDOFF.md",
    "agent_final": "artifacts/agent_final.md"
  }
}
```

`workspace.base_sha` is the exact commit checked out at run start for git-backed runs.
`workspace.final_sha` is the exact commit representing the final workspace state. If the run ends dirty/untracked, kernel creates a synthetic snapshot commit and records it here.
`workspace.continuation_ref` is a kernel-managed ref pointing at `workspace.final_sha` so the snapshot remains reachable.
For continuation, set next run `target.base_ref` to the prior `workspace.final_sha`.
`artifacts/commits.json` includes agent-created commits only; kernel synthetic snapshot commits are excluded.

`agent_session_id` is optional and records the driver session/thread identifier when available (for `codex_exec`, this is the `thread.started.thread_id` value).
`artifacts.agent_final` is optional and records the driver-provided final assistant message when available.

## HANDOFF.md Template

```
# Summary
- **Run:** <run_id>
- **Status:** ok | failed | needs_human
- **Repo:** <repo>
- **Branch:** <actual branch> | (none)

## Intent
Describe the requested goal and the approach taken.

## Changes
List key files touched with rationale.

## Validation
- lint: pass/fail + log reference
- tests: pass/fail + log reference

## Risks / Next Steps
Bullet list of follow-ups, risks, or manual verifications.
```

Kernel implementations should pre-populate the template even if the agent fails to produce a summary.

## Receipts

Each receipt is JSON:

```json
{
  "source": "github",
  "kind": "pull_request",
  "id": "123",
  "url": "https://github.com/...",
  "description": "Opened PR agentctl-run-01J...-builder",
  "timestamp": "2026-02-23T18:31:00Z",
  "idempotency_key": "sha256-..."
}
```

Receipts are mandatory when `kind = ops` or `acceptance.receipts_required = true`.
