# Search

`ctx search` finds agent-history records indexed into Core. The v0.26 search
epoch has an immutable lexical generation and an optional semantic sidecar:

- immutable Core/Tantivy generations under `search/lexical`, containing complete
  policy-selected normalized records and source identities;
- flat-F32 semantic projections under `search/semantic`;

The Core/Tantivy generation contains policy-selected meaningful text, complete
normalized records, and the metadata needed to match indexed events. Search
snippets and typed event/session presentation come from those stored records.

Exact ctx history-retrieval invocations and their uniquely linked, successful,
payload-only results are retained as complete Core records but excluded from
ranked discovery. This prevents a later search from ranking an earlier `ctx
search`, `show`, `list events`, or `locate` payload as if it were the
underlying agent history. Classification is syntax- and linkage-based, not a
text regex: failed-result diagnostics, warnings, stderr, mixed records, unknown
status, and ambiguous results remain searchable. Direct `show` and event
enumeration still return the complete excluded records.

Ordinary results include primary and subagent sessions. When history carries an
exact root-session claim, ctx groups sessions by that claim and returns one best
result per root task before repeating a root; a session without that claim is
its own group. Agent scope is result metadata or an explicit filter; it does
not silently rerank relevance. Human output uses only the pluralized result
count in the heading. Each result has separate `Event` and `Time` rows showing
the short ctx event ID and the matched event's exact UTC RFC 3339 millisecond
timestamp; an indexed event without a timestamp says `time unavailable`. These
timestamps do not change result ordering.

## Search examples

```bash
ctx search "build failure"
ctx search "storage layout" --provider codex
ctx search "release checklist" --source-root personal --source-root archive
ctx search "deployment failure" --source-group work
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search "permission denied" --content-scope outputs
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --refresh off
ctx search "signed metadata" --term checksum --term release
ctx search "token budget" --limit 5
ctx search "token budget" --session <ctx-session-id>
ctx search "token budget" --exclude-session <ctx-session-id-or-prefix>
ctx search "human decisions" --primary-only
ctx search "this current task" --include-current-session
ctx search "mail provider throttled bulk mailbox setup" --backend hybrid
ctx search "pricing decisions from the launch review" --backend semantic
ctx status --format json
ctx index
```

A result can include:

- ctx-owned event and session IDs;
- the provider-owned session ID when known;
- the exporter-declared `provider_key` and `source_id` for custom history
  sources; human output labels these results as `provider_key/source_id`;
- title, Core-backed snippet, one-based final rank, and result scope;
- the backend-provided `retrieval_score`, which is diagnostic and can be
  non-monotonic after query-coverage and family shaping;
- compatibility session importance and the additional-match count for session
  results; like `retrieval_score`, session importance is not an ordering contract;
- provider, event sequence, timestamp, workspace, and working directory;
- stable ctx citations;
- copyable `suggested_next_commands` for `show` and scoped search.

Search result IDs are ctx-owned. Commands accept complete IDs or unambiguous
prefixes of at least eight hex characters. Provider-owned IDs are metadata;
provider lookup must be explicit.

After filters and active-session exclusion, default search selects one exact
event champion per exact session. It then reads the generation-owned direct
claims coalesced across that session and diversifies in stable rounds by the
literal provider root when one was claimed; a session with no root claim is
its own ranking family. Search does not infer roots by walking parents, build
copy components, or collapse similar content. `--events` and explicit
`--session` searches remain dense and skip this shaping.

`--verbose` keeps the complete event and session IDs and additionally shows the
stored event sequence plus available workspace/working-directory, branch,
agent, and session-lineage context. Equal or redundant context values are shown
once. Paths remain verbose-only.

## Filters

Search filters narrow text and JSON output:

- `--provider <provider>`;
- `--history-source <provider_key/source_id>` for the canonical custom route
  identity;
- `--provider-key <key>`, `--source-id <id>`, and
  `--source-format <format>`;
