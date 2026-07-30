# ADR 0050: Route Harness Capabilities Through an In-Process Command Surface

- Status: Accepted
- Date: 2026-07-30
- Refines: ADR 0014 (provider-facing adapter assembly)
- Refines: ADR 0015 (provider-facing tool manifest ownership)
- Refines: ADR 0024 (the frozen built-in schema set)
- Refines: ADR 0049 (the MCP model-facing route)

## Context

Fiasco exposed history, skills, web search, delegation, runtime-handle controls,
and MCP as separate provider tool schemas. The contracts were deterministic,
but the provider surface grew with every harness capability. Composing two
capabilities also required another model turn even when the second operation
could consume the first result directly.

A real CLI and shell were considered as the common capability layer. That would
provide mature pipes and redirects, but a child Fiasco process cannot access
the current process's live agent activities, handles, promoted futures, or MCP
clients. Solving that mismatch would require server mode or IPC before the
command-surface idea could be tested.

The harness already has typed `Tool` implementations for every capability.
Those implementations can remain the execution contracts while a small
in-process command adapter provides compact syntax and linear composition.

## Decision

- Keep `bash`, `read`, and `write` as native provider-visible tools.
- Add one provider-visible `fiasco` tool with a required `command` string and
  optional exact `stdin` string.
- Move Fiasco-owned skills, web search, history, delegation, handle controls,
  and MCP adapters into a hidden run-scoped command registry.
- Keep every hidden adapter's existing `Tool` implementation and typed
  `tool.yaml` as the authoritative description, schema, validation, and
  execution contract.
- Resolve a fixed route table such as `history search`, `agent start`, and
  `mcp`. Include only routes whose internal adapters were enabled at assembly.
- Provide `help` and route-specific help from the enabled route table and
  internal specs.
- Parse words, single and double quotes, escapes, linear `|`, and one terminal
  `> path`. Do not implement general shell syntax.
- Compile `name=value` and one unambiguous positional value against the
  internal JSON schema. Let `-` consume stdin explicitly; otherwise permit
  stdin to fill exactly one missing required property. Reject ambiguous or
  unused input.
- Execute pipeline stages sequentially in the owning run process. Buffer the
  complete successful pre-artifact output between stages and stop on the first
  error.
- Apply terminal redirect only after the complete pipeline succeeds. Reuse the
  native `write` tool so replacement is atomic and follows the same path
  contract.
- Treat the complete command as one outer tool call. Hooks, events,
  cancellation, foreground promotion, transcript identity, and artifact policy
  apply to `fiasco`; internal stages do not emit separate tool calls or runtime
  envelopes.
- Defer server mode, remote control, arbitrary-harness compatibility, streaming
  pipelines, binary/image transport, append, and a general command plugin
  system.

Explicitly injected application or test tools may remain provider-visible. The
hidden command route table covers Fiasco-owned built-ins rather than becoming a
dynamic plugin protocol.

## Consequences

- The normal provider schema set stays compact as harness capabilities grow.
  Optional web search and MCP alter the `fiasco` command catalog and
  description, not the number of provider tools.
- Root, GeneralTask, and compaction requests retain one sorted, frozen schema
  set.
- Linear dependent operations can execute without an intervening model call.
- Live agent handles and MCP sessions remain available because every stage runs
  in the owning process.
- There is one parser and argument conversion boundary in addition to each
  internal tool's own typed validation.
- Pipelines buffer complete results, require UTF-8 when feeding typed arguments
  or files, and reject native image attachments.
- Earlier stage side effects cannot be rolled back when a later stage fails.
- Hooks and event consumers observe the outer `fiasco` call, not each internal
  stage. Stage-specific diagnostics remain in the returned error context.
- The operator CLI and provider command language may share domain
  implementations without being required to share syntax or lifecycle.

## Alternatives Considered

- **Keep every built-in as a provider tool.** Rejected as the default because
  schema count grows with the harness and dependent operations require extra
  model turns.
- **Expose only `bash` and real Fiasco child-process CLI commands.** Deferred
  because live run state and sessionful MCP clients are process-local; server
  mode would become an accidental prerequisite.
- **Implement a complete shell.** Rejected because `bash` already exists and a
  second shell grammar would add unsafe complexity unrelated to harness
  capability composition.
- **Add a `stdout` or output-path argument to every command.** Rejected because
  terminal `>` expresses file output once, after the whole pipeline succeeds,
  while reusing the existing atomic writer.
- **Persist or reconstruct live state for child commands.** Rejected because it
  conflicts with the runtime's single process-local activity authority and
  expands this decision into remote-control architecture.

## Related Documents

- [In-process command TODO](../todo-unified-command-surface.md)
- [Architecture](../architecture.md#tool-and-command-registries)
- [Design choices](../design-choices.md#in-process-command-surface)
- [ADR 0014](0014-flat-tool-adapters-and-explicit-assembly.md)
- [ADR 0015](0015-local-tool-yaml-manifests.md)
- [ADR 0024](0024-freeze-built-in-schemas-across-agent-roles.md)
- [ADR 0049](0049-progressive-mcp-artifacts.md)
