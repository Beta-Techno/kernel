# Kernel

Layer 1 of the autonomous minds substrate: a minimal, verifiable kernel that runs agent work units with isolation, policy, and durable artifacts. This repo will grow into `agentctl` / `agentd`, but the first milestone is codifying the run format (`runfmt/0.1`).

## Layout

- `runfmt/work_unit.schema.json` — JSON Schema for WorkUnit submissions.
- `runfmt/events.md` — normalized event registry for `events.norm.jsonl`.
- `runfmt/artifacts.md` — artifact bundle contract (RUN.json, HANDOFF, receipts, etc.).

Everything else (CLI, daemon, MCP gateway) will target this ABI.