- repeatable `--source-root <configured-name>`;
- repeatable `--source-group <configured-group>`;
- `--workspace <name-or-path>`;
- `--since <rfc3339-or-days>d`;
- `--event-type <event-type>`;
- `--content-scope all|transcript|calls|outputs`;
- `--file <path>`;
- `--session <ctx-session-id-or-prefix>`;
- repeatable `--exclude-session <ctx-session-id-or-prefix>`;
- repeatable `--term <query-or-keyword>`;
- `--events`;
- `--primary-only`;
- `--limit <n>`;
- `--backend hybrid|semantic|lexical`;
- `--semantic-weight <0.0-1.0>`;
- `--refresh background|off|wait`;
- `--include-current-session`.

`--since` accepts RFC 3339 timestamps or a day window such as `30d`.
`--file` searches normalized touched-file metadata; it does not inspect the
current filesystem. Repeatable `--term` values broaden the query with OR-style
semantics rather than acting as required terms.

Root and group selectors use the exact case-sensitive names reported by
`ctx sources`; each name is 1 to 64 ASCII letters, digits, hyphens, or
underscores. Repeated root names, repeated groups, and a request containing
both kinds form one OR selection set. That set intersects with independent
provider, source-identity, workspace, time, event, file, session, and agent
filters, so those filter classes combine with AND semantics. Selectors resolve
only against the pinned Core generation. An unknown root or group fails the
request instead of being ignored or resolved from newer live configuration;
the rejection diagnostic does not echo the selector contents.
Omitting both selector kinds searches every source in the generation,
including automatic sources that have no configured root name.

JSON `query` echoes the normalized positional query and repeatable-term
alternatives, trimming surrounding whitespace and joining nonempty alternatives
with ` OR ` in argument order. Suggested scoped-search commands preserve the
positional and `--term` argument shape and safely quote each value. They also
preserve a non-default data root with a shell-quoted `ctx --data-root <path>`
prefix.

Search requires a nonempty query, at least one nonempty `--term`, or
`--file <path>`. Other filters only narrow an actual search.

`--content-scope` cannot be combined with `--event-type`, even when the exact
event type belongs to the selected content scope. Use the exact event-type
filter or the class-aware content scope, not both.

Ordinary search uses the all-agent, root-diverse behavior described above. Use
`--primary-only` only when a deliberately narrow search should exclude
subagent work.

Direct CLI searches automatically exclude the current session tree for Codex,
DeepSeek Harness, Grok Build, Pi, Claude Code, Goose, Hermes, Shelley, Qwen
Code, and Mux when the current session can be identified unambiguously.
Unsupported or ambiguous detection fails open: ctx leaves the history
included. `--include-current-session` restores the automatically excluded
tree. Repeat `--exclude-session <ctx-uuid-or-unambiguous-prefix>` to exclude
exact named sessions; the option is repeatable and conflicts with `--session`.
MCP searches do not automatically exclude the caller's session.

`--limit` defaults to `20` and is capped at `200`. Ordinary search returns
one result per root task before repeating a root. Use `--session` for dense
hits inside one session or `--events` for dense event hits across sessions.

## Content scopes

Content scope is a query-time selection over existing searchable events. The
default resolves to `all`, so omitting `--content-scope` and passing
`--content-scope all` have identical retrieval behavior.

| Scope | Searchable event types | Lexical weight within the scope |
| --- | --- | --- |
| `all` | `message` | `1.0` |
| `all` | `summary` | `0.9` |
| `all` | `tool_call`, `command_started` | `0.8` |
| `all` | `tool_output`, `command_output`, `command_finished` | `0.6` |
| `all` | any other or future searchable event type | `0.8` |
| `transcript` | `message`, `summary` | `1.0`, `0.9` respectively |
| `calls` | `tool_call`, `command_started` | ordinary lexical strength (`1.0`) |
| `outputs` | `tool_output`, `command_output`, `command_finished` | ordinary lexical strength (`1.0`) |

The relative message/summary weighting is therefore preserved in
`transcript`, while a class-specific calls or outputs search does not carry
over the downweighting used to mix that class into `all`. Class-aware search
does not infer diagnostic importance and does not automatically collapse
events with duplicate text.

MCP invocation terms keep the class of the record that carries them. Separate
Warp and Copilot CLI invocation records are calls. A combined Codex terminal
`tool_output` remains an output, including when its searchable body projection
contains the invocation server, tool, or arguments. The record is never
dual-classified as both a call and an output.

