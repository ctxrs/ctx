# Work Recorder Productization Implementation Status

Updated: 2026-06-22T19:26:21-05:00

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

## Foundation Review Fixes

Integrated implementation work after the first architecture/data model review:

- Generated Work Record and evidence IDs now use UUIDv7 through a shared
  `new_id()` helper, while existing serialized UUID fields remain compatible.
- Centralized local path helpers now cover `work.sqlite`, `blobs/`, `inbox/`,
  and `device.json` under the public Work Recorder data root.
- Public CLI JSON responses now carry `schema_version: 1` envelopes, including
  `record`, `records`, `summary`, `evidence`, and `AgentContextPacket` output
  from `ctx context --json`.
- Evidence capture now requires an attached Work Record for current writes.
  `ctx evidence run` without `--record` creates a command evidence Work Record
  automatically before attaching the evidence.
- Full stdout/stderr content is stored as content-addressed local-only artifacts
  under `blobs/<shard>/<sha256>`, while the evidence row stores bounded redacted
  previews and an artifact pointer.
- Focused tests cover UUIDv7/path helpers, versioned JSON wrappers, context
  packet output, hidden compatibility aliases, and artifact-backed evidence
  storage.

Known remaining review item: migrated legacy databases can still contain nullable
evidence rows that predate this productization pass; current store writes and
CLI-created evidence require or create a Work Record attachment.

## Foundation Re-Review Fixes

Integrated implementation work after the second architecture/data model review:

- Archive JSON now includes `schema_version: 1` while preserving the existing
  `version: 1` archive field for compatibility.
- `AgentContextPacket::from_work_context` preserves the default `local_only`
  visibility instead of upgrading records to `reportable`.
- `ctx evidence run --json` now prints the persisted evidence row after storage,
  so stdout/stderr fields are bounded safe previews rather than raw command
  output.
- Evidence output storage now has an `evidence_artifacts` join table so stdout
  and stderr artifacts are both attached to the evidence item. The existing
  `evidence.artifact_id` remains as a primary compatibility pointer.
- Artifact rows for command output now use `redaction_state = 'safe_preview'`
  for their preview text while keeping raw blobs local-only.
- Store open backfills legacy inline evidence stdout/stderr into blob artifacts
  and rewrites inline columns to safe previews.
- Archive import preflights evidence references, remains DB-atomic, and imports
  evidence through the artifact-backed transactional path.

## Foundation Third Re-Review Fixes

Integrated implementation work after the third architecture/data model review:

- `WorkRecordArchive` now includes a backward-compatible `artifacts` payload
  array carrying evidence stream artifact metadata and content-addressed output
  payloads.
- Export reads local blob files for evidence artifact links and writes the full
  payload content into the archive, while evidence stdout/stderr fields remain
  safe previews.
- Import validates artifact payload hashes and byte sizes, writes local blob
  files through canonical content-addressed paths, inserts artifact rows, and
  links stdout/stderr artifacts to evidence inside the same DB transaction.
- Legacy archives without `artifacts` still import through the older evidence
  preview path.
- Privacy and CLI docs now state that full evidence output is stored in
  local-only blob files and included in JSON archives.
- Added explicit archive round-trip coverage for both stdout and stderr payloads
  after the architecture/data reviewer PASS noted it as useful follow-up
  coverage.

## Capture Spool Integration

Integrated implementation work:

- Added `work-record-capture` with JSONL spool writer/importer, pending/tmp/
  processing/done/failed spool accounting, failed import retention with sidecar
  error metadata, and stable dedupe-key IDs.
- Added hidden `ctx capture write-fixture` for local fixture generation and
  public `ctx capture import` with schema-versioned JSON output.
- `ctx status` reports capture spool counts and `ctx validate` reports failed
  or processing spool files.
- Capture archive construction now uses the current archive schema, including
  `schema_version` and nested artifact payload preservation.

## VCS And PR Integration

