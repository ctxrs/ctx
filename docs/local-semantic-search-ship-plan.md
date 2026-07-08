# Local Semantic Search Ship Plan

This plan captures the dogfood findings from the July 7, 2026 local run on a
power-user ctx corpus and the implementation path to make local semantic search
safe to ship by default.

## Dogfood Baseline

- Fresh `ctx setup` identified 32,384 records / 13.1 GiB in 2.94s, but the
  daemon autostart path left a stale/non-running daemon before history indexing
  completed.
- Manual daemon lexical refresh imported 32,379 sessions / 429,851 events in
  3m58s, peaking at 665 MB RSS.
- Default semantic indexing skipped with `model_cache_missing`, even though
  compatible model caches existed elsewhere on disk.
- A configured-cache semantic batch embedded 5,000 event chunks in 9m06s,
  peaking at 1.83 GB RSS and covering only 3,702 of 429,934 searchable events.
- Incremental lexical refresh for a synthetic Codex session took 4.05s.
- Incremental semantic refresh with a configured cache made the synthetic marker
  strict-semantic Hit@1 in 50.73s.
- Warm lexical event searches were 20-34 ms. Warm semantic/hybrid searches were
  about 690-735 ms with `sqlite_vec0`, with query embedding about 170-185 ms and
  vector scan about 20-21 ms.

## Pre-Scheduling-Fix Dogfood Notes

- The lite-turn projection reduced the real local corpus from 430,093 indexed
  events to 108,252 semantic searchable documents.
- The semantic index key was bumped for the lite-turn corpus, so old event-level
  vectors are ignored. After the bump, this machine reports 0 embedded
  lite-turn items and about 108,000 queued lite-turn documents.
- Default model cache discovery now succeeds on this machine without setting
  `CTX_SEMANTIC_CACHE_DIR`.
- A foreground daemon pass against the real data root did not reach semantic
  indexing because history refresh consumed the whole bounded dogfood window:
  - a `--max-chunks 1024` pass was interrupted after 4m18s;
  - peak RSS was about 203 MB;
  - history refresh imported 519 new events and semantic vector counts were
    unchanged.
- A tighter `--max-chunks 256` pass was interrupted after 2m17s;
  - peak RSS was about 203 MB;
  - history refresh imported 38 new events and semantic vector counts were
    unchanged.
- This failed the ship bar because large-history refresh work could starve
  semantic indexing. The scheduling fix below is intended to make semantic
  bootstrap explicit daemon work rather than something reached only after
  refresh finishes.

## Scheduling-Fix Dogfood Notes

- After adding semantic-bootstrap scheduling and bounded lite-turn projection
  queries, real daemon passes on the real local data root now do semantic work
  before history refresh:
  - `ctx daemon run --once --max-chunks 64 --json` completed in 22.2s,
    skipped history refresh with `semantic_bootstrap_in_progress`, indexed
    64 chunks / 18 items, and peaked at 1.09 GiB RSS.
  - Warm default-memory shape `--max-chunks 512` completed in 50.7s, indexed
    512 chunks / 184 additional items, and peaked at 1.17 GiB RSS.
  - A higher-throughput experiment with `CTX_SEMANTIC_THREADS=4` and
    `CTX_SEMANTIC_EMBED_BATCH=64` indexed 1,024 chunks in 58.6s but peaked at
    4.68 GiB RSS, which is not acceptable as a default.
  - `CTX_SEMANTIC_THREADS=2` with `CTX_SEMANTIC_EMBED_BATCH=64` was worse for
    this corpus: 1,024 chunks in 1m52.8s and 4.54 GiB RSS.
- Strict semantic search now works on the partial local index. A representative
  search scanned 1,600 sqlite-vec chunks in 15ms, with query embedding at
  239ms and total command wall around 0.86s. Relevance is still not
  representative because coverage was only about 0.55%.
- The current implementation is materially better and no longer starves
  semantic behind refresh, but the safe-memory initial semantic backfill still
  extrapolates to hours on this corpus, not the sub-60-minute target.

## Adaptive-Default Dogfood Notes

- The default semantic embed policy is now one adaptive rule, not separate
  background/turbo tiers:
  `min(20% total RAM, 50% available RAM, 10 GiB)`, floored at `1 GiB`.
  Threads and embedding batch size derive from that budget, with env vars kept
  only as operator/debug overrides.
