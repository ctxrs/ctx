# Work Observability Release Hardening Status

Date: 2026-06-21

## Local Scope

- Base feature-completion commit: `eaf7c6e` (`Record Work observability done-ness pass`).
- Initial release-hardening checkpoint: `bb6895f` (`Harden Work observability release candidate`).
- Hardening review-fix commit: `64a4012` (`Address Work observability hardening review`).
- Final reviewer-blocker fix commit: `1e075ef` (`Fix Work hardening review blockers`).
- Status/artifact commit: this file is committed after `1e075ef`; use branch `HEAD` for the final local status commit.

This pass is local release-candidate hardening only. It does not push ctx,
open a ctx PR, release, or enable hosted/team/enterprise sync.

## What Landed

- Work Report UI hardening:
  - safe external-link handling for PR URLs;
  - fallback display for pull-request links that only have `target_id`;
  - light-theme contrast fixes for Work Report metadata and raw-transcript notes;
  - focused regression coverage for unsafe PR URL rendering.
- CLI and daemon redaction/trust hardening:
  - `ctx work timeline --json` now emits redacted timeline events without raw `payload_json` or `artifact_ref`;
  - CLI evidence trust refresh now recomputes from the full evidence set, so an older failure is not hidden by a later pass;
  - daemon evidence creation no longer accepts client-submitted `verified` evidence trust as verified provenance;
  - GitHub PR URL parsing rejects non-HTTP(S) schemes.
- Store/search data-model hardening:
  - Work search document IDs include workspace identity;
  - no-text search filtering uses concrete predicates instead of optional-filter SQL;
  - FTS delete is workspace-scoped;
  - migration `0076` is preserved and migration `0077` drops the old strong-link uniqueness index for upgraded databases;
  - cross-workspace primary-key collisions now return an error instead of becoming silent no-ops;
  - duplicate strong PR/commit links across multiple Work records are allowed and surfaced as duplicates rather than blocked.
- Public docs/examples:
  - `docs/work-records.mdx` now distinguishes compatibility `ChangeSet`/`Contribution` commands from first-class `wrk_` Work Record observability commands;
  - e2e walkthrough command ordering and Work ID selection are corrected;
  - example Work Report no longer implies raw transcript notes are emitted in markdown output.

## Validation

Focused validation was run with resource-safe Cargo settings where applicable:

- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml -p ctx-http --bin ctx agent_work_cli::tests --locked`
  - PASS, 38 tests. Rerun after `1e075ef`.
- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml -p ctx-store work_observability --locked`
  - PASS for `work_observability_rejects_cross_workspace_primary_key_collision`.
- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml -p ctx-store work_record_links_allow_duplicate_pull_request_targets --locked`
  - PASS. Rerun after `1e075ef` to cover migration `0077`.
- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml -p ctx-store work_search_docs_reject_cross_workspace_doc_id_collision --locked`
  - PASS.
- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml -p ctx-daemon --lib workspaces::route_contract::work::tests --locked`
  - PASS, 7 tests. Existing unrelated unused-import warnings remain.
- `pnpm -C core/apps/web exec vitest run src/pages/workReport/WorkReportView.test.tsx src/api/clientWorkspaces.work.test.ts`
  - PASS, 4 tests. Rerun after `1e075ef`.
- `cargo fmt --manifest-path core/Cargo.toml --all -- --check`
  - PASS. Rerun after `1e075ef`.
- `pnpm -C core/apps/web typecheck`
  - PASS. Rerun after `1e075ef`.
- `pnpm -C core/apps/web lint`
  - PASS. Rerun after `1e075ef`.
- `pnpm -C core/apps/web build`
  - PASS. Rerun after `1e075ef`; existing Vite/Browserslist/chunk warnings remain.
- `git diff --check`
  - PASS.
- `git diff --check eaf7c6e`
  - PASS.

## Final Broad Verification Addendum

Recorded: `2026-06-21T01:48:03-05:00`

Verified implementation head before this status-only addendum: `89a6179`
(`Record Work observability release-hardening status`).

Resource controls used for broad Rust verification:

- Cargo was serialized through the ctx safe wrapper and host lock at
  `/tmp/ctx-cargo.lock`;
- the wrapper ran with `MemoryMax=34G`, `MemorySwapMax=4G`, `jobs=2`, and
  `test_threads=2`;
- no parallel Cargo commands were launched during the broad Rust gates.

Final broad command results:

- `core/scripts/dev/cargo-safe.sh test --manifest-path core/Cargo.toml --workspace --locked`
  - FAIL_FAST invocation error only: `manifest path core/Cargo.toml does not exist`.
  - No Rust build or tests ran for this malformed root invocation. The safe
    wrapper is rooted under `core/`, so the equivalent broad workspace command
    below was run from `core/`.
- `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --workspace --locked`
  - PASS from `core/`. Broad Rust workspace unit, integration, and doc tests
    completed successfully under the safe wrapper.
- `scripts/dev/cargo-safe.sh build --manifest-path Cargo.toml --workspace --locked`
  - PASS from `core/`. Broad Rust workspace dev build completed successfully
    under the safe wrapper in `2m 05s`.
- `pnpm -C core/apps/web test`
  - PASS, `253` test files and `1952` tests.
- `pnpm -C core/apps/web typecheck`
  - PASS.
- `pnpm -C core/apps/web lint`
  - PASS with `--max-warnings=0`.
- `pnpm -C core/apps/web build`
  - PASS. Existing Vite/Browserslist/chunk-size warnings remain, but the
    production build completed successfully.
- `cargo fmt --manifest-path core/Cargo.toml --all -- --check`
  - PASS.
- `git diff --check`
  - PASS before the broad-verification status edit. Rerun after the final
    broad-verification addendum commit `7a5454e` and passed with a clean
    worktree.

Remaining accepted deferrals after this addendum:

- Buildkite was not run because this task explicitly forbids pushing ctx,
  opening a ctx PR, or releasing.
- Broad Bazel sweeps were not run in this final addendum; the requested local
  broad verification lane was Cargo workspace test/build, full web gates,
  formatting, and diff hygiene.
- Hosted/team/enterprise control-plane sync remains out of scope.
- Provider-backed LLM summary generation remains rejected in the public local
  Work route slice.
- The private scratch PR/repo remain available for inspection until explicitly
  cleaned up.

## E2E Sample

The local e2e sample created a disposable ping-pong game, captured Work records,
recorded evidence, rendered a Work Report, created a private scratch draft PR,
and posted the redacted Work Report as a PR comment.

- Scratch repo local path: `/tmp/ctx-work-observability-e2e-20260620`
- Isolated ctx data root: `/tmp/ctx-work-observability-e2e-data-20260620`
- Private scratch GitHub repo: `luca-ctx/ctx-work-observability-e2e-scratch`
- Draft PR: `https://github.com/luca-ctx/ctx-work-observability-e2e-scratch/pull/1`
- PR comment: `https://github.com/luca-ctx/ctx-work-observability-e2e-scratch/pull/1#issuecomment-4760981674`
- PR-linked Work ID: `wrk_ea836e3327c44209a4099b150b9bec6d`
- Screenshot artifact: `artifacts/ping-pong-game.png`
- Redacted Work Report artifact: `artifacts/e2e-work-report.md`