Integrated implementation work:

- Added `work-record-vcs` with Git/jj workspace inspection, redacted remote URL
  normalization, repo fingerprinting, linked-worktree detection, and GitHub/
  GitLab PR URL parsing.
- Added `ctx vcs inspect --json` and `ctx pr parse <url> --json`, both with
  schema-versioned JSON output.
- Added CLI tests for VCS inspection redaction and confidence-labeled PR parse
  output.
- Resolved merge overlap with capture root subcommands and fixed Clippy
  `needless_borrow` findings in the PR parser.

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
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`:
  PASS after foundation review fixes. Covered 11 CLI integration tests, 3 core
  unit tests, 9 store unit tests, and doc-tests for core/store.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
  PASS after foundation review fixes. Covered fmt, check, clippy, and tests;
  Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
  installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`:
  PASS after foundation review fixes. Wrote local release dry-run manifest,
  checksum, and timing artifacts under
  `target/ctx-artifacts/release-dry-run/`.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`:
  PASS after foundation re-review fixes. Covered 11 CLI integration tests, 4
  core unit tests, 9 store unit tests, and doc-tests for core/store.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
  PASS after foundation re-review fixes. Covered fmt, check, clippy, and tests;
  Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
  installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh && git diff --check`:
  PASS after foundation re-review fixes.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`:
  PASS after archive payload fixes. Covered 11 CLI integration tests, 4 core
  unit tests, 9 store unit tests, and doc-tests for core/store.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
  PASS after archive payload fixes. Covered fmt, check, clippy, and tests; Bazel
  lane recorded `skipped` because neither `bazel` nor `bazelisk` is installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh && git diff --check`:
  PASS after archive payload fixes.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-store --lib -- --test-threads 1`:
  PASS after adding both-stream archive round-trip coverage.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`:
  PASS after adding both-stream archive round-trip coverage.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-capture --lib -- --test-threads 1`:
  PASS after capture merge. Covered 3 capture unit tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx capture -- --test-threads 1`:
  PASS after capture merge. Covered 2 capture CLI integration tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && git diff --check`:
  PASS after capture merge. Covered fmt, check, clippy, and tests; Bazel lane
  recorded `skipped` because neither `bazel` nor `bazelisk` is installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-vcs --lib -- --test-threads 1`:
  PASS after VCS merge. Covered 7 VCS unit tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx --test cli -- --test-threads 1`:
  PASS after VCS merge. Covered 15 CLI integration tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && git diff --check`:
  PASS after VCS merge. Covered fmt, check, clippy, and tests; Bazel lane
  recorded `skipped` because neither `bazel` nor `bazelisk` is installed.

## Reviewer Status

Architecture/data model reviewer returned FAIL on head `eb0d8f9` because IDs
were UUIDv4, public JSON/context output was not consistently versioned,
`blobs/`/`inbox/`/`device.json` path helpers were missing, and evidence output
was still inline or unattached. The fixes above are integrated locally and
were committed at `b7abdca`.

Architecture/data model reviewer returned FAIL on head `b7abdca` because archive
JSON lacked a top-level `schema_version`, default context output upgraded
local-only records to `reportable`, `ctx evidence run --json` returned raw
stdout/stderr from the in-memory object, evidence attached only one stream
artifact, and import/migration paths could bypass artifact-backed output. The
foundation re-review fixes above are integrated locally and awaiting re-review
after commit.

Architecture/data model reviewer returned FAIL on head `77d227f` because JSON
archive export/import preserved only evidence safe previews, not the full
artifact-backed stdout/stderr blob content. The archive payload fixes above are
committed at `6c33fb1`.

Architecture/data model reviewer returned PASS on head `6c33fb1`. Follow-up
concerns were limited to future binary artifact support and an optional
both-stream archive round-trip test; the test has been added locally and is
awaiting commit.

## Blockers

None proven yet.

## Accepted Deferrals

None accepted yet.
