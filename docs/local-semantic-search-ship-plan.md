# Local Semantic Search Ship Plan

This plan captures the dogfood findings from the July 7-8, 2026 local runs on
a power-user ctx corpus and the implementation path for local semantic search.
The local Apache CLI, lexical search, setup, import/export, daemon refresh,
status, local query service, and local semantic search remain free local
functionality. Semantic search is implemented as first-class daemon
functionality, but ships during prerelease as an explicit opt-in until dogfood
and private relevance evals justify flipping the default.

## Product And API Decision

- No paid gate in the local Apache CLI for lexical search, daemon indexing,
  setup, import/export, status, local query service, or local semantic search.
- Future paid product surface should live in hosted or team/enterprise memory:
  cross-device continuity, shared/team memory, admin controls, policy,
  compliance, hosted acceleration, and LLM summaries. The free local CLI should
  stay useful enough to create trust.
- Daemon maintenance and semantic search are both disabled by default for the
  prerelease. Advanced users opt into daemon-owned background maintenance with:

  ```toml
  [daemon]
  enabled = true
  ```

- Semantic requires the daemon. The supported prerelease semantic opt-in shape
  is:

  ```toml
  [daemon]
  enabled = true

  [search]
  semantic = true
  ```

- `CTX_DAEMON_ENABLED=1` and `CTX_SEARCH_SEMANTIC=1` are available as
  operator/test overrides. `CTX_DISABLE_DAEMON=1` and
  `CTX_DISABLE_SEMANTIC_SEARCH=1` force them off.
- Daemon without semantic is valid and useful: it owns lexical incremental
  refresh and can later own additional local query-service work. The semantic
  query-embedding socket is created only when semantic is enabled. Semantic
  without daemon is invalid.
- There is no `auto` search mode. Omitted backend means lexical when semantic is
  disabled and hybrid when semantic is enabled. Explicit `--backend lexical`
  remains available.
- There is no product `max-runtime-seconds` option. Tests and dogfood can wrap
  foreground daemon commands in process-level timeouts; the product daemon runs
  until `--once`, failure, idle exit, or normal service shutdown.
- `ctx setup`, `ctx import`, and `ctx search` should not write `config.toml` for
  implicit defaults. The config file is user-managed override surface.
- `ctx setup` should be repeatable. If an existing user later enables
  `[daemon] enabled = true` and `[search] semantic = true` and reruns setup,
  setup should leave existing data intact, start daemon-owned indexing when
  possible, and let the daemon acquire the local embedding model and build
  missing semantic sidecars.
- This branch supports the semantic query service on Unix. Non-Unix semantic
  opt-in is blocked for v1 until there is an equivalent query-service transport.

## Current Branch Addendum

- Config now has `[daemon] enabled = true|false` and
  `[search] semantic = true|false`. Both are default unset/off for prerelease
  dogfood, and both have env overrides.
- Default search backend resolution is config-aware: lexical by default while
  semantic is off, hybrid by default while semantic is on, and explicit semantic
  fails fast when disabled.
- Status, doctor, MCP status, and index status report `semantic.status =
  disabled` with `reason = semantic_disabled` when semantic is not enabled.
- Setup refuses the invalid semantic-without-daemon configuration, runs
  foreground lexical indexing when daemon maintenance is not enabled, reports
  semantic background estimates only when semantic is enabled, and states that
  the daemon will download the local embedding model if needed.
- The daemon does not create or mutate semantic sidecars when semantic is
  disabled.
- When semantic is enabled and the local embedding model is missing, the daemon
  enters `acquiring_model`, downloads/initializes the model through fastembed,
  verifies the cache, and records `model_acquisition_failed` if acquisition
  fails.
- On Unix, the daemon now exposes a private `0600` Unix socket query service for
  query embeddings. CLI search no longer initializes or downloads the embedding
  model in the foreground; semantic/hybrid search asks the daemon query service
  for the query vector, then performs local vector scan/hydration/ranking.
- The query service is intentionally narrow for v1: it embeds query text only.
  Full vector search can move into the daemon later if command startup,
  sqlite-vec scan, or hydration becomes the dominant latency.
- Search with semantic enabled and default background refresh attempts to
  autostart the daemon before hybrid/semantic retrieval. Explicit
  `--refresh off` does not autostart daemon work; strict semantic fails with an
  actionable daemon-query-service error when the daemon is not running.
