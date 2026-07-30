# Artifact Contract

Tool output is part of the execution record, even when it is too large for the
model context. Fiasco therefore stores the complete output as a run-local
attachment and keeps only a bounded envelope in the originating message.

## Location And Reference

```text
.fiasco/runs/<run-id>/artifacts/
  <tool-call-id>-<ulid>.<extension>
```

Each spill creates a new file without overwriting an existing attachment. The
tool call id is sanitized for readability; the ULID provides run-local
uniqueness without making the filename depend on the content.

The persisted `ArtifactRef` contains only:

- `path`: workspace-relative when the workspace contains the attachment
- `media_type`: the media type recorded when the attachment was created

There is no metadata sidecar, content digest, artifact id, embedded run id or
call id, or recorded file length in the reference.

## Mutable Attachment Semantics

An `ArtifactRef` names a current run-local attachment, not an immutable content
identity. Ordinary workspace operations may update the file after the tool
result is committed. A later `read` or history search observes the file's
current bytes and current filesystem length.

The originating message remains a historical record. Its preview, media type,
and total/head/tail/omitted byte counts describe the output at generation time
and are not rewritten when the attachment changes. Consumers that need the
original bytes must preserve them separately; the artifact contract does not
provide content-addressed history.

Before history search follows a reference, the local reader:

1. canonicalizes the current run's artifact directory and referenced path;
2. requires the referenced path to remain inside that directory; and
3. requires the current target to be a regular file.

These are path and file-type boundaries, not content-integrity checks. A
reference into another run, a symlink that escapes the current run directory,
or a directory masquerading as an artifact is rejected.

## Model-Facing Envelope

Small UTF-8 foreground results remain inline. When a result exceeds
`inline_bytes`, the complete bytes are written to the current run and the model
receives a compact `[Tool output]` envelope with:

- whether the tool result is an error
- whether any source bytes were omitted from the preview
- generation-time total, preview-head, preview-tail, and omitted byte counts
- an optional preview limitation when content cannot be previewed safely
- media type and relative attachment path
- instructions to use bounded `read`, or `bash` with `rg`

An artifact may have `truncated: false` when its configured preview contains all
source bytes; artifact-backed and truncated are separate properties.

The normal textual preview retains both the beginning and ending because
compiler and command failures commonly appear at the end of command output.
Binary and non-UTF-8 data is never decoded into a lossy inline string. Its
envelope reports zero preview bytes and
`preview_limitation: binary_or_non_utf8`, with media type and path only.

An image returned by `read` follows the same attachment contract and is also
sent to the model as a native image attachment. After every result from that
assistant tool-call batch has been emitted in original call order, fiasco
appends one user message containing the images and a runtime reminder that
lists their source call ids in attachment order. OpenAI Chat uses `image_url`
data URLs, OpenAI Responses uses `input_image`, and Anthropic uses base64 image
sources. JPG, PNG, and WebP pass through; GIF first frames and BMP files are
normalized to PNG for a consistent provider surface.

Artifact-reference overhead and ordinary conversation text still count toward
the provider context. Successful and failed foreground results share this
per-result policy; a large error is an artifact-backed bounded result rather
than an unbounded exception string in the next model request.

### Asynchronous Delivery

Promoted-tool results and reusable-agent activity results use the same
independent result policy as foreground tools. Small UTF-8 output stays inline.
Large, binary, and non-UTF-8 output is preserved as an attachment and the
delivered body contains the ordinary bounded `[Tool output]` envelope.

Payload limiting happens before fiasco adds the `<runtime_handle>` status
wrapper. The wrapper, result metadata, and inspection instruction are never
part of the preview budget. If an image read exceeds the foreground window and
receives a runtime handle, its binary result remains artifact-backed. Reading
that attachment again attaches its current image bytes on demand.

A status-less runtime-handle notice is only a running acknowledgement and has
no result artifact. An activity-result notice includes `status` and keeps its
`ArtifactRef` in the same runtime-handle content block.

### History-Query Boundaries

History commands use the same envelope when their returned JSON or JSONL is too
large. For `history search`, that attachment contains the complete result after
applying `history_search_max_matches`; it does not contain older matches
omitted by the query cap. A `truncated: true` search result therefore means
“refine the regex,” while an artifact preview means “inspect this complete
bounded result.”

Full-text history search reads the exact `ArtifactRef` stored with each
foreground or asynchronous result in `messages.jsonl`; it does not parse the
model-facing preview or guess from a call id. It validates the path boundary,
reads current filesystem metadata, and streams the current candidate through
bounded `rg` search. Artifact matches return the exact path and owning message
ref. Reused call ids remain unambiguous because each result carries its own
path, while later content mutation is intentionally visible.

## Lifecycle

Artifacts belong to a run, not to long-term memory. Memory extraction may cite
or summarize selected attachments, but raw tool output is not automatically
promoted into user or project memory.

Fiasco does not delete run artifacts automatically in the launch release.
Cloud deployments may apply an external retention policy to completed run
directories.
