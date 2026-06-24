# Work Recorder Provider Release Implementation Status

Last updated: 2026-06-24T01:29:30Z.

## Current Integration Branch

- Repo: `ctxrs/ctx`
- Worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch: `work-record`
- Current local head: this status checkpoint commit on `work-record`
  (`Record ctx records contract validation`; pending push).
- Previous certification: Buildkite public release verification build #73
  passed 26/26 for the earlier baseline head
  `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- Buildkite #74 failed on the previous checkpoint because `clippy` and the
  gated live-provider E2E lane exposed release-gate issues. The clippy issue
  was fixed earlier. The current `38d7864` checkpoint also makes the
  live-provider gated job truly opt-in and keeps normal CI on lane definitions
  only. A new Buildkite verification run is required after this status
  checkpoint is pushed.

## Scope

This program owns only the Work Recorder `ctx` product. ADE freeze, ADE DNS,
ADE desktop release, `ade.ctx.rs` migration, production hosted launch, and
`ctx.rs` cutover are out of scope unless the user explicitly changes that.

## Active Workstreams

- Provider live/E2E coverage:
  `ctx/wr-provider-live-e2e-20260623`, worker Mencius
  (`019ef68e-c900-7ad0-bad9-4cd0eeb45c62`).
- Release/R2/Buildkite/Hetzner 0.1.0 infrastructure:
  `ctx/wr-release-buildkite-010-20260623`, worker Laplace
  (`019ef68e-cc12-7232-a10d-1dc765db637c`).
- Dashboard visual/provider polish:
  `ctx/wr-dashboard-visual-provider-20260623`, worker Einstein
  (`019ef68e-d1ed-7392-82c1-f3a5b93c1d54`).
- Docs/security/site truth:
  `ctx/wr-docs-security-site-20260623`, worker Lorentz
  (`019ef68e-cf02-7da2-b15f-a6bad986d8ea`).
- Private hosted/API staging contract:
  `ctx-private` branch `ctx/wr-hosted-contract-20260623`, worker Locke
  (`019ef68e-d914-7b63-a5c2-c610cf017102`).
- Read-only architecture/release/SDLC review:
  Socrates (`019ef68e-dbfb-7eb2-9e49-812dfc43d901`).

## Current State

- Controlling plan copied into this repo for provenance.
- Product decisions from the user conversation were incorporated into
  `exec_plan.md` on 2026-06-23 after the release-lane integration began. These
  decisions supersede stale path/name/setup/dashboard assumptions in the current
  branch.
- Current local head before the product-decision implementation split:
  `8845bc4 Add Work Recorder 0.1.0 release candidate lanes`; this is one commit
  ahead of `origin/work-record` and will be followed by a product-decision plan
  checkpoint before worker branches are created.
- Initial public and private worktrees created.
- Implementation agents launched for the initial provider/release/docs/hosted
  streams. Dashboard/CLI polish was launched through the alternate worker-agent
  path after stale prior ctx-MCP children consumed the ctx-MCP session limit.
- Public implementation was reassigned to fresh `*-active` branches from the
  pushed `8cc7719` checkpoint after the original ctx-MCP public workers stayed
  queued without branch changes.
- Active alternate-worker branches:
  - `ctx/wr-provider-architecture-active`
  - `ctx/wr-provider-p0-codex-claude-pi-opencode-active`
  - `ctx/wr-provider-p0-antigravity-gemini-cursor-active`
  - `ctx/wr-provider-longtail-active`
  - `ctx/wr-vcs-pr-active`
  - `ctx/wr-dashboard-cli-polish`
- `ctx/wr-release-docs-active` was created from `8cc7719`, but a fresh
  alternate worker could not be launched yet because the alternate pool reached
  its six-agent limit. Release/docs remains covered by the earlier ctx-MCP
  worker until a fresh slot opens.
- Workers were instructed to avoid broad concurrent Cargo and use focused,
  resource-safe validation.
- Integration worker `ctx/wr-integrate-longtail-matrix` reconciled
  `858b115 Classify long-tail work recorder providers` into the current shared
  provider matrix schema. The integration preserved current provider IDs,
  carried source evidence URLs into matrix metadata, added path-existence-only
  P1/P2 `discovered_unsupported` CLI discovery rows, and left separate
  historical `copilot` and `droid_factory_ai` rows blocked pending an alias vs
  separate-contract decision. Validation was not run per manager instruction.
- Product-decision implementation was integrated and pushed through
  `1c895fe51a92a3ad12d4916605c5e65727e13e32`.
- Old worker threads were closed after their commits/reviews were integrated.
  Fresh workers were launched from the pushed checkpoint so the remaining work
  is no longer dependent on stale queued/interrupted branches.
- Buildkite API branch-trigger attempt without override returned
  `Branches have been disabled for this pipeline`. The manager retried with
  `ignore_pipeline_branch_filters: true`, which scheduled build #74 for the
  current head.
- Buildkite #74 result for `1c895fe51a92a3ad12d4916605c5e65727e13e32`:
  failed.
  - `fmt`, docs, and pipeline contract passed.
  - `clippy` failed on three `clippy::derivable_impls` findings in
    `crates/work-record-core/src/provider.rs`.
  - `gated live provider E2E` was `broken` on the
    `release-linux-managed`/`release-linux-x64-stage` queue.
  - Manager fixed the clippy failure in
    `6b450b7 Derive provider default enums` and validated locally with
    `cargo-lowio clippy -p work-record-core --all-targets -- -D warnings`.
  - The broken gated live provider E2E job was routed to the provider and
    release workers for semantics/pipeline remediation.

## 2026-06-23 Product Contract Checkpoint

Public checkpoint:

- Commit: `38d7864 Tighten ctx records product contract`.
- Scope:
  - Public CLI/report/dashboard/publish language now uses `ctx` and work-record
    concepts instead of treating `Work Recorder` as the product name.
  - PR publish markers now use `ctx-records:pr-comment`.
  - `ctx setup --no-open` and headless setup still start/reuse the local
    dashboard and print the dashboard URL plus `ctx dashboard --no-open`.
  - Codex provider matrix no longer claims assistant/tool-call/parent-child
    fidelity for prompt-history imports.
  - Dogfood screenshot docs match the six generated dashboard screenshots.
  - The Buildkite `provider-live-e2e-gated` job is gated by
    `CTX_LIVE_PROVIDER_E2E=1` plus a provider-specific opt-in, and
    `run-selected` writes a skipped artifact if the global opt-in is absent.
- Public validations:
  - `git diff --check` passed.
  - `bash scripts/check-docs.sh` passed.
  - `bash scripts/check-buildkite-pipeline.sh` passed.

Buildkite #77 update:

- Buildkite #77 was triggered for pushed head `88ed6b2`, but the
  `provider-live-e2e-gated` step still materialized as `broken` before running:
  `https://buildkite.com/luca-king/ctx-public-release-verification/builds/77#019ef6de-eb30-4963-b9a5-0519de2c77b4`.
