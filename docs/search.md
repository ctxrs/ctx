# Search

`ctx search` finds agent-history records while leaving provider sources
authoritative for content. The v0.26 search epoch has three independent,
rebuildable consumers:

- Tantivy lexical generations under `search/lexical`;
- flat-F32 semantic generations under `search/semantic`;
- the optional read-only SQL metadata projection in `relational.sqlite`.

The lexical index contains the full policy-selected meaningful text as
indexed-only terms. It stores identity, filter, ordering, citation, and exact
typed-locator metadata, but it stores no message body or display preview.
Search snippets are hydrated from the exact provider source through the
resolver bound to the active lexical generation. A stale, changed, missing, or
unsupported source produces a typed unavailable/stale result and schedules
refresh where applicable; ctx never substitutes an old database row or an
index-stored body.

Default results are session-diverse: ctx shows the strongest matching event
from each session, then lets you drill into dense event-level results.

## Search examples

```bash
ctx search "build failure"
ctx search "storage layout" --provider codex
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --refresh off
ctx search "signed metadata" --term checksum --term release
ctx search "token budget" --limit 5
ctx search "token budget" --session <ctx-session-id>
ctx search "review findings" --include-subagents
ctx search "this current task" --include-current-session
ctx search "mail provider throttled bulk mailbox setup" --backend hybrid
ctx search "pricing decisions from the launch review" --backend semantic
ctx status --format json
```

A result can include:

- ctx-owned event and session IDs;
- the provider-owned session ID when known;
- title, hydrated snippet, rank, result scope, and match reasons;
- session importance and the additional-match count for session results;
- provider, event sequence, timestamp, workspace, and working directory;
- source path/cursor metadata and citations;
- copyable `suggested_next_commands` for `show`, `locate`, and scoped search.

Search result IDs are ctx-owned. Commands accept complete IDs or unambiguous
prefixes of at least eight hex characters. Provider-owned IDs are metadata;
provider lookup must be explicit.

## Filters

Search filters narrow text and JSON output:

- `--provider <provider>`;
- `--history-source <plugin/source-or-provider_key/source_id>`;
- `--provider-key <key>`, `--source-id <id>`, and
  `--source-format <format>`;
- `--workspace <name-or-path>`;
- `--since <rfc3339-or-days>d`;
- `--event-type <event-type>`;
- `--file <path>`;
- `--session <ctx-session-id-or-prefix>`;
- repeatable `--term <query-or-keyword>`;
- `--events`;
- `--include-subagents`;
- `--limit <n>`;
- `--backend hybrid|semantic|lexical`;
- `--semantic-weight <0.0-1.0>`;
- `--refresh background|off|wait`;
- `--include-current-session`.

`--since` accepts RFC 3339 timestamps or a day window such as `30d`.
`--file` searches normalized touched-file metadata; it does not inspect the
current filesystem. Repeatable `--term` values broaden the query with OR-style
semantics rather than acting as required terms.

Search requires a nonempty query, at least one nonempty `--term`, or
`--file <path>`. Other filters only narrow an actual search.

Default search excludes subagent sessions so primary human-agent intent stays
prominent. Use `--include-subagents` for implementation details, reviews, test
output, and failure analysis. When `CODEX_THREAD_ID` is available, ctx also
excludes the active Codex session tree by default; use
`--include-current-session` to include it.

`--limit` defaults to `20` and is capped at `200`. Default search returns one
diverse result per session. Use `--session` for dense hits inside one session
or `--events` for dense event hits across sessions.

## Retrieval backends

`--backend lexical` queries the active Tantivy generation using BM25-style
lexical ranking. The meaningful body is indexed in full but is not stored in
Tantivy. Result rendering hydrates exact provider bytes in a bounded,
source-grouped batch.

`--backend semantic` queries the flat-F32 generation under
`search/semantic`. Semantic projection enumerates eligible records from lexical
metadata, hydrates exact provider content through the generation-bound
resolver, filters control messages, then chunks and embeds it. The semantic
generation stores vectors, hashes, offsets, and generation binding rather than
plaintext transcript chunks.

`--backend hybrid` blends lexical and semantic evidence with reciprocal-rank
fusion. `--semantic-weight` controls the semantic contribution and defaults to
`0.35`. Hybrid uses semantic evidence only when the semantic generation is
bound to the active lexical generation, coverage is complete, and pending
dirty work is drained; otherwise it returns lexical results with a structured
fallback reason.

The production embedding model is
`intfloat/multilingual-e5-small` with 384-dimensional vectors. Queries use the
E5 `query: ` input contract and document chunks use `passage: `. The model,
chunking, source projector, and lexical generation policies participate in
generation identity, so incompatible derived data is rebuilt rather than
silently reused.

Search does not download models, initialize semantic storage, or perform
foreground semantic catch-up. Explicit semantic search reports a local error
when its cached runtime/model or compatible generation is unavailable. Hybrid
remains lexical-safe in those cases.

## Refresh and freshness

`--refresh background` is the default. With daemon maintenance enabled, search
serves the published generation while the daemon owns bounded provider
discovery, source refresh, immutable candidate-generation construction,
publication, relational catch-up, and semantic catch-up. The daemon retains
the catalog and exact resolver bound to the published generation; query-time
rendering does not rediscover providers.

On a fresh root, search may perform a bounded foreground lexical bootstrap. If
daemon maintenance is disabled, background mode uses the bounded foreground
source-refresh path for supported discovered providers and enabled automatic
history-source plugins.

`--refresh wait` performs foreground source refresh and fails if source-level
or system-level work cannot complete. Isolated malformed records are reported
and skipped while valid records can still be published. It does not wait for
complete semantic coverage.

`--refresh off` queries the currently published generations without provider
discovery, plugin execution, refresh scheduling, semantic catch-up, or model
download. Exact result hydration still reads the provider records identified
by the generation-bound locators. It is read-only with respect to ctx indexes;
it is not an offline-content cache mode.

Only sources with supported automatic import participate in automatic refresh.
Explicit-only sources require `ctx import --provider ... --path ...`.
Winner-only provider precedence prevents combining a selected replacement with
stale defaults.

`ctx status`, `ctx index status`, and search JSON report lexical generation,
resolver/catalog availability, semantic generation binding and coverage,
relational projection state, daemon work, and typed fallback reasons.

## Source availability

Because provider files are the sole content authority, deleting, moving, or
changing one can make an otherwise matching index record temporarily
unrenderable. ctx does not emit an empty placeholder or use prior-epoch
content. Use `ctx locate` to inspect provenance and run a source refresh or
explicit import to publish a generation matching the current provider bytes.

`ctx show session` hydrates ordered events by source and preserves transcript
order. Any typed hydration failure fails the complete selected transcript
rather than returning a mixture of exact and stale content.

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
- pagination and candidate-pool truncation metadata.

Raw output can contain queries, absolute paths, hydrated snippets, provider
metadata, and transcript-derived content. Treat it as private local data and
review it before sharing.
