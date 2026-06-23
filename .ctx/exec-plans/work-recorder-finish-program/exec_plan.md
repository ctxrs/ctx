# Work Recorder Finish Program

This ExecPlan is the controlling plan for taking the new `ctx` Work Recorder product from the current partially integrated state to a shippable, defensible product. Keep this file current as the work proceeds. Update progress, decisions, evidence links, branch names, test results, and remaining gaps after each major slice.

## Purpose

`ctx` is no longer primarily the ADE. The public `ctxrs/ctx` repo should become the Work Recorder product:

> ctx records agent work so it can be attached to PRs, searched later, and shared with teammates.

The ADE work can remain preserved in `ctxrs/ade`. The new `ctxrs/ctx` must be a focused, standalone, useful local tool with a future hosted/team layer. The user wants a finished product, not a prototype, and prior "done" criteria were too weak. This plan is intentionally strict.

## Current State Snapshot

Primary task/session:

- Task: `feb64c1c-e58c-40f8-b1e9-1094dca0646e`
- Primary session: `28afdbd0-6180-4f83-8c97-f731117ece6d`
- Workspace: `b05b17d9-9712-4efc-bd8c-548b3b108bbf`
- Active managed parent worktree: `/home/daddy/.ctx/worktrees/b05b17d9-9712-4efc-bd8c-548b3b108bbf/fc00e4cd-b640-4ce3-a2b9-ac6fd06a98d8`

Important code paths and branches seen before this plan:

- Main integration branch: `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`, branch `work-record`.
- This branch currently has uncommitted changes in README, CLI, capture, report, store, docs, and check scripts. Start by understanding and either committing or fixing these changes. Do not lose them.
- Finished/partial branches exist under `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/`, including:
  - `wr-finished-cli-product`
  - `wr-finished-dashboard-v2`
  - `wr-finished-vcs-shims`
  - `wr-finished-pr-publish`
  - `wr-finished-ci-lanes`
  - `wr-finished-provider-import`
  - `wr-finished-security-code`
  - `wr-finished-visual-dogfood`
  - `agent-work-semantics-primary`
  - `agent-work-semantics`
- There are also scratch dogfood artifacts under `/home/daddy/code/ctx-multi-repo-workspace/scratch/`. Treat them as evidence inputs only, not as product code or completion proof.

## Active Orchestration State

Last updated: 2026-06-23T16:26:30Z by the primary session.

Canonical public integration target:

- Repo/worktree: `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch: `work-record`
- Current local head before the finish-program integration checkpoint: `7f71c09`
- Remote: `origin` = `https://github.com/ctxrs/ctx.git`
- Dirty state at reorientation: README, CLI, capture, report, store, docs, and check/release scripts had uncommitted hardening changes. These changes include provider fixture fail-closed behavior, share-safe CLI/report surfaces, archive/blob conflict hardening, release certificate validation, dogfood artifact sanitizer changes, and a shim scratch-space fix for `/tmp` quota pressure.
- Local validation completed before this plan update: `./scripts/check.sh fmt check clippy`, full Work Recorder library tests, full CLI integration tests after the shim scratch-space fix. Broader docs/product lanes still need rerun after final integration.

Canonical private hosted target:

- Repo/worktree: `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`
- Branch: `ctx/work-recorder-hosted-team`
- Current local head from inventory: `006e25706`
- Remote tracking: `origin/dev`, ahead 5 at inventory time.

Relevant existing public source branches/worktrees to inspect or integrate:

- Already-aligned or old source branches at the previous `origin/work-record` head: `ctx/work-record-storage-rich`, `ctx/work-record-provider-imports`, `ctx/work-record-shims-jj-hooks`, `ctx/work-record-agent-access`, `ctx/work-record-dashboard-report`, `ctx/work-record-pr-publish`, `ctx/work-record-security-docs`.
- Finished slice branches that may contain additional work beyond the current integration head: `ctx/wr-finished-storage-rich`, `ctx/wr-finished-provider-import`, `ctx/wr-finished-vcs-shims`, `ctx/wr-finished-dashboard-v2`, `ctx/wr-finished-search-rich`, `ctx/wr-finished-pr-publish`, `ctx/wr-finished-publish-live`, `ctx/wr-finished-release-install`, `ctx/wr-finished-ci-lanes`, `ctx/wr-finished-security-code`, `ctx/wr-finished-security-docs`, `ctx/wr-finished-visual-dogfood`, `ctx/wr-finished-cli-product`, `ctx/wr-finished-cli-integration`.
- Older port/foundation branches exist for provenance only unless inventory proves they carry missing commits: `ctx/work-record-store-foundation`, `ctx/work-record-capture-port`, `ctx/work-record-vcs-port`, `ctx/work-record-dashboard-port`, `ctx/work-record-search-port`, `ctx/work-record-ci-matrix`, `ctx/work-recorder-ci-release`.

