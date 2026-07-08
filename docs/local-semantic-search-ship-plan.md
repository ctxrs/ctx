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
  - semantic initial backfill: acceptable as multi-hour daemon work if it is
    resumable, observable, and lower priority than fresh incremental work;
  - lexical incremental p95: under 10 seconds;
  - semantic incremental p95: under 60 seconds after model cache is available;
  - warm hybrid search p95: under 1 second;
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
- Do not expose a daemon runtime-cap product option. Tests and dogfood scripts
  can wrap foreground daemon commands in process-level timeouts, but the daemon
  product behavior is to run until `--once`, failure, or idle exit.
- Keep the embedder warm within daemon loops.
- Prefer small, observable capped batches over hidden long-running work.
- Tests:
  - dirty queue drains before historical backfill;
  - a new assistant response updates the existing turn document hash;
  - semantic bootstrap skips history refresh and calls the semantic job first;
  - history refresh still runs when the store is missing or semantic is ready;
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

## Remaining Shippability Work

- Re-run real local dogfood after the semantic-bootstrap scheduling fix:
  confirm daemon passes skip history refresh during bootstrap, semantic chunks
  advance, and idle exit only happens once no job reports work.
- Reduce full-backfill embedding time without exceeding the memory target. The
  next viable experiments are likely reducing chunks per lite-turn document,
  using a faster local embedding runtime/model, or adding a memory-aware
  throughput profile rather than simply increasing ONNX batch/thread counts.
- Continue improving daemon history refresh incrementality for post-bootstrap
  steady state so repeated refresh passes stay cheap on the real local corpus.
- Add a real dogfood/eval command or script that records initial setup,
  daemon-job timings, RSS, incremental refresh latency, search latency, and
  relevance judgments in one artifact.
- Re-run full local dogfood after refresh scheduling is fixed:
  - fresh setup on an isolated data root;
  - full lexical completion;
  - full semantic completion;
  - appended-session incremental refresh;
  - semantic vs lexical relevance checks on representative local tasks.
