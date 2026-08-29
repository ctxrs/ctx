# Indexing and Semantic Search Spec

This spec records the product and architecture decision for semantic search.

## Decision

ctx makes automatic indexing the default and also supports manual indexing.
Automatic indexing uses a persistent background daemon; manual indexing starts
finite workers only for explicit refreshes. Semantic search remains an explicit
opt-in; when enabled, hybrid retrieval becomes the default.

The public indexing modes are:

| Mode | Meaning |
| --- | --- |
| `auto` | Default. Permit persistent background maintenance to keep indexes current. |
| `manual` | Run no persistent daemon. `ctx import` and `ctx search --refresh wait` can start finite workers for explicit updates. |

`ctx index mode` reads the mode; `ctx index mode auto` and
`ctx index mode manual` persist changes and immediately reconcile supervision.
When auto remains effective, ctx installs or repairs supervision and starts the
daemon; manual mode stops it and removes supervision.
The canonical config is `[indexing] mode = "auto"|"manual"`.

Search is an interactive read path and never becomes a duplicate importer.
Automatic background search may start or signal the persistent daemon. Manual
background search reads the current indexes without contacting a process;
explicit `--refresh wait` may start the same Core engine as a finite worker.
Neither search path performs inline history refresh or lexical publication in
the query process. After the finite worker publishes Core in manual mode, an
opted-in semantic or nonzero-weight hybrid `--refresh wait` may reconcile the
semantic document projection for that exact generation and embed the query in
the waiting foreground process. Daemon-free `--refresh off` and `--refresh
background` may embed a query in the foreground only when the exact semantic
generation and verified cached model assets are already ready, or through an
explicitly selected HTTP executor after the same exact preflight. They never
reconcile or write projection state.

The public retrieval modes are:

| Mode | Meaning |
| --- | --- |
| `hybrid` | Default when semantic search is enabled. Query lexical and semantic indexes together, then fuse/rerank candidates. |
| `semantic` | Semantic vector retrieval only. Useful for conceptual recall and debugging. |
| `lexical` | Default while semantic search is disabled. Core/Tantivy lexical and indexed path/token retrieval. Useful for exact strings, ids, paths, flags, and symbols. |

There is no public `auto` retrieval mode. `auto` made lexical and semantic feel
like fallback tiers. The desired model is not "try lexical, then maybe rescue
with semantic"; it is "hybrid uses both evidence sources when available."

Freshness is separate from retrieval mode:

| Freshness | Meaning |
| --- | --- |
| `background` | Default. Serve current indexes; start/poke persistent daemon work only in automatic indexing mode. Manual mode may query an already-ready semantic projection and is otherwise refresh-inert. |
| `off` | Serve current indexes and do not start, poke, wait for, or run indexing. A daemon-free query may use an already-ready semantic projection with verified cached model assets, or its explicitly selected HTTP executor after exact preflight. |
| `wait` | Wait for authoritative Core publication from the persistent daemon or a manual-mode finite Core worker, then search or fail with a clear local error. |

When an automatic-mode `wait` search needs semantic evidence, it also waits
for the daemon's semantic acknowledgement of the selected Core generation. If
Core advances during that bounded wait, search repins Core and semantic
together before querying. Lexical, zero-weight hybrid, and unsupported semantic
scopes skip this wait.

The existing `strict` behavior can map to `wait` for command-line users while
the public docs move to `wait`. Do not add compatibility aliases unless a
specific external contract requires them.

## Semantic Corpus

The primary semantic corpus is `lite_turn + deterministic rollups`.

`lite_turn` is one user message plus the last assistant message before the next
user message. Rollups are deterministic, functional documents created from
existing structured metadata:

- file rollup: touched paths/change kinds for the session
- command rollup: command preview/status/exit code when available
- error rollup: lines containing deterministic error markers such as `error`,
  `failed`, `panic`, `exception`, or `traceback`

No LLM is used to create semantic documents. No inferred "important findings"
or summarization is allowed in the local indexing path.

## Embedding Executor Contract

The built-in multilingual E5 executor is the local default. The command `ctx
semantic enable --executor builtin|URL` selects the executor used for both
document indexing and query embedding; bare `ctx semantic enable` preserves the
current selection. Each data root has one accepted vector space. An explicit
URL selection tries protocol V2 first and persists its endpoint, opaque
`space_id`, and dimensions. Fixed-E5 V1 is retained only as an endpoint-only
fallback when V2 returns 404; its vector identity remains the pinned built-in
contract. `schema_version` is not a config field.

