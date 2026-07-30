# Fiasco

> Orchestrate the agents. Contain the fiasco.

Fiasco is a headless multi-agent orchestrator written in Rust. It coordinates
delegated agents and long-running background jobs through one durable runtime
for local automation and cloud workloads.

The runtime deliberately shares one `AgentRunner` implementation and one tool
contract across root and child agents. Runs are portable and resumable without
a database, distributed scheduler, sandbox, or approval system. Transcript
inspection embeds fmtview behind a storage-neutral boundary; the agent runtime
itself remains headless.

## Features

- OpenAI OAuth with device login, refresh, Codex auth import, and one 401 retry
- OpenAI-compatible Responses and Chat Completions streaming APIs
- Anthropic-compatible Messages streaming API
- streamed text, fragmented tool calls, and usage/cache-token fields in events
- compact provider surface with native `read`, `write`, and `bash` plus one
  in-process `fiasco` command tool; `read` supports bounded UTF-8 text and image
  attachments for vision-capable configured models
- typed harness commands for skills, optional web search, history, agents, and
  runtime handles, with linear pipelines and terminal atomic redirect
- exact-first, atomic multi-edit writes with CRLF/BOM preservation
- run-local artifact spill for large tool results with bounded head/tail previews
- optional local context compaction recorded as ordinary messages, with
  read-only regex history retrieval
- Agent Skills discovery with progressive `SKILL.md` loading
- progressively documented MCP stdio servers routed through `fiasco mcp`
- command hooks for run and tool lifecycle events
- concurrent direct-tool batches whose unfinished calls continue under
  process-local runtime handles
- asynchronously delegated general-task subagents that reuse the same runner
- an installable skill for graph-based orchestration with ordinary file tools
- ordinary Markdown user/project memory maintained with normal file tools
- self-contained run directories and optional NDJSON events
- tail-first transcript inspection for completed and running runs

## Important Security Boundary

Fiasco does not provide a security sandbox in this release. Built-in tools,
MCP servers, hooks, and child agents run with the same permissions as the
fiasco process. Run it only in environments where that access is appropriate.

## Install

Fiasco requires Rust and `rg` (ripgrep). Artifact-backed compacted-history
search invokes `rg` directly and expects it on `PATH`.

```bash
cargo install --path .
```

The binary is named `fiasco`.

## Quick Start

Without a config file, fiasco uses a deterministic echo provider:

```bash
fiasco run "hello"
```

Run a task with machine-readable events:

```bash
fiasco run --output ndjson "inspect this repository and run its tests"
```

Runtime output is stored beneath the current project:

```text
.fiasco/runs/<run-id>/
  run.json
  messages.jsonl
  events.jsonl
  final.md
  artifacts/
```

On an interactive terminal, inspect opens the current newline-complete tail and
loads older messages lazily:

```bash
fiasco inspect <run-id>
fiasco inspect <run-id> --follow
```

`--follow` refreshes newly completed message lines while preserving fmtview's
attached, detached, and paused browsing states. A redirected `--follow` is
rejected because this mode requires an interactive terminal.

When stdout is redirected, inspect instead writes exact newline-complete JSONL.
The explicit form is useful in scripts. The legacy metadata/final-output view
remains available separately:

```bash
fiasco inspect <run-id> | jq -c .
fiasco inspect <run-id> --output ndjson
fiasco inspect <run-id> --summary
```

Resume an interrupted or failed run after repairing its message tail:

```bash
fiasco resume <run-id>
```

Every complete newline is visible to inspectors, including a possible prefix of
the final assistant/tool exchange. Before resume, Fiasco matches the trailing
assistant's tool calls against ordered results and discards that exchange if
any result is missing. It then appends a user/runtime reminder that the process
stopped and workspace or external side effects may already have occurred. It
does not restore ordinary asynchronous tool jobs, active child work,
process-local input, or undelivered results. Existing child threads keep their
complete transcripts and remain discoverable through `agent list`; an explicit
`agent send` adds a crash reminder and starts a new activity. Resume the parent
run rather than invoking `fiasco resume` on a child id directly.

Before resume, the process supervisor, cgroup, or container must have killed
the previous fiasco process and all locally managed descendants. Remote
jobs and other external side effects are not covered by that assumption.

## Prompt Layout

