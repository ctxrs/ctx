# Work Recorder Provider Coverage and 0.1.0 Release Program

## Purpose

Take the new `ctx` Work Recorder product from a certified local launch
candidate to a real first release candidate with credible provider coverage,
real release infrastructure, final docs/site content, and a stronger SDLC.

This program is only for the Work Recorder product. It must not own the ADE
freeze, ADE DNS, ADE desktop release, or `ade.ctx.rs` migration. A separate ADE
freeze manager owns that work.

## Product Direction

`ctx` is the Work Recorder CLI and local/hosted record layer. Its promise is:

> Record what agents do so the work can be attached to PRs, searched later, and
> shared with teammates.

The core UX should remain passive:

- users install and run `ctx setup`;
- ctx captures local Git/jj/gh activity without per-task ceremony;
- ctx imports existing provider history where the provider exposes a stable
  local format;
- ctx passively captures new provider work where the provider exposes hooks,
  plugins, wrappers, logs, or protocols that can be integrated safely;
- agents can use `ctx search`, `ctx context`, `ctx report`, and `ctx publish`
  for explicit enrichment, but basic recording must not require agents to
  remember special commands.

## Binding Product Decisions Added 2026-06-23

These decisions supersede older wording and implementation assumptions in this
plan, docs, code, release scripts, and tests:

- Public product name remains `ctx`.
- Public concept language is `work records` and "ctx records agent work".
- `work graph` is allowed as an advanced/internal data model term, but not as
  the primary product name.
- Avoid branding public filesystem paths, docs, commands, README copy, release
  URLs, completion certificates, and site content around `work-record` as the
  product name.
- Default data root is `~/.ctx` itself. `--data-root` and `CTX_DATA_ROOT` mean
  the ctx root itself, not a parent directory that gets `work-record` appended.
- Canonical local layout is:
  - `work.sqlite`
  - `objects/`
  - `spool/`
  - `shims/`
  - `config.toml`
  - `logs/`
- Rename public/local layout terminology from `blobs` to `objects` and from
  `inbox` to `spool`.
- Ignore old ADE state if present. Do not warn merely because old ADE dirs
  exist and do not touch old ADE state.
- If new Work Recorder data exists at the old `~/.ctx/work-record/` layout,
  provide safe migration/compatibility or a one-time move path covered by
  tests.
- `spool/` is the durable safety queue for passive capture. Shims/hooks may
  write directly to SQLite when cheap and safe, but must fall back to small raw
  capture envelopes in `spool/` when SQLite is locked/unavailable/migrating or
  capture code errors.
- Underlying commands must keep working even if capture fails. This must be
  covered by tests for DB lock/failure fallback, malformed spool entries,
  retry/repair, and `git`/`gh`/`jj` command pass-through.
- Default `ctx setup` is low-friction and mostly non-interactive. It should
  create/update local layout, install/update Git/gh/jj capture shims, enable
  shims for future shells with a managed shell rc block when safe, import known
  provider history, and start/open the dashboard when interactive desktop/browser
  conditions allow it.
- Default `ctx setup` must not ask about optional persistent services. It asks
  only on real ambiguity/failure. Required flags: `--no-open`, `--no-import`,
  `--no-shell-update`, `--service`, `--yes`, `--dry-run`.
- No launchd/systemd/Windows service by default. Recording works without a
  daemon/service.
- `ctx dashboard` starts or reuses a small localhost dashboard server and opens
  it. The dashboard should live-update while open as new work is recorded. In
  headless/SSH/CI or with `--no-open`, print the URL and command instead of
  opening a browser.
- Optional always-running dashboard/background service is opt-in only:
  `ctx service install`, `ctx service status`, `ctx service uninstall`, and
  `ctx setup --service`.
- `ctx uninstall` removes shims, shell rc managed block, and optional service if
  installed, but keeps recorded data. `ctx uninstall --delete-data` deletes the
  local store, objects, spool, logs, and config after explicit confirmation or
  force.
- `ctx status` must include database path, shim status, dashboard URL/running
  status, and spool pending count.

Additional required checks for the final certifier:

- README/docs/site/CLI help use `ctx` plus work-records language and do not
  expose stale `~/.ctx/work-record`, `blobs`, or `inbox` as the canonical
  product layout.
