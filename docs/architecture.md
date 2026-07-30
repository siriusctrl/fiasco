# Architecture

Fiasco is a headless Rust orchestrator for multiple agents and background jobs.
Its architecture favors a small, inspectable execution core over a UI or
platform framework.

## Runtime Flow

```text
job/CLI
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
        -> compacted-state metadata / compacted-history reader
     -> EventSink
```

`AgentRunner` is the only model/tool loop. It calls a provider, executes the
returned tool calls, appends complete tool results, and repeats until the model
returns a final answer with no unresolved background work. There is no model
step limit; provider stream-idle timeouts, request deadlines, and explicit
cancellation remain real failure boundaries.

## Core Boundaries

### Model provider

`ModelProvider` translates the canonical message and tool shapes to one wire
protocol. OAuth, API keys, SSE parsing, provider errors, and provider-specific
cache hints stay behind this boundary. The runner owns the non-resetting model
request deadline; streaming providers apply the per-call idle interval while
opening the HTTP request through its response headers and before each next valid
SSE event. A protocol-signalled structurally incomplete normal response is
discarded and retried once as a fresh request with an ephemeral tail reminder;
ordinary transport and provider errors remain terminal for that run.

Initial adapters:

- OpenAI OAuth
- OpenAI-compatible Responses or Chat Completions
- Anthropic-compatible Messages
- deterministic echo for tests and smoke runs

Canonical user content can include image attachments. Adapters project those
to native Chat `image_url`, Responses `input_image`, or Anthropic base64 source
blocks; the agent loop does not assemble provider wire shapes.

### Tool and command registries

Every executable capability still implements `Tool`, but normal runs assemble
two registries. The provider registry contains the native `bash`, `read`,
`write`, and `fiasco` schemas. A hidden run-scoped command registry contains
skills, optional web search, history, delegation, handle controls, and optional
MCP. Explicitly injected application tools may remain provider-visible.

Both registries cache each adapter's spec at registration, remain sorted, and
reject duplicate names. Only the provider registry is frozen into the model
request and resume hash. Root, GeneralTask, and compaction calls therefore see
one deterministic provider schema set even when the internal capability
catalog grows.

`fiasco` is a syntax and composition adapter, not a second planner or agent
loop. It resolves a fixed enabled route table, compiles CLI-like arguments
against each internal tool's JSON schema, and calls that `Tool` in the owning
run process. Its grammar supports quoted words, a linear buffered fail-fast
pipeline, exact stdin substitution, and one terminal atomic redirect:

```text
history search result-old
history search result-old | agent send handle=01J... mode=followup message=-
history read m37 before=2 after=2 > recovered.jsonl
```

It is not a shell: `&&`, `||`, append, variables, globbing, command
substitution, loops, background jobs, and file-descriptor redirection are not
implemented. The native `bash` tool remains available when real shell semantics
are required. Pipeline stages are sequential and pass complete pre-artifact
output. UTF-8 is required when output becomes a typed argument or file, and
native image attachments cannot enter a pipeline.

The complete command is one provider tool call. Hooks, runtime events,
foreground promotion, cancellation, transcript identity, result limiting, and
artifact preservation apply to the outer `fiasco` call; hidden stages do not
emit separate assistant calls or envelopes. A terminal redirect reuses the
native `write` contract and occurs only after all stages succeed. Earlier stage
side effects are not rolled back after a later failure.