Changing content scope does not alter retained or indexed bodies, Core schema,
or index generations, and it does not require an index rebuild. Search still
uses semantic evidence only for transcript messages: `all` and `transcript`
retain normal semantic/hybrid behavior, while `calls` and `outputs` make a
hybrid request explicitly fall back to lexical retrieval with structured
diagnostics. Semantic-only calls/outputs requests fail with a typed unsupported
scope error instead of returning a misleading empty result. Search still
matches and returns the same complete policy-selected records; only query-time
event eligibility and lexical weighting change.

## Retrieval backends

`--backend lexical` queries the active Tantivy generation using BM25-style
lexical ranking. Result rendering reads the corresponding imported Core
events.

`--backend semantic` queries the flat-F32 generation under
`search/semantic`. Semantic projection enumerates eligible imported Core
records, filters control messages, then chunks and embeds them. The semantic
generation stores vectors, hashes, offsets, and generation binding rather than
plaintext transcript chunks. Readiness accounts for all pre-content-filter Core
candidates: each candidate is either an acknowledged active flat-F32 event or
an intentionally filtered event. Metadata filters are then applied to the
acknowledged active flat-F32 events; a query does not require intentionally
filtered Core matches to have vectors.

`--backend hybrid` blends lexical and semantic evidence with reciprocal-rank
fusion. `--semantic-weight` controls the semantic contribution and defaults to
`0.35`. A zero semantic weight is exactly lexical retrieval: ctx does not
contact the semantic query service, initialize a model, open or scan a vector
generation, or perform any other vector work. Hybrid uses semantic evidence
only when the semantic generation is bound to the active lexical generation,
coverage is complete, and pending dirty work is drained. When semantic is
disabled or otherwise unavailable, hybrid may return lexical results with a
structured fallback reason.

The production embedding model is
`intfloat/multilingual-e5-small` with 384-dimensional vectors. Queries use the
E5 `query: ` input contract and document chunks use `passage: `. The model,
chunking, source projector, and lexical generation policies participate in
generation identity, so incompatible derived data is rebuilt rather than
silently reused. Both document and query embedding stay local; external and
bring-your-own embedding services are not part of semantic search.

Enable local semantic search with:

```bash
ctx semantic enable
ctx semantic status
```

Configuration is sparse. With no `[semantic]` section, semantic search is
disabled and indexing intensity is `"quiet"`. The canonical opt-in is:

```toml
[semantic]
enabled = true
```

Semantic document indexing supports only `quiet` and `full`. Quiet is the
background-friendly default. For persistent full behavior, add
`indexing_intensity = "full"` under `[semantic]` in `config.toml`; this version
has no persistent CLI setter. Full removes deliberate inter-batch pacing but
continues to obey safety, resource, and admission limits.

Intensity applies to semantic document construction during initial backfill,
incremental refresh, rebuild/recovery, daemon reconciliation, and finite
foreground reconciliation. It does not affect interactive query embedding,
lexical indexing, or embedding identity/readiness. It is distinct from
`[indexing] mode`, which selects automatic or manual indexing ownership.

In automatic indexing mode, use full intensity only while waiting for the
current projection:

```bash
ctx semantic enable --wait --intensity full
```

The temporary intensity does not rewrite `config.toml` and expires when the
wait ends, the waiting command crashes, or the daemon restarts. Inspect both
configured and effective intensity with `ctx semantic status`.

Semantic opt-in is independent of indexing mode. In auto mode, the daemon keeps
the semantic projection caught up. For explicit manual synchronization, use the
existing mode control and wait refresh:

```bash
ctx index mode manual
ctx search "pricing decisions" --backend semantic --refresh wait
```

Lexical search remains available while embeddings build. When semantic coverage
is ready, the default hybrid backend uses lexical and semantic evidence together
automatically.

Only a manual CLI `--refresh wait` that actually needs semantic evidence may
acquire the opted-in local model, initialize semantic storage, and perform
foreground semantic catch-up. Explicit semantic search reports a typed local
error when model, runtime, or generation convergence fails; it never silently
changes a semantic-only request into lexical retrieval. Hybrid remains
lexical-safe in those cases.

## Refresh and freshness