Current management decision:

- First create a clean integration checkpoint for the understood hardening patch on `work-record`.
- Then spawn implementation/review subagents with disjoint scopes. The VCS/capture worker owns the shim failure and must verify/fix it independently; the primary should not continue solo in that loop.
- The dashboard worker must treat the React/Vite dashboard requirement as a product blocker until proven implemented with Playwright screenshots and manual visual review.
- The hosted worker must inspect `ctx-private` only and report whether staging is actually launch-shaped or merely scaffolded.
- The completion certifier must not run until implementation, tests, Buildkite/release evidence, hosted decision, and review artifacts are current.

Active workers started from checkpoint `b7c61ab`:

- VCS/Capture worker: `agent_DidtT1lTSG2akZC5DN0d_Q`, branch/worktree `ctx/wr-finish-capture-vcs` at `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-finish-capture-vcs`.
- Store/Search worker: `agent_Caf_VyUlTNybNcXfD7zjpw`, branch/worktree `ctx/wr-finish-store-search` at `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-finish-store-search`.
- Dashboard React worker: `agent_zHT6BLLAT46bi0SfJE72-Q`, branch/worktree `ctx/wr-finish-dashboard-react` at `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-finish-dashboard-react`.
- CI/Release worker: `agent_I27Kk4yTT3-zrznV85mVnA`, branch/worktree `ctx/wr-finish-ci-release` at `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-finish-ci-release`.
- Docs/Security worker: `agent_VFC6aSrIRAKBU1rpWy_G2w`, branch/worktree `ctx/wr-finish-docs-security` at `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-finish-docs-security`.
- Hosted/Staging worker: `agent_24FWhnb-QNiB3GAiGAtgiw`, private branch/worktree `ctx/work-recorder-hosted-team` at `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`.
- Integration Captain: `agent_mkTqn_PgQ_GVZWL00IYr_A`, read-only branch inventory and merge-order analysis.
- Completion Gap Audit: `agent_AsX0BlUZTfmd-pE84bmanA`, read-only strict criteria matrix before final certification.

## Non-Negotiable Product Decisions

1. Public `ctx` is the Work Recorder product. Do not re-center the product around the ADE.
2. Passive capture is the main UX. The user should not need `ctx work begin` or per-task ceremony.
3. CLI usage is mainly setup/configuration, search/query, dashboard/report opening, publishing PR evidence, and rare agent-mediated enrichment.
4. Local storage lives under the user-level ctx data directory, e.g. `~/.ctx`, not inside repos by default.
5. The local store is SQLite plus content-addressed/spooled raw artifacts/blobs where needed. Do not force large transcripts/tool output through Git.
6. Workspaces are organizational constructs that can include one or more repos. Single-repo should be zero-friction; multi-repo must be first-class.
7. Git and jj should both be first-class VCS integrations.
8. GitHub PR linking/publishing should be automatic when safely detected and agent-mediated through a ctx skill/CLI when automatic capture cannot be deterministic.
9. Dashboard technology must be a real React UI, not a server-side HTML string builder.
10. The internal dashboard must use open-source-safe components. Oatmeal/Tailwind Plus is fine for the public marketing/docs site, but not for reusable OSS dashboard primitives.
11. Dashboard stack direction:
    - React + Vite for local dashboard SPA.
    - Tailwind + shadcn/ui + Radix for UI primitives.
    - TanStack Query/Table/Virtual for data, tables, and large transcripts.
    - Tremor/Recharts or similar open-source charting only where useful.
    - Extract/readapt useful ADE transcript components into a read-only `WorkTranscriptView`; do not pull in the live ADE runtime.