Every local adapter keeps its typed compile-time `tool.yaml` beside its Rust
module, including hidden command adapters. Standalone tools live directly under
`src/tools/<tool>/`; cohesive handle and history families live under
`src/tools/<family>/<member>/`. The manifest name is the internal deterministic
lookup name; paths never derive names. The common loader validates both prose
fields and joins them with a `Returns:` semantic boundary. `fiasco help`
derives its enabled catalog and route-specific help from the same specs, so the
Rust module continues to own arguments, semantic validation, and execution.
The base `bash` adapter uses a non-login shell and inherits the fiasco
process environment, avoiding per-call profile output and PATH rewrites.
The base `read` adapter returns up to 400 text lines under a 65,536-byte cap.
When the byte cap lands inside a multi-line range, it backs up to the newest
complete line and returns an exact continuation offset. Supported images are
artifacted and carried separately as canonical model attachments.
The loader rejects unknown manifest fields, empty or padded prose, and
non-object input schemas. Domain engines remain separate: process-local
execution coordination is owned by `RuntimeHandleManager`, skills by
`SkillRegistry`, and trajectory retrieval by
`TrajectoryReader`. MCP artifact loading, command compilation, client
lifecycle, and the thin command adapter remain in `mcp.rs` and `src/mcp/`.

`build_app_tools` assembles process-wide capabilities. `RunToolAssembly` is the
single path that moves Fiasco-owned command adapters out of the provider
registry, adds run-scoped history, delegation, and handle controls, and
registers one `fiasco` adapter for every Root and GeneralTask run. The `history`
and `handle` family modules explicitly register their complete member sets.
Only an unfinished outer provider call receives a runtime handle through
foreground promotion.

The persisted Root and GeneralTask profiles have one identical command
capability set. History is available before the first call whether automatic
compaction is configured or not. Remaining delegation depth is persisted,
shown in the runtime reminder, and checked by the internal `delegate` adapter
before child creation; zero returns a local error. Optional web search and MCP
add routes to the `fiasco` description before the run starts. Memory paths add
no schema or command. Neither provider schemas nor enabled routes change during
one run.

### MCP artifacts

An MCP configuration entry binds a namespace to a stdio transport and an
artifact directory. `MCP.md` supplies model-generated name and description
metadata plus a capability-oriented source map. `catalog.json` is the exact
captured `tools/list` array and remains outside model context. Detailed
Markdown may group highly related commands around shared objects and workflows;
the runtime never interprets that grouping.

Startup loads every configured artifact and connects its server, then registers
one fixed internal MCP adapter behind the `fiasco` `mcp` route. The initial
runtime reminder lists only each namespace, description, and absolute
source-map path. The model uses ordinary `read` calls for progressive
documentation and invokes:

```text
mcp <namespace> <remote-tool> [name=value ...]
```

The shared compiler applies shell quoting, resolves the captured exact remote
tool name, and converts top-level values using its input schema. `mcp compile`,
`mcp call`, and the internal command adapter use this same implementation. Text MCP
content returns directly without a harness JSON envelope; structured-only data
returns as its JSON value, while rich non-text results retain the exact MCP
result shape for artifact handling.

`mcp capture` is the only writer of `catalog.json`. `mcp check --live` compares
the artifact with the current server explicitly; normal startup does not
refresh, hash, regenerate, or ask a model to repair artifacts. A changed
artifact is simply observed by a later process.

The provider config declares one capability set for the selected model rather
than maintaining a model-name registry or probing endpoints. The stable system
prompt defines absent modalities as unavailable; the initial runtime reminder
records the concrete set as `current model supported modalities: [...]`.
`read` receives the corresponding image-enabled flag during app-tool assembly
and returns a normal tool error for an image under a text-only configuration.
Its static YAML schema and description do not change.

### Run storage

Each run is a portable directory beneath `<workspace>/.fiasco/runs/<run-id>/`.
It contains run metadata, append-only complete messages, structured events, the
final answer, and artifacts. It may also contain
`graphs/g<N>.yaml` files whose nodes represent durable work-item topology and
main-agent-accepted outcomes. This is what persistence means in the launch
runtime:
a cloud worker can retain or inspect a job without a database.

