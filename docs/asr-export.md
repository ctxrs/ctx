# ASR export

`ctx export asr` converts native history into an [Agent Session Record](https://agentsessionrecord.org) 0.1 archive bundle. Converters include Claude Code, Codex, OpenCode, Kilo Code, MiMo Code, and Kiro CLI SQLite conversations.

```sh
ctx export asr --all --out ./asr
ctx export asr --all --provider codex --out ./codex-asr
ctx export asr --session NATIVE_SESSION_ID --provider claude --out ./session-asr
ctx export asr --all --provider codex --path ./rollouts --out ./asr
```

Automatic input selection uses ctx's existing provider-root discovery. `--path`
selects a native transcript, database, or directory and requires `--provider`. Session IDs
are native IDs, not ctx citation IDs; Claude subagents use their `agentId`.
Export reads provider files without importing them into the search store.
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
not read the live UI databases, global scheduler definitions or deleted backups.

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

Legacy Kiro `conversations` rows use their native working-directory key as the
session ID, as specified by the current ASR registry. They require an explicit,
stable installation/profile namespace:

```sh
ctx export asr --all --provider kiro --namespace my-kiro-installation --path ./data.sqlite3 --out ./asr
```

Reuse that namespace for the same installation across exports and relocations.
It is not an input-file path or an output-directory identity. The legacy JSON may
also contain a conversation UUID; it is retained as provider metadata rather than
silently overriding the registry's directory-key rule. Kiro ACP/v3 session stores
and older Kilo editor-extension task directories are separate formats.

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