The normative URL, authentication, privacy, wire protocol, identity, and
responsibility boundary is the
[external semantic executor contract](semantic-executors.md). This document
owns only its daemon lifecycle integration.

The daemon constructs one executor per applied configuration and uses it for
both indexing and query embedding. Endpoint identity drift fails closed without
falling back to E5. Rerunning `ctx semantic enable --executor URL` explicitly
accepts the current identity; if it changed, ctx wipes and rebuilds only the
derived semantic index. Core history and lexical generations remain intact.

`ctx semantic status` reads persisted and observed local state only. It does
not require the token, send it, probe either route, or make any network request.

## Setup UX

`ctx setup` should initialize local state, identify/index or enqueue available
history, start persistent daemon maintenance in automatic mode, and return
promptly. Manual setup starts no worker. Setup should not block for full
semantic completion by default.

`ctx semantic enable` records the semantic-search opt-in without changing the
selected executor. An explicit URL also discovers and accepts the endpoint's
vector-space identity. In auto mode the command starts or recovers daemon-owned
executor preparation and indexing;
`--wait` waits for the current projection. A user who wants
automatic catch-up from manual mode runs `ctx index mode auto` first. Lexical
search remains available while embeddings build; hybrid retrieval uses both
indexes when coverage is ready.

Default human output should include a strong foreground signal:

```text
ctx is indexing your local agent history in the background.

Found:
  115,123 records
  13.0 GB source history

Estimated readiness:
  lexical search:  ~14 min
  semantic search: ~45 min

Watch progress:
  ctx index watch

Search now:
  ctx search "test failure"
```

The exact words can change, but the output must communicate:

- background indexing is underway
- how much source history was identified
- lexical and semantic readiness are separate jobs
- how to watch/wait in the foreground
- search can run before indexing completes

`ctx setup --format json` reports the same counts/status as structured fields;
output format does not change daemon-autostart behavior. `ctx setup --no-daemon`
is the one-run daemon-autostart opt-out. The deprecated `--catalog-only` flag is
ignored and does not change setup behavior.

The long-lived daemon reloads effective daemon and semantic configuration
between maintenance cycles. A later supported semantic opt-in plus repeat setup
must activate daemon-owned query service and indexing in the existing process;
config-file mutation alone is not activation. Status reports current requested
configuration, last daemon-applied configuration, reload failure, and observed
semantic runtime ownership separately. A config parse/read failure is
fail-closed for semantic work: ctx stops the query service, releases the
executor, reports `daemon_config_reload_failed`, and retains `last_run_*` only
as historical status. Core refresh continues independently, and the semantic
runtime can recover after a later valid reload. Executor rotation follows the
same no-fallback rule: once a newly requested executor differs from the applied
executor, ctx stops the old query service and releases the old executor before
it prepares the replacement. If replacement activation fails, ctx does not
resume or send work to the old executor; requested intent remains visible,
while applied semantic state and runtime ownership are inactive and the
semantic job is not reported as enabled.

## Foreground Progress Commands

The `index` command group shows focused indexing state and controls automatic
indexing. Its status and readiness commands do not become indexing workers.

```text
ctx index
ctx index --format json
ctx index mode
ctx index mode auto
ctx index mode manual
ctx index watch
ctx index watch --format jsonl
ctx index wait --lexical
ctx index wait --semantic
ctx index wait --all
ctx index wait --all --format json
```

`ctx index watch` refreshes until complete. On an interactive terminal, watch
redraws one progress block in place; when redirected, it appends plain snapshots
without terminal control sequences. `--format jsonl` writes one JSON object per
snapshot instead. `ctx index wait` exits zero when the requested readiness is
reached and non-zero on timeout/error; wait uses `--format json` for one
structured result. `ctx status` remains the complete one-shot authority.
It includes daemon and supervisor health; `ctx index` is the smaller one-shot
indexing status view.

Example watch output:

```text
Current index  ready  generation 01J...
Refresh        running  7 / 12 sources  scanning
Semantic      pending  58,090 searchable / 30,480 embedded
```

Progress uses only fields reported by the verified generation, refresh job, and
semantic projection. It does not derive synthetic work units, rates, remaining
work, or failure counts from unrelated counters.

## Architecture

The daemon owns:

- discovered native/provider history refresh
- lexical projection refresh
- semantic document projection
- semantic embedding
- opted-in model acquisition, pinned-hash verification, and repair
- deletion/dirty queue cleanup
- status/job JSON for foreground observers

The search command owns:

- argument parsing
- opening existing indexes read-only when possible
- retrieval over the current lexical/semantic indexes
- automatic persistent-daemon signal/autostart for background freshness
- explicit wait authority for a persistent daemon or manual finite Core worker
- clear freshness/retrieval status in JSON

The setup command owns:

- creating the data root/config/store
- source discovery and scanning
- persistent daemon autostart only in automatic mode and unless explicitly
  disabled with `--no-daemon`; the deprecated `--catalog-only` flag is ignored
- printing initial background indexing estimates and status commands
- queueing model acquisition for the daemon without downloading in the setup
  process

The foreground `index` command owns:

- showing a one-shot indexing status view
- reading and updating the persisted indexing mode
- installing or repairing persistent supervision in auto mode
- stopping and removing persistent supervision in manual mode
- reading daemon/store/semantic status
- displaying progress
- waiting on readiness
- never doing embedding itself

`ctx daemon run` remains an advanced foreground command. It blocks until stopped
and does not change the persisted indexing mode. Automatic mode owns ordinary
background startup; there is no separate public daemon start command.

## Implementation Principles

- No public `auto` retrieval mode.
- No lexical-then-semantic fallback as the default strategy.
- With automatic indexing disabled, direct CLI `--refresh off` and
  `--refresh background` queries may embed from verified cached model assets
  or use the configured HTTP executor only after
  exact-generation preflight succeeds; they never acquire a model, reconcile
  coverage, or write projection state. Passive preflight shares the existing
  Flat transaction lock from SQLite sidecar inspection through control-schema
  validation and exact Flat-generation pinning. It refuses WAL and rollback
  journals and opens the main database immutable/read-only without creating
  lock, WAL, or SHM files. Ordinary daemon and Reconcile preflight remains
  WAL-aware so committed daemon work is visible.
  An opted-in manual-mode semantic or nonzero-weight hybrid `--refresh wait`
  may prepare the selected executor, acquire the pinned local model when
  selected, reconcile semantic coverage for that exact generation, and embed
  the query.
- No model download from foreground setup, import, status, doctor, MCP, or
  index-observer commands. Acquisition belongs to the opted-in daemon in auto
  mode and the explicit manual `--refresh wait` exception above; unverified
  bytes must fail closed before cache publication.
- No duplicate inline importer. Persistent and finite publication both use the
  same daemon/Core refresh engine.
- A finite Core worker installs no supervision, runs no watcher/timer/semantic/
  upgrade maintenance, admits at least one request before idle exit, and exits
  only when Core work is terminal and IPC is quiescent. Manual semantic
  reconciliation occurs afterward in the explicit waiting query process, not
  in that Core worker.
- No LLM-generated semantic documents.
- Prefer one persisted semantic-document projection over reconstructing the
  corpus from raw events for every worker pass.
- Keep exact lexical search first-class inside `hybrid`; it is not a crutch.
- Keep compatibility only where an existing external SDK/contract requires it.
  Do not preserve old terms merely because they existed.

## End-to-End Plan

1. Rename retrieval mode surface:
   remove `SearchBackendArg::Auto`, default `--backend` to `hybrid`, update
   docs/JSON/tests to use `hybrid|semantic|lexical`.

2. Split freshness from retrieval:
   introduce `background|off|wait` terminology for search freshness while
   mapping or replacing the current `RefreshArg::Auto|Off|Strict` behavior.

3. Make search read-only under daemon ownership:
   never run inline `refresh_before_search`; serve the existing index, let
   automatic background mode signal/autostart persistent work, leave manual
   background and `off` inert, and let explicit `wait` request finite Core work
   in manual mode.

4. Make setup foreground-light:
   keep setup initialization and source scanning visible, start daemon work,
   print found counts/estimated readiness/watch commands, and avoid waiting for
   full semantic indexing by default.

5. Add `ctx index`:
   implement the one-shot status view, `mode`, `watch`, and `wait` by reading
   existing daemon job and semantic worker status. Mode changes reuse the
   persistent supervision lifecycle.

6. Move semantic corpus toward `lite_turn + rollups`:
   replace raw event chunking in the semantic worker with deterministic
   turn/rollup documents and stable document IDs. Persist projection state so
   incremental refresh avoids full-history scans.

7. Test:
   add CLI parsing tests for removed `auto`, default `hybrid`, and freshness
   modes; setup output/status tests; daemon-owned search refresh tests; index
   status/watch/wait tests; semantic corpus unit tests for deterministic docs.

8. Verify:
   run formatting, targeted Rust tests, search/daemon integration tests, and a
   small real-data count/eval smoke before merge.