Fiasco keeps its built-in system prompt independent of the workspace,
tool-agnostic, and unchanged across normal agent calls. Each typed `tool.yaml`
owns its capability's workflow guidance as well as its schema. Native manifests
become provider schemas; internal manifests drive `fiasco` help and argument
conversion, so removing a command also removes its catalog guidance. At the
start of each run, the
first user message contains a synthetic `<runtime-reminder>` block before the
original request. The reminder snapshots the workspace path, `AGENTS.md` (or
lowercase `agents.md` when the canonical name is absent), discovered skill
metadata, memory locations, and any delegated-task instructions that apply to
the role. It also records the role and remaining
delegation depth; GeneralTask guidance lives here rather than in a second
system prompt. Provider tool schemas and enabled command routes are identical
for Root and GeneralTask, sorted, and frozen for the run; configuration or file
changes take effect on the next run. The environment section also states
`current model supported
modalities: [text]` (or `[text, image]`); the stable system prompt tells the
agent not to request an absent modality. Compaction reuses this stable
system/tool prefix and adds one final user instruction only to the compaction
request.

`messages.jsonl` uses the self-contained `fiasco-message` format. Each line has a
short `ref` (`m1`, `m2`, ...), `created_at`, `role`, and typed `content` blocks.
Those blocks are the exact provider-neutral messages replayed by the runner, so
tool failures, artifact refs, images, reasoning, and opaque provider
continuation items need no sidecar or reconstruction layout. Optional
compaction classification lives under `_fiasco` on the same line. The
sequence is derived from `ref` and the line position rather than duplicated.

The single process holding the run execution lease is the only writer; any
number of viewers may read complete message lines without taking a message-log
lock. A viewer may briefly show a prefix of a tool turn while its batch is being
written. The next writer trims a torn line and any semantically incomplete
trailing tool exchange before resuming appends. `run.json` declares
`"message_format": "fiasco-message"`, retains the original user prompt, and
freezes the stored profile plus remaining delegation depth. Compaction never
rewrites or deletes committed trajectory records.

The interactive inspector delegates the physical growing-file timeline to
fmtview-core and terminal lifecycle, JSON/chat rendering, search, navigation,
wrap, and follow state to fmtview. Fiasco owns run lookup and terminal-state
mapping; it does not duplicate newline paging or ratatui/crossterm behavior.

Stable agent instructions are folded scalar values in the typed, compile-time
`prompts/agents.yaml` registry. Every local model-facing tool adapter has a
typed `tool.yaml` beside it; standalone tools live under `src/tools/<tool>/`,
while handle, history, and graph families use
`src/tools/<family>/<member>/`. The manifest
always owns the complete model-facing name, purpose description, return
guidance, and input schema. Rust composes the two prose fields into the standard
provider description and owns validation, assembly, and execution.

## Graph-based Orchestration Skill

Fiasco keeps graph planning as optional model guidance rather than a built-in
runtime subsystem. Install the repository's `orchestrate-with-graphs` Agent
Skill into a project with:

```bash
npx skills add siriusctrl/fiasco \
  --skill orchestrate-with-graphs \
  --agent universal
```

The skill teaches an orchestrator when a dependency graph is useful, how to
store it under `.agents/graphs/`, how to distinguish accepted knowledge from
live execution, and how to coordinate independent work. It uses ordinary file
capabilities and explicit agent controls.

Each graph is a lightweight YAML mental model with `open` or `resolved`
lifecycle. Nodes represent outcomes, questions, or decisions rather than agent
threads. A graph may be resolved with unfinished nodes when its summary explains
that the remaining work was abandoned, superseded, or deliberately narrowed.
The complete workflow and file convention live in
[`skills/orchestrate-with-graphs/`](skills/orchestrate-with-graphs/).

If graphs later require shared remote storage, that service can expose a
dedicated access capability without moving scheduling or execution state into
the graph model.

## Provider Setup

Configuration is loaded from the first existing path:

1. `--config <path>`
2. `<workspace>/.fiasco/config.toml`
3. `$HOME/.fiasco/config.toml`
4. built-in echo defaults

Config files are not merged, and unknown fields are rejected so misspelled
settings fail at startup.

### OpenAI OAuth

```toml
[provider]
kind = "openai-oauth"
model = "gpt-5.6-sol"
modalities = ["text"]
```

Authenticate once:

```bash
fiasco auth login
```

Credentials are stored at `$FIASCO_HOME/auth.json` (default
`$HOME/.fiasco/auth.json`). If no fiasco credentials exist, the provider can
import a compatible `$CODEX_HOME/auth.json` or `$HOME/.codex/auth.json`.