- On this 64 GB machine, the release binary selected:
  `threads=8`, `batch_size=128`, `memory_budget_bytes=10 GiB`.
- Real daemon passes on the real local data root with no semantic tuning env
  vars:
  - `--max-chunks 2048` indexed 2,048 chunks in 1m10.7s, used 683% CPU, and
    peaked at 8.46 GiB RSS.
  - `--max-chunks 512` indexed 512 chunks in 20.5s, used 590% CPU, and peaked
    at 8.11 GiB RSS.
- After removing the public daemon runtime cap, a natural one-pass daemon slice
  (`ctx daemon run --once --max-chunks 5000 --json`) ran for 62.5s, indexed
  1,837 chunks / 660 lite-turn items, used 624% CPU, and peaked at 8.49 GiB
  RSS.
- A 10-minute foreground daemon-loop soak wrapped in a process-level timeout
  exercised the real service shape without a CLI runtime cap. It used 589% CPU,
  peaked at 8.76 GiB RSS, gave history refresh multiple turns, imported fresh
  events, remained recoverable after external termination, and reached
  8,253 / 108,589 embedded lite-turn items with 24,665 embedded chunks.
- A cleanup one-pass command cleared the expected stale lock after the external
  timeout and moved coverage to 8,254 / 108,589 items, 24,666 chunks, zero dirty
  items, and a 127 MB sidecar including WAL/SHM.
- Strict semantic search remains light despite the larger indexing policy:
  a cold-ish search took 1.75s wall, peaked at 266 MB RSS, scanned 4,672
  sqlite-vec chunks in 29ms, and spent 180ms in query embedding.
- At 7.6% coverage, the local basics eval over eight task-shaped queries showed
  lexical p95 24ms but zero hits for most long natural-language queries, while
  hybrid/semantic returned results with p95 about 2.1s / 2.0s respectively,
  query embedding about 175ms, vector scan about 86ms over 24,666 chunks, and
  hydration about 380ms. A small exact-substring oracle pass scored
  hybrid/semantic 4/8 versus lexical 2/8; manual inspection showed several
  misses were oracle/snippet artifacts, but relevance is not proven enough at
  partial coverage to replace a 30-50 query private manifest at higher coverage.
- Cache discovery now gives `CTX_SEMANTIC_CACHE_DIR` precedence over generic
  `HF_HOME`. Daemon semantic bootstrap now gets one semantic-first pass before
  the next daemon loop must attempt history refresh, preventing semantic
  backlog from starving fresh lexical import.

## Post-Projection-Fix Dogfood Notes

- Lite-turn construction now reads deterministic preview text from an indexed
  `event_search_lookup` table instead of joining the FTS table by `event_id` or
  reparsing raw event JSON in the hot path. The real SQLite plan for recent
  semantic work now uses `idx_events_role_occurred_seq` plus the lookup primary
  key, rather than scanning all FTS rows.
- The lookup table is limited to previewable user/assistant messages. On this
  corpus it contains 426,974 rows: 108,612 user messages and 318,362 assistant
  messages.
- A schema-45 lookup-only repair on the real 435k-event store took 1m47s,
  peaked at 964 MiB RSS, and avoided the 7m46s full FTS rebuild path observed
  before the repair was targeted.
- `ctx daemon status --json` on the incomplete real semantic sidecar is now
  effectively instant: under 0.01s and about 15 MiB RSS.
- The pathological one-chunk daemon pass no longer hangs before the worker:
  `ctx daemon run --once --max-chunks 1 --json` completed in 12.9s, peaked at
  286 MiB RSS, pruned 1,150 stale chunks, and indexed one chunk. This is still
  dominated by prune plus single-item embedding overhead, so it is not a
  throughput estimate.
- The default daemon worker chunk budget now lets the worker use its existing
  60s budget. A default `ctx daemon run --once --json` pass on the post-migration
  stale sidecar completed in 65.6s, indexed 1,407 chunks, used about 5.1 cores,
  and peaked at 8.18 GiB RSS. This is a conservative throughput slice because
  the sidecar is still invalidating old pre-lookup vectors while indexing new
  ones.
