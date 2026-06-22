# Work Inspector Full Suite Implementation Status

Updated: 2026-06-22T04:31:25-05:00

Branch: `ctx/agent-work-semantics-primary`

Base implementation commit: `87575cc Build Work Inspector capture suite`

Final implementation/hardening commit reviewed: `a6fac71 Harden Work Inspector
validation and docs`.

Status: validation passed locally. The first dedicated done-ness review on
`a6fac71` found the substantive implementation complete and failed only this
status note's stale bookkeeping. The first status-only follow-up commit was
`711c51a Record Work Inspector review status`; the reviewer then failed only the
remaining "re-check pending" wording. That bookkeeping was corrected, then the
user superseded the earlier PASS after opening the Work Inspector and finding
parts of it visually sparse. This status now includes the stricter
product-quality follow-up that started from clean head `a65fcd5`.

## Scope Landed

- Replaced the thin Work Report surface with a typed Work Inspector at the
  stable route `/workspaces/:id/work/:workId`.
- Added typed report v2 API projections for overview, transcript/context,
  commands, evidence, timeline, changes, artifacts, trust, and whitelist raw
  JSON.
- Added the Work Inspector UI with dashboard-style header, metrics, tabs,
  right-rail context, safe raw JSON projection, failure state, dark/light
  support, and mobile layout coverage.
- Added ADE/session projection and explicit CLI capture coverage for Work
  records, evidence command previews, deterministic git/PR metadata capture,
  import/export/search/context, and freshness/trust material.
- Added the session-artifact to Work-artifact bridge with safe typed metadata,
  authenticated artifact URLs, MIME handling, and executable-content download
  behavior for SVG/HTML.
- Added dogfood generation for five scratch projects and opened their Work
  Inspector pages in Chrome.
- Added docs and README language that positions Work records and the Work
  Inspector as the primary product surface, with ADE as optional.
- Hardened local development verification by using `scripts/dev/cargo-safe.sh`
  with a global Cargo lock, low I/O priority, memory cgroup, one Cargo job, one
  Rust test thread, and short disk-backed `TMPDIR=/var/tmp/ctxwi`.

## API Contract Summary

The Work Inspector route exposes typed, share-safe fields rather than compacted
text or arbitrary raw records:

- `overview`: objective/title/status/provenance/trust/freshness.
- `transcript` and `context`: bounded, redacted message/context previews.
- `commands`: command metadata, exit status, timestamps, and bounded redacted
  stdout/stderr previews.
- `evidence`: verification state, freshness, citations, and command links.
- `timeline`: ordered events with source IDs and redacted display payloads.
- `changes`: linked commits, pull requests, and change-set metadata.
- `artifacts`: safe artifact metadata, thumbnails/download links where safe,
  and unavailable placeholders when source artifacts are missing.
- `raw_redacted_json`: an explicit whitelist projection only. It is not a
  recursive dump of local/private raw data.

Default route responses, DOM, screenshots, search, and agent-readable JSON omit
raw transcripts, raw command output, raw local paths, auth material, and
private/local-only blobs.

## Changed Files In This Hardening Pass

- `README.md`
- `docs/work-records.mdx`
- `docs/examples/work-observability-e2e.md`
- `docs/settings/data-and-privacy.mdx`
- `core/apps/web/e2e/visual-work-inspector.spec.ts`
- `core/crates/ctx-daemon/src/daemon/workspaces/route_contract/work.rs`
- `core/crates/ctx-daemon/src/test_support/session_heads.rs`
- `core/crates/ctx-http/src/agent_work_cli.rs`
- `core/crates/ctx-http/tests/turn_lifecycle_events.rs`
- `core/crates/ctx-http/tests/workspace_active_snapshot_http.rs`
- `core/crates/ctx-providers/src/fake.rs`
- `core/crates/ctx-repo-onboarding-service/src/workspace_registration.rs`
- `core/crates/ctx-worktree-vcs-service/src/local_source.rs`
- `core/crates/ctx-worktree-vcs-service/src/worktree_creation.rs`

## Broad Verification Findings And Fixes

The final broad Rust verification was not treated as a rubber stamp. It exposed
several unrelated but release-relevant local failures, all fixed before the
final pass:

- `/tmp` tmpfs pressure caused SQLite `disk I/O error` during broad workspace
  tests. Mitigation: reran broad Cargo gates with short disk-backed
  `TMPDIR=/var/tmp/ctxwi`.
- A long scratch `TMPDIR` caused provider-account Unix socket path failures
  with `SUN_LEN`. Mitigation: kept the short `/var/tmp/ctxwi` temp root.
- `ctx_ui_sized_active_session_head_recovery_is_bounded` counted live
  projection rows as fixture rows. Fix: count only the intended UI-tool fixture
  IDs.
- `project_session_command_backfills_ade_session_work` relied on fixture state
  visibility that was not guaranteed across store managers. Fix: assert the
  persisted event before projecting.
- No-repo session diff and task-default-session tests inherited parent Git
  repositories from scratch directories. Fix: root-scoped repository checks now
  require a `.git` or `.jj` marker at the workspace root.
- Queued cancel lifecycle tests depended on timing. Fix: fake provider now has
  a deterministic `hold-after-tool-call` marker.
- Repeated workspace VCS stream subscription saw pre-repeat queued VCS messages.
  Fix: drain queued VCS messages before the repeat-subscribe assertion.
- Pull request inspector links with `target_id` but no `target_json` lacked
  owner/repo/number projection. Fix: bounded fallback parser for
  `provider:owner/repo#number`.

## Dogfood Records

Scratch root:

`/home/daddy/code/ctx-multi-repo-workspace/scratch/work-inspector-full-suite-20260621-145857`

The five Work Inspector pages were generated and opened in Chrome against the
local dogfood daemon on `127.0.0.1:4401`. URLs below intentionally omit any auth
token.

| Task | Workspace | Work ID | Inspector URL |
| --- | --- | --- | --- |
| `01-canvas-game` | `7aad22a6-a158-4c7e-8739-5589421f4054` | `wrk_44274df57b7343a3a2550988517cfb82` | `http://127.0.0.1:4401/workspaces/7aad22a6-a158-4c7e-8739-5589421f4054/work/wrk_44274df57b7343a3a2550988517cfb82` |
| `02-productivity-app` | `0a792892-9285-4e3c-90fa-a46f97931a4a` | `wrk_5950d10d41f841f0b657c13dc5f66f74` | `http://127.0.0.1:4401/workspaces/0a792892-9285-4e3c-90fa-a46f97931a4a/work/wrk_5950d10d41f841f0b657c13dc5f66f74` |
| `03-cli-utility` | `2b36dd2d-dbad-4d7a-9f2d-fba85b4c961b` | `wrk_545ae892d34b4ac09c7a21d0a57647a7` | `http://127.0.0.1:4401/workspaces/2b36dd2d-dbad-4d7a-9f2d-fba85b4c961b/work/wrk_545ae892d34b4ac09c7a21d0a57647a7` |
| `04-docs-site` | `4d41039f-9a98-49b2-8ef9-54334e1343a3` | `wrk_8d0eb609b6594d73af755b113abe3cfb` | `http://127.0.0.1:4401/workspaces/4d41039f-9a98-49b2-8ef9-54334e1343a3/work/wrk_8d0eb609b6594d73af755b113abe3cfb` |
| `05-api-visualization` | `d55e3288-f269-44d9-86a7-f92ff53403d7` | `wrk_bf806b2cf57649d3a199429801675cbc` | `http://127.0.0.1:4401/workspaces/d55e3288-f269-44d9-86a7-f92ff53403d7/work/wrk_bf806b2cf57649d3a199429801675cbc` |

Agent-readable JSON exports:

`/home/daddy/code/ctx-multi-repo-workspace/scratch/work-inspector-full-suite-20260621-145857/reports/inspector-json`

Screenshots:

`/home/daddy/code/ctx-multi-repo-workspace/scratch/work-inspector-full-suite-20260621-145857/screenshots/final-work-inspector`

Final screenshot set:

- `01-canvas-game-desktop-dark-overview.png`
- `01-canvas-game-desktop-dark-commands.png`
- `01-canvas-game-desktop-dark-artifacts.png`
- `01-canvas-game-desktop-dark-raw-json.png`
- `01-canvas-game-desktop-light-overview.png`
- `01-canvas-game-mobile-dark-overview.png`
- `02-productivity-app-desktop-dark-overview.png`
- `03-cli-utility-desktop-dark-overview.png`
- `04-docs-site-desktop-dark-overview.png`
- `05-api-visualization-desktop-dark-overview.png`
- `missing-work-desktop-dark-error.png`

