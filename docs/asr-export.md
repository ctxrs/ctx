# ASR export

`ctx export asr` converts native history into an [Agent Session Record](https://agentsessionrecord.org) 0.1 archive bundle. The 43 converters cover Claude Code, Codex, Pi, Grok Build, DeepSeek Harness, Gemini CLI, Antigravity, Qwen Code, Copilot CLI, Droid, Kimi, Qoder, Junie, Mistral Vibe, Tabnine, Mux, Cursor, OpenCode, Kilo, MiMoCode, Kiro, Crush, Goose, Lingma, Warp, Hermes, AstrBot, Shelley, Zed, Firebender, ForgeCode, DeepAgents, Devin, OpenClaw, NanoClaw, CodeBuddy, Continue, OpenHands, Auggie, RovoDev, Cline, Roo and Fx.

```sh
ctx export asr --all --out ./asr
ctx export asr --all --provider codex --out ./codex-asr
ctx export asr --session NATIVE_SESSION_ID --provider claude --out ./session-asr
ctx export asr --all --provider codex --path ./rollouts --out ./asr
ctx export asr --all --provider deepseek-harness --path ./sessions --out ./asr
```

Automatic input selection uses ctx's existing provider-root discovery. `--path`
selects a native transcript, database, or directory and requires `--provider`. Session IDs
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
produce ambiguous result linkage with `nativeCallId` and no paired `callId`. Display output and raw sidecars remain
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
JSONL sources are skipped; mixed SQLite stores are reread to capture current rows and lane facts. A changed session is spooled again; the exporter does not
assume native files only append. Source positions are rebuilt from each capture;
the destination's earlier observation order is never transcript order. Codex IDs derive from native
segment, physical line, and emission slot; native ordinals can repeat on distinct
rows and remain provenance at `com.openai.codex/position` alongside the line.
Segment headers provide creation order; ties use
segment identity as a deterministic serialization and declare `order`. Keep this database with the destination when continuing an export.
When the new stream has the previous stream as an exact prefix, only its suffix
is appended. An incomplete output tail is removed before appending. Claude response growth appends full message and inference replacements with
`supersedes` and growing folded `assembly.sourceIds`; existing bytes remain unchanged.
The latest observation of a repeated UUID keeps its first-seen position before
distinct response blocks are folded. Rewritten or deleted native positions,
other changed snapshots, and converter changes publish a replacement capture
with `observedAt`; earlier streams remain available. The manifest selects
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
metadata namespace. Unsafe native numbers use `<agent-namespace>/jsonText` keyed by the displaced
JSON Pointer, `inputText` for tool arguments, or whole-value JSON blobs. Required
numeric fields cannot use text substitutes. Large content and metadata also use
content-addressed JSON blobs. Configuration credential values and
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

## SQLite conversations

`--provider opencode|kilo|mimocode|kiro` selects a native SQLite converter.
OpenCode, Kilo and MiMo share verified message/part and sequenced-message storage
shapes. A single read transaction includes committed WAL state; bounded rows
are staged to disk before conversion. The logical-content digest detects updates
and deletions even when a writer keeps the same row IDs and modification times.
SQLite no-op exports still read and stage the selected source contents. Their
`source_bytes` counter measures staged JSON envelope bytes for converted sessions,
not database file size or total discovery I/O.

Native message, part and call IDs remain verbatim. A changed part is a snapshot
of its current state, with no invented pending/running/completed history. Rewrites
publish another stream snapshot while retaining earlier exported files. Kiro
history-array positions carry `idScope: capture`, including in UUID-keyed v2
conversations, because the native document is rewritten as a whole.

Kiro `conversations_v2` and legacy `conversations` rows prefer the native
`conversation_id`, including one nested in the JSON value. Only older rows
without that UUID use their native working-directory key as the session ID and
require an explicit, stable installation/profile namespace:

```sh
ctx export asr --all --provider kiro --namespace my-kiro-installation --path ./data.sqlite3 --out ./asr
```

Reuse that namespace for the same installation across exports and relocations.
The destination persists it in `.ctx-asr-namespace` and reuses it when subsequent
commands omit `--namespace`. Keep this state with a relocated export. A conflicting
namespace requires a separate destination. For Kimi and Copilot child conversations,
installation and native container scope compose as compact JSON text
`[installation, container]`; Kimi `main` remains its own conversation.
It is not an input-file path or an output-directory identity. The v3.2 identity
migration retains the directory key as provenance and does not assert continuity
between earlier directory identities and UUID identities. Kiro ACP/v3 session
stores and older Kilo editor-extension task directories are separate formats.

The native row bound defaults to 16 MiB, controlled by `--max-record-bytes`.
An oversized row with a usable session identity yields a `conversion_loss` gap;
this bounds a whole Kiro conversation document, not each history entry. OpenCode
text folded across many parts can spill to a blob. Source facts not projected
into standard records remain in provider metadata/vendor records with declared
losses. In particular, session-level revert boundaries, unassociated Kiro display
history/context state, and alternate migration representations need their vendor
metadata to interpret completely. This is not a claim of complete semantic
qualification for every provider release. Public tests identify authored inputs;
fixture validation and version qualification are distinct.

## SQLite converters

`--provider firebender|forgecode|deepagents|devin` accepts a native SQLite
file with `--path`, or uses the existing provider discovery rules. SQLite
sources are copied through ctx's read-only source-snapshot facility; exports
never create WAL or SHM files beside the source. Each session is a capture:
stable native IDs survive replacements, while array positions without native
IDs are marked `idScope: capture`. `observedAt` stays unchanged on a logical
no-op, including relocation. The byte limit applies to the whole SQLite row;
an oversized conversation becomes a gap rather than a partial transcript.

These converters use reverse-DNS agent names until their short names are
registered. Their native fields are retained in provider `_meta` values (large
values use blobs). Global schema and application-state rows are retained under
manifest `_meta["rs.ctx/sqliteApplicationState"]`, including on a single-session
export; they are not assigned to a session. Independent database reads do not
form one atomic capture. Native identities that collide across databases must
be exported separately.

Deep Agents projects chat only from the root namespace's `writes.messages`
channel, including message-ID revisions and removal deltas; checkpoints and
other channels remain native state. Non-root namespaces and Firebender child
conversations are preserved as vendor records because they lack separate
native session IDs. Devin retains the complete message forest, applies the
mechanical-sibling rule and selects `main_chain_id` as primary head. A null or
missing main head leaves the forest as audit data. Compaction retains both the
old forest and regenerated prefix with an unknown replacement range; exact
reconstruction of Devin's reinserted transcript is not claimed. Repeated Devin
request IDs remain native evidence without duplicated usage attribution.

Qualification is limited to the retained fixtures and authored semantic tests.
Firebender's fixture is schema-derived, Deep Agents' fixture was written by
AsyncSqliteSaver, and Devin's durable background heads have only authored
coverage. These are conversion capabilities, not current-harness certification.

## Document and event-store converters

Select these converters with `--provider codebuddy`, `continue`, `openhands`,
`auggie` or `rovodev`. CodeBuddy accepts CLI JSONL and IDE session catalogs with
message files; Continue accepts session history JSON; Auggie accepts exchange
snapshots; Rovo Dev accepts `session_context.json` with optional `metadata.json`;
OpenHands accepts SDK conversation directories or individual event paths.
Legacy `v1_conversations` trees are acquired, but unrecognized event variants
remain vendor records. Native format coverage is limited to the shapes exercised
by the converter tests and available fixtures; it is not a claim of complete
coverage of every producer release.

Changed documents and event-store captures produce a new immutable stream with
`observedAt`. Native message/event IDs keep record identities across captures;
source positions without stable IDs carry `idScope: "capture"`. Mutable session
metadata and state snapshots follow the recorded history. Independent sources
with a colliding session ID require separate bundles. Unknown tool rendering,
model labels and lineage are retained as native metadata without inferred links.
OpenHands condensation replaces individual forgotten entries; it never assumes
that the forgotten set is a prefix.

Document arrays are read one item at a time. The size limit applies to each
item and retained document metadata; an oversized document value stops that
capture without publishing a partial session. Large image/audio content moves
to binary blobs, and unsafe JSON numbers remain exact JSON text. Whole native
items accompany projections under provider metadata keys; unknown variants are
retained with declared semantic loss.

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
node /path/to/agent-session-record/tools/validator/bin/asr-validate.js ./asr --streaming --format json
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

## Shared conversion rules

`ctx-history-asr-core` owns the provider catalog, bounded spool, privacy filtering,
record identities and serialization. `ctx-history-asr-database` owns native
database acquisition and projection. `ctx-history-asr` owns journal conversion,
discovery integration and capture publication.

All sequences are increasing safe integers local to one capture. Mutable
side-state projections follow recorded history with snapshot assembly; ordering
across stores never claims timestamp chronology. Duplicate native call IDs leave
results unpaired, with the recorded ID in provider-qualified `unpairedCallId`
metadata. Missing external session, turn, or call targets are retained for the
reference validator's warning-class resolution; structural targets are never
invented to silence a diagnostic.

Unsafe integers retain their exact JSON spelling: tool inputs use `inputText`
without `input`, content and raw values use JSON blobs when needed, and other
values use `rs.ctx/unsafeJsonText` (field to JSON string) or
`rs.ctx/metadataJsonText` (the full metadata object as JSON text). Decoded image
and audio data over 64 KiB use binary blobs. Codex host-cell IDs are metadata,
not parent calls; a nearby `new_context` call does not establish a compaction
trigger. Unknown enum values remain native metadata.

## Accounting and representation policy

The v3.2 converters keep source-defined transcript positions, including native
array order and qualified SQL reader order. Mutable settings and side-store
snapshots follow the transcript. Arbitrary SQL ordering only determines snapshot
serialization. Record equality across replacement captures does not establish
historical chronology.

Stored tool output is `raw` unless the supported format explicitly identifies the
model-facing representation. That exception covers Claude transcript result blocks,
Codex function/custom output items, Copilot `result.content`, Qoder transcript blocks
(excluding raw/UI sidecars), Shelley model messages, Gemini function responses,
Kiro tool-use results, and Cline/Roo API histories. Other converters declare missing
`tool_result.content`, retaining raw/native and display evidence. This policy does
not qualify untested versions or infer model delivery from storage alone.

Accounting selects native identities and source contracts, never equal counts,
model names or nearby timestamps. Effective revisions replace earlier observations
of the same report; readers must not sum revision history or add cumulative totals
to per-call reports. Unknown inclusion of cache or child usage stays unknown.

| Agent | Selection policy |
| --- | --- |
| Claude Code | Last effective usage for a native response ID after folding its blocks; cost-state remains a separate cumulative session report. Synthetic/uncompleted placeholder zeros stay native; explicit completed zero reports remain values. |
| Codex | Response accounting identity and effective native reports; snapshots and cumulative totals stay distinct from per-response usage. |
| Pi | Assistant response report, with derived accounting identity only where the message contract supports it; error/aborted all-zero placeholders stay native. Compaction reports remain separately scoped summaries. |
| Copilot | `apiCallId` per-call reports; the native cost multiplier is never USD. |
| Kimi | Qualified request/step identity; final completion revises its request. Unattributed usage stays native. |
| OpenCode, Kilo, MiMoCode | Native completed step/response evidence; default all-zero unfinished counters stay native. |
| Goose | Prefer session ledger entries; without a ledger use qualified message reports. Retain overlapping message usage natively. Current-context and accumulated snapshots are separate counters; nullable native counters preserve reported zeros. Partial ledger coverage declares `usage`. |
| OpenHands | Unique response IDs join event and Metrics reports; duplicated/unjoinable reports remain native. Accumulated metrics remain cumulative summaries. |
| Cline, Roo | API request/report identity where present; UI request usage stays distinct from cumulative task state. |
| ForgeCode, DeepAgents, Gemini, Qwen Code, Mistral Vibe, Mux, Tabnine, Fx | Project only the format's explicit call, message, or aggregate reports at their recorded scope; preserve unjoinable native components without attributing them to another call. |
| Crush, Hermes, AstrBot, Shelley, Zed | Explicit ledger/message or current-state accounting at its native scope; current totals do not establish individual model calls. Crush all-zero schema defaults stay native. |
| OpenClaw | Native response/run accounting only with explicit identity; backing-harness and unjoined reports remain native with declared incomplete coverage. |
| Grok Build, DeepSeek Harness, Droid, Qoder, Junie, Antigravity, Cursor, Kiro, Lingma, Warp, Firebender, Devin, NanoClaw, CodeBuddy, Continue, Auggie, RovoDev | Retain explicit native usage as available; absent or unqualified accounting stays unknown. No report is manufactured from message counts, defaults, or matching timestamps. |

Fx current-state history totals initialized to zero stay native until a nonzero total is recorded; explicit event reports retain zero values.

Unassigned lane facts use manifest `_meta["<agent-namespace>/unassigned"]`, each
with `source`, optional `namespace`/`lane`, and one of `record` or `blob`. Source
selection includes these facts independently of the selected sessions, including
an empty session subset. They are not copied to every session, and unknown
ownership alone is not a loss. Unattributed source-file counts remain in the
command's summary. OpenClaw session filtering still reads other sessions to
resolve run ownership before collecting lane facts; this keeps selection from
changing ownership and adds source reads for filtered exports.