The durable message contract is `fiasco-message`. Every `messages.jsonl` line is
self-contained: `ref`, `created_at`, `role`, the exact typed provider-neutral
`content` blocks used by the runner, and optional assistant
`reasoning_content`. Tool-error state, structured artifact refs, images,
runtime-handle results, and opaque provider continuation items therefore need
no second representation or reconstruction layout. Compatible Chat reasoning
uses the sibling field because that is the exact replayable payload shape;
Responses reasoning remains an ordered opaque item whose provider is already
fixed by the run. Optional compaction state lives under `_fiasco` on the same
record. The one-based sequence is derived from the required `m<N>` ref and line
position rather than stored again.

Assistant function-call `arguments` remain the exact provider string in this
log, including malformed JSON. Parsing occurs only at the individual tool
execution boundary. An invalid call receives an ordered `is_error` result while
valid sibling calls from the same assistant message continue normally.

Image attachments remain typed base64 content blocks in the log. Provider
adapters project text and images to their native wire shapes only when building
a request.

`run.json` identifies the format as `fiasco-message` and records the model
modality declaration, persisted profile, and remaining delegation depth. Resume
requires the current model declaration to match and restores delegation
authority from that run snapshot rather than current depth configuration.

Only complete messages enter the durable transcript. Stream deltas are emitted
to live sinks but omitted from the persisted `events.jsonl` and are never
appended as partial conversation messages.

The one process holding the run execution lease is the sole writer. A newline
immediately exposes its message to lock-free readers, including a possible
prefix of the final assistant/tool batch. A torn physical line remains hidden.
Before an existing run starts another activity, the writer trims that line and
discards a trailing assistant tool call unless every result exists in original
call order. Malformed complete records, out-of-sequence refs, and impossible
result order fail loading. The current pre-release format intentionally does
not load older run-record versions.

One small line decoder validates physical records from any known sequence and
byte offset and preserves their exact raw representation. The async wrapper
retains a torn final line across EOF and streams forward without reading the
whole file. Trajectory loading exposes every complete line; writer recovery
performs the separate trailing tool-call/result check.

### Transcript inspection

`fiasco inspect` is a read-only adapter over the same durable boundary:

```text
inspect command
  -> TranscriptTimeline (Fiasco run routing/terminal state)
     -> fmtview-core::FileRecordTimeline (newlines/tail paging/follow refresh)
        -> fmtview (search/navigation/render/terminal lifecycle)
```

The command is selected before application config and provider composition.
Summary and exact NDJSON paths therefore need only the run directory, and the
interactive path constructs no provider, MCP client, hooks, tools, or runner.
Fiasco uses the released `fmtview` facade for embedding and the same-version
`fmtview-core` `FileRecordTimeline` for the physical growing-file source. It
does not own ratatui/crossterm behavior.

The generic file timeline opens at the last complete line without indexing the
whole history and loads individual older and newer records within bounded
budgets. It hides a torn physical tail and owns refresh/reset behavior for a
growing or replaced file. The Fiasco adapter does not parse message refs,
validate provider conversation semantics, or maintain a second physical-file
state machine. Those checks remain on the writer/resume trajectory path. An
inspector encountering an unusual concurrent rewrite may be reopened;
inspection is observational and is not a recovery authority.

Open runs report a live boundary; completed and closed runs report a terminal
boundary. Process-local activity status and failure events do not alter this
durable lifetime.

Interactive snapshot and follow modes are TTY-only. Redirected stdout defaults
to exact committed NDJSON, preserving every raw record byte and LF without
reserialization; `--summary` retains the metadata/final-output view. Events,
child-run trees, and handle controls are outside this transcript source.

The message file is created and directory-synced with the run. The writer's
cached next sequence is invalidated before cancellable I/O and restored only
after a complete record has synced. Multiple independent writers for one run
are outside the storage contract; the execution lease prevents that state in
the runtime.

The persisted run lifetime is intentionally coarse:

```text
new root/child -> open
open root      -> completed  # successful final result
open child     -> closed     # explicit close
```

The execution lease identifies an active writer. Lifecycle events record
activity and root failures without turning them into another durable state.
An open root can be resumed explicitly when it is not already leased.