## Reviewer Status

- Architecture/data-model review: PASS.
- Security/privacy review: PASS. Reviewer confirmed default public surfaces use
  typed share-safe projections and avoid raw/local/private leakage.
- Visual review: PASS across populated desktop dark, desktop light, mobile dark,
  commands, artifacts, raw JSON, and missing-record failure state.
- Fresh dogfood reconstruction review: PASS, 5/5 records reconstructable from
  the Work Inspector plus redacted agent-readable JSON alone.
- Dedicated final done-ness review on `a6fac71`: substantive PASS; temporary
  bookkeeping FAIL because this status file still said final review and hygiene
  were pending. No product, architecture, security, visual, test, dogfood, or
  deferral blockers were found.
- Final re-check on `711c51a`: implementation still PASS; temporary bookkeeping
  FAIL because this file still said the re-check was pending and did not mention
  `711c51a`.
- Current status-only `HEAD`: records the final re-check results above. No code,
  product, architecture, security, visual, test, dogfood, or deferral blockers
  remain in the recorded reviewer feedback.

## Product-Quality Follow-up From `a65fcd5`

The user opened the latest Work Inspector and did not accept the previous PASS
because parts of the example still looked incomplete. This follow-up treated
visual usefulness as a release gate rather than a documentation note.

What was sparse before:

- Commands had structural rows, but not enough useful stdout/stderr preview
  material to show what actually ran.
- The Changes tab had file/change metadata, but was not enough for a fresh
  reviewer to reconstruct the work without adjacent scratch-repo spelunking.
- The Artifacts tab could degrade into metadata-only evidence and did not prove
  that a real preview rendered in the browser.
- Child/subagent context was present in data, but needed a visible grouped
  contribution surface.

What changed in this pass:

- Enriched the rich dogfood Work record with a complete scratch canvas toy run:
  transcript/timeline, three commands, two share-safe command output previews,
  seven change items, five safe source snapshots, three evidence items, one
  screenshot artifact, one child/subagent review, and explicit review notes.
- Changed Inspector artifact previews to fetch authenticated artifact blobs in
  the browser and render `blob:` URLs, with Preview/Download buttons instead of
  capability-token links in the DOM.
- Added fail-closed source snapshot projection. Backend and UI both require
  `share_safe: true` and `redaction_class: local_redacted`; the default Changes
  view shows bounded excerpts only, while the redacted agent-readable JSON keeps
  the complete safe handoff material.
- Removed raw command CWD from route responses and UI validation proof, keeping
  only safe CWD labels such as `project root` or `captured workspace`.
- Grouped child/subagent events by linked child session/run so the Subagents tab
  shows a specific child reviewer contribution instead of generic context.

Rich dogfood Inspector:

- Workspace:
  `a4f13335-4f77-4f3f-9f83-96194fda8937`
- Work ID:
  `wrk_ade_task_4211e95ca67143e6a29ac18dea33a5a6`
- Local Inspector URL:
  `http://127.0.0.1:4412/workspaces/a4f13335-4f77-4f3f-9f83-96194fda8937/work/wrk_ade_task_4211e95ca67143e6a29ac18dea33a5a6`
- Redacted Inspector JSON:
  `scratch/work-inspector-product-quality-rich/reports/rich-inspector.json`
- Enriched import fixture:
  `scratch/work-inspector-product-quality-rich/reports/full-local-enriched-import.json`

Latest product-quality screenshots:

- `scratch/work-inspector-product-quality-rich/screenshots/rich-overview-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-transcript-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-subagents-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-commands-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-evidence-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-timeline-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-changes-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-artifacts-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-context-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-agent-handoff-desktop-dark.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-overview-mobile-light.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-commands-mobile-light.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-changes-mobile-light.png`
- `scratch/work-inspector-product-quality-rich/screenshots/rich-artifacts-mobile-light.png`

Manual screenshot inspection notes:

- Overview now has clear completeness counters for transcript, subagents,
  commands, changes, evidence, and artifacts; it no longer reads as an empty
  shell.
- Commands shows concrete command names, passing statuses, safe workspace
  labels, durations, and visible stdout previews.