- Schemas/config/default paths/tests use flat root, `objects`, and `spool`.
- Golden CLI output tests exist for setup, status, dashboard, and uninstall.
- Setup reruns are idempotent.
- Old ADE-present tests prove old ADE dirs are ignored and not modified.
- Dashboard open behavior is tested for interactive, headless, and `--no-open`
  cases.

## Product-Decision Workstreams Added 2026-06-23

Branch from the latest manager checkpoint and keep write scopes disjoint:

- `ctx/wr-root-layout-migration`: `work-record-core`, `work-record-store`,
  capture directory helpers, migration/compatibility tests, old-ADE ignored
  tests. Owns flat `~/.ctx`, `objects/`, `spool/`, and old
  `~/.ctx/work-record/` one-time compatibility.
- `ctx/wr-spool-shim-fallback`: capture/shim code and tests for SQLite locked
  or unavailable fallback to `spool/`, malformed spool repair/retry, and
  underlying `git`/`gh`/`jj` command pass-through.
- `ctx/wr-setup-dashboard-service-ux`: CLI setup/status/dashboard/uninstall/
  service command UX, managed shell rc block, dashboard localhost server/open
  behavior, golden output tests, and idempotency tests.
- `ctx/wr-docs-site-naming`: README/docs/site/release text and CLI help
  wording. Owns public language cleanup from stale `work-record` branding while
  preserving crate/package names where they are internal implementation details.
- `ctx/wr-release-ci-product-decisions`: Buildkite/release metadata/checks and
  completion certificate updates so CI explicitly checks these product
  decisions.
- Review-only agents after implementation merge: architecture/data, security
  privacy, CLI/product UX, docs truth, release/SDLC, dashboard visual, and final
  completion certification.

## Repositories and Branches

Canonical source repos:

- `/home/daddy/code/ctx-multi-repo-workspace/ctx` -> `ctxrs/ctx`
- `/home/daddy/code/ctx-multi-repo-workspace/ctx-private` -> `ctxrs/ctx-private`

Use manual worktrees under:

- `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/`
- `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/`

Current public Work Recorder branch:

- `ctxrs/ctx`: `work-record`

Current private hosted branch:

- `ctxrs/ctx-private`: `ctx/work-recorder-hosted-team`

Do not push to `ctx/main` without explicit user approval. It is acceptable and
expected to push `work-record` and the private hosted branch as work progresses.

## Source Baseline

The previous finish program reached:

- public `work-record` head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`;
- private hosted head `1b59e67f7` on `ctx/work-recorder-hosted-team`;
- Buildkite public release verification build #73 passed 26/26;
- completion certificate produced and inspected;
- final certifier returned PASS within the agreed local-first launch scope.

That scope was intentionally conservative. Known gaps become this program's
main work:

- native provider coverage beyond fixture import;
- real live E2E provider runs;
- OpenRouter/free-model smoke where agent harnesses support BYO model endpoints;
- real release publication infrastructure;
- first public Work Recorder docs/site content;
- stronger provider-specific SDLC lanes.

## Active Manager Snapshot

Last updated: 2026-06-23T20:30:12Z by the primary manager session.

Manager scope:

- Work Recorder `ctx` product only.
- ADE freeze, ADE DNS, ADE desktop release, and `ade.ctx.rs` migration remain
  out of scope for this manager.

Canonical public integration target:

- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch: `work-record`
- Local head: `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`
- Remote `origin/work-record`: `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`
- State at program start: clean.
- Previous Buildkite certification:
  `https://buildkite.com/luca-king/ctx-public-release-verification/builds/73`
  passed 26/26 for this head.

Canonical private hosted target:

- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`
- Branch: `ctx/work-recorder-hosted-team`
- Local head: `1b59e67f75b163b6a76d97766101f4f6ce7889e7`
- Remote `origin/ctx/work-recorder-hosted-team`:
  `1b59e67f75b163b6a76d97766101f4f6ce7889e7`
- State at program start: clean. The worktree tracks `origin/dev`, so
  `git status` reports `ahead 8`; the intended hosted branch remote matches.

Base source branch decisions:

- Public implementation workers should branch from `origin/work-record` unless
  a later integration checkpoint supersedes it.
- Private hosted/API workers should branch from
  `origin/ctx/work-recorder-hosted-team` or continue the existing canonical
  private worktree if their scope is narrow and coordinated.
- Do not push to `ctx/main`, publish `ctx.rs`, repoint `ctx.rs/install`, or
  cut over `api.ctx.rs` without explicit user approval.

Initial workstream assignments to create next:

- Provider architecture/metadata foundation.
- Codex and Claude Code provider coverage.
- Pi and OpenCode provider coverage.
- Antigravity CLI, Gemini CLI, and Cursor provider coverage.
- P1/P2 provider classification matrix.
- Release/R2/Buildkite/Hetzner 0.1.0 infrastructure.
- Docs/site local Work Recorder preview.
- Hosted/API staging contract in `ctx-private`.

Initial worktrees created:

- `ctx/wr-provider-architecture` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-provider-architecture`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-provider-codex-claude` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-provider-codex-claude`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-provider-pi-opencode` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-provider-pi-opencode`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-provider-antigravity-gemini-cursor` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-provider-antigravity-gemini-cursor`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-provider-matrix-longtail` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-provider-matrix-longtail`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-release-infra-010` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-release-infra-010`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-docs-site-preview` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-docs-site-preview`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-hosted-api-contract` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/wr-hosted-api-contract`
  from private head `1b59e67f75b163b6a76d97766101f4f6ce7889e7`.
- `ctx/wr-vcs-shims-pr-proof` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-vcs-shims-pr-proof`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.
- `ctx/wr-dashboard-cli-polish` at
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/wr-dashboard-cli-polish`
  from public head `71dfdb45543902b4f6bc01f5a961eabe5ef0e729`.

Worktree creation recorded: 2026-06-23T20:34:41Z.

## Out of Scope

- ADE freeze/DNS/release. Separate agent owns it.
- Publishing the new Work Recorder docs to `ctx.rs` without explicit user
  approval.
- Repointing `ctx.rs/install` or `api.ctx.rs` without explicit user approval.
- Production hosted/team launch unless explicitly approved.
- Destructive migration of existing ADE users or endpoints.

## Support Taxonomy

Every provider/harness must be classified with one of these statuses:

- `supported-live`: detection, passive capture or stable import, live E2E,
  dashboard rendering, PR evidence, docs, and CI/gated proof are all green.
- `supported-import`: stable existing-history import is proven, but passive
  live capture is not available or not implemented.
- `supported-wrapper`: ctx can run/capture via wrapper/shim, but native logs or
  hooks are unavailable.
- `fixture-only`: normalized fixture import works, but no real provider data is
  proven.
- `detected-unsupported`: ctx can detect local install/config, but no safe
  import/capture path exists.
- `blocked`: a concrete blocker exists, with evidence and next action.

Do not call a provider "supported" in public docs without at least
`supported-import` or `supported-wrapper`. Use `fixture-only` honestly.

## Provider Coverage Matrix

The provider matrix must cover Entire's public surface and ctx ADE's historical
surface.

Entire overlap:

- Claude Code
- Codex
- Copilot CLI
- Cursor
- Factory/Droid
- Gemini CLI
- OpenCode
- Pi

ctx ADE historical surface to account for:

- Codex
- Claude CLI / Claude CRP
- Pi
- Cursor
- OpenCode
- Gemini
- Antigravity CLI
- Copilot
- Droid / FactoryAI
- Goose
- OpenHands
- Amp
- cagent
- Qwen
- Mistral
- Kimi
- Auggie
- Cline / Roo-style agent surfaces
- Aider
- Continue / Cody
- Junie
- Kilo
- SWE-agent

Prioritization:

P0, must receive real implementation effort now:

- Codex
- Claude Code
- Pi
- OpenCode
- Antigravity CLI
- Gemini CLI
- Cursor

P1, should receive research plus at least a classified status now:

- Copilot CLI
- Factory/Droid
- Goose
- Amp
- OpenHands
- Qwen
- Mistral
- Kimi
- cagent

P2, classify honestly and implement only if cheap/obvious:

- Aider
- Cline/Roo
- Continue/Cody
- Auggie
- Junie
- Kilo
- SWE-agent

## Provider Row Requirements