### OpenAI-compatible

```toml
[provider]
kind = "openai-compatible"
model = "my-model"
modalities = ["text"] # use ["text", "image"] only for a vision model
base_url = "http://127.0.0.1:8000/v1"
api_key = "${OPENAI_API_KEY}" # or a literal key
protocol = "chat-completions" # or "responses"
reasoning_effort = "medium" # optional; provider/model-specific
```

`api_key` accepts either a literal key or a whole environment reference such as
`${OPENAI_API_KEY}`. Keep a literal key in the user config at
`$HOME/.fiasco/config.toml` with restrictive file permissions rather than in a
workspace file that may be shared. If `api_key` is omitted, fiasco reads
`OPENAI_API_KEY`. The removed OpenAI-compatible `api_key_env` field is rejected;
write `api_key = "${OPENAI_API_KEY}"` instead.

`reasoning_effort` is optional. Fiasco sends it as `reasoning_effort` for
Chat Completions and as `reasoning.effort` for Responses. If omitted, the
provider's model default is used. Common values include `none`, `minimal`,
`low`, `medium`, `high`, and `xhigh`; accepted values depend on the endpoint and
model.

When Chat Completions reasoning is configured, `max_output_tokens` is sent as
`max_completion_tokens`, as required by reasoning-capable Chat endpoints.

The OpenAI-compatible adapter retries an initial HTTP 429 up to three times
with bounded exponential backoff. Parent and child requests also share the
runtime model-concurrency limit described below.

If a Chat Completions stream explicitly returns `delta.reasoning_content`,
fiasco stores it in the assistant message's optional `reasoning_content`
field in `messages.jsonl` and
emits transient `model_reasoning_delta` events to live event sinks. Per-chunk
text and reasoning events are not written to `events.jsonl`; the complete
assistant message is the durable trajectory. Reasoning token counts are
preserved in persisted `model_completed` events when the provider reports them.
Reasoning is kept out of the visible answer and `final.md`. This records only
reasoning text exposed by the compatible endpoint; it cannot recover reasoning
that the provider does not return.

Inspect the persisted reasoning for a run with:

```bash
jq -c 'select(.role == "assistant" and has("reasoning_content")) | {role, reasoning_content}' .fiasco/runs/<run-id>/messages.jsonl
jq -c '.' .fiasco/runs/<run-id>/events.jsonl
```

`reasoning_content` is an OpenAI-compatible endpoint extension, not an official
OpenAI Chat Completions message field. Fiasco writes it only when the
endpoint explicitly returns reasoning text, and replays it as the same separate
field on later requests to that compatible Chat endpoint.

### Anthropic-compatible

```toml
[provider]
kind = "anthropic-compatible"
model = "my-model"
modalities = ["text"]
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"
```

See [configuration.md](docs/configuration.md) for runtime, compaction, artifact,
MCP, hook, and memory settings.

## Context Compaction And History

Automatic local compacted-state creation is disabled by default. Enable it with
thresholds appropriate to the model's context window:

```toml
[runtime]
max_output_tokens = 8192

[compaction]
compact_at_tokens = 100000
context_window_tokens = 131072
keep_recent_tokens = 20000
summary_max_output_tokens = 4096
history_search_max_matches = 50
```

Fiasco estimates the complete input from the first request and replaces that
estimate with provider-reported input usage whenever available. It can make an
additional model call when the tracked input reaches `compact_at_tokens`. The
call receives the original initial message, any previous compacted state, the
native older messages being replaced, and one final compaction instruction.
The successful user instruction and exact assistant response are appended to
`messages.jsonl`; the assistant response is the durable compacted state. Later
normal requests omit the compaction instruction and contain that exact
assistant message plus the exact recent suffix.

`context_window_tokens` is the model's configured nominal full window. Before
normal and compaction requests, fiasco checks its provider-neutral estimate
of system, schemas, active messages, and configured output allowance. It must
be greater than `compact_at_tokens`; if compaction cannot reduce the estimate
below it, the run fails locally. This is an early safety check, not a
tokenizer-exact provider guarantee. Setting the window requires an explicit
nonzero `runtime.max_output_tokens`; GeneralTask uses its separately configured
profile limit. Start/completion/failure records remain in `events.jsonl`;
compaction retries have numbered attempts, while a preflight rejection has no
started event or attempt because no provider request occurred. Compatible Chat `reasoning_content` and replayable opaque
provider items are included in the between-call estimate.