- Remediation after #77: removed the gated live-provider step from the default
  pipeline entirely. Default CI now records only
  `provider-live-e2e-lanes`; explicit live-provider runs remain available
  through `scripts/release-provider-live-e2e-lanes.sh run-selected` with
  `CTX_LIVE_PROVIDER_E2E=1` plus a provider-specific opt-in.
- Docs and the pipeline contract were updated to match that default
  definitions-only behavior. A new Buildkite run is required after this
  remediation is pushed.
- Follow-up public wording fix: changed the static report/dashboard empty-state
  text from `Raw transcript blobs remain withheld` to `Raw transcript objects
  remain withheld`. Validation:
  - exact stale-string scan for old product/marker/layout phrases returned no
    matches;
  - `cargo-lowio test --locked -p work-record-report -- --test-threads 1`
    passed;
  - `git diff --check` passed.

Buildkite #79 update:

- Buildkite #79 was triggered for pushed head `9992316`, but failed in
  `:rust: test` on
  `installed_gh_shim_falls_back_to_spool_when_database_is_locked`.
- Failure mode: the shared shim fallback test used `ctx setup`, which now
  correctly starts/reuses the dashboard. In the full CLI suite that background
  dashboard can race with the explicit `ctx capture import --json` by draining
  the spool first, causing the import command to report zero newly imported
  records even though capture succeeded.
- Remediation: the fallback helper now uses `ctx shim install --dir <temp>/shims`
  instead of full setup, isolating shim fallback from dashboard lifecycle.
- Validation after remediation:
  - `cargo-lowio test --locked -p ctx --test cli
    installed_gh_shim_falls_back_to_spool_when_database_is_locked --
    --test-threads 1` passed.
  - `cargo-lowio test --locked -p ctx --test cli -- --test-threads 2` passed,
    61 tests.

Buildkite #80 update:

- Buildkite #80 was triggered for pushed head `3b2f1fc`, and Rust tests passed,
  including the previously failing `installed_gh_shim_falls_back_to_spool...`
  case.
- #80 then failed in the Bazel lane because the Bazel `cargo_tests` runfiles did
  not include `docs/provider-support-matrix.json`, while
  `work-record-core` tests validate that provider matrix file.
- Remediation: `BUILD.bazel` now includes
  `docs/provider-support-matrix.json` in the `cargo_tests` data runfiles.
- Validation after remediation:
  - `cargo-lowio test --locked -p work-record-core provider_support_matrix --
    --test-threads 1` passed.
  - `CTX_REQUIRE_BAZEL=1 bash scripts/check.sh bazel` passed locally.