The loop injects newly completed runtime-handle results, optionally compacts an
old completed-message prefix, and requests model output. A final assistant
message appends alone. After a tool batch settles, the assistant, ordered
direct-tool results, and optional attachment are written in one ordered append;
their complete lines may become visible independently.

Handle state is process-local execution coordination. A promoted tool uses a
`j_<ulid>` handle until that process ends. A delegated agent uses its child run
id as the handle; only the child transcript, display name, parent id, and
open/closed lifetime are durable. Current activities, followups, pending output,
and tool handles are not reconstructed after a crash.

A status-less `<runtime_handle>` in a tool result acknowledges asynchronous
execution in the original provider `tool_call_id` slot. Later output is one
user/runtime content block correlated by handle, never a second tool result
with the same provider call id. One message batches all outputs ready at that
boundary. Each body follows the ordinary independent
inline/preview/artifact policy; the XML status wrapper is added and escaped
afterward.

Before every normal provider request, the runner snapshots current-process
active handles in stable handle order. The synthetic reminder tells the model
not to delegate work already represented there and is never persisted. It is
refreshed after compaction so newly completed work is delivered before the next
normal request.

The full run holds a filesystem execution lease. Resume rebuilds the recorded
profile, validates provider/model/workspace identity, repairs the message tail,
and continues from there. If the final assistant requested tools and its
ordered result sequence is incomplete, the assistant and existing result prefix
are discarded. A compaction request without a following state remains inert.
The root receives a warning about possible workspace or external side effects;
the call is never automatically replayed. Root restart reports that the prior
process, its mailbox input, and its asynchronous work stopped.

### Artifact storage

Large foreground tool outputs are never discarded and do not enter the live
context in full. The store writes the complete bytes and gives the model a
bounded beginning/end preview plus a relative run-local attachment path. The
path and media type are persisted with the result; later reads observe the
file's current contents without rewriting the generation-time preview.
Terminal background output uses the same policy:
small UTF-8 output stays inline and larger or binary output keeps the bounded
artifact envelope. Each result is limited independently; earlier output and
compaction do not change later representation. See
[artifacts.md](artifacts.md).

For immediate image reads, the runner appends every tool result from the batch
in assistant call order followed by one user attachment message. This keeps
native tool-call/result adjacency valid while still allowing several
concurrently read images to share one model input message.

### Context compaction and trajectory retrieval

Local compaction changes the active-context projection without rewriting prior
messages. `messages.jsonl` retains every committed completed message with a
stable `m<N>` ref whose number is its sequence, including the successful
compaction user instruction and exact assistant compacted state. Their `_fiasco`
state marks the pair and stores the covered prefix and first exact message kept.
A normal active
request excludes compaction instructions and older compacted states; it contains
the initial runtime message, newest exact assistant state, a short synthetic
user runtime reminder that identifies the state as context rather than a final
answer, and the exact recent ordinary suffix. The reminder exists only at an
active compaction boundary and is not appended to the trajectory.

The trigger uses a provider-neutral request estimate from the first call and
adopts provider-reported input usage whenever available. Configuring
`compact_at_tokens` controls compacted-state creation only; the normal system
prompt, `fiasco` schema, and history command routes are already present and
remain unchanged. The
additional request uses the same provider, model, system, and frozen schemas,
then appends one compaction user instruction. A returned tool call or empty
state is never executed or committed; fiasco records the invalid attempt and
retries that compaction request once.
Fiasco does not implement provider/server-side compaction.

The `history search` and `history read` command routes expose a read-only
`TrajectoryReader` boundary. The local implementation searches only messages
outside the active context, plus full textual artifacts linked to their tool
results. Search uses Rust regular expressions, returns newest matches up to a
configured cap, and has no cursor. Each match returns a sequence-addressed ref,
a `source` that distinguishes inline message content from a linked complete
artifact, and a bounded snippet. Read accepts that ref and a bounded
before/after window, returning chronological Chat-compatible JSONL and
expanding when necessary to preserve tool-call/result pairs. A future remote or
database-backed trajectory can implement the same reader without granting the
model filesystem write access.