`--refresh background` is the default. In auto mode, search health-checks
and, when needed, wakes or recovers the persistent daemon. In manual mode it
does not contact, start, or wake a process. Both serve the latest committed
lexical generation without waiting for optional semantic indexing.
The daemon owns bounded provider discovery, source refresh, immutable
candidate-generation construction, publication, and ordinary opted-in semantic
catch-up. Manual wait is the sole query-process exception: after finite Core
publication it may reconcile only the semantic projection for that exact pinned
generation.

On a fresh auto-mode root, background mode asks the daemon to publish the first
lexical generation. In manual mode, search performs no hidden bootstrap or
fallback import and can query only an already committed generation. Enabled
auto-refresh history-source plugins run through the same
daemon-owned, bounded Core refresh route; explicit-only sources still require
an explicit import.

`--refresh wait` wakes the persistent daemon in auto mode or starts a
finite Core worker in manual mode, then waits for the requested source frontier
and lexical-generation receipt. It fails with a typed source, lag, or system
error when that receipt cannot publish; it does not fall back to a foreground
importer. In manual mode, a semantic or nonzero-weight hybrid request then fully
reconciles semantic coverage for that same pinned Core generation and uses the
same foreground model runtime to embed the query. Lexical, zero-weight hybrid,
and unsupported semantic scopes do no semantic model or projection work.

`--refresh off` queries the currently published generations without provider
discovery, plugin execution, refresh scheduling, semantic catch-up, or model
download. It renders results from the active Core generation and is read-only
with respect to ctx indexes.

Only sources with supported automatic import participate in automatic refresh.
Explicit-only sources require `ctx import --provider ... --path ...`.
Winner-only provider precedence prevents combining a selected replacement with
stale defaults.

`ctx status` and search JSON report lexical generation, refresh state, semantic
generation binding and coverage, daemon work, supervisor health, and typed
fallback reasons. `ctx index` shows the smaller one-shot indexing status view;
`ctx index watch` follows it and `ctx index wait` blocks for readiness.

## Core-backed presentation

Search snippets come from the Core-backed searchable projection for complete
policy-selected records in the active verified Core/Tantivy generation. Full
show/list/MCP event output retrieves the exact stored Core content; it does not
rewrite the normalized body to include projected invocation text. Query-time
reads do not reopen provider history. Provider changes become searchable and
visible to show after explicit import or daemon refresh publishes a new Core
generation. `ctx show session` preserves provider event order.

For discovery-eligible selected content, the shared Core search projection
appends retained activity invocation protocol, server, tool, and present
arguments; result status, present text, and present structured content; and
literal fact values after the event body and provider-native structured
content. These values participate in ordinary lexical matching, ranking,
snippets, and semantic source text. A result using the
`normalized_body` capture disposition relies on the event's ordinary body,
which enters the projection exactly once.

Provider call identity, timestamps, durations, and capture-disposition labels
do not enter the search projection. Activity adds no dedicated filter,
selector, search result field, or SQL column; search sees one shared text
projection rather than separately addressable activity fields.

Use log-mode `ctx show session`, `ctx show event`, or `ctx list events
--content full` and filter JSON/JSONL rows client-side when exact provider
activity is needed. Query paths do not reopen provider history or issue hidden
network requests. Activity remains private local content, but matching queries
and snippets can surface its searchable values. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

## History reports

Use the agent history-search skill when a topic needs a cited synthesis rather
than a ranked list. The skill runs several searches, inspects cited events or
sessions with `ctx show`, and writes the report; ctx itself retrieves local
evidence.

## Machine output

Use text output for agent reading and `--format json` for scripts. JSON includes
the same result metadata and citations plus:

- `freshness`, describing refresh mode and outcome;
- `retrieval`, describing requested/effective backend, lexical generation,
  semantic status/fallback, coverage, and timing/scan diagnostics;
- `generated_at`, the RFC 3339 UTC render time;
- `result_window`, with `limit`, `returned`, and `more_available`;
- independent candidate-pool truncation metadata.

`more_available` is true only when the bounded search pass finds one additional
fully shaped result beyond the requested limit: a distinct session by default,
or an event with `--events`. Search does not run a second count scan or expose a
continuation cursor. Text output ends with exactly
`More results available.` only when that shaped sentinel exists.
Candidate-pool truncation remains separate and does not by itself set
`more_available`.

Raw output can contain queries, absolute paths, complete snippets, provider
metadata, and transcript-derived content. Treat it as private local data and
review it before sharing.
