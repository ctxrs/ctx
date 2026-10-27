# ASR export

`ctx export asr` converts native history into an [Agent Session Record](https://agentsessionrecord.org) 0.1 archive bundle. Supported converters include Claude Code, Codex, Pi, Grok Build DeepSeek Harness, Gemini CLI, Antigravity and Qwen Code.

```sh
ctx export asr --all --out ./asr
ctx export asr --all --provider codex --out ./codex-asr
ctx export asr --session NATIVE_SESSION_ID --provider claude --out ./session-asr
ctx export asr --all --provider codex --path ./rollouts --out ./asr
ctx export asr --all --provider deepseek-harness --path ./sessions --out ./asr
```

Automatic input selection uses ctx's existing provider-root discovery. `--path`
selects a native transcript or directory and requires `--provider`. Session IDs
are native IDs, not ctx citation IDs; Claude subagents use their `agentId`.
Export reads provider files without importing them into the search store.

The CLI journal converters use `--provider copilot`, `droid`, `kimi`, or `qoder`.
Copilot reads `events.jsonl`; Droid and Qoder read their session JSONL journals.
Kimi reads `sessions/<workdir>/<session>/agents/<agent>/wire.jsonl` plus the
session's bounded `state.json` metadata. Its ASR namespace is the native session
ID and its session ID is the native agent ID, so agents named `main` in separate
sessions cannot collide. `--session` accepts the containing native session ID
or agent ID for Kimi. Copilot's embedded subagent events are separate streams,
namespaced by their containing native session ID.

Copilot schedules become turn `trigger` objects only when the native message
identifies a scheduled firing that begins that turn. Creating a schedule is
retained as a vendor event. Tool names and IDs remain verbatim; repeated IDs
produce ambiguous result linkage with the native ID in metadata and no paired
`callId`. Display output and raw sidecars remain
separate from model-facing content. Kimi request records identify inference
attempts; step completion revises the final request of the same native turn/step.
Droid and Qoder message IDs are not treated as inference IDs.

These are lossy converters. Unknown events remain vendor records; additional
native fields remain in provider-qualified metadata. Kimi tool output is kept
as raw/display data because its request projector can rewrite it, with
`capture.content: absent` and a declared loss. Blobref media remains a native
resource reference. Context undo, detailed request projection, and unattributed
usage observations remain vendor data. Compactions with no identified replaced
range explicitly use `replaces.state: unknown`. The native Copilot 1.0.104
scheduler fixture qualifies that narrow mapping; authored semantic cases do
not establish general conformance for a provider release.

The JSON result reports sessions, unchanged sessions, records, bytes, gaps,
new snapshots and sources without a usable session identity.

The destination contains `bundle.json`, one current JSONL stream per session,
content-addressed blobs, and a private export-state database. This is an archive
of potentially sensitive history. Secret-question answers are removed, but
export does not apply general-purpose secret or personal-data redaction.

Repeat the command with the same destination to update the bundle. Unchanged
sources are skipped. A changed session is spooled again; the exporter does not
assume native files only append. The export database retains source-position assignments,
so newly discovered segments and appended native records do not renumber earlier
records. Keep this database with the destination when continuing an export.
When the new stream has the previous stream as an exact prefix, only its suffix
is appended. An incomplete output tail is removed before appending. If a folded
response, rewritten source or converter change alters earlier bytes, a new
snapshot is published and the earlier stream is retained. The manifest selects
one snapshot per session. Concurrent writers to one destination are serialized.

JSONL is read with a bounded line buffer; the default limit is 16 MiB and can be
changed with `--max-record-bytes`. Malformed or oversized complete native lines
become `gap` records. Incomplete native tails wait for a later export. Session
bodies and lookup indexes are spooled to disk, with a bounded SQLite cache.
Oversized folded Claude response content is kept in a blob. Codex compressed
rollouts are decoded incrementally. Unknown native records remain vendor records;
absence, capture states and declared losses distinguish unavailable facts from
recorded empty values. The declared vendor loss categories cover over-limit
native lines, metadata kept outside the selected transcripts, and native variants
retained as vendor records without a standard semantic projection. Export does
not read undocumented live UI databases, global scheduler definitions or deleted backups.

## Journal converters

Select `--provider junie`, `mistral-vibe`, `tabnine`, `mux`, or `cursor`.
These adapters read native sources directly and use the same bounded spool and
bundle writer. They do not open the searchable ctx store.

| Provider | Native inputs | Mapping limits |
| --- | --- | --- |
| Junie | `sessions/index.jsonl`, `session-*/events.jsonl` | UI step growth is folded within its prompt scope. UI results remain display content; model rendering is unknown. |
| Mistral Vibe | `meta.json`, `messages.jsonl` | Current metadata, instructions, configuration and cumulative statistics follow the journal. Image paths remain recorded references; referenced files are not opened. |
| Tabnine CLI | Chat recording JSONL, including child journals | Repeated message IDs replace the whole earlier snapshot at its first position. Saved checkpoint files are outside this converter's input contract. |
| Mux | `chat-archive.jsonl`, `chat.jsonl`, `partial.json`, `metadata.json`, `subagent-transcripts.json`, archived child journals | Archive precedes active history. A partial with the same `historySequence` and more parts replaces the effective row at the capture tail. Current metadata and archive index are also tail snapshots. |
| Cursor | `projects/<workspace>/agent-transcripts/<id>/<id>.jsonl` | Uses ctx's strict Cursor parser. IDE `state.vscdb`, CLI blob-DAG `store.db`, and external tool-output files have no qualified decoder here. Selecting a database reports an unidentified source; no chat semantics are guessed. |

Native message IDs survive source reordering. Positional identities in rewritable
sources carry `idScope: capture`; their equality across captures says nothing
about identity. Replacement captures have `observedAt`, while append and no-op
exports preserve it. `seq` describes serialization order within one capture,
not chronology across stores. Mutable sidecar records carry snapshot assembly.

Tool names remain verbatim. Reused native call IDs produce unpaired results;
no result is assigned to the latest call by position. Usage attributed to a
message or group of calls remains a summary rather than a fabricated inference.
Native values without a standard field remain under the provider's reverse-DNS
metadata namespace. Unsafe integers use lossless JSON text; large content and
metadata use content-addressed JSON blobs. Configuration credential values and
secret-question answers are excluded before metadata or blob publication.

The authored regression fixtures verify mapping semantics, source rewrites,
sidecar placement and missing information. They do not establish version-level
producer conformance; that additionally requires authentic native captures.

Pi uses JSONL session headers through version 3, including the OMP title-slot
variant. Its ASR identity is the header ID. A native parent path establishes
lineage only when the parent's header can be read; matching parent entry IDs
establish copied records. A path or filename is never converted into a session
ID. The newer version 4 lane backend is outside this converter's scope.

Grok Build reads persisted ACP v1 `session/update` and `_x.ai/session/update`
notifications. Adjacent text chunks are folded, and tool patches become
revisions. Tool UI output is `displayContent`; it does not establish what the
model received. Optional xAI response-boundary events provide inference evidence;
older streams without those events have no invented model calls. ACP v1 message
boundaries remain less precise than native message IDs.

DeepSeek Harness reads logical session version 0 in plain `.jsonl` and concatenated
`.jsonl.zstd` files. Its ASR agent name is `com.deepseek.harness` pending registry
registration. Message sources determine authorship; assistant tool blocks and
execution-start events produce one request. Request headers provide instructions
and call settings, while `request/context` supplies route/capacity metadata, not a
complete request. Packed stream rows remain vendor audit records; assembled
messages carry conversation content. Attachment references are retained, but
external attachment bytes are not read. Copied seed prefixes are marked for
accounting. A torn compressed frame can fail the export; the existing bundle
remains available for retry after the native writer finishes.

## Adding a converter

`crates/ctx-history-asr` owns ASR semantics, output identity and bundle publication.
Provider importers own searchable Core projection. Core deliberately excludes
some native data that ASR can retain, including encrypted content and request
context, so an ASR converter reads native sources. It shares the strict JSON
value decoder, source line bound and Codex metadata-owner selection with existing
history code. It must not
reconstruct native facts from rendered Core text.

1. Add a `Provider` variant with the ASR registry's agent name, native format and
   reverse-domain vendor namespace. Add the corresponding CLI provider selection
   and map it to existing source discovery.
2. Extend `source` to recognize the provider's native sources and obtain session
   identity from its documented identity fields. Keep different representations
   of the same source together; distinguish native segments from session IDs.
   Add a native reader at this boundary if the provider does not use JSONL.
3. Implement a provider module with `convert`. Reuse `content`, `tools` and
   `records` only where their semantics match the native contract. The spool
   provides indexed native positions, entity revisions and native-to-ASR record
   references without loading a session into memory. Keep stable positions and
   fixed emission slots for projections that can grow on re-export. Bump the
   converter revision in `bundle.rs` when changing existing mappings, so cached
   sessions are reconsidered.
4. Implement the provider's registry mapping, including provenance, copies,
   branches, usage, capture states and documented `_meta` destinations. Preserve
   native entity IDs verbatim. Do not infer success, authorship, lineage or a
   resolved model from absent fields. Keep schema variants as vendor data when
   their meaning is unknown, and declare concrete losses in the header.
5. Add authored semantic tests and appropriately sanitized native fixtures.
   Exercise ordinary conversion, repeat, append, source rewrite, relocation,
   incomplete tails, malformed lines, duplicate IDs, large content and relevant
   lineage. Label authored fixtures as authored; they are not native captures.
   Test secret-answer removal across vendor data and blobs as well as messages.
6. Run the owning tests and validate generated bundles against the ASR reference
   validator. Check semantic results as well as schema validity: a valid record
   can still misrepresent its source. Measure throughput, peak memory and output
   size with representative large histories.

```sh
scripts/bazelw test //crates/ctx-history-asr:unit_tests --config=dev-linux
node /path/to/agent-session-record/tools/validator/bin/asr-validate.js ./asr --format json
```

The ASR checkout is an optional external conformance oracle, not a dependency of
ctx's build or ordinary tests. Qualification applies to the native versions and
features actually exercised; preserved vendor data alone does not establish a
complete semantic mapping.

## Conversation-family converters

`--provider gemini` accepts native chat JSONL and legacy whole-document JSON.
The latter is read one message at a time; a migrated `.jsonl` sibling takes
precedence over `.json`. Re-appended message IDs retain full replacements with
`supersedes`. `$rewindTo` removes its named message too, so ASR targets the last
remaining unit. The documented `<tool_output_masked>` function-response marker
sets `capture.content.state` to `masked`; changed text alone does not.

`--provider qwen-code` reads UUID/parentUuid chat journals. Native
`parent_session`, `isSidechain`/`agentId` and `forkedFrom` fields establish
separate spawning, ownership and copy facts. Tree branches use the shared
mechanical-sibling rule. GenAI parts, tool responses, usage, titles, rewinds and
compression metadata are preserved. Backend-dependent output-token inclusion
is left unknown where the stored usage does not identify the backend.

`--provider antigravity` reads
`<conversation>/.system_generated/logs/transcript_full.jsonl`, preferring it over
`transcript.jsonl`. The conversation directory supplies identity. This rendered
trajectory dialect does not establish model-facing message content, tool-result
pairing or lineage: display content and capture states preserve that distinction.

These converters retain unsupported native fields in provider `_meta` and
unmapped event kinds as vendor records. The history files do not establish every
inference boundary, hidden instruction, or deleted revision. Gemini context
replacement snapshots and Qwen compressed-history arrays can lack the identities
needed to recover exact retained context; their native data and declared losses
are retained. Qualification is limited to the formats exercised by semantic tests
and existing fixtures, and is not a claim of coverage for every provider release.