- Daemon query socket startup is required when semantic is enabled. If the
  socket cannot bind, daemon startup fails visibly instead of running without a
  query service.
- Daemon model acquisition shields fastembed's `HF_HOME` override while filling
  the ctx-selected cache root, preserving ctx cache precedence during download
  as well as during normal model loading.

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

## Prerelease Opt-In Dogfood Notes

- With semantic disabled by default, the completed dogfood sidecar reports
  `semantic.status = disabled` and `reason = semantic_disabled`, while retaining
  coverage counts for diagnostics.
- After adding `[daemon] enabled = true` and `[search] semantic = true` to the
  isolated dogfood root, status reported ready with 60,726-60,740 searchable
  semantic documents and about 158,663-158,688 embedded chunks. The count moved
  during dogfood because live local work continued to be imported.
- A manual daemon query-service smoke exposed a private
  `0600` `daemon/query.sock`. Strict semantic search for
  `opal maple lantern semantic count drift` found the expected incremental
  marker at Hit@1 with foreground RSS under 90 MiB.
- First query through a daemon whose embedder was not warm took about 25s wall
  because the daemon query service had to initialize the embedding model. Warm
  query embedding through the daemon dropped to 3ms, but total CLI wall stayed
  around 8s on the dogfood root because vector scan plus hydration still run in
  the foreground process over about 158k chunks.
- Default search with semantic enabled and no manual daemon autostarted the
  daemon, returned effective `hybrid`, used semantic evidence, and found the
  same marker at Hit@1 in 8.56s wall with 87 MiB foreground RSS. Diagnostics:
  query embedding 221ms, sqlite-vec scan 1.087s, hydration 2.656s, 158,688
  chunks scanned.
- Strict semantic with `--refresh off` and no running daemon now fails fast with
  `daemon semantic query service is not available`, as intended.
- A clean foreground daemon `--once` pass over the isolated dogfood root still
  spent 2m44s / 469 MiB RSS on no-op history refresh over about 14.2 GiB /
  32,451 source files. This is acceptable as background daemon work for
  prerelease, but it remains a candidate for future refresh fingerprinting or
  source-level no-op avoidance. The idle loop now checks the idle deadline
  before starting another pass, so a no-op idle daemon does not launch an extra
  expensive refresh just to discover it should exit.

## Ship Goals

- `ctx setup` runs foreground lexical indexing by default during prerelease.
  When daemon maintenance is explicitly enabled, setup can start daemon-owned
  lexical indexing and report a truthful, actionable status. When semantic is
  also explicitly enabled, setup queues daemon-owned semantic indexing and model
  acquisition.
- Existing local model caches are discovered without env-var handholding; if no
  cache exists, the daemon should acquire the model or semantic status should
  explain exactly what failed.
- Semantic corpus is deterministic and small enough for local backfill:
  user-turn anchored lite-turn documents, not raw event/tool-output chunks.
- New local work is prioritized before historical backfill.
- Search output always exposes requested/effective backend and semantic fallback
  reason; common unsupported filters should fail clearly or fall back explicitly.
- While semantic is disabled, default search is lexical and explicit hybrid
  falls back with `semantic_disabled`.
- While semantic is enabled, default and explicit `hybrid` use semantic evidence
  only when semantic sidecar coverage is complete and dirty work is drained;
  partial coverage is available through explicit `semantic` for diagnostics and
  dogfood, not default ranking.
- Local dogfood on this corpus meets:
  - lexical initial refresh: under 5 minutes;
  - semantic initial backfill: about 2 hours on this 64 GB power-user
    corpus, acceptable as daemon work if it is resumable, observable, and lower
    priority than fresh incremental work;
  - lexical incremental p95: under 10 seconds;
  - semantic incremental p95: under 60 seconds after model cache is available;
  - warm hybrid search p95: target under 2.5 seconds with daemon-owned query
    embeddings and the conservative soft-filter overfetch window;
  - semantic worker RSS follows the adaptive memory budget and must remain
    below that selected budget during default daemon indexing.

## Readiness Gates

### Merge-Ready Gate For This Branch

