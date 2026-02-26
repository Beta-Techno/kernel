# runfmt/0.1 Event Registry

Each event is a single JSON object in `events.norm.jsonl` with the shape:

```json
{
  "v": "runfmt/0.1",
  "seq": 12,
  "ts": "2026-02-23T18:25:43.511Z",
  "run_id": "01J...",
  "type": "workspace.created",
  "data": { "branch": "agentctl-run-01J...-builder" }
}
```

Events are append-only. New event types may be added in backwards-compatible ways; existing `type` payloads must remain stable.

## Lifecycle

| type | data payload |
| ---- | ------------- |
| `run.created` | `{ "kind": "code_pr", "submitted_by": "operator-id" }` |
| `workspace.created` | `{ "path": "/runner/worktrees/01J...", "mode": "worktree", "branch": "agentctl-run-..." }` |
| `agent.started` | `{ "driver": "codex_exec", "cmd": ["codex","exec","--json",...] }` |
| `agent.stdout` | `{ "chunk": "base64" }` (only when redaction policy allows) |
| `agent.stderr` | `{ "chunk": "base64" }` |
| `agent.exited` | `{ "exit_code": 0, "reason": "completed" }` |
| `run.interrupted` | `{ "reason": "operator", "signal": "SIGTERM" }` |
| `run.finished` | `{ "status": "ok|failed|needs_human", "summary_ref": "RUN.json" }` |
| `artifact.skipped` | `{ "artifact": "artifacts/diff.patch", "reason": "outputs.want_patch=false" }` |

## Policy & Budgets

| type | data |
| ---- | ---- |
| `policy.denied` | `{ "capability": "repo.write", "reason": "autonomy_mode" }` |
| `policy.approved` | `{ "capability": "repo.write", "by": "human@time" }` |
| `budget.exceeded` | `{ "budget": "wall_seconds", "limit": 2700 }` |

## Workspace / Git

| type | data |
| ---- | ---- |
| `git.status` | `{ "clean": false, "tracked": 2, "untracked": 1 }` |
| `git.diff.stats` | `{ "files": 1, "insertions": 12, "deletions": 0 }` |
| `git.commits` | `{ "count": 2, "artifact": "artifacts/commits.json" }` |
| `file.write` | `{ "path": "AGENT_KERNEL_SMOKE_TEST.md", "bytes": 128 }` (coarse-grained) |

## Commands & Tools

| type | data |
| ---- | ---- |
| `command.exec` | `{ "cmd": ["npm","test"], "cwd": "repo", "id": "cmd-01" }` |
| `command.result` | `{ "id": "cmd-01", "exit_code": 0, "stdout_ref": "logs/validate.stdout.log" }` |
| `tool.call` | `{ "tool": "openaiDeveloperDocs.search", "args_hash": "sha256...", "capability": "mcp.github.read" }` |
| `tool.result` | `{ "tool": "openaiDeveloperDocs.search", "status": "ok", "receipt_ref": null }` |

## Receipts & Ops

| type | data |
| ---- | ---- |
| `receipt.recorded` | `{ "source": "github", "id": "PR#123", "url": "https://..." }` |

## Validation

| type | data |
| ---- | ---- |
| `validation.started` | `{ "commands": ["npm test"] }` |
| `validation.finished` | `{ "status": "passed|failed", "details_ref": "logs/validate.stdout.log" }` |

## Notes

- `agent.stdout` / `agent.stderr` chunks are base64-encoded after redaction. Store large blobs as files when possible and reference via `_ref` fields.
- `receipt.recorded` is mandatory for ops modes where `acceptance.receipts_required = true`.
- Event order is chronological; consumers MUST NOT assume contiguous event types.
- `seq` is a monotonically increasing per-run event counter starting at `1`.