Buildkite #81 update:

- Buildkite #81 was triggered for pushed head `7b6f4e8`. The Bazel lane passed,
  confirming the provider matrix runfiles fix.
- #81 then failed in `:clipboard: product decision regressions` because the
  Buildkite worker for that lane did not have `rg`; the product-decision helper
  used `rg` directly, causing every required-pattern assertion to look missing.
- Remediation: `scripts/check.sh` now routes product-decision searches through a
  helper that uses `rg` when available and falls back to `grep -R -E` on minimal
  workers.
- Validation after remediation:
  - `CTX_ARTIFACT_DIR=target/ctx-artifacts/product-decision-debug
    bash scripts/check.sh product-decisions` passed.
  - `env PATH=/bin:/usr/sbin
    CTX_ARTIFACT_DIR=target/ctx-artifacts/product-decision-grep-debug
    bash scripts/check.sh product-decisions` passed, proving the no-`rg`
    fallback.
  - `git diff --check` passed.
  - `bash scripts/check.sh product-decisions` passed.
  - `CTX_ARTIFACT_DIR=target/ctx-artifacts/provider-live-e2e-skip
    ./scripts/release-provider-live-e2e-lanes.sh run-selected` passed and wrote
    a skipped artifact with the global opt-in disabled.
  - `cargo-lowio fmt --check` passed.
  - `cargo-lowio test --locked -p ctx --test cli
    setup_status_golden_output_is_idempotent_and_validate_work --
    --test-threads 1` passed.
  - `cargo-lowio test --locked -p ctx --test cli
    setup_headless_skips_dashboard_without_no_open -- --test-threads 1`
    passed.
  - `cargo-lowio test --locked -p ctx --test cli
    dashboard_interactive_golden_output_starts_reuses_and_respects_open_modes --
    --test-threads 1` passed.
  - `cargo-lowio test --locked -p work-record-report -p work-record-publish --
    --test-threads 1` passed.
  - `PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome npx playwright
    test tests/dashboard.spec.ts --workers=1` passed in `apps/ctx-dashboard`.
- Review status:
  - Private hosted reviewer returned PASS for the dirty staging-boundary diff.
  - Public reviewer found the live-provider gate issue on the pre-checkpoint
    head; that issue is fixed in `38d7864`.
  - Public re-review of `38d7864` found stale dogfood script help and CLI test
    assertions that still used old `Work Recorder`, `ctx work recorder passive
    capture`, `ctx-work-record:finished-product`, and `ctx Work Recorder`
    public strings. The manager fixed those strings in the follow-up status
    checkpoint and reran focused validation before push.
- Follow-up public validations after the re-review:
  - `rg -n "ctx work recorder passive capture|ctx-work-record:finished-product|Work Recorder Finished Product|ctx Work Recorder|Build local-only Work Recorder"
    scripts crates/ctx-cli/tests crates/work-record-report
    crates/work-record-publish apps/ctx-dashboard docs -g '!target'` returned
    no matches.
  - `git diff --check` passed.
  - `cargo-lowio test --locked -p ctx --test cli
    setup_can_activate_passive_capture_in_shell_rc_and_deactivate_it --
    --test-threads 1` passed.
  - `cargo-lowio test --locked -p ctx --test cli
    publish_pr_comment_dry_run_renders_marker_bounded_redacted_markdown --
    --test-threads 1` passed.
  - `cargo-lowio test --locked -p ctx --test cli
    dashboard_export_writes_static_local_html_report -- --test-threads 1`
    passed.
  - `cargo-lowio fmt --check` passed.
  - `bash scripts/check-docs.sh` passed.
  - `bash scripts/check.sh product-decisions` passed.
  - `bash scripts/check-buildkite-pipeline.sh` passed.

Buildkite #82 update:

- Buildkite #82 was triggered for pushed head `1cfe797`. The Rust tests,
  examples, Bazel, provider fixture, provider live lane definition, product
  decision, and rich search/context lanes passed.
- #82 failed in `:bar_chart: dashboard/report artifact review` after `npm ci`
  because the managed Linux worker did not have the Playwright Chromium binary
  in its local cache. The screenshot review command failed before it could
  produce `desktop-overview.png`, and the dashboard visual gate correctly
  rejected the missing screenshot artifact.
- Initial remediation: the dashboard/report Buildkite lane was changed to
  install Playwright Chromium after `npm ci` and before
  `./scripts/check.sh dashboard-report-artifact-review`.
- Local validation for this remediation:
  - `bash scripts/check-buildkite-pipeline.sh` passed and checked that the
    dashboard/report lane installed Playwright Chromium.
  - `git diff --check` passed.
  - `npm --prefix apps/ctx-dashboard exec playwright --version` passed
    (`9.2.0`), confirming the local project Playwright command is available.

