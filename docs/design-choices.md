# Design Choices

## Internal Orchestrator First

Fiasco currently has no external users or compatibility commitments. It is an
internal multi-agent orchestrator for its maintainers' own workflows, so
operator convenience, code readability, and fast iteration take priority over
broad configurability, backward compatibility, and abstractions for
hypothetical consumers.

Rejected: carrying compatibility layers, packaging indirection, or generalized
extension points without a concrete internal need. Revisit this boundary only
when an actual external consumer or distribution requirement exists.

## One Rust Runner

Main tasks and subagents use one `AgentRunner`. Child runs differ only by
parent id, remaining delegation capacity, task instructions in their initial
message, and their own persisted run directory.

Rejected: a separate orchestrator agent type or child-specific model loop. That
would duplicate tool, provider, streaming, and persistence behavior.

## Two Model Timeout Scopes

Each provider call has a resettable stream-idle timeout and a non-resetting hard
deadline. Valid SSE events reset the idle interval even when they carry
reasoning, tool arguments, usage, or protocol state rather than visible text.
The hard deadline still bounds a connection that makes negligible progress
forever. Tool execution and model-slot queueing are outside both scopes.

Rejected: one hard timeout for the entire stream, because it kills a long but
healthy reasoning response; and an idle timeout alone, because heartbeat or
trickle traffic could retain a model slot indefinitely. See
[ADR 0011](adr/0011-model-stream-idle-timeout-and-request-deadline.md).

## Headless Runtime, Separate Inspector

The agent runtime emits structured events and portable artifacts without UI
state. The `inspect` command is a separate read-only adapter that embeds
fmtview's terminal frontend over a backend-neutral message timeline. Fiasco does
not own terminal rendering, viewport, search, or event-loop behavior.

Rejected: preserving the legacy Bun/Hono/Ink surfaces. A future service can wrap
the Rust library and event stream without owning agent behavior. Also rejected:
duplicating ratatui/crossterm logic inside the orchestration runtime.

## File-Based Runs

The launch runtime uses one self-contained directory per run instead of SQLite
or an event-sourced service. Complete messages and metadata are enough for
inspection and form the boundary for the bounded `fiasco resume` command; object
storage can archive the directory as a unit.

The persisted event log contains lifecycle and debugging records, not streaming
text or reasoning chunks. Complete messages are the searchable trajectory;
live event sinks carry transient deltas for interactive consumers.

Within a run, completed messages use the short ref `m<N>`, where `N` is the
durable one-based sequence. This gives history tools an explicit age/order
signal without exposing a separate sequence field or asking the model to parse
opaque ULIDs.

See [ADR 0001](adr/0001-durable-messages-transient-stream-deltas.md) for the
decision context and rejected alternatives.

Revisit when cross-run queries, multi-worker ownership, or server-side pagination
become concrete requirements.

## Minimal Tail Repair

Every newline-complete message is durable and visible. Before starting another
activity on an existing run, Fiasco removes a torn final record and checks only
the trailing assistant/tool exchange. If ordered results are missing, it drops
that assistant and the result prefix. A compaction request without a state is
inert. Resume injects one user/runtime warning about possible side effects
instead of synthesizing tool errors or replaying the discarded turn.

Restart does not reconstruct process-local handle state. Tool jobs, active child
work, mailbox input, and undelivered output are discarded; the root receives an
unconditional crash notice and decides what to retry. Direct child runs remain
discoverable as durable open threads. Their first explicit `agent send`
adds a child crash reminder and starts a new activity from the remaining
context. This assumes a supervisor has killed the old local process tree before
resume. See
[ADR 0044](adr/0044-newline-visible-messages-and-tail-repair.md) and
[ADR 0038](adr/0038-runtime-handles-and-explicit-restart.md).

## Self-Contained Message Log

`messages.jsonl` stores the runner's provider-neutral message directly. Each
model-readable record contains `ref`, `created_at`, `role`, typed `content`, and
optional assistant `reasoning_content`; rare local lifecycle classification
uses optional `_fiasco`. Compatible Chat reasoning keeps its upstream sibling
shape and exact text, while Responses reasoning remains an ordered opaque
provider item. There is no metadata sidecar, reconstruction layout, or second
transcript representation.