- Code compiles with Cargo and Bazel.
- Focused semantic, search, setup/status, and MCP tests pass.
- Full `cargo test -p ctx --tests` passes.
- Dogfood root with semantic disabled reports disabled, not misleading pending
  work.
- Dogfood root with semantic enabled reports ready at full coverage.
- Default semantic-enabled search can autostart the daemon query service and
  return effective hybrid results.
- Explicit semantic with `--refresh off` and no daemon fails clearly instead of
  silently falling back.
- No public `auto` mode or `max-runtime-seconds` product option remains.
- The implementation does not check in the private judged relevance eval.

### Prerelease Opt-In Ship Gate

- At least one dogfood machine completes daemon-owned initial lexical refresh
  and semantic backfill from an existing local corpus without manual env-var
  cache setup.
- Setup/status/index watch messaging is understandable for disabled, acquiring
  model, indexing, ready, and failure states.
- Incremental semantic freshness for a single new user turn is under 60s p95
  after the model cache is available.
- Foreground search RSS remains under 150 MiB on the dogfood corpus when the
  daemon query service is available.
- Warm hybrid/semantic p95 stays under 10s on the power-user dogfood corpus,
  with a tracked path to return below 2.5s through vector/hydration
  optimization.
- No-op background refresh cost is documented and acceptable for prerelease, or
  reduced with source-level no-op avoidance.

### Default-On Flip Gate

- Private judged eval lives outside this public repo, preferably in an internal
  private repo or an untracked local eval package.
- Eval has at least 30-50 task-shaped queries from real local work, covering
  recent and older sessions, exact terms, fuzzy/natural-language searches,
  filtered searches, and negative/no-result cases.
- Hybrid beats lexical on judged quality: positive Hit@5 and MRR lift, no
  material Hit@1 regression on exact-term queries, and manually inspected
  failures have acceptable explanations.
- Hybrid fallback rate for normal unfiltered queries is low enough that default
  hybrid is not mostly lexical in practice.
- Warm hybrid p95 is at or below the product target on the dogfood corpus; if
  the target is subsecond, vector scan/hydration should move into a daemon
  query service or equivalent optimized path before the flip.
- Non-Unix support is either implemented or semantic remains gated by platform.

## Implementation Plan And Current Status

### 1. Setup, Daemon, And Status

- Done on this branch: setup, status, doctor, index status, and MCP status are
  config-aware, and setup refuses semantic-without-daemon.
- Done on earlier commits in this branch: daemon autostart/status, stale lock
  recovery, semantic-first bootstrap scheduling, bounded incremental refresh,
  and cached read-only status.
- Done in prerelease opt-in dogfood: semantic-enabled default search autostarts
  the daemon query socket, foreground search no longer loads the model, and
  strict `--refresh off` fails clearly when no daemon is available.
- Original implementation checklist:
  - `ctx setup` foreground output distinguishes inventory complete, daemon
    autostart requested, daemon definitely running, and daemon skipped or
    failed to spawn.
  - Daemon autostart bookkeeping is close enough to setup/import/search that
    the parent can write a status file when spawning fails or is skipped.
  - Status/watch/wait treat stale locks as recoverable state.
  - Background indexing is not claimed solely from pending inventory.
- Tests:
  - setup JSON/human output does not promise running daemon when autostart is
    disabled or skipped;
  - stale lock status is recovered or explicitly marked recoverable;
  - `ctx index watch` does not hang indefinitely behind a dead lock.

### 2. Semantic Model Cache Discovery

- Done on this branch: cache discovery was broadened, and daemon-owned model
  acquisition now handles a missing cache during semantic opt-in.
- Remaining after merge: dogfood the missing-model path on a throwaway root or
  mockable cache root, without deleting the real cache.
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

- Done on this branch: raw event documents were replaced by deterministic v2
  lite-turn documents with control-message filtering, lookup-table assembly,
  persistent backfill cursor, and exact cached count maintenance.
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

- Done on this branch: dirty/recent work is prioritized, bootstrap can skip
  history refresh, daemon loops keep a warm embedder, adaptive memory controls
  throughput, and clean incremental refresh was dogfooded.
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

- Decision: keep the judged relevance eval and real dogfood manifests out of
  this public repo to avoid reverse-engineering surface area. Use an internal
  private repo or local-only artifacts for judged query sets.
