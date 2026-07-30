# ADR 0049: Route Progressive MCP Artifacts Through One Command Tool

- Status: Accepted
- Date: 2026-07-24
- Refines: ADR 0014 (MCP adapter ownership and assembly)
- Refines: ADR 0015 (MCP model-facing contract packaging)
- Refines: ADR 0024 (optional MCP schema membership)
- Refined by: ADR 0050 (the fixed MCP adapter is now an internal `fiasco`
  command route)

## Context

Fiasco connected every configured MCP server at startup, called `tools/list`,
and registered one provider-visible `mcp__<server>__<tool>` adapter per remote
tool. Large MCP servers therefore placed every remote name, description, and
input schema in every normal model request even when a task needed one small
capability. Remote tools also competed in one flat namespace, and the model had
no progressive source map for deciding which subset of a server to understand.

The desired interaction is closer to an Agent Skill: keep a compact catalog in
the initial reminder, let the model read capability documentation on demand,
and preserve one deterministic execution boundary. Registration should be a
model-guided authoring workflow rather than a hidden model call during every
runtime startup.

## Decision

- Bind each configured stdio MCP transport to an artifact directory. Transport
  command, arguments, and environment references remain in Fiasco
  configuration.
- Require `MCP.md` and `catalog.json` in the artifact. `MCP.md` contains a
  model-generated lowercase namespace, concise description, and
  capability-oriented source map. `catalog.json` is the exact captured
  `tools/list` JSON array and remains outside model context.
- Permit source-map references to aggregate highly related commands by shared
  objects, identifiers, constraints, or workflows. This organization is
  authoring guidance, not a runtime validation rule.
- Put only configured namespace, description, and absolute `MCP.md` path in the
  initial runtime reminder.
- Register one optional, fixed `mcp` tool with one command string. Parse
  `<source> <tool> [name=value ...]`, allow a positional value only for a
  one-property input schema, and use the captured schema for top-level value
  conversion.
- Reuse the same artifact loader, command compiler, stdio client, and result
  renderer for the model-facing tool and the `mcp check`, `mcp compile`, and
  `mcp call` authoring commands. `mcp capture` writes the exact catalog.
- Return remote text content directly. Return a structured-only result as its
  JSON value. Preserve the exact MCP result JSON when rich non-text blocks
  require the complete protocol shape.
- Ship `skills/register-mcp/` as optional procedural guidance. The skill has the
  authoring model inspect the real server, generate the knowledge layer,
  compile representative examples, and explore safe read-only calls when
  conditions permit.
- Load configured artifacts once during process assembly. Do not hot-reload,
  hash, regenerate, auto-resume, or ask a hidden model to repair them.

## Consequences

- Every configured MCP set contributes one stable provider tool schema instead
  of one schema per remote command.
- Namespace collisions between servers move inside the command router and no
  longer compete with built-in tool names.
- The model spends context on one selected source map and relevant reference
  rather than every remote schema.
- Exact routing remains deterministic because model-written prose never
  replaces the captured remote name or input schema.
- An artifact can become stale when its server changes. Normal runtime calls
  surface the resulting server error; explicit capture and live check refresh
  and verify it.
- Configuration remains the explicit authority that starts an external
  process. Merely placing an artifact in a workspace does not execute its
  contents.

## Alternatives Considered

- **Keep one dynamic adapter per remote tool.** Rejected because it defeats
  progressive disclosure and makes context cost proportional to the complete
  remote catalog.
- **Make `mcp` expose list, load, and call operations.** Rejected because
  ordinary file reads already provide progressive documentation; discovery
  operations would turn the executor into a second knowledge API.
- **Generate name and description through a hidden startup model call.**
  Rejected because it adds nondeterministic latency and cost to every run and
  creates another cache or persistence problem.
- **Store transport commands inside auto-discovered artifacts.** Rejected
  because configuration should explicitly select external processes; artifact
  installation alone should not start one.
- **Copy an existing MCP CLI runtime.** Rejected because Fiasco needs a smaller
  single grammar and must share its exact loader/compiler with the model-facing
  `Tool` contract.

## Related Documents

- [MCP configuration](../configuration.md#mcp)
- [Architecture](../architecture.md#mcp-artifacts)
- [Register MCP Skill](../../skills/register-mcp/SKILL.md)
- [ADR 0014](0014-flat-tool-adapters-and-explicit-assembly.md)
- [ADR 0015](0015-local-tool-yaml-manifests.md)
- [ADR 0024](0024-freeze-built-in-schemas-across-agent-roles.md)