For every provider row, produce:

- install detection method;
- auth detection method, without leaking secrets;
- history file/log locations, if any;
- hook/plugin/shim/protocol options;
- whether existing history can be imported;
- whether new runs can be captured passively;
- whether subagent/child-agent sessions can be represented;
- fidelity fields that are available: user prompts, assistant messages, tool
  calls, tool output, command output, files touched, artifacts, model/cost,
  token usage, parent/child session edges;
- redaction/privacy considerations;
- provider-specific E2E blockers;
- public docs wording;
- tests and fixture path.

Provider rows must be persisted as docs and machine-readable metadata, not only
as prose.

## Architecture Workstream

Goal: prevent one-off provider implementations.

Required outputs:

- common `ProviderCaptureAdapter` or equivalent trait/interface;
- stable provider support metadata schema;
- normalized provider event envelope;
- raw-source retention/spool contract;
- fidelity/source/trust fields;
- idempotency keys for provider sessions/events;
- import cursor model for incremental imports;
- hook/wrapper failure isolation rules;
- redaction boundary before dashboard/export/hosted sync;
- provider-specific artifact/blob handling;
- docs for how to add a new provider.

Acceptance:

- architecture review subagent approves that P0/P1 providers use the same
  adapter/capture contract;
- no provider writes directly to store tables without going through the common
  capture/import path;
- schema migration tests pass;
- import idempotency tests pass;
- malformed/partial transcript tests pass;
- large transcript/tool-output tests pass.

## Provider Implementation Workstreams

Parallelize aggressively. Each worker owns disjoint provider files/tests/docs.
Each provider worker must create/update one provider status row, fixtures,
tests, and docs.

Suggested workers:

1. Codex worker
   - Upgrade beyond prompt-only history where possible.
   - Research current `~/.codex` sessions/history structure.
   - Capture subagent/session lineage if available.
   - Live E2E with existing Codex on this machine.

2. Claude Code worker
   - Research hooks, settings, transcript/project directories.
   - Implement deterministic import or hook capture where possible.
   - If hooks are available, prove no failure can break Claude command flow.
   - Live E2E with Claude Code where credentials allow.

3. Pi/OpenCode worker
   - Pi is a key strategic target.
   - Research Pi extensions/RPC/history/session tree.
   - Research OpenCode plugin/log/history options.
   - Use OpenRouter/free-model smoke where these harnesses support BYO model.

4. Antigravity/Gemini worker
   - Treat Antigravity CLI as the forward path and Gemini CLI as still-live
     production/legacy.
   - Research current CLI install/history/hook behavior for both.
   - Implement at least detection/import classification for both.

5. Cursor/Copilot/Droid worker
   - Cover Entire overlap and ADE provider surface.
   - Determine whether history import is possible or only wrapper/shim capture.
   - Implement supported status where feasible.

6. OpenHands/Goose/Amp/cagent worker
   - Cover agent harnesses likely to have open-source logs/protocols.
   - Prefer deterministic local import over managed execution.

7. Model-vendor CLI worker
   - Qwen, Mistral, Kimi.
   - Determine whether these are actual agent harnesses, model-provider
     endpoints, or ADE-managed adapters.
   - Classify honestly; do not overclaim.

8. Long-tail worker
   - Aider, Cline/Roo, Continue/Cody, Auggie, Junie, Kilo, SWE-agent.
   - Research/classify.
   - Implement only if low-risk and clear.

Each worker must include tests, docs, and a support row. No worker may mark its
provider `supported-live` without a live artifact.

## Live E2E Program

Create a gated live E2E suite that is separate from normal unit tests.

Required scenarios:

- clean temporary repo;
- run provider with prompt: create a tiny deterministic change;
- ctx captures prompt/session/tool/command/diff evidence at the highest
  available fidelity for that provider;
- create local branch/commit;
- create or simulate GitHub PR path using `gh` where safe;
- attach/publish a Work Record report in dry-run by default;
- export dashboard;
- open/dashboard screenshot with Playwright;
- query the record with `ctx search` and `ctx context`;
- redaction scan of exported dashboard/report/archive.

Credentialed providers must be opt-in through environment variables and
Buildkite secret scopes. Non-credentialed fixture/provider rows still run in
normal CI.