- A cleaner isolated run copied the real schema-45 `work.sqlite` to a fresh
  data root with no vector sidecar. The copy took 45.8s. Three default daemon
  passes then indexed 4,720 chunks / 2,418 semantic items in 183.8s total,
  with zero dirty churn, history refresh skipped for semantic bootstrap, and
  peak RSS between 8.18 and 8.25 GiB. That sample was useful for throughput
  shape, but was replaced by the completed v2 dogfood run below.
- During incomplete bootstrap, eager recent dirty detection is skipped. Recent
  dirty detection is reserved for the complete/clean incremental path; bootstrap
  relies on ordered backfill plus bounded prune.

## Final V2 Dogfood Notes

- The v2 semantic corpus excludes deterministic transcript scaffolding
  (`<environment_context>`, `<turn_aborted>`, `<subagent_notification>`, and
  unified-exec process-limit warnings) from both semantic anchors and
  lite-turn boundaries. On the isolated real local corpus this reduced semantic
  documents from 108,614 v1 lite-turn anchors to 60,715 v2 anchors.
- A full daemon-owned v2 backfill on the isolated real local corpus completed
  in 2h02m53s, with max RSS 9,961,304 KiB, average CPU 466%, 60,715 / 60,715
  embedded items, 157,251 chunks, and a 1.2 GiB sidecar.
- A final-binary repair pass after review fixes completed in 1m46.5s, peaked at
  4.66 GiB RSS, repaired 12 stale events / 16 pruned chunks, and ended ready at
  60,715 / 60,715 items, 157,817 chunks, and zero dirty items.
- `ctx daemon status --json` on the complete sidecar is read-only/cache-only:
  under 0.01s and about 15.8 MiB RSS. It no longer exact-counts the work DB or
  sidecar from the foreground status path, and stale worker/job status files are
  ignored when their `model_key` does not match the current semantic corpus.
- The final rough eight-query search gate over the completed sidecar completed
  24 runs with no command failures. Lexical p95 was 27ms. Semantic p95 was
  2.17s and hybrid p95 was 2.26s with the safer 1,000-candidate soft-filter
  overfetch window. Typical diagnostics: query embedding 170-185ms, sqlite-vec
  scan about 535-575ms over 157,817 chunks / 243 MiB of vectors, and hydration
  about 170-440ms.
- The 1,000-candidate soft-filter window is intentionally conservative because
  current-session/subagent filters are applied after vector retrieval. A lower
  200-candidate window measured faster, but risks under-filling results without
  a proper refill loop.
- Final bounded incremental dogfood:
  - importing one new lite-turn session took 2.87s and 88 MiB RSS;
  - status immediately reported 60,717 searchable, 60,716 embedded, and one
    queued item;
  - the daemon pass reached ready in 33.85s and 437 MiB RSS;
  - the worker embedded exactly one chunk in 290ms after 168ms model init;
  - semantic search found the new marker at Hit@1 in 2.03s.
- A previous incremental pass before bounding recent repair took 1m34.9s and
  embedded 1,048 chunks, which showed that complete-index incremental refresh
  was too eager. The worker now uses a bounded incremental slice when initial
  queued work is at or below the recent-dirty window, while full bootstrap keeps
  scanning until its worker budget is exhausted.
- The sqlite-vec hot path uses cheap count-parity readiness for search. Deep
  payload drift is repaired by writable daemon maintenance rather than audited
  before every read-only search; the full audit was measured as a hidden
  35s-per-search cost and is not acceptable on the hot path.
- A later bursty incremental dogfood pass exposed two readiness issues:
  incomplete bootstrap tails needed a persistent backfill cursor, and the
  cached v2 searchable count could drift because event-level cache adjustment
  still counted deterministic control-message users. The fixes are:
  - persist the backfill cursor across daemon passes until the sidecar becomes
    ready;
  - make the event-level semantic count predicate match the v2 SQL control
    filter;
  - refresh the cached searchable count exactly inside writable daemon/worker
    maintenance, while keeping foreground status/search cache-only.
- Post-fix stale-count repair on the isolated real root corrected 60,728 stale
  searchable items to 60,725 exact searchable items, reached ready with queued
  zero in 1m16.88s, peaked at 510,476 KiB RSS, and indexed two chunks from live
  work discovered during the pass.
