# Search

`ctx search` finds matching events in indexed local agent history. It uses the
same versioned query model across the CLI, JSON, MCP, and SDKs:
`ctx-search-v1`.

Default results are session-diverse: ctx returns the strongest matching event
from each session. Use `--events` for dense event results across sessions, or
`--session <ctx-session-id>` to search densely inside one session.

## Query Semantics

A positional query is one lexical `all` clause. Every analyzed word must occur
in the same indexed event, but word order does not matter:

```bash
ctx search "disk io pressure"
```

This means `disk AND io AND pressure`. It is not an exact phrase, and it does
not mean `disk OR io OR pressure`.

Use the query-construction flags when a different relationship is intentional:

| CLI form | `ctx-search-v1` form | Meaning |
| --- | --- | --- |
| positional text | `any: [{"all": ...}]` | Require every analyzed word in one event. |
| `--term <text>` | `any: [{"all": ...}]` | Add an all-words alternative; repeat for OR alternatives. |
| `--phrase <text>` | `any: [{"phrase": ...}]` | Require analyzed words adjacent and in order. |
| `--literal <text>` | `any: [{"literal": ...}]` | Require a punctuation-preserving contiguous value. |
| `--semantic <text>` | `any: [{"semantic": ...}]` | Add one conceptual-recall alternative. |
| `--must <text>` | `must: [{"all": ...}]` | Add a global all-words requirement. |
| `--exclude <text>` | `must_not: [{"all": ...}]` | Exclude matches containing every analyzed word in the clause. |

All positive positional, `--term`, `--phrase`, `--literal`, and `--semantic`
clauses are alternatives in `any`. Combining positional text with one of those
flags broadens the positive alternatives. `--must` and `--exclude` apply to
every alternative:

```bash
# (disk AND io AND pressure) OR (storage AND latency)
ctx search --term "disk io pressure" --term "storage latency"

# Exact analyzed phrase, globally constrained to Codex and excluding Postgres.
ctx search --phrase "small writes" --must codex --exclude "postgres vacuum"

# Preserve punctuation in a filename or symbol.
ctx search --literal "logs_2.db" --must codex

# Union lexical and conceptual alternatives, then apply the hard requirement.
ctx search "disk io pressure" \
  --semantic "the indexing job made the workstation sluggish" \
  --must codex --backend hybrid
```

A request may contain only positive `must` clauses, but it may not contain only
exclusions. Empty clauses are invalid. V1 does not infer Boolean query syntax,
wildcards, regular expressions, fuzzy matching, boosts, proximity, or synonyms
from strings. Prefer several focused searches when one request would mix many
unrelated concepts.

## Structured Queries

`--query-file` accepts the canonical JSON text-expression. Query-construction
flags cannot be combined with it, but filters, backend, result mode, refresh,
limit, and output flags remain available:

```json
{
  "version": "ctx-search-v1",
  "any": [
    { "all": "disk io pressure" },
    { "phrase": "storage latency" },
    { "literal": "logs_2.db" },
    { "semantic": "the indexing job made the workstation sluggish" }
  ],
  "must": [
    { "all": "codex" }
  ],
  "must_not": [
    { "all": "postgres vacuum" }
  ]
}
```

```bash
ctx search --query-file ./query.json --backend hybrid --limit 20 --json
```

Each clause object has exactly one `all`, `phrase`, `literal`, or `semantic`
key. `semantic` is allowed at most once and only in `any`. Structured callers
may use lexical `all`, `phrase`, or `literal` clauses in `must` and `must_not`.
Unknown versions, fields, matchers, or placements fail closed.

## Backends

`--backend` accepts `lexical`, `semantic`, or `hybrid`. Backend selection never
changes the meaning of a clause:

- `lexical` searches SQLite FTS and rejects a query containing `semantic`.
- `semantic` requires one semantic clause as the sole positive `any`
  alternative. Lexical `must` and `must_not` clauses, filters, and exact
  phrase/literal verification remain hard requirements.
- `hybrid` may union explicit lexical and semantic alternatives and rank the
  bounded union together.
- `hybrid` without `--semantic` may semantically rerank a bounded set that was
  already lexically eligible. It never expands lexical eligibility.

Automatic hybrid reranking is attempted only for one lexical `all` or `phrase`
alternative with optional lexical `all` requirements. The semantic input is the
normalized alternative followed by each requirement in request order, separated
by newlines; exclusions, filters, and matcher labels are not embedded. If that
optional reranker is unavailable, ctx returns the unchanged lexical result set
and order and reports why in diagnostics.

Explicit semantic recall is different: if the local daemon query service,
model, or semantic index is unavailable, ctx returns a typed readiness error.
It does not silently run a lexical query with different meaning. Incomplete
semantic coverage can return bounded partial results only when the response
clearly reports incomplete coverage and result completeness.

The backend defaults to lexical while semantic search is disabled and hybrid
after semantic search is explicitly enabled. Semantic search requires the ctx
daemon. See [Daemon And Semantic Indexing](daemon-semantic-indexing-spec.md) for
opt-in and status instructions.

