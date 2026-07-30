# Architecture Decision Records

Architecture Decision Records (ADRs) explain why a durable technical choice was
made. Runtime contracts remain in their topic documents; ADRs preserve the
decision context, credible alternatives, and consequences that would otherwise
be lost in commits or chat history.

## When To Write One

Add an ADR when a change establishes or revises a cross-module invariant,
persistence or provider contract, security boundary, or other costly-to-reverse
choice with credible alternatives. Ordinary bug fixes, local refactors, and
straightforward feature additions do not need an ADR.

## Workflow

1. Create `NNNN-short-title.md` using the next four-digit number.
2. Start with `Proposed`; change it to `Accepted` when the decision is adopted.
3. Treat accepted ADRs as historical records. Fixing typos is fine, but a
   replaced decision requires a new ADR marked `Supersedes: ADR NNNN`; mark the
   old ADR `Superseded by ADR NNNN`. For a narrowly scoped change, use
   `Refines: ADR NNNN (scope)`, leave the old record accepted, and add an
   explicit forward note without rewriting its original decision.
4. Add the record to the index below and link it from the relevant contract or
   `docs/design-choices.md` summary.

Use these sections:

```text
# ADR NNNN: Title

- Status: Proposed | Accepted | Rejected | Superseded
- Date: YYYY-MM-DD
- Supersedes: ADR NNNN (when applicable)
- Refines: ADR NNNN (scope, when applicable)

## Context
## Decision
## Consequences
## Alternatives Considered
## Related Documents
```

## Index

- [ADR 0001: Persist complete messages and keep stream deltas
  transient](0001-durable-messages-transient-stream-deltas.md)
- [ADR 0002: Embed prompt assets and keep tools with their
  owners](0002-compile-time-prompt-assets-and-tool-ownership.md)
- [ADR 0003: Add local compaction without rewriting the
  trajectory](0003-append-only-local-compaction-and-history-retrieval.md)
- [ADR 0004: Keep the normal agent prefix and core history tools
  stable](0004-stable-agent-prefix-and-core-history-tools.md)
- [ADR 0005: Persist Chat-compatible messages with a metadata
  sidecar](0005-openai-chat-compatible-message-log.md)
- [ADR 0006: Resume at complete-message boundaries and keep child coordination
  separate](0006-complete-message-resume-and-durable-child-coordination.md)
- [ADR 0007: Emit compacted-history guidance only at its active
  boundary](0007-compacted-history-guidance-at-active-boundary.md)
- [ADR 0008: Consolidate agent prompts in a typed YAML
  registry](0008-typed-agent-prompt-registry.md)
- [ADR 0009: Maintain memory through ordinary
  tools](0009-memory-through-ordinary-tools.md)
- [ADR 0010: Use parent-controlled background work without step or execution
  caps](0010-parent-controlled-background-work.md)
- [ADR 0011: Separate model stream idleness from the request
  deadline](0011-model-stream-idle-timeout-and-request-deadline.md)
- [ADR 0012: Record compaction as messages](0012-record-compaction-as-messages.md)
- [ADR 0013: Use sequence-addressed message refs](0013-sequence-addressed-message-refs.md)
- [ADR 0014: Flat tool adapters and explicit
  assembly](0014-flat-tool-adapters-and-explicit-assembly.md)
- [ADR 0015: Package local tool contracts as typed YAML
  manifests](0015-local-tool-yaml-manifests.md)
- [ADR 0016: Separate tool purpose from return
  guidance](0016-separate-tool-purpose-and-return-guidance.md)
- [ADR 0017: Concurrent tool batches and explicit task
  controls](0017-concurrent-tool-batches-and-explicit-task-controls.md)
- [ADR 0018: Limit tool output per result](0018-limit-tool-output-per-result.md)
- [ADR 0019: Group related tool adapters without deriving
  names](0019-group-related-tool-adapters.md)
- [ADR 0020: Unify background task runtime
  notices](0020-unify-background-task-runtime-notices.md)
- [ADR 0021: Mark an active compacted state as continuation
  context](0021-compacted-state-continuation-reminder.md)
- [ADR 0022: Send native image attachments after ordered tool
  results](0022-native-image-attachments-after-tool-results.md)
- [ADR 0023: Declare model input modalities](0023-declare-model-input-modalities.md)
- [ADR 0024: Freeze built-in schemas across agent
  roles](0024-freeze-built-in-schemas-across-agent-roles.md)
- [ADR 0025: Fork or isolate delegated context](0025-fork-or-isolate-delegated-context.md)
- [ADR 0026: Keep planning graphs file-backed and separate from task
  execution](0026-file-backed-planning-graphs.md) (superseded by ADR 0048)
- [ADR 0027: Correlate delegate recovery with its originating
  call](0027-correlate-delegate-recovery-with-originating-call.md)
- [ADR 0028: Keep cross-tool workflow in the stable system
  prompt](0028-stable-cross-tool-workflow.md) (superseded by ADR 0039)
- [ADR 0029: Recover incomplete model output at narrow
  boundaries](0029-recover-incomplete-model-output.md)
- [ADR 0030: Use one result policy for foreground and background
  work](0030-uniform-foreground-and-background-results.md)
- [ADR 0031: Validate the complete initial graph topology before
  creation](0031-validate-complete-initial-graph-topology.md) (superseded by ADR 0048)
- [ADR 0032: Store each message as one self-contained
  record](0032-self-contained-message-log.md)
- [ADR 0033: Isolate delegated context](0033-isolate-delegated-context.md)
- [ADR 0034: Resume from atomic turn checkpoints](0034-atomic-turn-checkpoints.md)
- [ADR 0035: Model delegated agents as reusable tasks](0035-reusable-agent-tasks.md)
- [ADR 0036: Interrupt agent activities on process
  restart](0036-interrupt-agent-activities-on-restart.md)
- [ADR 0037: Embed fmtview over a checkpoint
  timeline](0037-embed-fmtview-over-checkpoint-timeline.md)
- [ADR 0038: Use runtime handles and explicit
  restart](0038-runtime-handles-and-explicit-restart.md)
- [ADR 0039: Keep feature guidance with
  capabilities](0039-keep-feature-guidance-with-capabilities.md)
- [ADR 0040: Initialize complete graph
  documents](0040-initialize-complete-graph-documents.md) (superseded by ADR 0048)
- [ADR 0041: Close active agent
  threads](0041-close-active-agent-threads.md)
- [ADR 0042: Store compatible Chat reasoning beside assistant
  content](0042-chat-reasoning-sibling-field.md)
- [ADR 0043: Compact message and checkpoint
  payloads](0043-compact-message-and-checkpoint-payloads.md)
- [ADR 0044: Expose complete lines and repair only the resume
  tail](0044-newline-visible-messages-and-tail-repair.md)
- [ADR 0045: Delegate transcript paging to
  fmtview-core](0045-delegate-transcript-paging-to-fmtview-core.md)
- [ADR 0046: Treat artifacts as run-local mutable
  attachments](0046-run-local-mutable-artifacts.md)
- [ADR 0047: Collapse durable run lifetime and handle
  discovery](0047-collapse-run-lifetime-and-handle-discovery.md)
- [ADR 0048: Move orchestration graphs to an Agent
  Skill](0048-move-orchestration-graphs-to-a-skill.md)
- [ADR 0049: Route progressive MCP artifacts through one command
  tool](0049-progressive-mcp-artifacts.md)
- [ADR 0050: Route harness capabilities through an in-process command
  surface](0050-in-process-command-surface.md)