- Post-fix clean incremental dogfood imported one fresh marker source in 7.95s
  and 87,564 KiB RSS, then reported exactly one queued semantic item. A daemon
  pass skipped history refresh with `semantic_bootstrap_in_progress`, embedded
  exactly one chunk, reached ready in 49.45s, and peaked at 449,768 KiB RSS.
  Semantic search found the new marker at Hit@1 in 2.47s over 158,663 chunks;
  lexical found the same marker in 0.47s because the query shared exact marker
  tokens.

## Ship Goals

- `ctx setup` starts daemon-owned indexing by default and reports a truthful,
  actionable status.
- Existing local model caches are discovered without env-var handholding; if no
  cache exists, semantic status explains exactly what is missing.
- Semantic corpus is deterministic and small enough for local backfill:
  user-turn anchored lite-turn documents, not raw event/tool-output chunks.
- New local work is prioritized before historical backfill.
- Search output always exposes requested/effective backend and semantic fallback
  reason; common unsupported filters should fail clearly or fall back explicitly.
- Default and explicit `hybrid` use semantic evidence only when semantic
  sidecar coverage is complete and dirty work is drained; partial coverage is
  available through explicit `semantic` for diagnostics and dogfood, not default
  ranking.
- Local dogfood on this corpus meets:
  - lexical initial refresh: under 5 minutes;
  - semantic initial backfill: about 2 hours on this 64 GB power-user
    corpus, acceptable as daemon work if it is resumable, observable, and lower
    priority than fresh incremental work;
  - lexical incremental p95: under 10 seconds;
  - semantic incremental p95: under 60 seconds after model cache is available;
  - warm hybrid search p95: target under 2.5 seconds with the conservative
    soft-filter overfetch window; subsecond search needs a future query-service
    or refill/overfetch optimization rather than more hot-path audits;
  - semantic worker RSS follows the adaptive memory budget and must remain
    below that selected budget during default daemon indexing.

## Implementation Plan

### 1. Setup, Daemon, And Status

- Make `ctx setup` foreground output distinguish:
  - inventory complete;
  - daemon autostart requested;
  - daemon definitely running;
  - daemon skipped or failed to spawn.
- Move daemon autostart bookkeeping close enough to setup/import/search that the
  parent can write a status file when spawning fails or is skipped.
- Ensure status/watch/wait treat stale locks as recoverable state. Prefer
  removing stale locks during status calculation or surfacing a `recoverable`
  field plus the next command.
- Do not claim background indexing is underway solely from pending inventory.
- Tests:
  - setup JSON/human output does not promise running daemon when autostart is
    disabled or skipped;
  - stale lock status is recovered or explicitly marked recoverable;
  - `ctx index watch` does not hang indefinitely behind a dead lock.

### 2. Semantic Model Cache Discovery

- Keep env-var precedence, but broaden default discovery:
  - `$HF_HOME`;
  - `$CTX_SEMANTIC_CACHE_DIR`;
  - `$FASTEMBED_CACHE_DIR`;
  - `<data-root>/semantic-model-cache`;
  - common local cache roots such as `~/.cache/fastembed`,
    `~/.cache/huggingface/hub`, and repo-local `.fastembed_cache` when present.
- Status should report the selected cache root or the checked roots when missing.
- Search and daemon must resolve the same cache root.
- Tests:
  - cache is found in data root;
  - cache is found in a common fallback root without `CTX_SEMANTIC_CACHE_DIR`;
  - env vars still override fallback roots.

### 3. Lite-Turn Semantic Documents

- Replace raw event documents with deterministic lite-turn documents.
- Anchor each semantic document on a user message event id.
- Text format:
  - `user:` followed by the user message text;
  - `assistant:` followed by the last assistant message before the next user
    message in the same session/run, if present;
  - optional deterministic metadata already available from the store
    (provider, source format, cwd, title/workspace hints) remains in the
    semantic header.
- Do not use LLM summaries, inferred decisions, or heuristic "importance"
  labels.
- Tool calls, command output, reasoning, and lifecycle notices should not create
  standalone semantic documents. They may remain discoverable lexically.
- Hydrated semantic snippets should come from the lite-turn text range so result
  previews explain why the vector matched.
- Maintain a normal `event_search_lookup` projection for semantic document
  assembly. FTS remains the lexical index; semantic by-id/recency work must not
  join FTS by unindexed columns.
