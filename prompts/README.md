# Prompt Assets

`agents.yaml` is the typed registry for stable agent-level prose. Every prompt
is a folded `>-` scalar, so YAML removes source-only line wrapping and strips
the final newline before Rust sees the value. `src/prompts.rs` parses the
embedded file once, rejects unknown or empty fields, and exposes the named
prompts directly.

Runtime assembly, precedence, dynamic values, argument validation, and
execution contracts remain in Rust. Project `AGENTS.md`, model modalities,
runtime role, remaining delegation depth, skill metadata, memory paths, and
the delegated task text are dynamic inputs. Stable GeneralTask guidance lives
in this registry and Rust inserts it into the child's initial runtime reminder.
The stable system prompt defines only tool-agnostic behavior:
user-intent boundaries, evidence, schema authority, concurrent-call semantics,
instruction precedence, process permissions, and final reporting. Tool names
and feature workflows do not belong in that prefix, so removing a native tool
or hidden command also removes its prompt influence. The initial runtime
reminder carries concrete run state and concise feature guidance only when that
feature is present. Its child guidance points to the paired, self-contained
delegated task. Keeping the stable rules in the shared system prefix preserves
byte-identical Root and GeneralTask prefixes.

Every local tool adapter keeps a typed `tool.yaml` beside its Rust module.
Standalone adapters live at `src/tools/<tool>/`; cohesive handle and history
families live at `src/tools/<family>/<member>/`. Every manifest declares its
complete deterministic lookup name rather than deriving it from its path.
Native manifests become provider schemas. Hidden command manifests remain the
authoritative argument and execution contracts and supply route-specific
`fiasco help`. `description` states purpose, usage, side effects, and
constraints; `returns` states the successful result shape, interpretation, and
tool-specific follow-up. The loader joins them as
`<description>\n\nReturns: <returns>` for the standard provider description. The
Rust module owns arguments, semantic validation, and execution. Domain state
stays in its subsystem: for example, handle adapters call
`RuntimeHandleManager`, and `load_skill` calls `SkillRegistry`. Other stable model
instructions live with the behavior that assembles them, such as
`src/artifact/model-instruction.md`.

When one call needs a structured aggregate rather than incremental mutation,
the manifest may include a concise representative example in both its prose and
JSON Schema. Examples explain shape only; Rust remains authoritative for
semantic validation.

These are compile-time assets, not runtime overrides or dynamically discovered
plugins. External executable tools integrate through one fixed internal `mcp`
adapter reached through the provider-visible `fiasco` command surface. Their
exact server-provided catalogs stay in progressively documented MCP artifacts
rather than entering the provider tool-schema prefix.

`returns` is required even when one short sentence is sufficient. A manifest
should explain its own behavior without assuming that a sibling tool is
available; cross-tool follow-up is described in capability terms unless the
dependency is intrinsic to the returned data. Generic artifact spill and error
behavior stays in runtime-generated result guidance rather than being copied
into every manifest. This authoring split does not add a private provider field
or claim a formal output schema.

Native tool descriptions are sent through the provider's sorted tool-schema
field. The `fiasco` description contains the frozen enabled route catalog;
route-specific details remain progressively available through `fiasco help`.
Core history, delegation, and handle-control routes are present in every Root
and GeneralTask run. Optional web search and MCP routes are selected during the
run-assembly path. Configured MCP source-map metadata enters the dynamic
reminder. Memory uses ordinary file tools and therefore adds no schema or
command. Remaining delegation depth is runtime state, not schema membership.
Compaction reuses that system/tool prefix and adds the `compaction_request`
prompt as the final user message.
Normal context after a successful checkpoint adds `compaction_resume` inside a
synthetic user runtime reminder immediately after the exact assistant state.
Every local manifest is static. Startup integrations may add optional command
routes before the run is frozen, but remaining delegation depth never changes
provider-schema or command-catalog membership.