One execution lease gives a run exactly one writer while allowing any number
of lock-free viewers. A newline makes that record visible immediately. A viewer
may briefly show a prefix of the final tool turn; only a torn physical line
stays hidden. Semantic cleanup runs once before the next writer activity rather
than complicating the read-only viewer.

Rejected: a two-file commit protocol, a provider-specific durable wire format,
duplicated message bodies, and compatibility code for unreleased runs. See
[ADR 0032](adr/0032-self-contained-message-log.md),
[ADR 0042](adr/0042-chat-reasoning-sibling-field.md), and
[ADR 0044](adr/0044-newline-visible-messages-and-tail-repair.md).

## Append-Only Local Compaction

Compaction reduces the active model request without changing the raw evidence:
complete messages remain append-only, and successful compaction instructions
and assistant states are recorded in the same self-contained message log. Exact
compacted details remain available
through read-only regex search and ref-centered reads; the retrieval interface
can be backed by local files or a future remote store.

Rejected: destructive transcript rewriting, cursored pagination in the initial
tool contract, vector retrieval without a demonstrated need, and relying on a
provider-specific server-side compaction API. See
[ADR 0012](adr/0012-record-compaction-as-messages.md).

## Tool-Local Workflow Guidance

The stable system prompt contains only behavior that remains valid with any
tool subset. Each `tool.yaml` is the authority for its capability's purpose,
workflow, arguments, and results; references to optional sibling capabilities
stay generic. Dynamic run facts such as remaining delegation depth and active
handle state stay in runtime reminders or results. This lets tool ablations
remove both a schema and its prompting influence.

Rejected: hard-coded feature prose in the shared system prompt, duplicating
tool schemas there, and appending harness-validation steps to ordinary user
tasks. See [ADR 0039](adr/0039-keep-feature-guidance-with-capabilities.md).

## Uniform Background Delivery

Delegated children and promoted tools use one runtime notice shape. A
status-less handle block means work is running; a terminal block adds status
around the same independently bounded inline-or-artifact result used by
foreground tools. Ready handle results share one runtime message, and internal
kind or provider call ids stay out of model-facing XML. Payload limiting happens
before the runtime wrapper, whose structure and instructions are never clipped.
Each normal request also receives a non-durable snapshot of active handles,
names, and states so compaction cannot make the model forget already delegated
work. A post-compaction refresh delivers newly terminal artifacts first; when a
compaction continuation reminder already exists, both dynamic sections share
that one synthetic message.

Rejected: separate start/result protocols, forcing every small result through a
second artifact read, one runtime message per ready handle set, and persisting
repeated active-state snapshots. See
[ADR 0030](adr/0030-uniform-foreground-and-background-results.md).

## Recover Model Output At Narrow Boundaries

OpenAI-compatible tool arguments remain exact provider strings in durable
messages and are parsed only when that one tool executes. Malformed JSON becomes
an ordered error result for its own call, so valid siblings still run and the
model can correct the call on its next turn. Separately, a provider-signalled
structurally incomplete assistant response gets one fresh repair request with
an ephemeral tail reminder; partial content is neither persisted nor executed,
and provider-reported usage for that discarded attempt stays observable.

Rejected: parsing tool arguments while assembling the whole assistant response,
blindly retrying transport errors, and persisting partial assistant text. See
[ADR 0029](adr/0029-recover-incomplete-model-output.md).

## Artifact-First Tool Output

Large results are preserved in full but represented in model context by a small
envelope. Its path and media type identify a mutable attachment in the current
run; later reads observe current file contents while the message keeps its
generation-time preview. This was chosen over destructive truncation and over
placing unbounded stdout in every subsequent model request. Each result is
limited independently; fiasco does not retain a cumulative preview budget
across a run because compaction can free context and later small results should
remain directly readable. See [ADR 0018](adr/0018-limit-tool-output-per-result.md)
and [ADR 0046](adr/0046-run-local-mutable-artifacts.md).

## Markdown Memory

Memory is human-editable Markdown outside the transcript. Ordinary `read`,
`write`, and `bash` capabilities inspect and update it. Small changes stay in
the current run; a large independent consolidation uses an ordinary durable
GeneralTask child.