Buildkite #83 update:

- Buildkite #83 was triggered for pushed head `97de373`. It passed the core
  Rust/check/test/Bazel/product-decision/provider lanes that had exposed prior
  regressions.
- #83 failed in `:bar_chart: dashboard/report artifact review` after
  Playwright Chromium was installed. The browser binary launched, then exited
  with `libnspr4.so: cannot open shared object file`, so the worker is missing
  Playwright's Linux runtime dependencies, not only the browser cache.
- Remediation: the dashboard/report Buildkite lane now runs
  `npm --prefix apps/ctx-dashboard exec playwright install --with-deps chromium`
  after `npm ci` and before the screenshot review. The pipeline contract now
  requires this dependency-aware install command so future edits do not
  accidentally drop it.
- Local validation for this remediation:
  - `bash scripts/check-buildkite-pipeline.sh` passed and now checks for the
    dependency-aware Playwright install command.
  - `git diff --check` passed.

Buildkite #84 update:

- Buildkite #84 was triggered for pushed head `25a9659`. The same
  dashboard/report lane failed with the same `libnspr4.so` missing-library
  error.
- The #84 log showed the command
  `npm --prefix apps/ctx-dashboard exec playwright install --with-deps chromium`
  running, but no Playwright dependency-install output appeared before the
  dashboard review. The likely cause is npm argument parsing: without `--`,
  `npm exec` can consume option-looking arguments before they reach
  Playwright.
- Remediation: the dashboard/report Buildkite lane now runs
  `npm --prefix apps/ctx-dashboard exec -- playwright install --with-deps chromium`
  so `--with-deps` is unambiguously passed to Playwright. The pipeline contract
  now requires the exact `npm exec --` invocation.
- Local validation for this remediation:
  - `bash scripts/check-buildkite-pipeline.sh` passed and now checks for the
    exact `npm exec --` invocation.
  - `git diff --check` passed.
  - `npm --prefix apps/ctx-dashboard exec -- playwright --version` passed
    (`Version 1.61.0`), confirming the argument separator form works with the
    local npm/Playwright toolchain.

Buildkite #85 / Codex import integration update:

- Buildkite #85 was triggered for pushed head `3c6a8d3`. It failed before the
  dashboard lane in `:test_tube: examples` with
  `vtable constructor failed: work_record_search` / `The database schema
  changed`.
- Root cause found locally: normal `Store::open` recreated FTS virtual tables
  on every open. `ctx setup` now correctly starts a dashboard server, so the
  examples lane can have one dashboard connection and one CLI connection
  touching the same SQLite database. Recreating FTS tables under another
  connection invalidates prepared statements.
- Remediation: `264f09e Avoid recreating FTS tables on store open` makes FTS
  initialization non-destructive and adds
  `opening_second_store_does_not_recreate_fts_tables_under_readers`.
- Side-task merge: `066436f Merge Codex session import dogfood slice` merged
  `origin/ctx/wr-codex-session-import-dogfood` into `work-record`, including
  commit `5307d113bc5b4115db374003289ce60e27114a92`.
- Codex session import is now treated as implemented in this integration
  branch:
  - `ctx capture import-codex-sessions --input <path> --json`;
  - `ctx capture import-local-providers` prefers `~/.codex/sessions`;
  - default `ctx setup` performs bounded fast Codex session import
    (newest 250 session files / 64 MiB cap);
  - imported sessions produce per-session Work Records with stable IDs.
- Local validation:
  - `cargo-lowio test --locked -p work-record-store
    opening_second_store_does_not_recreate_fts_tables_under_readers --
    --test-threads 1` passed.
  - `CTX_ARTIFACT_DIR=target/ctx-artifacts/examples-post-codex-merge
    bash scripts/check.sh examples` passed. This reproduced the Buildkite
    examples lane locally, exercised setup's bounded Codex sessions import, and
    imported 85 sessions / 2,765 events from the local Codex session tree.
  - `cargo-lowio test --locked -p work-record-store -p work-record-capture
    -p ctx -- --test-threads 1` passed: 63 CLI tests, 20 capture tests,
    46 store tests, and doctests.
  - `cargo-lowio test --locked -p work-record-core provider_support_matrix --
    --test-threads 1` passed.
  - `bash scripts/check-docs.sh` passed.
  - `bash scripts/check.sh product-decisions` passed.
  - `bash scripts/check-buildkite-pipeline.sh` passed.
  - `cargo-lowio fmt --check` passed.
  - `git diff --check` passed.

Buildkite #86 update:

- Buildkite #86 was triggered for pushed head
  `72424738de083783fee73cd0272a1dc987886bb5`. It failed in
  `:rust: clippy` before the rest of the matrix ran.
