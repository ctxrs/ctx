# Search

`ctx search` finds agent-history records indexed into Core. The v0.26 search
epoch has an immutable lexical generation and an optional semantic sidecar:

- immutable Core/Tantivy generations under `search/lexical`, containing complete
  policy-selected normalized records and source identities;
- flat-F32 semantic projections under `search/semantic`;

The Core/Tantivy generation contains policy-selected meaningful text, complete
normalized records, and the metadata needed to match indexed events. Search
snippets and typed event/session presentation come from those stored records.

Default results are session-diverse: ctx shows the strongest matching event
from each session, then lets you drill into dense event-level results.
Human output labels the result window as relevance ordered and identifies
whether it contains primary sessions or primary plus subagent sessions. Each
result's `Event` row shows the short ctx event ID and the matched event's exact
UTC RFC 3339 millisecond timestamp; an indexed event without a timestamp says
`time unavailable`. These timestamps do not change result ordering.

## Search examples

```bash
ctx search "build failure"
ctx search "storage layout" --provider codex
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search "permission denied" --content-scope outputs
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
- title, Core-backed snippet, one-based final rank, result scope, and match reasons;
- the backend-provided `retrieval_score`, which is diagnostic and can be
  non-monotonic after query-coverage and session-diversity shaping;
- compatibility session importance and the additional-match count for session
  results; like `retrieval_score`, session importance is not an ordering contract;
- provider, event sequence, timestamp, workspace, and working directory;
- stable ctx citations;
- copyable `suggested_next_commands` for `show` and scoped search.

Search result IDs are ctx-owned. Commands accept complete IDs or unambiguous
prefixes of at least eight hex characters. Provider-owned IDs are metadata;
provider lookup must be explicit.

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
- `--workspace <name-or-path>`;
- `--since <rfc3339-or-days>d`;
- `--event-type <event-type>`;
- `--content-scope all|transcript|calls|outputs`;
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

Default search excludes subagent sessions so primary human-agent intent stays
prominent. Use `--include-subagents` for implementation details, reviews, test
output, and failure analysis. When `CODEX_THREAD_ID` is available, ctx also
excludes the active Codex session tree by default; use
`--include-current-session` to include it.

`--limit` defaults to `20` and is capped at `200`. Default search returns one
diverse result per session. Use `--session` for dense hits inside one session
or `--events` for dense event hits across sessions.

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
plaintext transcript chunks.

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
silently reused.

Search does not download models, initialize semantic storage, or perform
foreground semantic catch-up. Explicit semantic search reports a typed local
error when its cached runtime/model or compatible generation is unavailable; it
never silently changes a semantic-only request into lexical retrieval. Hybrid
remains lexical-safe in those cases.

## Refresh and freshness

`--refresh background` is the default. Search health-checks and, when needed,
wakes or recovers the default-enabled persistent daemon, then serves the latest
committed lexical generation without waiting for optional semantic indexing or
independent Pro materialization. The daemon owns bounded provider discovery,
source refresh, immutable candidate-generation construction, publication,
opted-in semantic catch-up, and Pro catch-up. The query process never becomes a
foreground history writer.

On a fresh root, background mode asks the daemon to publish the first lexical
generation. If daemon maintenance is disabled, search performs no hidden
bootstrap or fallback import and can query only an already committed
generation. Enabled auto-refresh history-source plugins run through the same
daemon-owned, bounded Core refresh route; explicit-only sources still require
an explicit import.

`--refresh wait` wakes the daemon and waits for the requested source frontier
and lexical-generation receipt. It fails with a typed source, lag, or system
error when that receipt cannot publish; it does not fall back to a foreground
importer or wait for complete semantic or Pro coverage.

`--refresh off` queries the currently published generations without provider
discovery, plugin execution, refresh scheduling, semantic catch-up, or model
download. It renders results from the active Core generation and is read-only
with respect to ctx indexes.

Only sources with supported automatic import participate in automatic refresh.
Explicit-only sources require `ctx import --provider ... --path ...`.
Winner-only provider precedence prevents combining a selected replacement with
stale defaults.

`ctx status` and search JSON report lexical generation, refresh state, semantic
generation binding and coverage, daemon work, and typed fallback reasons.
`ctx index watch` and `ctx index wait` expose a smaller readiness-only view.

## Core-backed presentation

Search snippets come from the Core-backed searchable projection for complete
policy-selected records in the active verified Core/Tantivy generation. Full
show/list/MCP event output retrieves the exact stored Core content; it does not
rewrite the normalized body to include projected invocation text. Query-time
reads do not reopen provider history. Provider changes become searchable and
visible to show after explicit import or daemon refresh publishes a new Core
generation. `ctx show session` preserves provider event order.

The top-level exact `mcp_tool_call: {server, tool}` attribution object remains
metadata. That object is not indexed and adds no search input, filter, result
field, selector, match, ranking signal, snippet, `why_matched` value, SQL column,
or Local Pro fact. Use log-mode `ctx show session`, `ctx show event`, or
`ctx list events` and filter JSON/JSONL rows client-side when exact attribution
metadata is needed.

The separately stored, content-governed `mcp_exchange.invocation` has a narrow
lexical body projection. On a policy-selected record, ctx projects the
invocation server value, tool value, and the compact JSON representation of
arguments whose capture state is `present`. It adds no synthetic server/tool
labels. `absent`, `unavailable`, and `omitted` arguments contribute no terms.
These values use ordinary lexical body matching and ranking under the record's
existing event type; they are not new fields or selectors.

`mcp_exchange.provider_call_id` remains unsearchable metadata and adds no
filter, selector, result field, ranking signal, or snippet. Response status,
failure, timing, and structured payload capture are stored and retrievable in
full-content event output but add no search terms. Response text represented by
the `normalized_body` disposition retains the event's existing normalized-body
search behavior exactly once; the exchange does not duplicate it.

This projection adds no semantic text, filter, search result field, SQL column,
Local Pro fact, or hidden network request. Sensitive invocation arguments can
therefore become searchable in the local lexical index, but ctx does not send
them over the network. The lexical projector revision participates in
generation identity, so an older generation must be rebuilt or pass the narrow
same-epoch preservation migration before the new terms are searchable; stored
historical rows with no captured exchange remain unchanged. See
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
