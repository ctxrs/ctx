# Work Recorder Productization Implementation Status

Updated: 2026-06-22T18:24:00-05:00

Task: `feb64c1c-e58c-40f8-b1e9-1094dca0646e`

Public repo: `ctxrs/ctx`

Public branch: `work-record`

Local public worktree:
`/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`

Starting public head: `e265a0e`

Plan provenance:

- Reviewed manager plan copied from the manager task worktree into this branch at
  `.ctx/exec-plans/work-recorder-productization/end_to_end_plan.md`.
- Required companion status files were created in the same directory so progress
  does not depend on the manager task worktree.

## Current State

Status: public foundation slices integrated and locally validated.

The implementation has not yet reached any milestone gate. The first action is
to map the current codebase against the reviewed plan, then split implementation
work into bounded slices with separate reviewers.

## Active Workstreams

- Public Work Recorder repo/product split: pending mapper output.
- Local data model, capture, search, and dashboard: pending mapper output.
- Hosted/private staging implementation: pending private worktree setup and
  private repo instruction review.
- Buildkite/release/platform verification: pending CI mapper output.
- Dogfood, screenshots, and final review: pending product implementation.

## Mapping Results

Read-only mapper subagents reviewed the public branch, private repo, dashboard
gap, local storage/capture/search gap, and CI/release gap.

Public branch current state:

- The branch is already a slim Work Recorder repo with four Rust crates:
  `ctx-cli`, `work-record-core`, `work-record-store`, and
  `work-record-report`.
- No tracked ADE-only product code was found in this public branch.
- The implemented CLI is currently nested under `ctx workspace ...` and
  `ctx work ...`; the plan's final public shape wants plain root commands.
- README has product-forward language that is ahead of implementation:
  dashboard, passive capture, shims/hooks, hosted sync, PR publish, installer
  URLs, and local-history import are not implemented yet.
- Current storage has only `work_records` and `evidence` tables, uses RFC3339
  text timestamps, stores stdout/stderr inline, and uses
  `work-record.sqlite`; the plan requires `work.sqlite`, normalized tables,
  integer millisecond timestamps, row-level visibility/fidelity/sync metadata,
  blob references, FTS, and capture provenance.
- No dashboard/web app, Playwright visual tests, `.buildkite`, release scripts,
  installer scripts, or platform matrix exist yet.

Private/hosted current state:

- Canonical `ctx-private` checkout is dirty with unrelated cold-emailing work,
  so implementation must use a separate manual worktree.
- Created private worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`
  on branch `ctx/work-recorder-hosted-team` at `58ed2192e`.
- Hosted mapper recommends a new Work Recorder Worker/service surface instead
  of overloading the existing control-plane worker.

First dependency order:

1. Land public core schema/types and versioned store migrations.
2. Align docs/CLI truth and storage path contract.
3. Add resource-safe CI/release scripts and initial Buildkite pipeline.
4. Add capture spool/import/VCS/search on top of stable schema.
5. Add dashboard/report UI and visual fixtures.
6. Add private hosted staging after the public/local JSON contract stabilizes.

## First Integrated Slice

Integrated implementation work:

- Core contract expansion in `crates/work-record-core/src/lib.rs`:
  - typed enums for visibility, fidelity, sync state, confidence, redaction,
    source/session/run/event/VCS/PR/artifact/evidence/tag/sync/audit/context
    values;
  - serde DTOs for capture envelopes, Work Record metadata, sessions, runs,
    events, VCS workspaces/changes, pull requests, artifacts, evidence metadata,
    summaries, files touched, tags, record links/edges, sync metadata/outbox,
    sync batches/cursors/aliases, audit log, and agent context packets;
  - existing `WorkRecord`, `Evidence`, constructors, and current tests preserved.
- Docs truth pass in `README.md` and `docs/*.md`:
  - removed or qualified unimplemented claims about dashboard, passive
    hooks/shims, provider-history import, hosted sync, PR comment publishing, and
    live installer URLs;
  - documented the currently implemented nested CLI surface honestly.
- CI/release local scripts:
  - added resource-capped `scripts/ci-common.sh`;
  - upgraded `scripts/check.sh` and `scripts/bazel-test.sh`;
  - added `scripts/release-dry-run.sh`;
  - added an initial `.buildkite/pipeline.yml` for sequential Linux-style
    lanes and local artifact collection.

## Second Integrated Slice

Integrated implementation work:

- Root command CLI:
  - added root commands for the currently implemented behavior:
    `setup`, `status`, `uninstall`, `schema`, `record`, `list`, `show`,
    `search`, `context`, `report`, `evidence run`, `link-pr`, `export`,
    `import`, and `validate`;
  - preserved `ctx workspace ...` and `ctx work ...` as hidden compatibility
    aliases with integration-test coverage.
- Storage contract:
  - changed default DB path to `~/.ctx/work-record/work.sqlite`;
  - added WAL mode, busy timeout, and foreign-key enforcement on store open;
  - added one-way `PRAGMA user_version` migration foundation;
  - added normalized foundation schema tables, required indexes, unique
    constraints, sync/audit tables, and FTS projection tables when FTS5 is
    supported;
  - preserved current record/evidence/search/context/import/export APIs by
    keeping compatibility columns synchronized with normalized metadata.
- Docs:
  - updated README and docs to present root commands as primary;
  - retained explicit hidden compatibility alias notes;
  - kept dashboard, passive capture, hosted sync, PR publishing, and public
    installer flow marked as not implemented yet.

Known remaining gap after this slice: capture spool/import, VCS/PR normalized
write APIs, search/context ranking over FTS projections, dashboard/report UI,
hosted staging, and Buildkite platform evidence remain unimplemented.

## Validation

- `./scripts/check.sh` in the public `work-record-product` worktree: PASS at
  starting implementation checkpoint. Covered `cargo fmt --check`,
  `cargo check --workspace --all-targets`, and `cargo test --workspace
  --all-targets`; 10 CLI integration tests, 1 report unit test, and 4 store unit
  tests passed.
- `bash -n scripts/check.sh scripts/bazel-test.sh scripts/release-dry-run.sh scripts/ci-common.sh`:
  PASS after CI script integration.
- `git diff --check`: PASS after docs/core/CI integration.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
  PASS after docs/core/CI integration. Covered fmt, check, clippy, tests; Bazel
  lane recorded `skipped` because neither `bazel` nor `bazelisk` is installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`:
  PASS. Wrote local release dry-run manifest, checksum, and timing artifacts
  under `target/ctx-artifacts/release-dry-run/`.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx --locked -- --test-threads 1`:
  PASS after root command CLI integration. 11 CLI integration tests passed,
  including hidden compatibility aliases.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
  PASS after root command and storage foundation integration. Covered fmt,
  check, clippy, tests; Bazel lane recorded `skipped` because neither `bazel`
  nor `bazelisk` is installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`:
  PASS after root command and storage foundation integration.

## Reviewer Status

No implementation reviewer has passed a milestone gate yet.

## Blockers

None proven yet.

## Accepted Deferrals

None accepted yet.
