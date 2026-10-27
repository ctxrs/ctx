# ASR export

`ctx export asr` converts native history into an [Agent Session Record](https://agentsessionrecord.org) 0.1 archive bundle. Converters cover Claude Code, Codex, Cline/Roo task directories, and fx transactional schema-v3 logs.

```sh
ctx export asr --all --out ./asr
ctx export asr --all --provider codex --out ./codex-asr
ctx export asr --session NATIVE_SESSION_ID --provider claude --out ./session-asr
ctx export asr --all --provider codex --path ./rollouts --out ./asr
```

Automatic input selection uses ctx's existing provider-root discovery. `--path`
selects a native transcript or directory and requires `--provider`. Session IDs
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

## Task captures and transactional fx logs

```sh
ctx export asr --all --provider cline --namespace my-stable-installation --path ./tasks --out ./cline-asr
ctx export asr --all --provider roo --namespace my-stable-installation --path ./tasks --out ./roo-asr
ctx export asr --all --provider fx --path ./sessions --out ./fx-asr
```

Cline and Roo task IDs require an installation/profile namespace. Choose it once
and reuse it when moving the same native task tree or making a fresh export.
API arrays, UI arrays and task metadata retain their own order; timestamps never
join separate stores. UI-only text stays display-only. Whole-document changes
publish new captures with an observation timestamp. Unique native identities
(including timestamp-keyed UI rows) remain stable; positional rows and collisions
are marked capture-scoped. Sequence numbers apply only within each capture.

fx schema-v3 export validates authority, log generation, committed sequence and
byte watermark through the native replay reader. Uncommitted tails are excluded.
It preserves the committed event log as audit evidence, maps configuration and
usage events at their native positions, then emits current history/state as a
snapshot. A replacement does not create invented message revisions. Existing
native fx 0.0.6 fixtures qualify tool-free and read-file scenarios; task-array
regressions are authored semantic cases. This cursor does not cover newer Cline
SDK stores, fx schema-v4 conversation logs or legacy fx snapshot-only stores.
Marked secret-question answers are removed before task/native blob projection.
Unknown facts and source values are retained under provider metadata; preserved
native data does not imply full semantic mapping. Non-UTF8 content and outside-task
state/artifacts have declared losses. fx also retains the native replay limits
(512 MiB committed bytes and 256 MiB aggregate strings/replacement state).