- Failure mode: clippy on the Buildkite Rust toolchain rejected
  `unwrap_or_else(|| "imported")` in the Codex session Work Record title
  fallback with `clippy::unnecessary-lazy-evaluations`.
- Remediation: changed the fallback to `unwrap_or("imported")` and let
  rustfmt normalize the expression.
- Local validation:
  - `CTX_ARTIFACT_DIR=target/ctx-artifacts/clippy-after-codex-import-fix
    bash scripts/check.sh clippy` passed.
  - `cargo-lowio fmt --check` passed.
  - `git diff --check` passed.

Private hosted checkpoint:

- Commit: `4823ec837 Keep Work Recorder hosted staging reserved` on
  `ctx-private` branch `ctx/work-recorder-hosted-team`.
- Scope:
  - Hosted export route validates request shape but returns an explicit 501
    reserved/staging response instead of fake queued success.
  - Work Recorder worker Wrangler/package surfaces are staging-only; no prod
    route, prod Wrangler env, or `deploy:prod` remains for this worker.
  - Hosted allowed visibility/redaction states exclude `sync_full` and `raw` in
    code and Neon constraints.
  - Readiness defaults the Work Recorder profile to staging only.
- Private validations:
  - `git diff --check` passed.
  - `pnpm -C work-recorder-worker typecheck` passed.
  - `pnpm -C work-recorder-worker exec vitest run test/worker.test.ts
    --pool=threads` passed, 30 tests.
  - `pnpm -C work-recorder-worker readiness:check:local` passed with only the
    staging Wrangler environment reported.
  - `pnpm -C llm-relay-worker exec vitest run
    test/cloudflare-neon-readiness.test.mjs --pool=threads` passed, 8 tests.

## Validation

- Integrated shared provider capture contract:
  `e9cb475 Add shared provider capture contract`.
- Focused validations run serially under `/usr/local/bin/cargo-lowio` with
  `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p work-record-core provider_ -- --test-threads 1`
    passed.
  - `cargo-lowio test -p work-record-capture provider_fixture_replay --
    --test-threads 1` passed.
  - `cargo-lowio test -p work-record-capture
    codex_history_import_is_prompt_only_summary_fidelity_and_idempotent --
    --test-threads 1` passed.
  - `cargo-lowio test -p work-record-store
    sync_cursor_roundtrips_source_position_metadata -- --test-threads 1`
    passed.
- Integrated dashboard/CLI polish:
  `ce832d5 Polish work recorder CLI JSON and dashboard provider views`.
- Additional focused validations:
  - `cargo-lowio test -p ctx --test cli
    root_setup_status_schema_and_validate_work -- --nocapture --test-threads 1`
    passed.
  - `npm run build` in `apps/work-recorder-dashboard` passed with Vite's
    existing chunk-size warning.
- Integrated VCS/shim hardening:
  `eaef996 Harden Work Recorder VCS shims`.
- Manager follow-up fix:
  - Added the missing `PassiveShimStatus::Unreadable` match arms for CLI JSON
    state/path reporting.
  - Changed shim temp fallback so explicit unusable scratch/data dirs cause a
    direct exec of the wrapped command instead of falling through to `/tmp`,
    preserving stdout/stderr on hosts where `/tmp` can create dirs but fail
    writes under quota.
- VCS/PR focused validations run serially under `/usr/local/bin/cargo-lowio`
  with `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p ctx --test cli
    installed_shim_uses_system_utilities_when_path_shadows_capture_helpers --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    installed_shim_preserves_real_command_when_capture_scratch_is_unavailable
    -- --test-threads 1` passed after the manager follow-up fix.
  - `cargo-lowio test -p ctx --test cli
    root_status_reports_unreadable_path_shim_without_failing --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    pr_parse_json_reports_confidence_labeled_link -- --test-threads 1`
    passed.
  - `cargo-lowio test -p ctx --test cli
    publish_pr_comment_dry_run_renders_marker_bounded_redacted_markdown --
    --test-threads 1` passed.
- Integrated Work Recorder docs/site preview:
  `1d07661 Integrate Work Recorder docs site preview`.
- Docs/site preview validations:
  - `git diff --check HEAD^ HEAD` passed for the docs-site commit.
  - `./scripts/check-docs.sh` passed.
  - `npm run build` in `apps/work-recorder-dashboard` passed with Vite's
    existing chunk-size warning and regenerated tracked `dist/` assets for the
    dashboard/site preview bundle.
  - `TMPDIR=/var/tmp/ctxwr-site-preview
    PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/google-chrome npm run test --
    tests/site-preview.spec.ts` passed after a narrow Playwright locator fix
    for the tabbed site preview.
- Site preview screenshot artifacts:
  - `target/ctx-artifacts/dashboard-react/desktop-site-preview-desktop-overview.png`
  - `target/ctx-artifacts/dashboard-react/desktop-site-preview-mobile-boundaries.png`
  - `target/ctx-artifacts/dashboard-react/mobile-site-preview-desktop-overview.png`
  - `target/ctx-artifacts/dashboard-react/mobile-site-preview-mobile-boundaries.png`