12. Playwright is the default visual/e2e inspection mechanism. Do not rely on ad hoc Chrome hacks or "screenshot maybe failed" as acceptance.
13. Resource-safe local builds must be the default, using the existing cargo-safe/check-local work where appropriate. Avoid nuking the machine during Rust builds.
14. Hosted/team layer belongs in `ctx-private`, likely Cloudflare Workers + Neon + R2. Build staging-compatible shapes, but do not require production API cutover to consider the local product shippable.

## Architecture Target

### Local Product

The local product should work without the ADE:

- `ctx setup` installs/configures passive capture.
- Provider hooks/importers capture existing and future agent history.
- Git/jj/gh shims or hooks capture VCS/PR evidence without breaking the underlying tools.
- The store persists structured sessions, messages, tool calls, commands, artifacts, diffs, PR refs, commits, reviews, evidence, and summaries.
- `ctx search`, `ctx query`, `ctx dashboard`, `ctx report`, and `ctx publish` expose this data to humans and agents.
- Agents can consume the data via CLI/JSON and a ctx skill, not only through a UI.

The local product should not require a long-running daemon for capture. It may use a local server when the dashboard is open or for optional live refresh. If a daemon exists, justify it clearly and make fallback/no-daemon behavior robust.

### Dashboard

Build a real dashboard with durable product views:

- Overview: recent work, repos, providers, PRs, evidence status, search entry.
- Repo view: sessions/work records for one repo or workspace, linked commits/PRs.
- Session/run detail: transcript, tool calls, commands, outputs, artifacts, files touched, subagent/child session relationships when available.
- PR/evidence report: what changed, why, transcript links, tests, commands, screenshots/artifacts, stale evidence warnings, publish status.
- Search/explore: snippets with deep links into sessions/tool calls/artifacts.
- Settings/status: capture health, installed hooks/shims, provider imports, storage/redaction status.

Use extracted ADE transcript rendering only as a clean read-only component over normalized Work Recorder DTOs. Avoid importing ADE workbench state, live session supervisor, provider launch controls, scroll-warming runtime, or desktop shell logic.

### Capture/Integration

First-class integrations to prove:

- Codex CLI history/import and ongoing capture where available.
- Claude Code hooks/import and ongoing capture where available.
- Pi coding agent import/capture path, at minimum provider history/protocol capture and clearly documented limitations.
- Git commands via safe trace/shim/hook path.
- jj commands via first-class VCS path.
- GitHub PR creation/linking/publishing via gh shim and/or agent-mediated `ctx publish` skill.

Where providers do not expose subagent internals, model them honestly:

- Primary sessions and child/subagent sessions share one sessions table/model with role/relationship/source fidelity metadata.
- Capture "provider exposed child session" only when the provider exposes it.
- Otherwise capture manager transcript references, tool calls, command spans, and any imported child transcript files with lower fidelity labels.

### Hosted/Team Layer

`ctx-private` should define and implement a staging-ready hosted shape:

- Auth/team/org model.
- Device/user install identity.
- Redacted work-record upload API.
- PR report/evidence sync API.
- Artifact storage in R2 or equivalent.
- Neon schema/migrations.
- Team dashboard/API enough to validate the data model.
- Billing/enterprise placeholders documented, not necessarily a fully launched billing product unless already implemented.

Default sync must be conservative. Raw transcripts/tool output should be local-only unless explicitly configured.

## Workstream Parallelization

The primary agent must manage subagents and avoid doing all implementation itself. Use separate worktrees/branches for disjoint write sets, then merge/rebase into `work-record`.

Recommended workers:

1. **Integration Captain**
   - Inventory all branches/worktrees.
   - Decide which branch is canonical for each feature.
   - Merge/rebase finished slices into `work-record`.
   - Keep commits small enough to review, but do not split just for optics.
   - Push `work-record` after green integration checkpoints. Do not push to `ctx/main`.

2. **Dashboard Team**
   - Replace any Rust/server-side HTML string dashboard with React/Vite dashboard.
   - Build shadcn/Radix/Tailwind/TanStack/Tremor/Recharts stack if not already present.
   - Extract/adapt ADE transcript viewer into a read-only WorkTranscriptView.
   - Add Playwright visual tests and screenshot artifacts.
   - Manual screenshot review is mandatory.