For linked local artifacts, the query reads the structured `ArtifactRef` from
the completed message content. It does not parse the
model-facing preview prose or guess from a call id. The reader canonicalizes
the path, requires it to remain a regular file inside the current run's artifact
directory, reads its current filesystem length, and invokes `rg` with bounded
output. It stops after the requested newest matches plus one, avoiding
whole-artifact heap loads and unnecessary older scans.

The launch local message source still materializes one run's trajectory JSONL
per history query. Artifact contents remain streamed and bounded. If run sizes
outgrow this simple backend, an indexed local or remote `TrajectoryReader` can
replace it without changing the model-facing tools.

A normal profile compacts only when both history command adapters and at least
one generic artifact inspection tool (`read` or `bash`) remain available,
preserving exact recovery as part of the compaction contract.

### Skills and instructions

The system prompt contains only stable, tool-agnostic instructions loaded from
the typed `prompts/agents.yaml` registry. YAML folded scalars remove source-only
line wrapping before Rust receives each value. `src/prompts.rs` parses the
embedded registry once and rejects unknown or empty fields. Rust owns prompt
precedence, section ordering, dynamic values, and runtime-reminder framing.
Each typed tool manifest owns its feature workflow. Ablating a command adapter
also removes its route and catalog guidance. Concrete run state and feature
availability may still appear in dynamic reminders and results. The first user message's
ordinary text `content` carries a
`<runtime-reminder>` block with model/workspace state, the workspace
`AGENTS.md` or lowercase fallback, sorted skill metadata, configured MCP
source-map metadata, memory paths, and optional delegated instructions,
followed by the original request. A skill body enters the
conversation only after the model invokes `fiasco` with `skill load`. That
result omits the
already-catalogued name and description, and includes the absolute skill
directory so relative references remain resolvable. The `SKILL.md` entry path
is implied by that directory and is not repeated.

The repository ships `skills/orchestrate-with-graphs/` as optional procedural
guidance. It uses ordinary file tools to maintain workspace YAML under
`.agents/graphs/`; no graph parser, validator, scheduler, or storage subsystem
exists in the runtime. This keeps the mental model independently installable
and ablatable. A future remote graph service may add an access capability
without coupling graph semantics to agent execution.

The repository also ships `skills/register-mcp/`. It has an agent capture the
exact remote catalog, generate a compact namespace and capability-oriented
source map, aggregate highly related commands into progressive references, and
exercise the runtime's own check, compile, and call paths before accepting the
artifact.

### Memory

Memory is durable knowledge about the user and projects. An ordinary agent's
runtime reminder exposes two Markdown locations. The normal `read`, `write`,
and `bash` tools inspect and update them. A large independent consolidation can
use the same durable GeneralTask child mechanism as other delegated work; an
external cron or job scheduler can invoke the convenience consolidation
command. See [memory.md](memory.md).

### Agent orchestration

A subagent is a child invocation of the same `AgentRunner`. It has its own run
directory and transcript, a `parent_run_id`, and exact remaining delegation
depth. The launch runtime runs children in-process, shares the parent workspace
and base tools, and decrements that remaining capacity for each child.
“Shared workspace” means parent and child operate on the same working project
files; it is not a special second workspace abstraction. Child
transcripts stay out of the parent context; only bounded, sequenced activity
results and artifact references return to the parent.

All direct calls returned in one assistant message start concurrently and share
one foreground window. The runner returns as soon as all settle. At the
configured deadline, the same in-flight futures for only unfinished calls move
under process-local runtime handles without stopping or restarting. The runner resumes and
tracks every unfinished future before awaiting promotion events, then commits
tool-result messages in original call order with their original provider call
ids; events may show actual completion order. A promoted handle is only a
running acknowledgement, so dependent work waits for the separate result
message correlated by that handle.