- Remaining outside this repo:
  - add a small JSONL manifest runner for private local dogfood/evals that
    records query, backend requested/effective, fallback code, elapsed ms,
    semantic diagnostics, and top result ids/snippets;
  - keep the harness read-only with `--refresh off` by default;
  - store real judged manifests in an internal private repo or an untracked
    local path;
  - make the default-on decision depend on the private eval gate above.

### 6. Prerelease Feature Flag Rollout

- Done on this branch:
  - `[search] semantic = true|false`;
  - `[daemon] enabled = true|false`;
  - `CTX_SEARCH_SEMANTIC`;
  - `CTX_DISABLE_SEMANTIC_SEARCH`;
  - `CTX_DAEMON_ENABLED`;
  - `CTX_DISABLE_DAEMON`;
  - default search backend is lexical until semantic is enabled;
  - daemon maintenance is default off for prerelease;
  - setup/import/search do not write default values to `config.toml`;
  - no public `auto` mode.
- Remaining product work:
  - decide whether cloud-randomized feature flags should live outside this CLI
    config path. The local CLI should continue to honor explicit TOML/env
    values as the final authority.
  - if remote rollout is added, it should only populate/override an internal
    default for users who have not explicitly set `[search] semantic`.
  - before flipping the default, ship at least one prerelease build with
    opt-in dogfood feedback, relevance review, and clear `ctx index status`
    guidance.

### 7. Daemon Query Service

- Done on this branch:
  - daemon starts a private Unix socket when semantic is enabled;
  - CLI semantic/hybrid search asks the daemon for query embeddings;
  - the query service reuses the daemon's warm embedder or initializes from an
    existing cache, but does not download independently;
  - explicit semantic search fails when the daemon query service is unavailable.
- Remaining after v1:
  - consider moving vector scan/hydration/ranking into the daemon if process
    startup or per-command sqlite opening becomes the bottleneck;
  - add a refill loop for post-vector filters so candidate count can drop below
    the conservative 1,000 soft-filter window without under-filling results.

## Parallel Implementation And Review Plan

- Main agent owns branch hygiene, test orchestration, final integration, and
  commits.
- Worker A can own daemon/query-service changes only:
  `crates/ctx-cli/src/semantic/daemon.rs`,
  `health_search.rs`, `paths_status.rs`, `preamble.rs`.
- Worker B can own config/setup/API changes only:
  `config.rs`, `main.rs`, `commands/search.rs`, `commands/setup.rs`,
  `commands/status.rs`, `commands/index.rs`, `mcp.rs`.
- Worker C can own tests only:
  `crates/ctx-cli/src/semantic/tests.rs` and `crates/ctx-cli/tests/*`.
- Explorer/adversarial reviewers should be read-only and check:
  - semantic cannot run without daemon;
  - semantic disabled never creates sidecars or downloads models;
  - default search remains lexical until opt-in;
  - explicit semantic errors are actionable;
  - setup is repeatable for existing users who opt in later;
  - foreground query does not initialize/download the model;
  - daemon query socket is private and stale sockets are cleaned up;
  - status/watch stay read-only and fast;
  - no new broad compatibility fallbacks, hidden modes, or duplicate config
    concepts are introduced.

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
- Consider moving full vector search into the daemon if subsecond
  semantic/hybrid search becomes a hard product requirement. The current branch
  removes foreground query-model setup, but each CLI command still opens the
  store/sidecar and scans sqlite-vec locally.
- Add a focused daemon query-service integration test that exercises a real
  socket with a fake or cached embedder shape, if it can be done without making
  CI download a model.
- Dogfood the model-acquisition path on a throwaway cache root, measuring the
  user-visible `acquiring_model` and `model_acquisition_failed` states without
  disturbing the real shared cache.
- Reduce no-op history refresh cost for very large local histories, likely with
  source-level fingerprints or cheaper skip checks before scanning tens of
  thousands of source files.
- Keep semantic enabled behind explicit prerelease opt-in until private judged
  evals show that hybrid beats lexical on normal task-shaped queries at full
  coverage, not just synthetic marker queries.
- Keep improving relevance evaluation with a private judged query manifest. The
  rough dogfood gate is useful for latency and smoke testing, but synthetic
  incremental markers in the isolated corpus can contaminate top results.
