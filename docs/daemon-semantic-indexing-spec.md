# Daemon And Semantic Indexing

The ctx daemon owns bounded background freshness and serves local semantic
queries. Search remains an interactive read path: it queries committed indexes
and does not perform corpus backfill, model acquisition, or an unbounded inline
refresh.

Daemon maintenance and semantic search are opt-in. A daemon without semantic
search is useful: it keeps the lexical index fresh in the background. Semantic
search requires the daemon because the daemon owns the shared local model and
query service.

## Enable It

Enable daemon maintenance with:

```bash
ctx daemon enable
ctx setup
```

That command writes the explicit daemon override to `~/.ctx/config.toml`:

```toml
[daemon]
enabled = true
```

To enable both daemon maintenance and local semantic search, configure both
settings and rerun setup:

```toml
[daemon]
enabled = true

[search]
semantic = true
```

```bash
ctx setup
ctx index watch
```

Unset settings keep their built-in defaults and are not written to config by
setup. Semantic-without-daemon is invalid. `ctx daemon disable` writes an
explicit daemon opt-out; `ctx daemon run --force` is available for a deliberate
foreground troubleshooting run while automatic starts are disabled.

## Commands

```bash
ctx daemon status
ctx daemon run
ctx daemon run --once --json
ctx daemon enable
ctx daemon disable

ctx index status
ctx index watch
ctx index wait --lexical
ctx index wait --semantic
ctx index wait --all
```

`ctx daemon run` runs the same coordinator in the foreground. The `ctx index`
commands observe daemon/store state; they do not become a second indexing
worker. `index status` prints one snapshot, `index watch` follows progress, and
`index wait` exits when the requested readiness converges or fails.

`ctx setup` initializes and inventories the local store. With daemon
maintenance enabled, it schedules background indexing, prints discovered work
and separate lexical/semantic readiness estimates, starts the daemon when
appropriate, and returns without waiting for all semantic embeddings. Use
`ctx setup --wait` when foreground lexical convergence is intentional and
`ctx setup --no-daemon` for a one-run autostart opt-out. JSON and catalog-only
setup do not autostart background maintenance.

## Responsibilities

The daemon owns:

- native provider and enabled history-source refresh;
- lexical projection maintenance;
- deterministic semantic-document projection and embedding;
- dirty/deleted semantic sidecar cleanup;
- the local semantic query service;
- watcher, inventory, retry, coverage, and job status.

`ctx search` owns query validation, bounded retrieval over committed indexes,
result rendering, and optional scheduling of background freshness. When the
daemon is enabled, search does not duplicate daemon-owned history refresh.

`ctx search --refresh background` serves current indexes while asking the
daemon to catch up. `--refresh off` is strictly read-only and neither starts nor
pokes the daemon. `--refresh wait` waits for currently discovered lexical and
enabled semantic work to converge, then searches or fails clearly.

Foreground search has priority over starting another semantic document batch.
An explicit semantic query uses the already-resident model and ready sidecar;
it never downloads a model or initiates indexing.

## Quiet Background Work

Background indexing is designed to converge without dominating normal machine
use. The policy is internal and adaptive; there are no CPU, memory, or disk
tuning flags in this release.

- Import work is split into bounded byte/unit groups. Previously unseen small,
  stable files can share one efficient atomic transaction; changed, appended,
  ambiguous, interrupted, or replacement work uses a durable resumable
  publication path.
- Daemon mode admits at most one bounded group per scheduling slice, runs at low
  priority, accounts for source reads, logical writes, filesystem metadata
  operations, and observed SQLite WAL growth, then yields or backs off between
  groups.
- Fresh batching reduces transaction and full-text-index write amplification;
  pacing controls when efficient transactions run rather than turning every
  record into a separate commit.
- Full-text merge and checkpoint work is resumable. A pinned SQLite reader
  pauses new bulk admission at the WAL high-water mark and retries with backoff
  instead of spinning or allowing the WAL to grow without bound.
- Semantic thread count, batch size, duty cycle, and model-load admission are
  selected from effective machine/cgroup memory, CPU, platform, and current
  load. Each active embedding batch remains bounded, and a foreground query
  prevents the next document batch from starting.
- Semantic sidecar reads, writes, pruning, and maintenance use the same disk
  discipline as lexical indexing.

This is a best-effort resource policy, not a claim that every filesystem or
device has identical performance. `ctx status`, `ctx index status`, and
`ctx doctor` expose enough state to distinguish useful background progress from
a stalled or degraded job.

## Watcher Failure And Reconciliation

The filesystem watcher is an optimization, not the only source of correctness.
After watcher loss, the daemon performs one bounded reconciliation and enters a
degraded state. Watcher registration retries with capped exponential backoff;
fallback inventories become progressively less frequent, up to a five-minute
cadence. Inventories are resumable, paced by source bytes and directory/stat
operations, and never overlap each other.

A daemon restart reuses minimal durable timing state so a crash loop cannot
trigger an immediate full-tree inventory on every restart. After watcher
recovery, ctx reconciles before resetting the degraded backoff. Even a healthy
watcher has a paced periodic safety inventory so missed events eventually
converge.

Status reports watcher state, the last error, current inventory progress, last
completed inventory, and the next watcher retry or fallback inventory time.

## Semantic Model And Privacy

Local semantic documents are deterministic projections of indexed transcript
content and structured metadata. No LLM creates summaries, "important
findings," or other inferred documents during local indexing.

When semantic maintenance is explicitly enabled, the daemon may download the
pinned local embedding runtime and model from documented distribution
sources. Artifacts are bound to immutable revisions and verified by the digest
or signature required by that distribution. Acquisition failures use persisted
exponential backoff, so a missing network or bad artifact does not create a
retry storm. Status includes the failure class, attempt count, and next retry
time.

The model download sources are:

- CPU/ONNX model files from
  `https://huggingface.co/intfloat/multilingual-e5-small` at immutable revision
  `614241f622f53c4eeff9890bdc4f31cfecc418b3`, with required files verified
  against compiled digests;
- the Apple Core ML bundle at
  `https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz`,
  bound to the same source revision and archive SHA-256
  `94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e`.

Model acquisition uploads no query, transcript, snippet, result, path, provider
metadata, or other local history. Foreground CLI search, MCP, and SDK calls
never download a model. Disable semantic indexing and use `--refresh off` for a
strictly read-only, no-model-acquisition search path.

The semantic sidecar stores local vectors, hashes, offsets, and state beside the
main ctx data. Both stores and all search/status output are private local data;
they can contain transcript-derived information and are not share-safe.

## Search Readiness

The three retrieval backends keep fixed meanings:

- `lexical` uses the local full-text/path indexes and rejects semantic clauses;
- `semantic` performs explicit vector recall and fails with a typed readiness
  error when the daemon query service, model, or index is unavailable;
- `hybrid` can combine explicit lexical and semantic alternatives. Without an
  explicit semantic clause, it may rerank only lexically eligible results.

There is no general lexical-then-semantic fallback. If optional automatic
hybrid reranking is unavailable, ctx returns the unchanged lexical set/order
with explicit diagnostics. Explicit semantic intent is never silently replaced
with lexical intent. Partial semantic coverage is reported as partial through
coverage and completeness diagnostics.

Machine-readable status includes lexical and semantic job progress, watcher
degradation, semantic coverage and dirty/queued counts, model readiness,
acquisition retry state, heartbeat/errors, and next scheduled work. Search JSON
and MCP structured results separately report semantic attempted/readiness,
coverage, completeness, and effective backend for that request.