Rejected for launch: a dedicated memory tool or profile, vector databases,
automatic recording of every successful run, Rust-side semantic heuristics,
and making raw transcripts or artifacts equivalent to curated memory. See
[ADR 0009](adr/0009-memory-through-ordinary-tools.md).

## Runtime Handles, Durable Agent Threads

Direct calls from one assistant message start concurrently under one shared
foreground window. Results remain in original call order; only unfinished exact
futures receive process-local `j_<ulid>` handles. `agent start` starts a
reusable GeneralTask whose handle is its durable child run id. `agent list`
handles both discovery and named snapshots; wait-any `agent wait`, `agent
inspect`, mode-required `agent send`, `agent stop`, and `agent close` complete
the control surface. Close also cancels and joins active agent work before
making the durable lifetime closed.
Only agent identity, name, transcript, profile, remaining delegation capacity,
and open/closed lifetime survive a process restart; activity coordination does
not. Agent loops have no arbitrary model-step cap, and asynchronous work has no
hard execution deadline. See
[ADR 0038](adr/0038-runtime-handles-and-explicit-restart.md) and
[ADR 0047](adr/0047-collapse-run-lifetime-and-handle-discovery.md).

## Graphs as an Installable Mental Model

Complex tasks may use the `orchestrate-with-graphs` Agent Skill to maintain a
workspace YAML graph with ordinary file tools. Nodes express outcomes,
questions, and accepted knowledge; the orchestrator decides what to execute and
when. Graph lifecycle is only `open` or `resolved`, and resolution of the whole
graph does not require every exploratory branch to finish.

The runtime has no graph-specific initialization, listing, validation,
scheduling, or recovery path. This keeps optional planning policy independently
installable and makes tool ablation honest. A remote graph service remains a
future access concern rather than a reason to emulate distributed state in
local files. See
[ADR 0048](adr/0048-move-orchestration-graphs-to-a-skill.md).

## Isolated Delegation

Every delegate call starts an isolated GeneralTask child. Its prompt must carry
the complete objective and task-specific context because parent messages,
compaction state, and artifacts are not copied. The child uses the configured
GeneralTask model, owns a normal self-contained trajectory, and resumes through
the same runner as root runs. This keeps parent-child coordination durable
without coupling child execution to parent context projection. See
[ADR 0033](adr/0033-isolate-delegated-context.md).

## Conservative File Mutation

`write` supports complete writes and atomic multi-region replacements. Targets
must be unique and non-overlapping in the original file. A conservative
whole-line indentation fallback handles formatting drift; broad fuzzy or
similarity matching is rejected because a plausible wrong edit is worse than a
clear retry request.

## Direct Host Execution

The launch runtime intentionally executes tools and hooks with the fiasco
process permissions.

Rejected for launch: a partial permission UI that could be mistaken for an OS
sandbox. The `Tool`/runner boundary remains available for a future real runtime
isolation layer.

## Provider Adapters Own Wire Details

The loop understands canonical messages and tool calls only. OAuth refresh,
provider headers, SSE event shapes, and prompt-cache hints stay in provider
modules.

Image reads use one canonical attachment block. Each provider adapter owns its
native multimodal projection, while the message log stores the canonical image
attachment block directly. Direct batches
emit all paired tool results before one attachment message, avoiding ambiguous
interleaving under concurrent completion. See
[ADR 0022](adr/0022-native-image-attachments-after-tool-results.md).

## Stable Prompt Prefix

The built-in system prompt contains only product identity and tool-agnostic
operating rules. Workspace `AGENTS.md`, skill metadata, memory paths, and
stable GeneralTask guidance are snapshotted into a synthetic runtime reminder
at the start of each run; lowercase `agents.md` is used only when `AGENTS.md`
is absent. The delegated task text follows in the same initial user message as
ordinary content. Tool descriptions and feature workflows
remain in native provider schemas or internal command manifests rather than
being duplicated in the system prompt. The `fiasco` schema and core history
routes are present from the first normal call. Root and GeneralTask use the
same provider schema set and freeze it for the run; compaction reuses the same
system and schemas. Remaining delegation depth is persisted runtime state and
never changes schema membership. Optional startup command routes are resolved
before the run starts. Memory paths do not alter the tool schema. See
[ADR 0024](adr/0024-freeze-built-in-schemas-across-agent-roles.md).