## Filters And Result Modes

Query filters narrow both text and JSON results:

- `--provider <provider>`;
- `--history-source <plugin/source-or-provider_key/source_id>`;
- `--provider-key <key>`, `--source-id <id>`, and
  `--source-format <format>` for custom history;
- `--workspace <name-or-path>`;
- `--since <rfc3339-or-days>d`, such as `30d`;
- `--event-type <type>`;
- `--file <path>`, which searches indexed touched-file metadata rather than the
  current filesystem;
- `--session <ctx-session-id-or-prefix>`;
- `--events`;
- `--include-subagents`;
- `--include-current-session`;
- `--limit <n>`, from 1 to 200.

Search requires a positive text clause or `--file <path>`. Provider, workspace,
time, session, source, event, exclusion, and output flags only narrow an actual
search.

By default, ctx searches primary-agent sessions. Add `--include-subagents` for
delegated implementation details, review findings, test output, or failure
analysis. When `CODEX_THREAD_ID` is available, ctx also excludes the active
Codex session tree by default; add `--include-current-session` when that tree is
the intended target.

Result IDs are ctx-owned. `ctx show` and `ctx locate` accept a full ctx ID or an
unambiguous prefix of at least eight hexadecimal characters:

```bash
ctx show event <ctx-event-id> --window 5
ctx show session <ctx-session-id>
ctx locate event <ctx-event-id>
```

Provider-owned session IDs remain metadata and require an explicit provider
lookup on commands that support one.

## Freshness

`--refresh` is independent of the retrieval backend:

- `background` is the default. With the daemon enabled, search schedules
  bounded catch-up and queries the currently committed indexes. With the daemon
  disabled, ctx performs bounded in-process lexical refresh.
- `off` is strictly read-only over the existing ctx indexes. It does not read
  provider history, run plugins, start or poke the daemon, download a model, or
  write index rows.
- `wait` waits for currently discovered lexical and enabled semantic catch-up
  to converge, then searches. It fails rather than silently serving stale
  results when catch-up cannot complete.

Search never performs semantic corpus backfill or model acquisition. The
enabled daemon owns those tasks. MCP search is always read-only and does not
refresh provider history.

## Bounded Execution And Diagnostics

Every search runs inside one shared work envelope. A small `--limit` controls
returned results; it does not permit unbounded candidate retrieval or payload
hydration behind the scenes. Semantic work uses part of the same envelope
rather than adding a second one.

The v1 hard maxima are:

- 64 KiB structured query JSON; 32 clauses; 1,024 UTF-8 bytes per clause; 8,192
  clause bytes total; and 32 analyzed tokens per clause;
- literals from 3 through 256 bytes;
- 1,024 candidates per positive seed, 16,384 candidate rows examined, and
  8,192 unique candidate IDs retained;
- 8,192 residual checks, 16 MiB total verification input, and 16 KiB per
  verification lookup;
- 256 hydrated rows, 8 MiB total hydration/snippet input, and 64 KiB per event;
- 512 KiB returned snippet/result text, 2 MiB serialized response, and 200
  returned results;
- one fixed elapsed-time budget shared by retrieval, verification, hydration,
  and rendering.

Structurally oversized requests fail before execution. Candidate or payload
budget exhaustion may return bounded partial results with explicit truncation
reasons. Elapsed-time exhaustion is a typed error instead of a timing-dependent
partial ranking.

Lexical and semantic branch ranks are merged by one versioned reciprocal-rank
fusion rule, hard constraints are verified before payload hydration, and final
ties use explicit stable fields rather than incidental SQLite row order.

`ctx search --json` uses `schema_version: 2` and includes the canonical `query`
plus a `query_execution` object on successful searches, including untruncated
ones. `query_execution` reports resolved and consumed query, candidate,
verification, hydration, payload, response, result, and elapsed budgets;
`truncated` and typed `truncation_reasons`; and semantic attempted/readiness,
coverage, candidate, completeness, and effective-backend diagnostics. Inspect
these fields instead of inferring completeness from a short result list.

## Semantic Data And Privacy

Semantic vectors and deterministic document metadata live in a private local
sidecar beside the main ctx store. Local semantic documents are constructed
functionally from indexed history; ctx does not use an LLM to summarize or
infer findings during indexing.

Foreground search, MCP, and SDK calls never download a model or upload a query.
When both daemon and semantic indexing are explicitly enabled, daemon
maintenance may download only the pinned, verified local runtime/model
artifacts from their documented distribution sources. It uploads no query,
transcript, result, path, provider metadata, or other local history data.

Search snippets, JSON, citations, paths, MCP output, and the SQLite stores can
contain private transcript text and secret-shaped strings. They are local data,
not redacted or share-safe output.

## History Reports

ctx retrieves indexed evidence; it does not synthesize conclusions. For a
history report, run several focused searches, inspect promising events or
sessions with `ctx show`, compare the cited evidence, and write the conclusion
separately.
