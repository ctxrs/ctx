# Work Recorder Provider Release Implementation Status

Last updated: 2026-06-23T21:02:34Z.

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

Concurrent worker Cargo/rustc processes were stopped by the manager after they
violated the host-level resource-safety rule. Remaining validation should be
run by the manager, one command at a time, under the global Cargo lock.

## Open Coordination Items

- Merge the provider architecture branch before integrating provider-specific
  work, unless a provider worker produces an intentionally isolated patch.
- Keep provider support claims aligned with the support taxonomy in
  `exec_plan.md`.
- Record Buildkite, R2, Hetzner, provider live E2E, docs preview, and final
  completion-certifier evidence here as the program advances.
