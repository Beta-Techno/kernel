# Kernel

Layer 1 of the autonomous minds substrate: a minimal, verifiable kernel that runs agent work units with isolation, policy, and durable artifacts. This repo will grow into `agentctl` / `agentd`, but the first milestone is codifying the run format (`runfmt/0.1`).

## Layout

- `runfmt/work_unit.schema.json` — JSON Schema for WorkUnit submissions.
- `runfmt/events.md` — normalized event registry for `events.norm.jsonl`.
- `runfmt/artifacts.md` — artifact bundle contract (RUN.json, HANDOFF, receipts, etc.).

Everything else (CLI, daemon, MCP gateway) will target this ABI.

## agentctl

`agentctl` is the first Layer 1 runner implementation.

Core commands:

- `cargo run -- run --spec /path/to/work-unit.json`
- `cargo run -- list --limit 20`
- `cargo run -- show <run_id>`
- `cargo run -- rerun <run_id>`

Bundled specs:

- `agentctl/runfmt-example.json` — deterministic schema/offline example (`workspace_mode: scratch`, `driver: noop`).
- `agentctl/smoke-worktree.json` — runnable local worktree smoke spec against current repo (`target.repo: "."`).

Smoke run:

- `AGENTD_ROOT=/tmp/agentd-smoke cargo run -- run --spec agentctl/smoke-worktree.json`

## Copying context to clipboard

Use `scripts/copy_context.sh` to copy a formatted repo context snapshot to clipboard.

Examples:

- Full tracked context:
  - `scripts/copy_context.sh`
- Only changed files:
  - `scripts/copy_context.sh --changed`
- Include untracked files too:
  - `scripts/copy_context.sh --include-untracked`
- Restrict to a subtree:
  - `scripts/copy_context.sh --path agentctl/src`
- Print to stdout instead of clipboard:
  - `scripts/copy_context.sh --stdout`