- Manager visual inspection:
  - Desktop overview/provider taxonomy is visually usable, with the provider
    matrix and release/install posture populated instead of sparse placeholder
    panels.
  - Mobile boundaries/install preview fits without text overflow and keeps the
    Work Recorder-only scope explicit.
- Integrated long-tail provider classifications:
  `48b7b23 Integrate long-tail provider classifications`.
- Manager follow-up fix:
  - Updated `work-record-report`'s embedded dashboard asset list to match the
    generated Vite dashboard bundle after the docs-site preview split. The
    stale embedded `index-*` asset names caused CLI tests that compile
    `work-record-report` to fail before test execution.
- Long-tail focused validations run serially under `/usr/local/bin/cargo-lowio`
  with `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p work-record-core
    provider_support_matrix_scaffold_parses_and_covers_all_provider_ids --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    import_local_providers_reports_longtail_detected_unsupported_rows --
    --test-threads 1` passed after the dashboard asset embed fix.
  - `cargo-lowio test -p ctx --test cli
    import_local_providers_imports_codex_history_and_reports_unsupported_native_hooks
    -- --test-threads 1` passed.
- Integrated P0 provider coverage:
  `dc6ca16 Integrate P0 provider coverage`.
- P0 provider integration decisions:
  - Preserved the shared provider capture contract and ported Pi session JSONL
    into the common `ProviderCaptureAdapter` path.
  - Preserved the long-tail provider inventory from `48b7b23` while adding P0
    Codex/Pi supported-import rows and fixture-only rows for Claude Code,
    OpenCode, Antigravity CLI, Gemini CLI, and Cursor.
  - Kept passive provider-native hooks explicitly out of scope for the current
    public candidate. Passive capture remains limited to local Git/jj/gh shim
    command activity.
- Manager follow-up fixes:
  - Fixed Pi session cursor construction to read `occurred_at` from the
    concrete provider event envelope instead of an `Option`.
  - Updated the Pi session redaction-count test to match the stricter merged
    privacy behavior.
  - Added provisional Work Record creation for explicit Pi session imports,
    matching the existing provider fixture import path and avoiding dangling
    `work_record_id` foreign keys during session/event persistence.
  - Reconciled the provider support status helper signature with long-tail
    provider rows by assigning them the public `detected-unsupported` status.
- P0 provider focused validations run serially under
  `/usr/local/bin/cargo-lowio` with `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p work-record-capture
    pi_session_import_replays_documented_session_jsonl_and_is_idempotent --
    --test-threads 1` passed after the cursor/redaction fixes.
  - `cargo-lowio test -p work-record-capture
    provider_fixture_replay_supports_opencode_fixture -- --test-threads 1`
    passed.
  - `cargo-lowio test -p work-record-capture
    provider_fixture_replay_supports_antigravity_gemini_and_cursor --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    provider_fixture_import_supports_additional_p0_fixture_providers --
    --test-threads 1` passed after the long-tail support-status fix.
  - `cargo-lowio test -p ctx --test cli
    pi_session_import_json_reports_documented_session_fidelity --
    --test-threads 1` passed after the provisional-record fix.
  - `cargo-lowio test -p ctx --test cli
    import_local_providers_imports_codex_history_and_reports_unsupported_native_hooks
    -- --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    import_local_providers_imports_discovered_pi_sessions -- --test-threads 1`
    passed.
  - `cargo-lowio test -p work-record-store
    migration_upgrades_existing_v1_mvp_store_with_rich_schema --
    --test-threads 1` passed.
- Integrated release/CI product-decision enforcement:
  `55646d6 Enforce ctx records product decisions in release CI`.
- Release/CI product-decision checks:
  - `bash -n scripts/check.sh scripts/check-buildkite-pipeline.sh
    scripts/release-candidate-metadata.sh
    scripts/release-completion-certificate.sh scripts/release-platform-blocker.sh
    scripts/release-provider-live-e2e-lanes.sh scripts/install.sh` passed.
  - `bash scripts/check-docs.sh` passed.
  - `bash scripts/check-buildkite-pipeline.sh` passed and now verifies the
    product-decision lane plus completion-certificate product-decision evidence.
  - `git diff --check HEAD^ HEAD` passed.
  - `bash scripts/check.sh product-decisions` intentionally failed before the
    remaining product-decision implementation/docs branches land. Current
    blockers are stale public README/docs/dashboard-source `work-record`,
    `~/.ctx/work-record`, `blobs`, and `inbox` wording plus missing
    implementation/golden-test markers for `objects/`, `spool`, setup/status/
    dashboard/uninstall outputs, setup idempotency, old ADE ignore, and
    dashboard interactive/headless/`--no-open` behavior.