Manual screenshot review passed: the artifact shows the sample ping-pong game
with scoreboard, paddles, ball, court, and controls visible.

Leak checks passed for the generated report/context output: no scratch local
repo path, no `payload_json`, and no `sk-` token appeared in the checked output.

Cleanup if desired:

```bash
gh pr close 1 --repo luca-ctx/ctx-work-observability-e2e-scratch --delete-branch
gh repo delete luca-ctx/ctx-work-observability-e2e-scratch --yes
rm -rf /tmp/ctx-work-observability-e2e-20260620 \
  /tmp/ctx-work-observability-e2e-data-20260620 \
  /tmp/ctx-work-observability-e2e-outputs
```

## Review Status

Initial post-checkpoint review returned FAIL findings from:

- security/privacy reviewer `019ee898-7c28-7951-ba24-ac2460dc74e0`;
- product/UI reviewer `019ee898-8017-7431-8d94-0b4e7f81206c`;
- docs/spec/examples reviewer `019ee898-8378-75b0-beb5-5d5f2bc4697b`;
- search/data reviewer `019ee898-86c6-7720-851c-c4bd99d049be`;
- test/SDLC reviewer `019ee898-8a8c-7bd0-8428-b3f189c09d4e`.

All actionable findings from those reviews are addressed in `64a4012` and
`1e075ef`.

Final specialist re-review verdicts:

- security/privacy reviewer `019ee898-7c28-7951-ba24-ac2460dc74e0`: PASS on `1e075ef`.
- product/UI reviewer `019ee898-8017-7431-8d94-0b4e7f81206c`: PASS on `1e075ef`.
- docs/spec/examples reviewer `019ee898-8378-75b0-beb5-5d5f2bc4697b`: PASS on `1e075ef`.
- search/data reviewer `019ee898-86c6-7720-851c-c4bd99d049be`: PASS on `1e075ef`.
- test/SDLC reviewer `019ee898-8a8c-7bd0-8428-b3f189c09d4e`: code/test coverage PASS; status/artifact bookkeeping is satisfied by this status/artifact note.

Earlier dedicated final done-ness reviewer
`019ee8b7-44ed-7182-9025-12d16ddde619` returned PASS_READY on
`ctx/agent-work-semantics-primary@4a830c7` before the final broad-verification
addendum.

Final broad-addendum done-ness reviewer
`019ee8f0-a3bf-7a31-a68c-1b0d910f9002` returned PASS on
`ctx/agent-work-semantics-primary@7a5454e`: no blocker-level issues, the
worktree was clean, the final broad verification addendum was present, commands
and deferrals were coherent, and local no-push done criteria were satisfied.

## Accepted Deferrals

- Buildkite was not run because this task explicitly forbids pushing ctx,
  opening a ctx PR, or releasing.
- Full Rust workspace tests and builds were rerun and passed in the final broad
  verification addendum above. Broad Bazel sweeps remain deferred because they
  were not part of the requested no-push local verification lane.
- Hosted/team/enterprise control-plane sync remains out of scope.
- Provider-backed LLM summary generation remains rejected in the public local
  Work route slice.
- The private scratch PR/repo remain available for inspection until explicitly
  cleaned up.