The normal agent receives the `fiasco` schema with `history search` and
`history read` routes from its first provider request whether or not
`compact_at_tokens` is configured. Changing the threshold controls
compacted-state creation only; it does not change the normal system prompt,
provider schemas, or command catalog. Before anything has been compacted, the
history commands simply have no compacted prefix to search or read.

Two read-only tools recover exact details omitted from the active request:

- `history search <pattern>` applies a Rust regular expression only to
  compacted messages and their linked textual tool-result artifacts. Results
  are newest-first. Each match contains `ref`, `source`, and `snippet`: refs are
  run-local sequence addresses such as `m37` (smaller numbers are older), while
  `source` is `message` for inline content or `artifact` for a linked complete
  spilled result.
- `history read m37 before=2 after=2` reads a bounded chronological window
  around one ref. It returns JSONL records shaped as
  `{"ref":"m<N>","message":<OpenAI Chat-compatible message>}` and keeps tool
  calls paired with their results.

The local reader invokes `rg` for bounded-memory searches inside full textual
artifacts, so ripgrep must be available on `PATH` for that part of
`history search`. A future remote reader can provide the same contract from a
database or service.

Neither command has a cursor. `history_search_max_matches` limits a query to its
newest matches; if reached, older matches are omitted and the model must refine
the regex. This is distinct from artifact preview truncation: if the bounded
JSON/JSONL tool result is too large, its complete returned content is saved as
an artifact and can be inspected with `read`, continuing from the returned
`line_offset` or `byte_offset`; `bash`/`rg` is also useful for targeted searches. Query-limit
omissions are not present in that artifact.

Each assembled agent run has sorted, frozen provider schemas and command
routes. A run compacts only when both history adapters and at least one artifact
inspection tool (`read` or `bash`) are present. The compaction request reuses
the same system prompt and tool schemas; a tool-call response is rejected
rather than executed.

This release implements local model-generated compacted states only. It does not use
OpenAI or another provider's server-side compaction API.

## Large Tool Results

Small results are returned inline. Large results are written in full under the
current run and replaced in model context with:

- beginning and ending previews
- generation-time byte counts and media type
- a project-relative run-local attachment path

The model can inspect the complete output with bounded `read` calls or search it
with `bash` plus `rg`. The path names a mutable attachment: later inspection
observes its current contents, while the original preview remains unchanged.
Every tool result is limited independently; a previous large result never
suppresses a later small result. See
[artifacts.md](docs/artifacts.md).

## Tools And Background Work

The launch tool surface is intentionally small:

- `read`: bounded UTF-8 reads for a known path, or model attachments for jpg,
  jpeg, png, gif, webp, and bmp images
- `write`: full-file creation/replacement or an atomic list of targeted edits
- `bash`: local discovery, `rg`, tests, builds, and other Bash commands; returns
  combined stdout/stderr and adds a status line only for unsuccessful
  completion. It uses a non-login shell and inherits fiasco's environment
  without loading profile files
- `fiasco`: CLI-like in-process access to skills, optional web search, history,
  delegation, runtime handles, and optional MCP

The `fiasco` input has a required `command` string and optional exact `stdin`.
Its enabled catalog includes:

```text
skill load <name>
web search <query> [count=<integer>] ...
history search <pattern>
history read <ref> [before=<integer>] [after=<integer>]
agent start name=<string> prompt=<string|->
agent send handle=<id> mode=<steer|followup> message=<string|->
agent list [handles=<JSON array>] [include_closed=<boolean>]
agent inspect <handle> [before_seq=<integer>] [limit=<integer>]
agent wait [handles=<JSON array>]
agent stop <handle>
agent close <handle>
mcp <source> <tool> [name=value ...]
```

`help` lists the enabled routes; `help <command path>` returns that command's
typed description and input schema. Optional web search and MCP add routes
before the run begins instead of adding provider schemas.

Quoted words, `name=value`, exact stdin substitution with `-`, linear `|`, and
one terminal `> path` are supported. Pipelines run sequentially in the owning
process, pass the complete successful pre-artifact output, and stop at the first
error. Redirect occurs only after the whole pipeline succeeds and reuses
`write`'s atomic replacement contract. This is not a shell: use native `bash`
for `&&`, `||`, append, variables, globbing, command substitution, loops,
background jobs, or file-descriptor redirection. Pipeline input must be
unambiguous UTF-8; native image attachments cannot be piped or redirected.

