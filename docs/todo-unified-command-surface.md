# TODO: In-Process Command Surface

## Handoff Status

- Status: accepted direction, implemented in the current worktree.
- Baseline: `1737d7ee15791eb41a89fe7b2dff01d45a5814ab`.
- Target repository: `/root/fiasco`.
- Durable decision: [ADR 0050](adr/0050-in-process-command-surface.md).

This document began as an exploration of real child-process CLI commands,
server mode, and remote control. Those concerns are intentionally deferred.
The accepted problem is narrower:

> Keep Fiasco focused on the harness. Give the model one compact, CLI-like
> `fiasco` tool for most harness capabilities, then compose those capabilities
> in process with a small pipeline and redirect grammar.

The command surface is not the operator CLI and is not a shell. It is a compact
syntax layer over the existing typed `Tool` implementations.

## Provider Surface

The normal built-in provider-visible surface is:

```text
bash
read
write
fiasco
```

`bash`, `read`, and `write` remain native because their execution and result
contracts are already useful model primitives. `fiasco` has one stable input:

```json
{
  "command": "history search result-old | agent send handle=... mode=followup message=-",
  "stdin": "optional exact initial input"
}
```

Optional startup capabilities change the command catalog inside `fiasco`, not
the provider schema set. Any explicitly injected custom tool remains directly
provider-visible; the fixed command routes cover Fiasco-owned built-ins.

## Enabled Commands

Routes are enabled only when their internal tool is assembled:

```text
help [command path]
skill load <name>
web search <query> [count=<integer>] [country=<string>] [search_lang=<string>]
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

`help` returns the enabled catalog. `help <command path>` renders the existing
internal tool description and input schema, so command guidance continues to be
owned beside the implementation.

## Grammar

The accepted grammar supports:

```text
command [arguments]
command [arguments] | command [arguments]
command [arguments] | command [arguments] > path
```

- whitespace separates words;
- single and double quotes preserve whitespace and operators;
- backslash escapes the next character;
- `|` separates sequential pipeline stages;
- one final `> path` redirects the successful final result;
- `name=value` addresses an input property;
- one positional value may fill the sole unambiguous property;
- `-` or `name=-` consumes pipeline stdin exactly once.

This is deliberately not Bash. It does not implement `&&`, `||`, `>>`, file
descriptor redirection, variables, globbing, command substitution, heredocs,
loops, or background jobs. Those remain available through the native `bash`
tool when the model actually needs a shell.

## Execution Contract

All stages execute through the existing `Tool` implementations in the current
run process. Fiasco does not spawn its own binary and does not reconstruct live
runtime state.

- Pipelines are linear, buffered, and fail fast.
- A successful stage's complete pre-artifact output becomes the next stage's
  stdin.
- JSON object output can satisfy a later command's complete argument object.
- Otherwise stdin fills exactly one missing required argument, or an explicit
  `-` selects its destination.
- Ambiguous or unused stdin is an error.
- UTF-8 is required when bytes enter a typed argument or redirect.
- Native image attachments cannot enter a pipeline or redirect.
- A terminal redirect reuses the native `write` contract and atomically
  replaces the target file only after every stage succeeds.
- Earlier stage side effects are not rolled back if a later stage fails.

The provider submitted one `fiasco` tool call, so hooks, runtime events,
artifact limiting, transcript identity, cancellation, and foreground promotion
apply to that outer call. Hidden stages do not create extra assistant tool
calls, hook envelopes, or event records.

Separate provider tool calls still follow the runner's existing batch rule:
independent calls execute concurrently and results commit in original call
order. A single `fiasco` pipeline is sequential because each stage depends on
the previous result.

## Implementation Boundaries

- `src/tools/command/`: command parser, route catalog, schema-driven argument
  conversion, sequential execution, help, and redirect.
- `src/tools/assembly.rs`: splits the provider registry from the hidden command
  registry and assembles the one `fiasco` adapter.
- Existing built-in leaf tools and their `tool.yaml` files remain the
  authoritative typed execution contracts.
- `src/mcp.rs` and `src/mcp/`: still own MCP artifact loading, exact command
  compilation, stdio clients, and result rendering. The hidden MCP adapter is
  reached through the `mcp` command route.
- `AgentRunner`, `RuntimeHandleManager`, artifact storage, and message storage
  remain unchanged.

## Deferred Work

- server mode, remote frontends, or a control-plane protocol;
- making Fiasco compatible with arbitrary external harnesses;
- replacing the native harness with OpenCode, Pi, or another runtime;
- streaming or backpressured pipelines;
- binary/image pipelines;
- append, tee, or multiple redirects;
- a general plugin or dynamic command-discovery framework;
- operator CLI parity with the provider command language.

These may be reconsidered from real trajectory evidence. They are not
prerequisites for validating whether the compact in-process command surface
improves model behavior.

## Acceptance

- Provider requests expose `bash`, `read`, `write`, and `fiasco`, plus any
  intentionally injected custom test/application tools.
- History, skills, web search, MCP, delegation, and handle controls are not
  separate provider schemas.
- Quoting, typed arguments, stdin substitution, pipeline failure, help, and
  atomic redirect have focused tests.
- Agent lifecycle and compacted-history integration tests use `fiasco`
  commands end to end.
- Root, GeneralTask, and compaction requests retain one sorted, frozen schema
  set.
- The repository passes the standard formatting, check, clippy, test, package
  asset, and headless smoke gates.
