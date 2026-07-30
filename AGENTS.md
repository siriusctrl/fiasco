# AGENTS.md

This file is the operating map for agents working in this repository. Keep
product and user workflow in `README.md`, current tradeoffs in `docs/`,
architectural decisions in `docs/adr/`, and this file focused on navigation,
cross-cutting invariants, verification, and handoff.

## Source Map

- `src/agent/`: the single agent loop, run state, prompt assembly, compaction,
  and runtime-handle supervision.
- `src/model/`: provider-neutral contracts and provider adapters. Wire formats
  and authentication stay here, outside the agent loop.
- `src/tools/`: deterministic native provider tools plus the hidden in-process
  command registry. Each leaf owns a typed compile-time `tool.yaml`; related
  handle and history tools are grouped by family.
- `skills/`: installable Agent Skills whose optional procedural guidance stays
  outside the built-in tool surface.
- `src/storage/`, `src/artifact.rs`, and `src/trajectory/`: self-contained run
  storage, newline-visible transcripts, artifact preservation, and history
  access.
- `prompts/agents.yaml`: typed stable agent instructions. Dynamic prompt
  assembly remains in `src/agent/context.rs`.
- `src/config.rs`, `src/skills/`, `src/mcp.rs`, `src/mcp/`, `src/hooks.rs`,
  `src/memory.rs`, and `src/events.rs`: runtime configuration and integrations.
- `src/cli.rs`: command shape. `src/main.rs`: headless composition root. Runtime
  behavior does not belong in either file.
- `tests/`: cross-module and end-to-end behavior.
- `docs/`: maintained contracts and tradeoff summaries. `docs/adr/`: durable
  decisions, alternatives, and consequences.

See `docs/source-map.md` for the detailed ownership map.

## Core Invariants

- Rust is the only implementation language for the harness. Root and child runs
  use one `AgentRunner`; a subagent is a child run, not a second loop or class.
- Keep provider wire formats and authentication outside the agent loop. Keep one
  deterministic native provider registry plus one deterministic hidden command
  registry; the `fiasco` adapter and internal MCP adapter use the same `Tool`
  contract and cannot silently replace built-ins.
- Execute one assistant tool-call batch concurrently under one shared foreground
  window. Commit results in original call order. Promote only unfinished exact
  futures, without stopping or restarting them.
- A delegated child is isolated and receives its complete objective and
  task-specific context. Its run id is its durable identity; only its transcript,
  parentage, display name, capability profile, and open/closed lifetime survive
  the process. Activity state, followups, pending output, and ordinary-tool
  handles are process-local; do not add a second durable task authority.
- A complete newline is the durable message boundary. Before resuming after a
  process crash, remove a torn final record and discard an incomplete trailing
  assistant/tool exchange by matching ordered call ids. Report the lost
  in-flight work and let the model decide what to inspect or retry. Do not
  reconstruct or relaunch old activities. Open child threads remain inert until
  an explicit message starts a new activity from their remaining transcript.
- Keep `messages.jsonl` as the single self-contained durable conversation source
  in the declared `fiasco-message` shape. Persist exact provider-neutral content
  and exact function-call argument strings; parse arguments only at the tool
  boundary. Do not add a parallel metadata or reconstruction log.
- One execution lease gives a run one writer and any number of lock-free
  viewers. Every complete newline is immediately visible, so a viewer may
  briefly show a prefix of the final tool turn. A torn physical line stays
  hidden; semantic tail repair belongs only to the next writer.
- Preserve every large or binary tool result in full under the run. Apply inline
  and preview limits independently per result. Treat its `ArtifactRef` as a
  mutable run-local attachment path plus media type: later reads observe current
  file contents, while the original message preview remains generation-time
  evidence. Add asynchronous status envelopes only after payload limiting. See
  `docs/artifacts.md`.
- Keep prompt, provider-schema, and command-catalog assembly deterministic and
  frozen per run. Root, child, and compaction requests use the same built-in
  schema set; compaction reuses the normal system prompt and never executes tool
  calls. Stable prose belongs in `prompts/agents.yaml`, static tool descriptions
  in leaf `tool.yaml`, and execution logic in Rust.
- Orchestration graphs are optional workspace files governed by an installable
  skill, not runtime state or a scheduler. Memory is user/project Markdown
  outside the live transcript, not a special execution subsystem. Both are
  manipulated through ordinary tools.
- Fiasco owns run routing and inspect command selection; fmtview-core owns the
  physical growing-file timeline, and fmtview owns rendering, navigation,
  search, and event handling.
- The launch runtime has no security sandbox or approval engine. Tools, hooks,
  and child processes inherit the fiasco process permissions; never imply
  otherwise.

## Change Discipline

- Treat fiasco as an internal harness with no external compatibility promise.
  Optimize for maintainer clarity and fast iteration; do not add compatibility
  layers or generality for hypothetical users.
- Implement only agreed product behavior. Before adding a fallback, validation,
  persistent state, recovery guarantee, or new subsystem beyond the current
  design, discuss the concrete need and tradeoff with the user.
- For rare crashes, prefer one durable message log, minimal trailing-turn
  repair, an explicit crash notice, and model-directed retry over transparent
  continuation machinery whose external side effects cannot be made exactly
  once.
- Keep checks at real external boundaries and states the program can produce.
  Prefer readable modules over speculative frameworks or defensive fallback
  layers. Review ownership before growing a file already near 400 lines.

## Contract References

- Runtime, handles, and restart: `docs/runtime-model.md`,
  `docs/architecture.md`, ADR 0034, ADR 0038, and ADR 0047.
- Messages, tail repair, and inspection: `docs/architecture.md`, ADR 0032,
  ADR 0037, and ADR 0044.
- Artifact storage and result envelopes: `docs/artifacts.md`.
- Prompt and tool assets: `prompts/README.md` and `docs/architecture.md`.
- Configuration, memory, and orchestration graphs: `docs/configuration.md`,
  `docs/memory.md`, `skills/orchestrate-with-graphs/`, and ADR 0048.
- The ADR index in `docs/adr/README.md` is the authority for superseded and
  refined decisions.

## Verification

Run for every code change:

```bash
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

For provider changes, run the mock-server contract tests covering streamed text,
fragmented tool arguments, error responses, and authentication refresh.

For runtime or artifact changes, also run a headless echo smoke and inspect the
run's `messages.jsonl`, `events.jsonl`, final output, and artifact paths/content.

For prompt or tool-description asset changes, verify `cargo package --list`
contains every referenced asset.

## Documentation Routing

- User-visible commands, setup, or supported features: `README.md`.
- Runtime or module boundaries: `docs/architecture.md`.
- Artifacts, memory, configuration, and prompt assets: their matching documents
  under `docs/` or `prompts/`.
- Current tradeoff summaries: `docs/design-choices.md`.
- Significant cross-module decisions or rejected credible alternatives: add or
  supersede an ADR and update its index. Do not silently rewrite an accepted
  decision.

## Review And Handoff

- Report checks actually run and any provider path not exercised with live
  credentials.
- Do not revert unrelated user changes.
- Use Conventional Commits with a body explaining what changed and why.
- Keep generated runs, credentials, and `target/` output out of Git.