Root and GeneralTask receive the same provider schemas and command routes.
Remaining delegation depth is frozen in run state and shown in the runtime
reminder. At zero, `agent start` returns a local error without creating a
child; the route does not disappear. Memory adds paths to the reminder, not a
schema or command. Configured MCP artifacts contribute only their namespace,
description, and source-map path to the runtime reminder, not their remote
schemas. The schemas and route catalog are frozen before the run's first normal
provider call.

`write` requires every edit target to identify one non-overlapping region in
the original file. It tries exact matching first, then a conservative whole-line
indentation normalization. It does not use broad fuzzy similarity that could
silently modify the wrong code.

All direct tool calls in one assistant message start concurrently and share one
foreground window. If all finish early, fiasco returns immediately. At the
configured deadline, it preserves each unfinished exact future, moves only
those calls under process-local `j_<ulid>` handles, and returns those handles;
no tool is stopped or restarted. The assistant message, tool-result messages in
original call order, and any attachment message are appended in that order;
each complete line becomes visible independently.
Results retain their original `tool_call_id`, even though completion events can
arrive in another order. The model should put only independent calls in one
batch and issue dependent work after seeing results.
The tool result is a status-less `<runtime_handle>` notice containing the
handle, kind, and name; it only acknowledges that work is running.

`agent start` requires a non-empty model-supplied display name and starts one
isolated, reusable `general-task` agent asynchronously. Its handle is the child
run id, so the handle addresses the same durable transcript across activities
and process restarts. Promoted outer-tool handles exist only in the current
process. Each output uses the ordinary artifact policy. At the next model
boundary, one user/runtime message batches every ready `<runtime_handle
status="...">` notice.

The handle commands below are values of `fiasco.command`:

```text
agent start name=inspect_tests prompt=-
agent wait 'handles=["01J..."]'
agent list include_closed=false
agent list 'handles=["01J..."]'
agent inspect 01J... limit=6 before_seq=42
agent send handle=01J... mode=steer message=-
agent send handle=01J... mode=followup message=-
agent stop 01J...
agent close 01J...
```

`agent wait` returns as soon as any selected handle has a result or status change,
while its snapshot may still show other selected handles running. An omitted
or empty `handles` list means all visible handles. `agent list`
returns all child agents owned by the current run plus current-process tool
jobs, including idle reusable agents and optionally closed ones. When handles
are named, it returns their current snapshots; `include_closed` affects
discovery rather than named lookup.
`before_seq` is exclusive and optional; `agent inspect` returns
`next_before_seq` when older messages exist.

## Skills

Fiasco discovers Agent Skills from lowest to highest precedence:

1. `$HOME/.agents/skills/*/SKILL.md`
2. `<workspace>/.agents/skills/*/SKILL.md`
3. `<workspace>/skills/*/SKILL.md`

Only skill name and description enter the stable prompt prefix. `fiasco`
command `skill load <name>` returns the instruction body without repeating that
metadata, plus the absolute skill-directory path needed to resolve referenced
files.

```bash
fiasco skills list
```

## MCP Artifacts

Each configured MCP source has a model-generated namespace and progressive
artifact containing `MCP.md`, an exact captured `catalog.json`, and optional
capability references. Only the namespace, short description, and absolute
`MCP.md` path enter the runtime reminder. The model reads that source map and
the relevant reference before using the `fiasco` MCP route:

```text
mcp github search_code query=McpRuntime language=rust limit=20
```

The provider sees only the general `fiasco` schema. Its MCP route resolves the
next two tokens as namespace and exact remote tool name, converts `name=value`
arguments using the captured input schema, and calls the configured stdio
server. A tool with one input property also accepts one positional value.

Configure the transport separately from the artifact:

```toml
[mcp.github]
artifact = ".agents/mcp/github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcp.github.env]
GITHUB_TOKEN = "${GITHUB_TOKEN}"
```

Authoring and validation use the same loader, compiler, client, and result
renderer as the runtime:

```bash
fiasco mcp capture github
fiasco mcp check github
fiasco mcp check github --live
fiasco mcp compile "github search_code query='McpRuntime'"
fiasco mcp call "github search_code query='McpRuntime'"
```

Install the repository's `register-mcp` Skill to have an agent capture,
document, explore, and validate an arbitrary configured stdio MCP:

```bash
npx skills add siriusctrl/fiasco \
  --skill register-mcp \
  --agent universal
```

The Skill organizes source maps around user-facing capabilities and encourages
grouping commands that share objects, identifiers, or workflows. It keeps the
raw per-tool catalog exact and outside model context.

