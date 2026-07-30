# Source Map

- `src/agent/runner.rs`: the only model/tool loop.
- `src/agent/runner/lifecycle.rs`: run creation, profiles, and finalization.
- `src/agent/runner/recovery.rs`: resume validation and durable result injection.
- `src/agent/compaction.rs`: local compaction planning, state calls, and
  active-context assembly.
- `src/agent/tool_execution.rs`: shared direct/background ordinary-tool hooks,
  events, foreground promotion, and artifact-backed output persistence.
- `src/agent/types.rs`: runner configuration, request, and result contracts.
- `src/agent/handle.rs`: process-local runtime-handle coordination.
- `src/agent/handle/control.rs`: discovery, status, message routing, stop, and close.
- `src/agent/handle/coordination.rs`: wait-any, active snapshots, and output delivery.
- `src/agent/handle/execution.rs`: promoted-tool and child-activity execution.
- `src/agent/handle/lifecycle.rs`: activity completion, cancellation, and events.
- `src/agent/context.rs`: deterministic prompt framing and dynamic reminder assembly.
- `src/prompts.rs`: typed access to the embedded agent prompt registry.
- `prompts/agents.yaml`: folded agent instructions for every fixed profile.
- `src/model/mod.rs`: canonical messages, tools, requests, responses, and provider trait.
- `src/model/openai_oauth.rs`: OAuth provider orchestration and 401 retry.
- `src/model/openai_compatible.rs`: Responses/Chat provider facade.
- `src/model/openai_request.rs`: OpenAI request serialization and continuation replay.
- `src/model/openai_stream.rs`: shared Responses/Chat SSE parsing.
- `src/model/openai_oauth_credentials.rs`: auth-file, Codex import, and JWT helpers.
- `src/model/openai_oauth_device.rs`: device-code request and polling.
- `src/model/anthropic_compatible.rs`: Messages adapter.
- `src/tools/mod.rs`: tool contract and deterministic sorted registry used by
  both provider-visible and hidden command assemblies.
- `src/tools/manifest.rs`: strict parser for embedded local `tool.yaml`
  contracts.
- `src/tools/assembly.rs`: the single process/run path that separates native
  provider tools from hidden command tools.
- `src/tools/command/`: the provider-visible `fiasco` adapter, fixed route
  catalog, CLI-like parser, schema-driven argument conversion, in-process
  pipeline, help, and terminal redirect.
- `src/tools/{bash,delegate,load_skill,read,web_search,write}/`: standalone local
  adapters with a typed compile-time `tool.yaml` beside their Rust code.
- `src/tools/history/{read,search}/`: compacted-history adapters;
  `src/tools/history/mod.rs` registers the complete family.
- `src/tools/handle/{close,inspect,list,send,stop,wait}/`: runtime-handle controls;
  `src/tools/handle/mod.rs` registers the complete family and owns their shared
  result projection.
- Every leaf manifest contains its complete deterministic lookup name, purpose,
  return guidance, and input schema; directory paths never derive names.
- `src/trajectory.rs` and `src/trajectory/`: provider-neutral history reader
  contracts plus local message/artifact search.
- `src/artifact.rs`: run-local attachment references, envelopes, and spill.
- `src/artifact/preview.rs`: bounded UTF-8-safe file and byte previews.
- `src/artifact/model-instruction.md`: compact model guidance for inspecting a
  spilled result.
- `src/storage/mod.rs`: run directories, metadata, events, and shared JSON
  persistence helpers.
- `src/storage/message_log.rs`: ordered message append, complete-line loading,
  and writer-tail recovery.
- `src/storage/message_log/decoder.rs`: synchronous line validation plus the
  forward async reader; it preserves exact raw lines and byte offsets while
  hiding only a torn final record.
- `src/storage/message_log/transcript.rs`: thin run-routing and terminal-state
  adapter over fmtview-core's growing-file timeline, plus exact complete-line
  NDJSON streaming.
- `src/storage/trajectory.rs`: classified append-only messages and
  compacted-history loading.
- `src/skills/mod.rs`: Agent Skills metadata discovery and body/path loading;
  the hidden command adapter is in `src/tools/load_skill/`.
- `skills/orchestrate-with-graphs/`: installable guidance for maintaining
  workspace YAML graphs as an orchestrator mental model with ordinary tools.
- `skills/register-mcp/`: installable workflow for capturing, documenting,
  exploring, and validating progressive MCP artifacts.
- `src/mcp.rs` and `src/mcp/`: rmcp stdio client lifecycle, MCP artifact
  loading, command compilation, result rendering, and the fixed internal `mcp`
  adapter reached through `fiasco`.
- `src/hooks.rs`: deterministic command-hook pipeline.
- `src/memory.rs`: user/project Markdown memory paths and reminder text.
- `src/config.rs`: TOML configuration.
- `src/events.rs`: runtime event contract and sinks.
- `src/cli.rs`: CLI command schema.
- `src/main.rs`: command composition root; inspect dispatch occurs before
  provider/runtime initialization and embeds fmtview over its generic file
  timeline.