- Tests:
  - one user + multiple assistant messages before next user becomes one doc
    containing only the user and final assistant message;
  - tool/output events do not increase semantic document count;
  - `event_embedding_documents_by_ids` reconstructs the same text used for
    hashing and stale filtering.

### 4. Worker Throughput And Freshness

- Prioritize dirty/recent lite-turn documents before historical backfill.
- Order lite-turn backfill by document activity, where a late assistant reply
  makes the user-anchor document recent again.
- Avoid running a full history refresh before every semantic-only batch when no
  refresh work is needed.
- During semantic bootstrap, if the store already has searchable documents, a
  local model cache is available, and semantic coverage is incomplete, the
  daemon skips history refresh for that pass with reason
  `semantic_bootstrap_in_progress` and runs semantic indexing first.
- Do not run eager recent dirty detection while semantic coverage is incomplete
  or dirty work is already queued.
- Do not expose a daemon runtime-cap product option. Tests and dogfood scripts
  can wrap foreground daemon commands in process-level timeouts, but the daemon
  product behavior is to run until `--once`, failure, or idle exit.
- Keep the embedder warm within daemon loops.
- Let default daemon semantic passes use the existing worker time budget; keep
  peak memory controlled by the adaptive embed policy rather than an artificially
  tiny per-pass chunk count.
- When initial queued semantic work is at or below the recent-dirty window,
  treat the pass as incremental: drain dirty-priority work or one recent page
  and stop. When queued work is larger, treat it as bootstrap/backfill and keep
  scanning pages until the worker budget is exhausted.
- Persist the historical backfill cursor across daemon passes while coverage is
  incomplete; clear it only once the current model-key sidecar reaches ready.
- Keep the cached semantic searchable count cheap for read-only status/search,
  but refresh it exactly during writable daemon/worker maintenance and keep
  event-level cache deltas aligned with the v2 lite-turn control-message
  predicate.
- Tests:
  - dirty queue drains before historical backfill;
  - a new assistant response updates the existing turn document hash;
  - semantic bootstrap skips history refresh and calls the semantic job first;
  - history refresh still runs when the store is missing or semantic is ready;
  - cached semantic counts ignore deterministic control-message users and update
    correctly when an event changes from searchable to control-like;
  - `--max-chunks` produces truthful `budget_exhausted` status for one-pass
    dogfood runs.

### 5. Evaluation Harness

- Add a small JSONL manifest runner for local dogfood/evals that records:
  - query;
  - backend requested and effective;
  - fallback code;
  - elapsed ms;
  - semantic diagnostics;
  - top result ids/snippets.
- Keep the harness read-only with `--refresh off` by default.
- Include dogfood manifests outside source-controlled private data; commit only
  generic examples and runner documentation.

## Fast-Fail Criteria

- If lite-turn corpus count remains close to event count on the dogfood corpus,
  stop and inspect the projection before optimizing embedding throughput.
- If default cache discovery still reports `model_cache_missing` on a machine
  with a valid common cache root, stop and fix discovery before running more
  semantic timings.
- If hybrid `effective_mode` is lexical for unfiltered queries after semantic
  coverage exceeds the activation threshold, stop and fix fallback gating.
- If semantic incremental freshness exceeds 60 seconds for a single new turn
  with a warm cache, stop and inspect dirty queue ordering and model reuse.
- If daemon history refresh runs before semantic bootstrap while searchable
  documents are present, semantic coverage is incomplete, and the model cache is
  available, stop and fix daemon scheduling before further timing work.

## Remaining Follow-Ups

- Add a refill loop for post-vector soft filters so default semantic/hybrid can
  reduce candidate count without risking under-filled filtered results.
- Add an idle/low-priority stale-sweep cadence for older externally changed or
  deleted documents that are not caught by recent dirty detection, while keeping
  normal ready-status daemon passes cheap.
- Consider a long-lived query service if subsecond semantic/hybrid search is a
  hard product requirement; the CLI process currently pays model/query setup and
  scans the sqlite-vec sidecar per command.
- Keep improving relevance evaluation with a private judged query manifest. The
  rough dogfood gate is useful for latency and smoke testing, but synthetic
  incremental markers in the isolated corpus can contaminate top results.