OpenRouter/free-model smoke:

- use only harnesses that support BYO endpoint/model;
- do not pretend OpenRouter validates Codex/Claude first-party integrations;
- record exact model and provider in evidence;
- keep prompts deterministic and cheap.

## Work Recorder CLI UX Workstream

Review and harden:

- `ctx setup`
- `ctx status`
- `ctx validate`
- `ctx uninstall`
- `ctx capture import`
- `ctx capture import-local-providers`
- `ctx search`
- `ctx context`
- `ctx report`
- `ctx dashboard`
- `ctx pr parse`
- `ctx link-pr`
- `ctx publish pr-comment`

Acceptance:

- commands have concise help;
- JSON output is stable and tested;
- setup is reversible;
- broken shims/hooks never break underlying Git/jj/gh/provider command;
- first-run demo path is fast and useful;
- docs explain local storage under `~/.ctx`.

## Dashboard Workstream

The dashboard is now React/Vite. Continue from that foundation.

Required improvements:

- real provider sessions from live E2E appear meaningfully;
- provider/session/detail views show prompts, assistant messages, tool calls,
  commands, artifacts, PR links, freshness, and redaction state;
- empty/sparse states explain whether data is missing due to provider fidelity
  vs no work recorded;
- dashboard refresh story is documented;
- responsive screenshots are manually reviewed;
- dashboard design uses existing component primitives, not ad hoc ugly UI.

Acceptance:

- Playwright visual suite covers representative full-data and sparse-data
  cases;
- nonblank/correct framing checks;
- manual screenshot review artifacts attached;
- dashboard reviewer signs off.

## Hosted and API Workstream

Use `ctx-private` only. Do not publish hosted production.

Required:

- define `api.ctx.rs` future Work Recorder API shape;
- keep actual cutover disabled until user approves;
- staging API can receive redacted sync payloads;
- Neon migrations cover teams/devices/work records/artifacts/cursors;
- R2 artifact upload/download uses safe visibility metadata;
- auth/team/device primitives exist;
- raw transcript sync is opt-in, not default;
- docs clearly separate local product from future hosted sync.

Acceptance:

- typecheck/test/readiness local;
- migration tests pass;
- redaction/security review passes;
- API route contract docs exist;
- no production credentials leaked.

## Site and Docs Workstream

Goal: create the final Work Recorder docs/site content that will eventually
replace `ctx.rs`, but do not publish/cut over `ctx.rs` yet.

Rules:

- reuse the exact current ctx.rs site shell/static-site machinery;
- update docs/content/copy only;
- keep ADE docs/site work out of this plan;
- produce local/staging preview artifacts;
- README and site must align.

Required docs:

- what ctx records;
- install/getting started;
- setup/uninstall;
- passive capture model;
- storage model;
- provider support matrix;
- PR evidence/reporting;
- dashboard;
- agent access with `ctx search/context`;
- privacy/redaction;
- hosted sync roadmap;
- release/install security;
- troubleshooting.

Acceptance:

- docs reviewer signs off for truthfulness;
- no ADE product claims remain in Work Recorder docs;
- no live `ctx.rs/install` claim until release URL is real;
- local preview screenshot reviewed.

## Release Infrastructure Workstream

Goal: first Work Recorder versioned release, likely `0.1.0`, using real release
infrastructure and R2 release storage patterned after the existing release
work.

Required:

- decide exact version number in plan, default `0.1.0`;
- release manifest schema;
- release metadata env files;
- SHA-256 checksums;
- R2 bucket/path layout;
- macOS/Linux shell installer;
- Windows PowerShell installer;
- platform artifacts for Linux x64, macOS arm64, macOS x64, Windows x64;
- FreeBSD worker/pool if feasible, otherwise explicit blocker with Hetzner
  provisioning attempt/evidence;
- installer dry-run and live install smoke against staging release path;
- release certificate includes artifact URLs, checksums, platform smoke,
  provider matrix, dashboard screenshots, and security review.

Authorization:

- The manager may use Infisical for Buildkite, Hetzner, Cloudflare, R2, and
  other release credentials.
- The manager may use Hetzner APIs to provision Buildkite workers/pools,
  including FreeBSD or alternative runners, if needed.