- Integrated root layout/migration:
  `8460b3b Migrate work recorder local layout`.
- Root layout/migration validations run serially under
  `/usr/local/bin/cargo-lowio` with `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p work-record-core
    local_layout_paths_are_flat_under_data_root -- --test-threads 1` passed.
  - `cargo-lowio test -p work-record-core
    ctx_data_root_env_is_the_ctx_root_itself -- --test-threads 1` passed.
  - `cargo-lowio test -p work-record-store
    store_open_creates_flat_objects_and_spool_layout -- --test-threads 1`
    passed.
  - `cargo-lowio test -p work-record-store
    store_open_migrates_old_work_record_layout_to_flat_root -- --test-threads
    1` passed.
  - `cargo-lowio test -p work-record-store
    store_open_leaves_old_layout_when_flat_destination_exists --
    --test-threads 1` passed.
  - `cargo-lowio test -p work-record-store
    old_ade_state_dirs_are_ignored_by_flat_layout_open -- --test-threads 1`
    passed.
  - `cargo-lowio test -p work-record-store
    store_open_and_blob_writes_repair_private_permissions -- --test-threads 1`
    passed.
- Integrated docs/site naming:
  `c80af9a docs: align ctx work records naming`.
- Manager follow-up fixes:
  - Resolved release-doc conflicts in favor of `ctx-release-artifacts` and the
    `ctx/records/release-candidate/v0.1.0/<git-commit>` public staging layout.
  - Renamed the README banner asset to avoid `work-record` in the public path.
  - Replaced avoidable dashboard demo references to internal `work-record-*`
    crate/app names.
  - Narrowed the product-decision scan to allow the machine-readable provider
    support matrix to carry internal source paths.
  - Rebuilt `apps/work-recorder-dashboard/dist` and updated the
    `work-record-report` embedded dashboard asset list for the new hash.
- Docs/site validations:
  - `bash scripts/check-docs.sh` passed.
  - `git diff --check` passed after merge conflict resolution and follow-up
    cleanup.
  - `npm run build` in `apps/work-recorder-dashboard` passed.
  - `bash scripts/check.sh product-decisions` now fails only on setup/status/
    dashboard/uninstall golden output markers, setup idempotency marker, and
    dashboard interactive/headless/`--no-open` coverage markers. The stale
    public `work-record`, `~/.ctx/work-record`, `blobs`, and `inbox` wording
    blockers are cleared.
- Integrated setup/dashboard/service CLI UX:
  `5917606 Update setup dashboard service CLI UX`.
- Manager follow-up fixes:
  - Updated setup/uninstall/shell-rc tests to use flat `shims/` and
    `work.sqlite` paths after root layout migration.
  - Renamed privacy doctor output from `inbox_dir`/`permissions_inbox` to
    `spool_dir`/`permissions_spool`.
  - Removed an unused old-layout import.
  - Tightened shim uninstall to remove the empty ctx-owned shim directory after
    removing shim files.
  - Renamed focused tests so the `product-decisions` CI gate can detect setup,
    status, dashboard, uninstall, idempotency, and interactive-open coverage.
- Setup/dashboard/service focused validations run serially under
  `/usr/local/bin/cargo-lowio` with `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p ctx --test cli
    setup_status_golden_output_is_idempotent_and_validate_work --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    setup_headless_skips_dashboard_without_no_open -- --test-threads 1`
    passed.
  - `cargo-lowio test -p ctx --test cli
    dashboard_interactive_golden_output_starts_reuses_and_respects_open_modes --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    optional_service_is_explicit_and_reversible -- --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    uninstall_golden_output_removes_shims_and_handles_product_data --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    setup_can_activate_passive_capture_in_shell_rc_and_deactivate_it --
    --test-threads 1` passed.
  - `bash scripts/check.sh product-decisions` passed after docs/site and CLI UX
    integration.
- Integrated spool/shim fallback:
  `aae6e61 Isolate shim capture fallback to spool`.
- Manager follow-up checkpoint:
  `6f0c7ee Validate spool fallback under ctx root`.
- Spool/shim integration review:
  - Nash (`019ef686-a9d0-71c1-9ee7-e75d10da4072`) reviewed the merged slice
    read-only and found stale test paths that still referenced the old
    `work-record/inbox`, `work-record/shims`, and `work-record/work.sqlite`
    layout.
  - The review also identified missing locked-database fallback coverage for
    `gh` and `jj`; the manager added those tests alongside the existing `git`
    fallback case.
