# Long-Term Memory

Fiasco memory is durable knowledge accumulated across runs. It is not the
live context window, transcript persistence, or a run summary.

## Scopes And Files

- Global user memory: `$FIASCO_HOME/memory/user/`
- Project memory: `<workspace>/.fiasco/memory/project/`

Both locations contain ordinary human-editable Markdown. Fiasco does not
define a database schema, vector index, or dedicated memory read/search API.
An ordinary agent run's initial runtime reminder names the resolved paths; the
model uses `read` for known files and `bash` with `rg` for discovery.

Project rules that every agent must obey belong in `AGENTS.md`. Memory records
user preferences, historical explanations, prior decisions, and evolving
project knowledge. A stable rule may later be promoted into `AGENTS.md` through
an explicit repository change.

## Updates

Memory has no dedicated model tool or agent profile. The ordinary `write` tool
can create or make targeted edits to either absolute memory path, while `bash`
can perform broader file operations. The dynamic memory reminder asks the model
to inspect existing memory, keep user and project scopes distinct, and record
curated durable knowledge rather than dumping a transcript.

For a small focused change, the current agent edits the Markdown directly. For
a large independent consolidation, an agent with remaining delegation depth
can use `agent start`,
continue other useful work, then use `agent wait` or accept the completed
activity result before verifying the files. This is the same durable
GeneralTask mechanism used for any other
delegated work; the harness does not need memory-specific execution or recovery
logic.

Fiasco deliberately does not auto-record every successful run. That would
turn transcripts into noisy memory without model judgment.

## Consolidation

`fiasco memory consolidate` starts an ordinary root run for the selected memory
paths. It may delegate to a general-task child when available and useful. The
agent may merge related facts, remove stale duplication, preserve provenance,
and rewrite the Markdown for clarity. The harness does no similarity scoring or
domain judgment.

Use an external cron, systemd timer, or cloud scheduler:

```cron
15 3 * * * /usr/local/bin/fiasco --workspace /workspace/project memory consolidate
```

Markdown remains the source of truth. A future index can be a rebuildable
projection, but vector search is not required for the launch runtime.

## Prompt And Persistence Behavior

Memory contents are not injected wholesale. When memory is enabled, every
ordinary root or GeneralTask reminder contains the resolved paths and a short
management reminder. A delegated consolidation has
its own run directory, transcript, events, artifacts, parent id, and durable
task record, so it follows the existing reusable-child contract without
inflating the parent context. If the process stops mid-activity, recovery
reports it interrupted and leaves the same child thread available for an
explicit retry.
