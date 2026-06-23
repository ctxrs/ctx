# Work Recorder Provider Release Implementation Status

Last updated: 2026-06-23T20:39:30Z.

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
- Workers were instructed to avoid broad concurrent Cargo and use focused,
  resource-safe validation.

## Validation

No new product validation has run since the baseline certification. This status
file records program start only.

## Open Coordination Items

- Merge the provider architecture branch before integrating provider-specific
  work, unless a provider worker produces an intentionally isolated patch.
- Keep provider support claims aligned with the support taxonomy in
  `exec_plan.md`.
- Record Buildkite, R2, Hetzner, provider live E2E, docs preview, and final
  completion-certifier evidence here as the program advances.
