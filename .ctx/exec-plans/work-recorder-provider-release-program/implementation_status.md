# Work Recorder Provider Release Implementation Status

Last updated: 2026-06-23T21:38:31Z.

## Current Integration Branch

- Repo: `ctxrs/ctx`
- Worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch: `work-record`
- Baseline head: `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`
- Previous certification: Buildkite public release verification build #73
  passed 26/26 for the baseline head.

## Scope

This program owns only the Work Recorder `ctx` product. ADE freeze, ADE DNS,
ADE desktop release, `ade.ctx.rs` migration, production hosted launch, and
`ctx.rs` cutover are out of scope unless the user explicitly changes that.

## Active Workstreams

- Provider architecture/metadata foundation:
  `ctx/wr-provider-architecture`
- Codex and Claude Code provider coverage:
  `ctx/wr-provider-codex-claude`
- Pi and OpenCode provider coverage:
  `ctx/wr-provider-pi-opencode`
- Antigravity CLI, Gemini CLI, and Cursor provider coverage:
  `ctx/wr-provider-antigravity-gemini-cursor`
- P1/P2 provider classification:
  `ctx/wr-provider-matrix-longtail`
- VCS/capture shim and Git/jj/gh/PR proof:
  `ctx/wr-vcs-shims-pr-proof`
- Release/R2/Buildkite/Hetzner 0.1.0 infrastructure:
  `ctx/wr-release-infra-010`
- Docs/site local Work Recorder preview:
  `ctx/wr-docs-site-preview`
- Private hosted/API staging contract:
  `ctx-private` branch `ctx/wr-hosted-api-contract`

## Current State

- Controlling plan copied into this repo for provenance.
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

- Merge the provider architecture branch before integrating provider-specific
  work, unless a provider worker produces an intentionally isolated patch.
- Keep provider support claims aligned with the support taxonomy in
  `exec_plan.md`.
- Record Buildkite, R2, Hetzner, provider live E2E, docs preview, and final
  completion-certifier evidence here as the program advances.