- Manager follow-up fixes:
  - Added the missing `ArchiveCounts::add` helper required by multi-envelope
    spool imports.
  - Updated CLI tests to use the flat ctx root layout: `work.sqlite`,
    `spool/`, and `shims/`.
  - Added locked-DB fallback coverage for all installed passive capture shims:
    `git`, `gh`, and `jj`.
  - Updated the path-shadowing shim test to match direct SQLite capture: no
    queued spool import remains, but the captured command evidence is present in
    the local store and export output.
  - Committed the `rusqlite` test dependency lockfile update used by the
    locked-DB CLI fallback tests.
- Spool/shim validations run serially under `/usr/local/bin/cargo-lowio` with
  `TMPDIR=$PWD/target/tmp`:
  - `cargo-lowio test -p work-record-capture
    shim_capture_imports_directly_when_database_is_available --
    --test-threads 1` passed after the accumulator fix.
  - `cargo-lowio test -p work-record-capture -- --test-threads 1` passed:
    19 tests, including direct import, DB-lock fallback, malformed spool,
    symlink rejection, failed spool retention, provider fixture replay, and Pi
    session replay.
  - `cargo-lowio test -p ctx --test cli
    installed_git_shim_runs_real_command_and_records_capture --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    falls_back_to_spool_when_database_is_locked -- --test-threads 1` passed:
    `git`, `gh`, and `jj` variants.
  - `cargo-lowio test -p ctx --test cli
    installed_jj_shim_runs_real_command_and_records_capture --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    installed_gh_shim_runs_real_command_and_records_capture --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    shim_capture_command_isolates_scratch_read_and_timestamp_errors --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli
    doctor_and_repair_retry_failed_capture_spool_files -- --test-threads 1`
    passed.
  - `cargo-lowio test -p ctx --test cli
    validate_reports_failed_and_processing_capture_spool_files --
    --test-threads 1` passed.
  - `cargo-lowio test -p ctx --test cli -- --test-threads 1` passed: 61 CLI
    tests.
  - `cargo fmt --manifest-path Cargo.toml --all -- --check` passed after
    running `cargo fmt`.
  - `bash scripts/check.sh product-decisions` passed.
  - `bash scripts/check-docs.sh` passed.
  - `git diff --check` passed.

Concurrent worker Cargo/rustc processes were stopped by the manager after they
violated the host-level resource-safety rule. Remaining validation should be
run by the manager, one command at a time, under the global Cargo lock.

## Orchestration Correction

The manager started manually porting the Antigravity/Gemini/Cursor provider
branch after stopping unsafe concurrent worker validation. That was corrected:

- The partial manual provider-port patch was stashed locally as
  `partial p0 provider manual port before subagent handoff`.
- The public integration branch was returned to a clean state at pushed head
  `515b069`.
- Fresh integration worktrees were created from `origin/work-record`:
  - `ctx/wr-integrate-p0-providers`
  - `ctx/wr-integrate-longtail-matrix`
  - `ctx/wr-integrate-docs-site`
  - `ctx/wr-release-010-active2`
- Fresh private integration worktree:
  - `ctx-private` branch `ctx/wr-hosted-contract-integrate`
- New workers own those branches and were instructed not to run Cargo/npm/
  Playwright validation. The manager will merge reviewed commits one at a time
  and run serial validation only.

## Open Coordination Items

- Implement the binding product decisions now tracked in `exec_plan.md`:
  canonical `~/.ctx` flat layout, `objects/`, `spool/`, low-friction
  non-service `ctx setup`, localhost dashboard process model, opt-in service,
  uninstall semantics, and public naming cleanup.
- Use the new parallel worker branches:
  - `ctx/wr-root-layout-migration` at
    `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-root-layout-migration`;
    worker `Ptolemy` (`019ef672-334c-7713-9458-a5eeee966cea`).
  - `ctx/wr-spool-shim-fallback` at
    `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-spool-shim-fallback`;
    worker `Ramanujan` (`019ef672-2fe6-7001-bd47-7cc6a02889ec`).
  - `ctx/wr-setup-dashboard-service-ux` at
    `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-setup-dashboard-service-ux`;
    worker `Godel` (`019ef672-36a8-7072-bba9-657d081ed16a`).
  - `ctx/wr-docs-site-naming` at
    `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-docs-site-naming`;
    worker `Boole` (`019ef672-3a22-7551-9d9d-c20318351f7d`).
  - `ctx/wr-release-ci-product-decisions` at
    `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-release-ci-product-decisions`;
    worker `Lovelace` (`019ef672-3deb-7953-ba87-c64bbe1b7dbf`).
- Workers were instructed not to run broad Cargo/npm/Playwright jobs. Manager
  validation remains serial under `/usr/local/bin/cargo-lowio` after commits
  are merged.
- Keep provider support claims aligned with the support taxonomy in
  `exec_plan.md`.
- Record Buildkite, R2, Hetzner, provider live E2E, docs preview, product
  decision checks, and final completion-certifier evidence here as the program
  advances.