- Do not spend unbounded resources. Document instance types, expected cost, and
  cleanup.

Acceptance:

- Buildkite release verification passes all required lanes;
- actual artifacts are uploaded to the intended R2 staging/release path;
- installers can install from those artifacts in smoke tests;
- no publishing/cutover of `ctx.rs/install` without user approval;
- release certificate and final certifier pass.

## SDLC Workstream

Required:

- resource-safe local check script;
- Buildkite matrix:
  - Linux x64
  - macOS arm64
  - macOS x64
  - Windows x64
  - FreeBSD if runner can be provisioned, otherwise documented blocker lane
  - provider fixture lanes
  - gated live provider lanes
  - dashboard visual lane
  - release dry-run lane
  - completion certificate lane
- test timing report;
- flaky test handling;
- artifact upload/download contract tests;
- security/static review;
- final done-check subagent.

Acceptance:

- local `scripts/check.sh all` or successor passes;
- Buildkite green for required matrix;
- live provider E2E either passes or documented blocker is accepted per provider
  row;
- final completion certificate is produced;
- certifier subagent returns PASS.

## Review Requirements

Use adversarial reviewers:

- architecture/data model reviewer;
- provider coverage reviewer;
- security/privacy/redaction reviewer;
- dashboard visual reviewer;
- release/SDLC reviewer;
- docs/truthfulness reviewer;
- hosted/API reviewer;
- final completion certifier.

Reviewers should be separate from implementation workers where practical.

## Branch and Commit Discipline

- Workers use separate manual worktrees when implementation scopes are disjoint.
- Commit each coherent slice.
- Push `work-record` regularly.
- Rebase/cherry-pick intentionally; do not leave giant unreviewed merge blobs.
- Keep exec plan updated with branch heads, worker assignments, Buildkite URLs,
  test commands, and accepted limitations.
- Do not rewrite user changes.

## Completion Criteria

Do not report final completion until all are true:

1. Provider support matrix covers Entire overlap and ctx ADE historical surface.
2. P0 providers are implemented or explicitly blocked with evidence:
   Codex, Claude Code, Pi, OpenCode, Antigravity CLI, Gemini CLI, Cursor.
3. P1 providers are at least researched/classified:
   Copilot, Factory/Droid, Goose, Amp, OpenHands, Qwen, Mistral, Kimi, cagent.
4. Every provider row has docs, tests/fixtures or blocker evidence, fidelity
   classification, and dashboard/report behavior.
5. Live E2E runs exist for all providers marked `supported-live`.
6. OpenRouter/free-model smoke exists for applicable BYO-model harnesses.
7. Passive capture remains no-ceremony for Git/jj/gh and all provider hooks or
   wrappers that claim passive capture.
8. Setup/uninstall are reversible and tested.
9. Store/import/search/report handle large and malformed provider data.
10. Dashboard renders useful real provider data and passes visual review.
11. PR evidence publish/dry-run works with current captured records.
12. Security/redaction review passes.
13. Hosted staging/API contracts are updated in `ctx-private`.
14. Work Recorder docs/site content is ready locally, reusing the existing site
    shell, with no publish/cutover.
15. Release version, manifest, checksums, R2 paths, shell installer, PowerShell
    installer, and platform artifacts are implemented.
16. Buildkite/Hetzner worker/pool needs are resolved or documented with
    evidence.
17. Buildkite is green for the required release matrix.
18. Release artifacts are uploaded to the approved R2 staging/release path and
    smoke-tested.
19. Final completion certificate is produced.
20. Final done-check subagent returns PASS.
21. All source changes are committed and pushed to the correct branches.
22. No `ctx/main`, `ctx.rs`, `ctx.rs/install`, or `api.ctx.rs` production
    cutover happens without explicit user approval.

## First Manager Actions

1. Update this plan with current branch heads and exact working directories.
2. Spawn provider research workers for P0/P1 groups.
3. Spawn architecture reviewer before provider workers go too deep.
4. Spawn release/SDLC worker for R2/Buildkite/Hetzner plan.
5. Spawn docs/site worker for local Work Recorder docs preview.
6. Keep ADE freeze out of scope and coordinate only if endpoint conflicts arise.
7. Push early, run checks early, and avoid waiting until the end for Buildkite.
