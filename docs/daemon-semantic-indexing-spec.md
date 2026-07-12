# Daemon-Owned Indexing and Semantic Search Spec

This spec records the product and architecture decision for local semantic
search.

## Decision

ctx should make local daemon-owned indexing and hybrid semantic search the
default path.

Indexing is background infrastructure. Search is an interactive read path.
When the daemon is enabled, `ctx search` should not perform inline history
refresh, lexical index refresh, semantic document projection, or embedding.
It should read the current indexes, optionally signal daemon work, and return
quickly.

The public retrieval modes are:

| Mode | Meaning |
| --- | --- |
| `hybrid` | Default. Query lexical and semantic indexes together, then fuse/rerank candidates. |
| `semantic` | Semantic vector retrieval only. Useful for conceptual recall and debugging. |
| `lexical` | SQLite FTS/path/token retrieval only. Useful for exact strings, ids, paths, flags, and symbols. |

There is no public `auto` retrieval mode. `auto` made lexical and semantic feel
like fallback tiers. The desired model is not "try lexical, then maybe rescue
with semantic"; it is "hybrid uses both evidence sources when available."

Freshness is separate from retrieval mode:

| Freshness | Meaning |
| --- | --- |
| `background` | Default. Serve current indexes and start/poke daemon work if needed. |
| `off` | Serve current indexes and do not start, poke, wait for, or run indexing. |
| `wait` | Wait for requested readiness from the daemon, then search or fail with a clear local error. |

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

## Setup UX

`ctx setup` should initialize local state, identify/index or enqueue available
history, start daemon maintenance when enabled, and return promptly. It should
not block for full semantic completion by default.

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

`ctx setup --json` should not autostart the daemon and should report the same
counts/status as structured fields. `ctx setup --no-daemon` initializes local
state without starting background work.

## Foreground Progress Commands

Add an `index` command group that observes daemon state. It should not become
the indexing worker.

```text
ctx index status
ctx index watch
ctx index wait --lexical
ctx index wait --semantic
ctx index wait --all
```

`ctx index status` prints the latest known state once. `ctx index watch`
refreshes until interrupted or complete. `ctx index wait` exits zero when the
requested readiness is reached and non-zero on timeout/error.

Example watch output:

```text
History import      [##########------]  62%  71,402 / 115,123 records
Lexical index       [######----------]  41%  47,812 / 115,123 records
Semantic docs       [########--------]  54%  58,090 / 105,439 docs
Semantic embeddings [###-------------]  22%  30,480 / 139,587 chunks

lexical usable: yes
semantic usable: partial
```

Progress should use fields already available from the store, daemon jobs, and
semantic worker reports. Estimates are allowed to be approximate and should be
labelled as estimates.

## Architecture

The daemon owns:

- discovered native/provider history refresh
- lexical projection refresh
- semantic document projection
- semantic embedding
- deletion/dirty queue cleanup
- status/job JSON for foreground observers

The search command owns:

- argument parsing
- opening existing indexes read-only when possible
- retrieval over the current lexical/semantic indexes
- optional daemon signal/autostart for background freshness
- clear freshness/retrieval status in JSON

The setup command owns:

- creating the data root/config/store
- source discovery/inventory
- daemon autostart unless disabled or JSON output
- printing initial background indexing estimates and status commands

The foreground `index` command owns:

- reading daemon/store/semantic status
- displaying progress
- waiting on readiness
- never doing embedding itself

## Implementation Principles

- No public `auto` retrieval mode.
- No lexical-then-semantic fallback as the default strategy.
- No foreground semantic embedding from `ctx search`.
- No duplicate inline refresh when daemon is enabled and running.
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
   when daemon is enabled or running, skip inline `refresh_before_search`,
   serve the existing index, and signal/autostart daemon work when allowed.

4. Make setup foreground-light:
   keep setup initialization and source inventory visible, start daemon work,
   print found counts/estimated readiness/watch commands, and avoid waiting for
   full semantic indexing by default.

5. Add `ctx index`:
   implement `status`, `watch`, and `wait` by reading existing daemon job and
   semantic worker status. Reuse `daemon_report` and `semantic_worker_report`.

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