- Changes shows changed files, linked commit/PR material, source outline,
  review notes, artifact fingerprint evidence, and bounded implementation
  excerpts with omitted-line notes.
- Artifacts renders the actual canvas screenshot preview and keeps action
  controls separate from the image URL.
- Subagents shows one grouped child reviewer contribution with event count and a
  safe contribution preview.
- Mobile light overview stacks cleanly and preserves the same completeness
  signal without overlap.

Additional reviewer results for the stricter pass:

- Adversarial visual/product review: PASS. Reviewer inspected actual screenshots
  and accepted the richer command, Changes, Artifacts, Context, and Subagents
  surfaces. Remaining comments were polish-level only: repeated card/chip
  patterns and long mobile Changes content.
- Fresh reconstruction review: PASS. Reviewer reconstructed the scratch canvas
  toy from the Inspector and redacted JSON alone. Accepted caveats: raw
  transcripts remain local, raw CWD stays redacted, and the dummy PR is local
  dogfood evidence rather than a pushed product PR.
- Combined security/code re-review: PASS. Reviewer confirmed artifact tokens are
  not present in the current Inspector DOM or screenshot strings; source
  snapshots fail closed on explicit safe markers; command CWD is omitted; and
  child/subagent grouping is acceptable.

Product-quality follow-up validation:

- `pnpm -C core/apps/web test -- WorkReportView.test.tsx browserResourceUrls.test.ts`:
  PASS, 15 tests.
- `pnpm -C core/apps/web typecheck`: PASS.
- `pnpm -C core/apps/web lint`: PASS.
- `pnpm -C core/apps/web build`: PASS with existing Vite chunk/dynamic-import
  warnings only.
- `CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 TMPDIR=/var/tmp/ctxwi scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml -p ctx-daemon inspector_source_outline_and_review_notes_are_share_safe --lib --locked`:
  PASS.
- `CTX_CARGO_JOBS=1 TMPDIR=/var/tmp/ctxwi scripts/dev/cargo-safe.sh build --manifest-path Cargo.toml -p ctx-http --bin ctx --locked`:
  PASS.
- JSON privacy grep against the refreshed rich Inspector JSON found no raw local
  paths, auth/token markers, raw payload keys, or executable URL markers.
- Browser DOM probe confirmed artifact previews use `blob:` URLs and no
  `token=`, `expires_at=`, or `Bearer` material appears in the artifact panel.

## Validation

Passed:

- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --workspace --locked`
  with `CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 TMPDIR=/var/tmp/ctxwi`.
- `scripts/dev/cargo-safe.sh build --manifest-path Cargo.toml --workspace --locked`
  with `CTX_CARGO_JOBS=1 TMPDIR=/var/tmp/ctxwi`.
- `pnpm --dir core/apps/web typecheck`.
- `pnpm --dir core/apps/web lint`.
- `pnpm --dir core/apps/web test`:
  253 files passed, 1956 tests passed.
- `pnpm --dir core/apps/web build`.
- `CTX_E2E_FORCE_REUSE_SERVER=1 CTX_E2E_PORT=4401 CTX_E2E_BROWSER=chromium CTX_E2E_BROWSER_CHANNEL=chrome CTX_E2E_SKIP_WEB_BUILD=1 CTX_E2E_DISABLE_VIDEO=1 pnpm --dir core/apps/web exec playwright test e2e/visual-work-inspector.spec.ts`:
  5 tests passed. The local auth token was supplied from the dogfood data root
  and was not recorded.

Final hygiene already passed before commit `a6fac71`:

- `cargo fmt --manifest-path core/Cargo.toml --all -- --check`.
- `git diff --check`.
- `git status --short`.

## Accepted Deferrals

- Hosted/team/enterprise sync remains out of scope for this public local pass.
- Optional LLM summaries remain off by default and are not evidence.
- Raw local/private transcript/data-lake material remains excluded from default
  route responses, DOM, screenshots, search, status files, and agent-readable
  JSON.
- Global arbitrary command shimming remains deferred. Deterministic capture is
  via ADE/session projection, explicit `ctx work evidence ... run -- ...`, and
  git/gh metadata/link capture.
- Dogfood records that did not have linked session artifact IDs show safe
  unavailable artifact placeholders. This is correct for the current records and
  does not leak raw paths.