The system prompt contains one stable rule for model modalities, while the
runtime reminder snapshots the configured values. The provider config defaults
to text-only and does not guess from model names. `read` rejects image input at
its execution boundary when `image` is absent. Rejected: endpoint probing,
model-name allowlists, and per-agent dynamic vision routing. See
[ADR 0023](adr/0023-declare-model-input-modalities.md).

Rejected for launch: conditionally adding history routes and hot-reloading
project context or command definitions inside a run. Appending revisions would
grow context, while replacing earlier messages would break the durable
transcript boundary and provider prefix-cache reuse. See
[ADR 0004](adr/0004-stable-agent-prefix-and-core-history-tools.md).

## In-Process Command Surface

The normal built-in provider surface is `bash`, `read`, `write`, and one
`fiasco` command adapter. Skills, web search, history, delegation, handle
controls, and MCP remain typed `Tool` implementations but live in a hidden
run-scoped registry. This keeps live handles and sessionful MCP clients in the
owning process while preventing the provider schema set from growing with each
harness capability.

`fiasco` supports quoted words, `name=value`, exact optional stdin, linear
buffered fail-fast `|`, and one terminal atomic `> path`. A complete stage
output can fill exactly one missing required argument or an explicit `-`
placeholder. Ambiguous input, binary/image composition, append, and general
shell syntax are rejected. The native `bash` tool remains the real-shell
escape hatch.

The whole command is one provider call. Hooks, events, promotion, cancellation,
result limiting, and artifacts apply to that outer call; hidden stages do not
create transcript tool pairs. Earlier stage side effects are not transactional.
Rejected for this boundary: child-process Fiasco commands that require server
mode to recover live state, a complete shell parser, per-command output-path
arguments, streaming pipelines, and a general plugin protocol.

See [ADR 0050](adr/0050-in-process-command-surface.md).

## Compile-Time Prompt Assets

Stable agent instructions are folded values in one typed `agents.yaml` registry;
each local tool keeps its static name, folded purpose description, folded return
guidance, and input schema in one typed `tool.yaml` beside its adapter. The
loader joins the two prose fields into the provider's standard description.
These assets are embedded with `include_str!` and parsed strictly. Rust remains
authoritative for prompt assembly, argument validation, and execution.
Every local adapter keeps its complete manifest beside its Rust module,
including adapters reached only through the command surface. Standalone tools
stay directly under `src/tools`; cohesive handle and history adapters are
grouped by family without deriving lookup names from paths. Domain engines
remain in their focused subsystems. Process and run capabilities are assembled
through one explicit path into native provider tools and hidden command tools.

See [ADR 0019](adr/0019-group-related-tool-adapters.md),
[ADR 0016](adr/0016-separate-tool-purpose-and-return-guidance.md),
[ADR 0015](adr/0015-local-tool-yaml-manifests.md),
[ADR 0014](adr/0014-flat-tool-adapters-and-explicit-assembly.md), and
[ADR 0008](adr/0008-typed-agent-prompt-registry.md) for the packaging and
ownership decisions.

## Progressive MCP Artifacts

Configured MCP servers expose one fixed internal MCP adapter through the
`fiasco mcp` route. Their exact remote `tools/list` catalogs stay in
`catalog.json`, while model-generated `MCP.md` files provide compact namespace
metadata and capability-oriented source maps. Detailed references may aggregate
commands that share objects, identifiers, or workflows; the runtime does not
interpret that grouping.

The internal command adapter, authoring CLI, and installable `register-mcp`
Skill all rely on the same Rust artifact loader and command compiler. Rejected:
dynamic per-tool provider schemas, a second list/load knowledge API inside
`mcp`, startup LLM summarization, artifact hashes, hot reload, and automatic
catalog repair.

See [ADR 0049](adr/0049-progressive-mcp-artifacts.md).

## External Scheduling

Memory consolidation is a command. Cron, systemd, Kubernetes, or another job
platform decides when it runs.

Rejected: an embedded scheduler and daemon lifecycle in the launch harness.