`agent start` starts a reusable GeneralTask agent immediately and returns its
child run id as the handle. `agent list` discovers durable direct children and
overlays current-process tool jobs and activity state. With named handles it
returns their current snapshots; `include_closed` extends all-handle
discovery. `agent wait` uses wait-any semantics: it returns when any selected
handle has a result or status change, or when its bounded interval expires.
`agent inspect`, `agent send`, and `agent close` operate only on agents. Inspect
projects a bounded page of the child's durable messages. Send queues a normal
user message with an explicit mode: `steer` makes it available after the
current complete tool batch, while `followup` waits for the current activity
boundary. An activity result leaves the thread idle; `agent stop` ends only
current work, while `agent close` rejects new input, cancels and joins current
work when necessary, clears queued input, and then durably closes the thread.
Asynchronous work has no hard execution deadline.

Every delegated child is isolated. It starts from its own runtime reminder and
the delegated prompt, which must contain the complete objective and any
task-specific context. The parent conversation, compaction state, and artifact
references are not copied. A child uses the configured GeneralTask model and
resumes solely from its own run messages through the same `AgentRunner` path.

There is no durable parent-side handle index, activity record, or pending-input
log. On root restart, tool jobs, followups, active work, mailbox input, and
undelivered output from the previous process are discarded. The root receives
an unconditional crash reminder and decides what to retry. `agent list`
finds child runs whose `parent_run_id` matches the current run without launching
them. The first explicit `agent send` to an old open child adds a child crash
reminder and starts a fresh activity from that child's complete transcript.

This recovery path assumes the runtime supervisor, cgroup, or container killed
the previous fiasco process and all locally managed descendants before
resume. A stale busy lease fails immediately. Remote work and external side
effects can survive and must be inspected after the restart reminder.

The durable child guarantee belongs to `agent start` child runs, and the parent
run is the only resume entrypoint. Memory consolidation
uses this same path rather than a special direct-tool child.

## Prompt And Cache Shape

Agent and compaction calls use one invariant, tool-agnostic built-in system
prompt and one sorted, frozen tool-schema set. The `fiasco` schema and history
routes are included from the first call; automatic compaction never mutates
this prefix. Feature-specific workflow stays with the native schema or internal
command manifest instead of the shared system prose. Project instructions,
skill metadata, memory paths, and stable GeneralTask guidance form a
deterministic runtime reminder at the start of each run. The delegated task
text follows as ordinary user content; the persisted initial message freezes
both. Optional startup routes are selected before the run starts. Agent role
and remaining delegation depth change only the initial runtime-reminder tail.
Current-process active-handle state is a non-durable synthetic reminder in
normal requests, while a compaction request changes only the message tail.

The durable trajectory remains append-only; before a normal model call, an
optional assistant compacted-state message can replace its older active prefix
while retaining the exact recent suffix. Large outputs become mutable run-local
attachments with bounded generation-time previews. These choices bound request
growth while keeping current attachment evidence inspectable and making
provider KV-cache reuse possible without making cache behavior part of the core
API.

### Hooks

Command hooks observe `run_start`, `run_end`, `tool_before`, and `tool_after`.
They receive JSON over stdin and inherit the host process permissions. For a
root, `run_end` follows terminal completion; for a reusable child it follows
each completed activity after the child becomes idle, not explicit close.
Hooks do not define a second execution path.

## Runtime And Inspector Surfaces

The execution path emits NDJSON runtime events for machines and a compact final
result for humans. The separate read-only inspect path can embed fmtview's TUI,
but no terminal state enters `AgentRunner` or storage contracts. A future API or
web client should consume the same runtime events and run artifacts rather than
introduce model logic in the transport.

## Deliberate Launch Omissions

- OS sandbox and interactive approvals
- browser frontend or a second transcript rendering stack
- built-in scheduler
- vector search
- provider/server-side compaction
- database-backed run indexing
- native dynamic plugins
- distributed subagents

These omissions reduce launch complexity. Existing boundaries allow them to be
added without creating another agent loop.