3. **Capture + Provider Team**
   - Codex, Claude, Pi import/capture paths.
   - Passive setup, provider discovery, hook installation/removal, and health.
   - Provider fidelity labels and limitations.
   - E2E proof for each provider path.

4. **VCS/PR Team**
   - Git, jj, gh capture/linking.
   - PR detection/publish/comment behavior.
   - Stale evidence and commit/PR linking rules.
   - Ensure shims never break underlying tools, even if ctx capture fails.

5. **Store/Search/Agent Access Team**
   - SQLite schema, migrations, indexes, FTS/vector/graph adjacency if justified.
   - Data retention and large-output spooling.
   - CLI JSON APIs for agents.
   - ctx skill docs/examples for agents to query prior work.

6. **Hosted Team**
   - `ctx-private` Cloudflare Workers/Neon/R2 staging implementation.
   - Upload/sync contracts from local product.
   - Redaction/security defaults.

7. **Docs/Site Team**
   - Public README and docs for local product.
   - Public site banner/copy using Oatmeal/Tailwind Plus only where license-safe.
   - CLI reference, storage/privacy, provider support matrix, jj/git docs, troubleshooting, uninstall.

8. **CI/Release/SDLC Team**
   - Buildkite matrix across Linux, macOS, Windows, FreeBSD if promised.
   - Resource-safe test commands.
   - Release artifacts/install scripts, including Windows PowerShell installer.
   - Shift-left checks that are fast and deterministic.

9. **Security/Privacy Review Team**
   - Shims/hook safety.
   - Path traversal, command injection, symlink, secret capture, redaction, raw transcript sync defaults.
   - Hosted upload threat model.

10. **Completion Certifier**
    - A final independent subagent whose only job is adversarial completion review against this ExecPlan.
    - It must inspect code, docs, tests, screenshots, Buildkite results, release artifacts, and dogfood evidence.
    - It must list unmet criteria. The program is not done until this subagent says every criterion is met or explicitly waived by the user.

## Required Implementation Slices

### Slice A: Branch/State Reconciliation

- Inventory all relevant `ctx` and `ctx-private` branches/worktrees.
- Record the selected source branch for every implemented feature.
- Rebase `work-record` onto latest `origin/main` or the agreed base.
- Merge finished feature branches with conflict resolution and tests after each cluster.
- Commit current uncommitted changes in `work-record` once understood and clean.
- Remove or quarantine stale scratch/generated artifacts from product paths.

### Slice B: Local Store + Data Model

Must support:

- machines/devices
- workspaces
- repos
- sessions/runs
- messages
- tool calls
- command executions
- files touched
- artifacts/blobs
- commits
- PRs
- evidence/reviews
- provider/source/fidelity metadata
- redaction/export visibility
- large output spooling
- schema migrations and versioning

Completion proof:

- Unit tests for schema/migration/idempotency.
- Large transcript/tool output tests.
- Multi-repo workspace tests.
- Same repo on multiple machines/devices modeled without corrupting identity.

### Slice C: Passive Capture

Must support:

- `ctx setup` and `ctx uninstall`/teardown.
- Git capture.
- jj capture.
- gh capture/PR linking.
- Provider import/capture for Codex, Claude, Pi.
- Failure isolation: if ctx capture fails, underlying git/jj/gh command still succeeds.
- Health/status diagnostics.

Completion proof:

- E2E fixture tests for git, jj, gh.
- Provider import tests using real local provider history fixtures.
- At least one real provider dogfood per supported provider where credentials/environment allow.
- Redaction tests.
- Uninstall tests proving shims/hooks are removed.

### Slice D: Agent Access

Must support:

- `ctx search` and/or `ctx query` with JSON output.
- Agent-readable context retrieval for repo/session/PR.
- ctx skill/runbook for agents to ask "what did previous agents get stuck on?" and produce useful analysis.

Completion proof:

- Fresh agent dogfood: ask a new agent to query recorded work and answer specific questions.
- Tests for stable JSON schemas.
- Docs/examples in README and CLI reference.

### Slice E: Dashboard

Must support:

- Real React/Vite dashboard app, not HTML string builder.
- Overview, repo/workspace, session detail, PR/evidence, search, settings/status.
- Read-only transcript viewer with tool calls/commands/artifacts.
- Local dashboard can open from CLI and refresh when new content arrives if a local server is running.

Completion proof:

- Playwright tests for each page.
- Visual screenshots attached as artifacts.
- Manual screenshot review using actual images, not just command exit codes.
- Nonblank/correctly framed checks.
- Dogfood dashboard over real captured work, not toy-only data.

### Slice F: PR Evidence/Publish

Must support:

- Build PR report from linked work records.
- Publish report link/comment or PR body section with a clear default and opt-out.
- Detect stale evidence when commits/diffs change.
- Work with gh CLI and manual/agent-mediated `ctx publish`.

Completion proof:

- Local test repo creates a real or fixture PR path.
- Report contains transcript/evidence/commands/artifacts links.
- Tests for idempotency, stale updates, opt-out.

### Slice G: Hosted/Staging

Must support in `ctx-private`:

- Staging API for team sync.
- Neon migrations.
- R2 artifact upload/download path.
- Auth/team/org/device primitives.
- Redacted payload contract from local ctx.
- Minimal team web/API view sufficient to validate the model.

Completion proof:

- Integration tests against staging/local emulator as appropriate.
- Security review for raw transcript defaults.
- Docs for hosted boundaries.

### Slice H: CI/Release/SDLC

Must support:

- Buildkite pipelines and workers/pools for all promised architectures.
- Linux/macOS/Windows/FreeBSD CLI build/test coverage or explicit documented waiver.
- Release artifact generation.
- Installer scripts for macOS/Linux shell and Windows PowerShell.
- `check-local` command that uses resource-safe defaults.
- Test taxonomy: unit, integration, provider fixtures, e2e, visual, release.

Completion proof:

- Buildkite green on required matrix.
- Local `scripts/check.sh` or equivalent passes.
- Release artifacts produced and smoke-tested.
- Timings recorded and slow/flaky tests identified.

## Strict Done Criteria

Do not report final completion until all are true:

1. `ctxrs/ctx` branch `work-record` contains only the focused Work Recorder product, with ADE references removed or clearly historical/non-product.
2. `ctxrs/ade` remains the preserved ADE repo and is not part of the new `ctx` product claim.
3. Local install/setup works on a clean machine profile.
4. Passive capture works without per-task user ceremony.
5. `ctx uninstall` or equivalent removes hooks/shims and restores shell/tool behavior.
6. Codex, Claude, and Pi are represented with honest first-class integration status and e2e proof or documented limitations.
7. Git and jj are both tested first-class VCS paths.
8. gh/PR capture and publish are tested.
9. Store/search works for large existing agent histories.
10. Dashboard is React/Vite and visually acceptable under manual screenshot review.
11. Dashboard shows real transcripts/tool calls/commands/artifacts/evidence, not sparse placeholder snippets.
12. Agent-access CLI/JSON and skill examples are working.
13. Hosted staging layer in `ctx-private` has the agreed contracts and tests.
14. Security/privacy review is complete and issues are fixed or explicitly waived.
15. Buildkite is green on all required platforms/architectures.
16. Local checks are resource-safe and documented.
17. Release artifacts/install scripts are built and smoke-tested.
18. README/docs/site explain the product clearly from first principles.
19. A final completion-certifier subagent signs off against this plan.
20. All work is committed and pushed to the correct branches. Do not push to `ctx/main` unless explicitly instructed.

## Immediate Next Actions

1. Stop the current local shim rabbit hole long enough to reorient around this plan.
2. Update this ExecPlan with current branch inventory and active workers.
3. Spawn subagents for Branch/State Reconciliation, Dashboard Technology Audit, Capture/VCS gaps, CI/Release gaps, and Completion Criteria Audit.
4. The primary agent should orchestrate and review; it should not self-implement every remaining feature.
5. If the shim failure is a blocker, assign it to the VCS/PR Team and keep the rest of the program moving in parallel.
6. Report progress in the session every 15 minutes or after each major checkpoint, whichever comes first.

## Progress Log

- 2026-06-23: Plan created by supervisory session after user escalated that previous completion criteria were insufficient. Primary session was active and chasing a shim failure; this plan redirects the work to full product completion with strict certification.
