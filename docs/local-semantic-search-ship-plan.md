# Local Semantic Search Ship Plan

This plan records the implementation and rollout path for local semantic search.
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
- Equal public-platform support uses one embedding model and one runtime family:
  `intfloat/multilingual-e5-small` through ONNX Runtime. The model produces
  384-dimensional vectors and requires `query: ` for query inputs and
  `passage: ` for indexed document chunks. The portable
  product shape is ctx-managed dynamic runtime assets, not Cargo build-time
  `ort-sys` downloads. The CLI artifact names remain stable; runtime archives
  are separate release sidecars acquired by installer metadata or daemon/runtime
  acquisition.
- Until a platform has both daemon query transport and a validated local ONNX
  Runtime asset, that artifact must remain lexical-safe and report semantic
  unavailable rather than pretending to support embeddings.

## Current Branch Addendum

- Production semantic embeddings have migrated from
  `sentence-transformers/all-MiniLM-L6-v2` to
  `intfloat/multilingual-e5-small`. Query and passage role prefixes are applied
  exactly once, and the new model key prevents pre-migration vectors from being
  counted as E5 sidecar coverage.
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
- On semantic-capable Unix builds, the daemon now exposes a private `0600` Unix
  socket query service for query embeddings. CLI search no longer initializes or
  downloads the embedding model in the foreground; semantic/hybrid search asks
  the daemon query service for the query vector, then performs local vector
  scan/hydration/ranking.
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
- Release plumbing now has a stable ONNX Runtime sidecar naming convention:
  `ctx-onnxruntime-linux-x64.tar.gz`,
  `ctx-onnxruntime-linux-aarch64.tar.gz`,
  `ctx-onnxruntime-macos-arm64.tar.gz`,
  `ctx-onnxruntime-macos-x64.tar.gz`,
  `ctx-onnxruntime-windows-x64.zip`, and
  `ctx-onnxruntime-freebsd-x64.tar.gz`. Runtime producer jobs may use validated
  `.tar.zst` intermediates, but release metadata and end-user installers only
  consume the checksum-verified `.tar.gz` transport and do not require zstd.
- Release metadata can describe those sidecars with
  `CTX_RELEASE_ONNXRUNTIME_ARTIFACT_<platform_key>`,
  `CTX_RELEASE_ONNXRUNTIME_SHA256_<platform_key>`, and
  `CTX_RELEASE_ONNXRUNTIME_VERSION`. The development installers place runtime
  assets under the selected runtime root at
  `onnxruntime/<version>/<platform>`.
- The public Buildkite pipeline has an opt-in runtime smoke matrix gated by
  `CTX_PUBLIC_CLI_NATIVE_SMOKE_MATRIX=1`. Each smoke installs and runs the exact
  CLI artifact and sidecar in an isolated data root, then publishes an
  exact-binary, exact-runtime proof. Cross-build and translated smoke evidence
  cannot satisfy a native release gate.

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
  the daemon query service, foreground search no longer loads the model, and
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

### 8. Equal Platform Runtime And Release Plumbing

- Target architecture: one embedding model and ONNX Runtime index format on
  every public platform. Core ML is an accelerator backend, not a second
  semantic corpus.
- Keep CLI artifact names stable. Runtime sidecars are additive assets named
  `ctx-onnxruntime-<platform>.<archive>` and preserve license and notice files.
- Release metadata may omit runtime keys entirely, but a published runtime must
  include artifact name, SHA-256, and version metadata as one complete set.
- Managed runners produce and natively smoke Linux x64, Linux arm64, and macOS
  arm64 when the native smoke matrix is enabled.
- Native Intel macOS x64, Windows x64, and FreeBSD x64 use gated
  `release-*-native` Buildkite queues. Release staging fails closed until all
  six canonical sidecars and native proofs are present.
- Windows native proof uses
  `scripts/smoke-daemon-semantic-release.ps1 -ProofOutput <path>`. Unix-like
  native hosts use
  `scripts/smoke-daemon-semantic-release.sh --proof-output <path>`.
- Fast-fail review criteria:
  - no semantic success claim without native exact-artifact smoke;
  - no fallback to a different embedding model without an index compatibility
    decision;
  - no foreground search model downloads;
  - no default config writes for daemon or semantic opt-in.

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