## Multi-Agent Orchestration

`fiasco` command `agent start` asynchronously starts the sole model-facing
`general-task` role as a reusable child agent;
there is no model-facing profile choice. The runtime reminder states the exact
remaining delegation depth. With the default `max_subagent_depth = 1`, the
first child has
zero remaining depth; `agent start` stays available there but fails locally. A
child is another invocation of the same runner, not a second agent class. Each
child:

- invokes the same `AgentRunner` and provider
- has a separate run id, transcript, events, and artifacts
- records its parent run id
- shares the working project, so it can inspect and modify the same files
- uses the configured GeneralTask model and output profile
- cannot delegate another child at the default depth limit

Parent and child model requests share `runtime.max_parallel_model_calls`, which
defaults to one for compatibility with rate-limited endpoints. Delegated-child
capacity remains independently controlled by `runtime.max_parallel_subagents`.

Every `agent start` command starts an isolated child with only its runtime
reminder and delegated prompt. The prompt must include the complete objective
and any task-specific context; the child does not inherit the parent
conversation. A completed activity leaves that child idle. `agent send`
resumes the same child with an ordinary user message; `mode=followup` queues
that message without blocking the parent, and a stopped agent stays paused
until its next `agent send`. `agent close` is the explicit end of the agent's
lifetime. It cancels and waits for current activity when needed, rejects new
input once closing begins, and discards any still-queued followups. Its
trajectory is stored in the child run, so reuse, resume, and history retrieval
do not depend on a live parent process.

`runtime.model_stream_idle_timeout_seconds` (default 300) stops a model stream
that produces no valid SSE event for that interval, while
`runtime.model_request_deadline_seconds` (default 3600) caps the complete model
API call even when the stream keeps making progress. Neither limit includes
tool execution or time spent waiting for the shared model slot.

Only child activity results return to the parent context; full child
transcripts remain in their own run directories. There is no parent-side
persistent coordination record or recovery state machine. On restart,
`agent list` discovers direct child runs by parent id and presents every open
thread as idle without launching it. The first explicit `agent send` reuses
the child's complete transcript after adding a crash reminder. Closed children
stay closed. Tool jobs and undelivered activity outputs from the previous
process are gone; the parent crash reminder leaves any retry decision to the
model.

The parent can inspect a child's latest messages (six by default), page
backward by sequence, and queue steering while it runs. Steering stays in the
process-local mailbox until it is appended as an ordinary user message after
the child's current assistant response and full tool-call batch, immediately
before its next model request. It does not interrupt or discard in-flight
tools.

## Long-Term Memory

Memory is durable knowledge about the user and projects, not the current
conversation. It is ordinary Markdown at two locations which are included in
an ordinary agent's initial runtime reminder:

- `$FIASCO_HOME/memory/user/` for cross-project user knowledge
- `<workspace>/.fiasco/memory/project/` for project-specific knowledge

There are no special memory tools. The model uses `read`, `write`, and `bash`
for small focused changes. For a large independent update, it can delegate an
ordinary `general-task` child, continue useful work, and reconcile the child
result before finishing.

```bash
fiasco memory consolidate
```

Use an external scheduler instead of embedding cron into the harness:

```cron
15 3 * * * /usr/local/bin/fiasco --workspace /workspace/project memory consolidate
```

See [memory.md](docs/memory.md).

## Architecture

```text
CLI/job
  -> AgentRunner
     -> ModelProvider
     -> provider ToolRegistry
        -> bash / read / write
        -> fiasco command adapter
           -> hidden command ToolRegistry
              -> skills / web / history / MCP
              -> RuntimeHandleManager
                 -> promoted direct Tool future
                 -> delegated child AgentRunner
     -> ArtifactStore
     -> RunDirStore
     -> EventSink
```

Provider wire formats never enter the loop. Hidden command adapters use the
same `Tool` contract as native adapters, and the MCP adapter routes through its
artifact registry into remote clients. Subagents use the same runner. Large
results use the same artifact contract regardless of source.

Read [architecture.md](docs/architecture.md) and
[design-choices.md](docs/design-choices.md) for the detailed boundaries and
tradeoffs.

## Development

```bash
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Run a complete local smoke task:

```bash
tmp=$(mktemp -d)
target/debug/fiasco --workspace "$tmp" run --output ndjson "smoke"
find "$tmp/.fiasco/runs" -maxdepth 3 -type f -print
```
