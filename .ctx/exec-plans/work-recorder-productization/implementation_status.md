# Work Recorder Productization Implementation Status

Updated: 2026-06-22T22:44:31-05:00

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
- Buildkite/release/platform verification: public matrix wiring present; live
  Buildkite runner evidence pending.
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

## CI/Release Matrix Worker Slice

Integrated implementation work on child branch `ctx/work-record-ci-matrix` in
`/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-ci-matrix`:

- Expanded `.buildkite/pipeline.yml` into an explicit public CI/release matrix:
  - Linux x86_64 sequential lanes for pipeline contract, fmt, docs, cargo
    check, clippy, cargo test, examples, Bazel, and release dry-run;
  - macOS arm64 and macOS x86_64 host release dry-runs on the known
    `ctx-mac-gui-shared-arm64` and `ctx-mac-gui-shared-x64` queues with
    serialized concurrency groups;
  - Windows x86_64 host release dry-run on the known `windows-x64` queue using
    the native PowerShell wrapper and the `x86_64-pc-windows-gnu` host-triple
    contract;
  - FreeBSD x86_64 blocker artifact lane because no `queue=freebsd-x64` runner
    is documented in the known Buildkite queue inventory.
- Added `scripts/check-buildkite-pipeline.sh` so the public pipeline shape can
  be validated locally without Buildkite credentials.
- Added `scripts/release-platform-blocker.sh` to emit machine-readable and
  Markdown blocker evidence for required platforms that lack native runners.
- Extended `scripts/check.sh` with `docs` and `examples` modes. The examples
  mode builds `ctx` once and runs checked-in examples against a temporary local
  data root through `CTX_BIN`.
- Extended `scripts/release-dry-run.sh` so each host lane records
  `platform`, `target_triple`, and `expected_host_triple`, and fails closed if
  the actual Rust host triple does not match `CTX_EXPECT_HOST_TRIPLE`.

Known remaining CI/release blockers:

- Live Buildkite credentials/runners were not exercised from this local worker,
  so there is no Buildkite URL or green hosted evidence yet.
- The public pipeline assumes these external queues exist and are attached to
  the public `ctxrs/ctx` Buildkite pipeline: `main-linux`,
  `release-linux-managed` with `ctx-runner-class=release-linux-x64-stage`,
  `ctx-mac-gui-shared-arm64`, `ctx-mac-gui-shared-x64`, and `windows-x64`.
- The Windows lane uses `scripts/ci-windows.ps1` so smoke/release dry-runs do
  not require Bash on `windows-x64`; it now bootstraps Rust GNU plus w64devkit
  under the Buildkite/ctx tool cache so `rustc -vV` reports
  `host: x86_64-pc-windows-gnu` and the GNU linker has the expected CRT
  and GCC runtime libraries.
- FreeBSD native release artifacts are blocked until a native
  `queue=freebsd-x64` Buildkite agent pool exists, or until a separate
  cross-build lane proves the FreeBSD linker/toolchain contract.

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

## Search And Agent Context Integration

Integrated implementation work:

- Added `work-record-search` with ranked local search/context packet builders,
  safe snippet redaction, citations, evidence metadata, token-budget truncation,
  pagination metadata, and optional share-safe dashboard links.
- `ctx search --json` now emits a schema-versioned redacted search packet.
- `ctx context --json` now uses the search packet builder to emit an
  `AgentContextPacket` with max-token controls and local-only visibility.
- Added tests for agent packet shape, why-matched/citations/evidence, token
  budget truncation, dashboard URL safety, and secret-like snippet redaction.

## Dashboard Export Integration

Integrated implementation work:

- Added local static dashboard export via `ctx dashboard export --output <dir>`.
- Dashboard output is share-safe by default: no JavaScript, no remote assets,
  bounded evidence previews, escaped HTML, redacted secret-like command
  fragments, and safe workspace labels instead of raw absolute workspace paths.
- Dashboard cards include local record counts, recent records, Work Record
  states, linked PR/repository cues, evidence command previews, captured output
  previews, capture/search context cues, and privacy framing.
- Export writes `index.html` plus static assets under the chosen output
  directory; it does not publish or sync anything.
- Manager visual review generated and inspected screenshots:
  - `/var/tmp/ctxwr-dashboard/dashboard.png` at 1280x900.
  - `/var/tmp/ctxwr-dashboard/dashboard-mobile.png` at 390x844.
- Visual review notes: desktop is populated with metrics, recent Work Records,
  PR link, evidence preview, capture/search context, and privacy text; mobile
  stacks cleanly with the same safe content. The first screenshot pass exposed
  raw workspace paths and token-like command fragments, which were fixed before
  committing this slice.

## Local Shims And Docs/Examples Integration

Integrated implementation work:

- Added `ctx shim install --dir <path>`, `ctx shim env --dir <path>`, and
  `ctx shim uninstall --dir <path>`.
- Generated local reversible wrapper scripts for `git`, `jj`, and `gh` that
  find the real tool later on `PATH`, preserve exit code/stdout/stderr, and
  best-effort spool capped command capture envelopes into the local inbox.
- Added hidden importer-facing `ctx capture write-shim-command` and capture
  envelope support for shim stdout/stderr, capped at 64 KiB per stream.
- Shim install refuses to overwrite unrecognized files; uninstall removes only
  ctx-marked wrapper scripts.
- README and docs now describe the implemented local-first surface: root
  commands, dashboard export, capture import, shims, VCS/PR helpers, export/
  import, privacy/storage, and explicit not-yet-shipped hosted/provider/import
  boundaries.
- Added `examples/local-record-workflow.sh`, which creates a temporary data
  root, records work, captures evidence, links a PR, exports a dashboard, exports
  archive JSON, and validates storage.
- Added `examples/capture-spool-fixture.sh`, which writes a capture fixture,
  imports it, searches it, and validates storage.
- Added `scripts/check-docs.sh` for doc/example syntax and product-claim checks.

## Capture Auto-Import, Doctor, And Repair

Integrated implementation work:

- Normal Work Recorder commands now import pending capture spool `.jsonl` files
  before serving results, so shim captures become visible without a daemon.
- Auto-import failures are retained as `.failed` files and reported to stderr
  with a `ctx doctor` / `ctx repair` pointer instead of being silently ignored.
- Added root `ctx doctor` as the product-facing local health check command while
  preserving `ctx validate`.
- Added `ctx repair [--json]` to retry failed capture spool files and import
  anything that succeeds.
- Added tests proving normal commands auto-import pending captures and
  `doctor`/`repair` can retry a failed capture file into the record store.
- README and docs now describe auto-import, `doctor`, and `repair`.

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
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-search --lib -- --test-threads 1`:
  PASS after search merge. Covered 2 search unit tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx --test cli -- --test-threads 1`:
  PASS after search merge. Covered 20 CLI integration tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && git diff --check`:
  PASS after search merge. Covered fmt, check, clippy, and tests; Bazel lane
  recorded `skipped` because neither `bazel` nor `bazelisk` is installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`:
  PASS on integrated local product head after foundation, capture, VCS, and
  search merges.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-report --lib -- --test-threads 1`:
  PASS after dashboard redaction hardening.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 cargo build -p ctx --bin ctx`:
  PASS before dashboard dogfood export.
- Dashboard dogfood commands:
  `target/debug/ctx setup`, `target/debug/ctx record`,
  `target/debug/ctx evidence run`, `target/debug/ctx link-pr`,
  `target/debug/ctx capture write-fixture`, `target/debug/ctx capture import --json`,
  and `target/debug/ctx dashboard export --output /var/tmp/ctxwr-dashboard`:
  PASS. Generated `file:///var/tmp/ctxwr-dashboard/index.html`.
- Chrome headless screenshots using `/var/tmp` profile/cache/temp:
  PASS. Generated `/var/tmp/ctxwr-dashboard/dashboard.png` and
  `/var/tmp/ctxwr-dashboard/dashboard-mobile.png`.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && git diff --check`:
  PASS after dashboard export hardening. Covered fmt, check, clippy, workspace
  tests, and `git diff --check`; Bazel lane recorded `skipped` because neither
  `bazel` nor `bazelisk` is installed.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-capture -p ctx -- --test-threads=1`:
  PASS after local shims integration. Covered 24 CLI integration tests and 3
  capture unit tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 cargo build -p ctx --bin ctx && ./scripts/check-docs.sh && CTX_BIN=target/debug/ctx CTX_EXAMPLE_TMPDIR=/var/tmp/ctxwr-examples ./examples/local-record-workflow.sh && CTX_BIN=target/debug/ctx CTX_EXAMPLE_TMPDIR=/var/tmp/ctxwr-examples ./examples/capture-spool-fixture.sh && git diff --check`:
  PASS after docs/examples integration. The local workflow example generated a
  dashboard under `/var/tmp/ctxwr-examples/.../dashboard/index.html`.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && ./scripts/check-docs.sh && git diff --check`:
  PASS after local shims and docs/examples integration. Covered fmt, check,
  clippy, workspace tests, docs/example syntax/product-claim checks, and
  `git diff --check`; Bazel lane recorded `skipped` because neither `bazel` nor
  `bazelisk` is installed.
- `cargo fmt --all && TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-capture -p ctx -- --test-threads=1`:
  PASS after capture auto-import/doctor/repair integration. Covered 26 CLI
  integration tests and 3 capture unit tests.
- `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && ./scripts/check-docs.sh && git diff --check`:
  PASS after capture auto-import/doctor/repair integration. Covered fmt, check,
  clippy, workspace tests, docs checks, and `git diff --check`; Bazel lane
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

Dashboard visual self-review returned PASS for the current local dashboard slice
after redaction hardening. A separate adversarial UI/security review is still
required after docs/shims and any hosted/report refinements are integrated.

## Blockers

None proven yet.

## Accepted Deferrals

None accepted yet.

## 2026-06-22 Local Product Review Blocker Fix

- Owner: manager continuation after primary agent hit usage limit.
- Commit state: committed at `703eaba` and followed by release-matrix commit
  `6d73e2c`.
- Scope:
  - Moved public redaction to shared `work-record-core` helpers and reused it
    from store previews, dashboard HTML, and agent search/context snippets.
  - Persisted capture provenance through `capture_sources` plus
    `source_id` links for imported shim records/evidence.
  - Rebuilt FTS as a real, redacted projection and made search use FTS before
    falling back to LIKE.
  - Added regression coverage for secret redaction, source persistence, and
    evidence-only FTS matches.
- Status: focused CLI/unit gates and the full capped local check are PASS.

## 2026-06-22 Public CI/Release Matrix Integration

- Owner: manager integration of the CI/release worker slice.
- Commit state: committed at `6d73e2c`.
- Scope:
  - Added a public `.buildkite/pipeline.yml` with Linux, macOS arm64, macOS x64,
    Windows x64, and documented FreeBSD blocker lanes.
  - Added `scripts/check-buildkite-pipeline.sh` to validate the pipeline shape
    locally, including Buildkite dry-run parsing when `buildkite-agent` is
    installed.
  - Added `scripts/release-platform-blocker.sh` for machine-readable missing
    platform evidence.
  - Extended `scripts/check.sh` with docs/examples lanes and updated release
    dry-run host-triple guards.
- Local validation on clean head `6d73e2c`:
  - `./scripts/check-buildkite-pipeline.sh`: PASS.
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`: PASS.
  - `./scripts/check-docs.sh`: PASS.
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`: PASS.
  - `./scripts/release-platform-blocker.sh freebsd-x64 x86_64-unknown-freebsd target/ctx-artifacts/freebsd-blocker`: PASS.
  - `git diff --check`: PASS.
- Remaining external evidence gap:
  - Live Buildkite green URLs are still pending.
  - Native FreeBSD remains blocked until a runner/pool or proven cross-build
    path exists.
  - Local Bazel remains skipped because neither `bazel` nor `bazelisk` is
    installed on this host.

## 2026-06-22 Review Blocker Remediation Pass

- Owner: manager continuation after adversarial local product/security and
  CI/release reviews.
- Commit state: uncommitted changes on top of `de1c718`.
- Local product/security fixes:
  - Added shared share-safe text redaction for secrets and common local absolute
    paths.
  - Applied share-safe rendering to dashboard record title/body/tags/tag
    summaries, PR links, evidence commands/previews, and Markdown context.
  - Withheld unsafe PR URLs from dashboard/context links instead of rendering
    credential-bearing or non-HTTPS links.
  - Made JSON search/context ranking consider evidence stdout/stderr-only
    matches, not only record fields and evidence commands.
  - Added CLI/unit regressions for dashboard record-field redaction, Markdown
    context redaction, share-safe local path redaction, and evidence-output-only
    search/context results.
- CI/release fixes:
  - `scripts/check.sh bazel` still skips locally when Bazel is missing, but now
    fails when `CTX_REQUIRE_BAZEL=1`.
  - The Buildkite Bazel lane sets `CTX_REQUIRE_BAZEL=1`, so release lanes cannot
    pass through an unproven Bazel skip on CI.
  - Added native macOS arm64, macOS x64, and Windows x64 platform-smoke lanes
    before host-native release dry-runs.
  - Added `scripts/check.sh platform-smoke`, including host-triple enforcement,
    binary build, setup, record, search, context, dashboard export, and validate.
- Local validation on this uncommitted pass:
  - `cargo fmt --all`: PASS.
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-core -p work-record-report -p work-record-search -p ctx -- --test-threads=1`: PASS.
  - `./scripts/check-buildkite-pipeline.sh`: PASS.
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/check.sh platform-smoke`: PASS.
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`: PASS.
  - `./scripts/check-docs.sh`: PASS.
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`: PASS.
  - `git diff --check`: PASS.
- Remaining external evidence gap:
  - Live Buildkite green URLs still need to be generated from the pushed branch.
  - Native FreeBSD remains blocked until a runner/pool or cross-build lane is
    available.
  - Hosted/private staging remains a separate active workstream.

## 2026-06-22 Buildkite Queue Routing Remediation

- Build 24:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/24`
  - Outcome: FAILED before matrix expansion because the trigger used an invalid
    full commit SHA.
- Build 25:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/25`
  - Triggered on `work-record` at `b0dd4c2`.
  - Outcome so far: matrix expanded correctly, but the first matrix job stayed
    scheduled on `queue=main-linux`.
- Repo-owned remediation:
  - routed Linux verification and FreeBSD blocker lanes to the known working
    `queue=release-linux-managed` with `ctx-runner-class=release-linux-control`;
  - left Linux release dry-run on `ctx-runner-class=release-linux-x64-stage`;
  - local `./scripts/check-buildkite-pipeline.sh`: PASS after routing change.

## 2026-06-22 Buildkite Rust Toolchain Routing Remediation

- Build 26:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/26`
  - Triggered on `work-record` at `75b2bcb`.
  - Pipeline upload and contract lanes passed.
  - Fmt lane failed on `ctx-runner-class=release-linux-control` with
    `cargo: command not found`.
- Repo-owned remediation:
  - rerouted Linux verification and FreeBSD blocker lanes to
    `ctx-runner-class=release-linux-x64-stage`, matching the existing Linux
    release dry-run runner class that is intended to have Rust/Cargo tooling;
  - updated the pipeline contract check accordingly.

## 2026-06-23 Hosted Worker Foundation Checkpoint

- Private repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`
- Private branch/head:
  `ctx/work-recorder-hosted-team` / `006e25706`
- Hosted implementation landed:
  - Cloudflare Worker package `work-recorder-worker`;
  - Neon migration `0013_work_recorder_foundation.sql`;
  - device registration endpoint;
  - metadata-only sync batch endpoint with idempotency;
  - sync cursor endpoint;
  - SHA-256-verified R2 blob upload endpoint;
  - bearer-token auth for protected routes;
  - explicit rejection of raw transcript/prompt/tool-output fields in hosted
    sync payloads;
  - Work Recorder readiness profile in the shared Cloudflare/Neon readiness
    script;
  - Buildkite pipeline wrapper `.buildkite/pipelines/work-recorder-worker.yml`.
- Hosted validation:
  - `pnpm typecheck`: PASS;
  - `pnpm test`: PASS, 21 tests;
  - `pnpm exec vitest run test/cloudflare-neon-readiness.test.mjs
    --pool=threads` in `llm-relay-worker`: PASS, 8 tests;
  - `pnpm readiness:check:local`: PASS with dummy local-only env and no
    Cloudflare/Neon API calls;
  - `wrangler deploy --dry-run --env staging`: PASS through
    `scripts/buildkite/run_work_recorder_worker_check.sh`;
  - real staging readiness against `ctx-work-recorder-staging`: PASS;
  - live staging smoke against
    `https://ctx-work-recorder-staging.fancy-sea-92df.workers.dev`: PASS for
    health, device registration, metadata sync, cursor read, and SHA-256
    verified blob upload;
  - `git diff --check`: PASS.
- Hosted staging provisioned:
  - Neon role `ctx_work_recorder` was provisioned through the Neon branch-role
    API;
  - Neon migration `0013_work_recorder_foundation.sql` was applied to the
    primary production Neon branch;
  - Infisical prod contains `WORK_RECORDER_DATABASE_URL`,
    `WORK_RECORDER_SHARED_TOKEN`, and `CTX_WORK_RECORDS_R2_BUCKET`;
  - R2 buckets `ctx-work-record-blobs-staging` and `ctx-work-record-blobs`
    exist;
  - Cloudflare Worker `ctx-work-recorder-staging` is deployed with required
    secrets.
- Remaining external hosted gaps:
  - production Worker `ctx-work-recorder` has not been deployed or routed;
  - GitHub app/webhook configuration and PR comment mutation are still not
    wired;
  - hosted full-transcript sync remains intentionally disabled;
  - local `buildkite-agent pipeline upload --dry-run` now parses the private
    worker pipeline when `BUILDKITE_AGENT_TOKEN` from Infisical is supplied as
    `BUILDKITE_AGENT_ACCESS_TOKEN`;
  - remote Buildkite proof is still blocked from this local session because no
    Buildkite API token or active Buildkite job context is available to create
    and observe a hosted build.

## 2026-06-22 Buildkite Token Dry-Run Follow-Up

- Private worker pipeline parser:
  - command:
    `BUILDKITE_AGENT_ACCESS_TOKEN=<from Infisical BUILDKITE_AGENT_TOKEN> buildkite-agent pipeline upload --dry-run .buildkite/pipelines/work-recorder-worker.yml`;
  - repo/worktree:
    `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx-private/work-recorder-hosted-team`;
  - branch/head:
    `ctx/work-recorder-hosted-team` / `006e25706`;
  - outcome: PASS;
  - coverage: Buildkite parsed the `work-recorder-worker` step on the
    `main-linux-control` queue.
- Remaining external Buildkite gap:
  - no `BUILDKITE_API_TOKEN` / `BUILDKITE_TOKEN` was available in Infisical;
  - this shell is not inside an active Buildkite job context;
  - therefore no new remote Buildkite build URL was created for the private
    worker pipeline from this local session.

## 2026-06-22 Buildkite Rust Bootstrap Remediation

- Build 27:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/27`
  - Triggered on `work-record` at `e29008e`.
  - Pipeline upload and contract lanes passed.
  - Fmt lane failed on `ctx-runner-class=release-linux-x64-stage` with
    `cargo: command not found`.
- Repo-owned remediation:
  - added `ctx_ensure_rust_toolchain` in `scripts/ci-common.sh`;
  - made CI/release Rust entrypoints bootstrap stable Rust through rustup when
    Cargo is missing on a Buildkite host;
  - required `cargo`, `rustc`, `cargo fmt`, and `cargo clippy`;
  - made the bootstrap a no-op when tools already exist;
  - serialized rustup installation/component work with
    `CTX_RUSTUP_LOCK` or `${TMPDIR:-${CTX_REPO_ROOT}/target/tmp}/ctx-rustup.lock`
    to avoid overlapping agent/job cache races without depending on `/tmp`.
- Local validation on `work-record` with uncommitted bootstrap changes on
  `ffeebbc`:
  - `bash -n scripts/ci-common.sh scripts/check.sh scripts/release-dry-run.sh`:
    PASS;
  - `./scripts/check-buildkite-pipeline.sh`: PASS;
  - `git diff --check`: PASS;
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/check.sh platform-smoke`:
    PASS;
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`:
    PASS;
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
    PASS, with local Bazel recorded as skipped because Bazel/Bazelisk is not
    installed.
- Post-audit lock-path hardening:
  - commit: `60d92cc`;
  - changed the rustup lock default away from `/tmp`;
  - public final-audit agent reran:
    - `bash -n scripts/check.sh scripts/ci-common.sh scripts/release-dry-run.sh`:
      PASS;
    - `./scripts/check-buildkite-pipeline.sh`: PASS;
    - `./scripts/check-docs.sh`: PASS;
    - `git diff --check`: PASS;
  - manager reran:
    - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`:
      PASS, with local Bazel recorded as skipped because Bazel/Bazelisk is not
      installed;
    - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`:
      PASS.
- Remaining external evidence gap:
  - push the committed bootstrap remediation to `origin/work-record`;
  - trigger and observe a fresh public Buildkite build;
  - prove the Buildkite Bazel lane on a host with Bazel/Bazelisk because CI
    sets `CTX_REQUIRE_BAZEL=1`;
  - native FreeBSD remains blocked until a runner/pool or proven cross-build
    lane exists.

## 2026-06-22 Buildkite Docs Tooling Remediation

- Build 28:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/28`
  - Triggered on `work-record` at `f1770b2`.
  - Pipeline upload, contract, and fmt lanes passed.
  - Docs lane failed on `ctx-runner-class=release-linux-x64-stage` because
    `scripts/check-docs.sh` required `rg`, which is not installed on that
    Buildkite host.
- Repo-owned remediation:
  - `scripts/check-docs.sh` now uses `rg` when available and falls back to
    recursive `grep -E` otherwise;
  - the docs check still fails closed on missing required docs/examples and on
    false shipped-feature wording.
- Local validation on `work-record` with uncommitted docs fallback changes on
  `60d92cc`:
  - `./scripts/check-docs.sh`: PASS;
  - `PATH=/usr/bin:/bin bash scripts/check-docs.sh`: PASS, proving the no-`rg`
    path;
  - `git diff --check`: PASS.
- Remaining external evidence gap:
  - commit and push the docs tooling remediation;
  - trigger and observe a fresh public Buildkite build.

## 2026-06-22 Buildkite Sccache Wrapper Remediation

- Build 29:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/29`
  - Outcome: failed in checkout because the trigger used an invalid full SHA for
    `3f1b534`. This was an operator-trigger error, not a repo/product failure.
- Build 30:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/30`
  - Triggered on `work-record` at
    `3f1b53421e7c929dc463e49a6679fb77c66a2404`.
  - Pipeline upload, contract, fmt, and docs lanes passed.
  - `cargo check` failed before checking source because the Buildkite agent
    injected `RUSTC_WRAPPER=/usr/bin/sccache`, and sccache failed with
    `path must be shorter than libc::sockaddr_un.sun_path` on the agent's
    checkout/socket path.
- Repo-owned remediation:
  - `scripts/ci-common.sh` now unsets inherited sccache `RUSTC_WRAPPER` by
    default during `ctx_init_resource_env`;
  - sccache remains opt-in through `CTX_USE_SCCACHE=1`.
- Local validation on `work-record` with uncommitted sccache wrapper changes on
  `3f1b534`:
  - `RUSTC_WRAPPER=/usr/bin/sccache TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/check.sh check`:
    PASS;
  - `bash -n scripts/ci-common.sh scripts/check.sh scripts/release-dry-run.sh scripts/check-docs.sh`:
    PASS;
  - `git diff --check`: PASS.
- Remaining external evidence gap:
  - commit and push the sccache wrapper remediation;
  - trigger and observe a fresh public Buildkite build.

## 2026-06-22 Buildkite Bazelisk Bootstrap Remediation

- Build 32:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/32`;
  - failed in the Bazel lane because `CTX_REQUIRE_BAZEL=1 ./scripts/check.sh bazel`
    found neither `bazel` nor `bazelisk` on the Linux runner.
- Repo-owned remediation:
  - `scripts/ci-common.sh` now bootstraps Bazelisk when Bazel is required and
    no `bazel`/`bazelisk` binary is already on `PATH`;
  - default Bazelisk version is pinned to `v1.29.0`, overrideable with
    `CTX_BAZELISK_VERSION`;
  - bootstrap downloads into `target/tool-cache/bazelisk/bin` and uses
    `target/tool-cache/bazelisk-home` plus `target/tool-cache/bazel-output`;
  - `.bazelignore` excludes `target` so Bazel does not traverse repo-owned
    tool/cache/build output directories;
  - the Bazel test target now includes `Cargo.lock`, records a
    `MODULE.bazel.lock`, and the Bazel test wrapper can locate the runfiles
    repo root before invoking Cargo;
  - Linux x86_64 is supported, with Linux arm64, macOS x86_64/arm64, and
    Windows x86_64 asset selection to avoid regressing other hosted lanes;
  - unsupported platforms and missing `curl` fail clearly only in required
    mode; normal local optional Bazel mode still records a skip.
- Remaining external evidence gap:
  - commit and push the Bazelisk bootstrap remediation;
  - trigger and observe a fresh public Buildkite build proving the Bazel lane.

## 2026-06-22 Buildkite Bazel Cargo Environment Remediation

- Build 33:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/33`;
  - got through Bazelisk bootstrap, then failed inside `//:cargo_tests` because
    `cargo` was missing from the Bazel test environment;
  - Bazel test setup also reported `zip: command not found` while creating
    `test.outputs/outputs.zip`.
- Repo-owned remediation:
  - `scripts/check.sh` now runs `ctx_ensure_rust_toolchain` before invoking
    required/optional Bazel checks;
  - the Bazel invocation forwards sanitized Rust/Cargo environment values into
    tests: `PATH`, `CARGO_HOME`, `RUSTUP_HOME`, `CARGO_BUILD_JOBS`,
    `RUST_TEST_THREADS`, and `TMPDIR`;
  - the Bazel invocation passes `--nozip_undeclared_test_outputs`;
  - `scripts/bazel-test.sh` prepends `${CARGO_HOME}/bin` to `PATH`, exports
    `CARGO_HOME`/`RUSTUP_HOME`, fails clearly if `cargo` is still unavailable,
    and writes timing artifacts under `CTX_ARTIFACT_DIR` or `TEST_TMPDIR`
    instead of `TEST_UNDECLARED_OUTPUTS_DIR`.
- Local validation:
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 CTX_REQUIRE_BAZEL=1 ./scripts/check.sh bazel`:
    PASS on the stable file state after the Cargo/PATH/no-zip remediation.
- Remaining external evidence gap:
  - commit and push the remediation;
  - trigger and observe a fresh public Buildkite build proving the Bazel lane on
    the Linux runner.

## 2026-06-22 Buildkite Windows PowerShell Remediation

- Build 34:
  - URL: `https://buildkite.com/luca-king/ctx-public-release-verification/builds/34`;
  - Linux fmt, docs, check, clippy, test, examples, and Bazel lanes passed;
  - Windows smoke failed before product code because `bash` was not on the
    `windows-x64` runner `PATH`.
  - Direct final-state inspection from this shell was blocked by Buildkite
    login and no `BUILDKITE_API_TOKEN`/`BUILDKITE_TOKEN` was available.
- Repo-owned remediation:
  - added `scripts/ci-windows.ps1`, a native PowerShell wrapper for
    `platform-smoke` and `release-dry-run`;
  - Windows smoke and release dry-run lanes now invoke PowerShell directly and
    no longer depend on Bash/Git Bash;
  - the PowerShell wrapper bootstraps Rust through `rustup-init.exe` if needed,
    keeps `CARGO_HOME`, `RUSTUP_HOME`, temp files, and tool downloads under
    repo-owned `target/` paths, enforces `CTX_EXPECT_HOST_TRIPLE`, and preserves
    the same low Cargo/test resource caps as the Bash wrappers;
  - release dry-run writes the Windows `.exe`, manifest, checksums, and timing
    artifacts without requiring Unix tools.
- Local validation:
  - `bash -n scripts/check.sh scripts/ci-common.sh scripts/bazel-test.sh scripts/check-docs.sh scripts/check-buildkite-pipeline.sh scripts/release-dry-run.sh`:
    PASS;
  - `./scripts/check-buildkite-pipeline.sh`: PASS, including the Windows
    PowerShell wrapper contract;
  - `./scripts/check-docs.sh`: PASS;
  - `git diff --check`: PASS.
- Remaining external evidence gap:
  - commit and push the Windows PowerShell remediation;
  - trigger and observe a fresh public Buildkite build proving Windows smoke and
    Windows release dry-run.

## 2026-06-22 Buildkite Head Trigger Follow-Up

- Build 34 evidence:
  - Linux fmt, docs, check, clippy, test, examples, Bazel, and Linux release
    dry-run passed;
  - the only reported failure was Windows smoke trying to run Bash on a runner
    where `bash` was unavailable.
- Branch evidence:
  - latest published remediation head `1d2ed69` switches Windows smoke and
    Windows release dry-run to native PowerShell via `scripts/ci-windows.ps1`;
  - Buildkite did not start a run for `1d2ed69`;
  - build 36 only ran the intermediate `606fcb7` Git Bash wrapper commit and
    was canceled.
- Remaining external evidence gap:
  - push a new docs-only head on `origin/work-record` to trigger a fresh
    Buildkite webhook run for the PowerShell remediation.

## 2026-06-22 Buildkite Linux Smoke Gap Follow-Up

- Build 37:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/37>
- Branch/head:
  `work-record` / `1d2ed69` (`Run Windows lanes with native PowerShell`)
- Outcome:
  - a fresh Buildkite run did start for the PowerShell remediation head;
  - read-only review found that the matrix still lacked an explicit Linux
    platform smoke lane before the Linux release dry-run, while macOS and
    Windows already had smoke lanes;
  - Build 37 is treated as superseded by the repo-owned Linux smoke-lane fix
    rather than final release-verification evidence.
- Remediation in progress:
  - add `platform-smoke-linux-x64` after Bazel;
  - make `release-dry-run-linux-x64` depend on the Linux smoke lane;
  - update the Buildkite pipeline contract to require the Linux smoke lane.

## 2026-06-22 Buildkite Platform Smoke Remediation Follow-Up

- Build 39:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/39>
- Outcome:
  - operator trigger error; the build was created for a mistyped full commit
    SHA and failed checkout with `not our ref`;
  - canonical evidence continues on the corrected Build 40.
- Build 40:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/40>
- Branch/head:
  `work-record` / `0d4c232b2bd1697e7a8d3f0e8bec0daa5d34ed59`
- Outcome:
  - Linux shift-left gates through Bazel passed;
  - the new Linux smoke lane failed immediately with exit 141 from a
    `ctx record --json | sed | head` pipeline under `set -o pipefail`;
  - Windows smoke reached the native PowerShell wrapper and Rust bootstrap, but
    `Run-Cargo` was called without a named `-Args` parameter, so PowerShell ran
    bare `cargo` and never produced `target/debug/ctx.exe`;
  - macOS smoke lanes were blocked before commands ran by stale shared-agent
    checkout cleanup failures under the default checkout path.
- Remediation in progress:
  - parse the Linux shell smoke record id without a SIGPIPE-prone `head`
    pipeline;
  - call `Run-Cargo -Args` explicitly in the Windows PowerShell smoke and
    release dry-run paths, and assert that in the pipeline contract;
  - route macOS smoke/release lanes through Buildkite's `custom-checkout#v1.8.0`
    plugin with `skip_checkout: true` and per-build checkout subdirectories, so
    default checkout cleanup does not touch the stale shared-agent directory.

## 2026-06-22 Buildkite 41 Follow-Up

- Build 41:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/41>
- Branch/head:
  `work-record` / `3da27084eae98d84bea392a4b46121cd319302d7`
- Outcome:
  - pipeline contract, fmt, docs, cargo check, clippy, cargo test, examples,
    Bazel, and the FreeBSD blocker artifact passed;
  - Linux smoke still failed with exit 141, now traced to
    `rustc -vV | awk` host-triple parsing under `set -o pipefail`;
  - macOS custom checkout succeeded and checked out the right commit, but the
    macOS agent pre-command hook expected
    `$BUILDKITE_BUILD_CHECKOUT_PATH/scripts/buildkite/macos_agent_pre_command.sh`;
    because the repo was cloned below the default checkout path, the hook failed
    before the command ran;
  - Windows smoke still invoked bare `cargo`; using a PowerShell parameter named
    `Args` continued to collide with automatic argument handling.
- Remediation in progress:
  - make `ctx_detect_host_triple` capture `rustc -vV` before parsing;
  - set macOS `$BUILDKITE_BUILD_CHECKOUT_PATH` itself to an isolated `/tmp`
    per-build path, while keeping `custom-checkout#v1.8.0` and
    `delete_checkout: true`;
  - rename PowerShell wrapper parameters to `CargoArgs`/`CtxArgs` and call
    `Run-Cargo -CargoArgs ...`.

## 2026-06-22 Buildkite Bad-SHA Checkout Follow-Up

- Build 39:
  - failed during checkout before product validation;
  - the build targeted a bad full SHA beginning `0d4c23261bd...`;
  - the real `origin/work-record` head is
    `0d4c232b2bd1697e7a8d3f0e8bec0daa5d34ed59`.
- Local/remote verification:
  - `git rev-parse HEAD` and `git rev-parse origin/work-record` both report
    `0d4c232b2bd1697e7a8d3f0e8bec0daa5d34ed59`;
  - `git fetch origin 0d4c232b2bd1697e7a8d3f0e8bec0daa5d34ed59` succeeds and
    `git cat-file -t` reports `commit`.
- Assessment:
  - build 39 is a transient bad-SHA webhook/checkout failure, not a product
    code or pipeline-contract failure.
- Remaining external evidence gap:
  - push a new docs-only head on `origin/work-record` to trigger a fresh
    Buildkite webhook run from a valid branch head.

## 2026-06-22 Buildkite Smoke Parser And Windows Cargo Fix

- Build 40:
  - Linux smoke reached product code and failed with exit 141 from the smoke
    record-id parser pipeline under `pipefail`;
  - Windows smoke invoked `Run-Cargo` positionally, so PowerShell did not bind
    the array to the `-Args` parameter, `cargo` printed help, and
    `target/debug/ctx.exe` was missing.
- Repo-owned remediation:
  - `scripts/check.sh` now captures the full `ctx record --json` output before
    parsing the id, removing the `sed | head` early-close path that produced
    SIGPIPE/141;
  - `scripts/ci-windows.ps1` now calls `Run-Cargo -Args (...)` for both
    platform smoke and release dry-run builds;
  - the Buildkite contract now asserts the Windows wrapper uses named Cargo
    arguments.
- Local validation:
  - `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/check.sh platform-smoke`:
    PASS;
  - shell syntax, Buildkite contract, docs, and diff checks passed;
  - focused subagent review found no remaining issue in the Linux parser or
    Windows `Run-Cargo` dirty changes.
- Remaining external evidence gap:
  - commit and push the remediation;
  - trigger and monitor a fresh public Buildkite run through all required
    release-verification lanes.

## 2026-06-23 Buildkite Windows MSVC Environment Follow-Up

- Build 43:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/43>
- Branch/head:
  `work-record` / `b8fc9154b3a5855e71d17a96d13881ec1a8a78b5`
- Outcome:
  - PASS: pipeline upload, pipeline contract, fmt, docs, cargo check, clippy,
    cargo test, examples, Bazel, Linux smoke, macOS arm64/x64 smoke, Linux
    release dry-run, macOS arm64/x64 release dry-runs, and the FreeBSD blocker
    artifact;
  - FAIL: Windows smoke bootstrapped Rust and then failed compiling with the
    MSVC host toolchain because `link.exe` was not on PATH;
  - Windows release dry-run was skipped because it depends on Windows smoke.
- Repo-owned remediation:
  - `scripts/ci-windows.ps1` now imports the Visual Studio/MSVC build
    environment from `vswhere.exe`, `VsDevCmd.bat`, or `vcvars64.bat` when the
    `x86_64-pc-windows-msvc` lane does not already have `link.exe` on PATH;
  - the script keeps Rust/Cargo caches under `target\tool-cache` and temp files
    under `target\tmp`;
  - if Visual Studio Build Tools are truly absent, the lane fails before cargo
    with a direct `link.exe`/Build Tools diagnostic instead of surfacing a Rust
    linker error after dependency compilation starts.
- Remaining external evidence gap:
  - commit and push the remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows GNU Toolchain Follow-Up

- Build 45:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/45>
- Branch/head:
  `work-record` / `cd73fb979802e2e987e6ad07ae299a127aff2362`
- Outcome:
  - PASS before Windows: pipeline contract, fmt, docs, cargo check, clippy,
    cargo test, examples, Bazel, and the start of all smoke lanes;
  - FAIL: Windows smoke proved the runner has no Visual Studio Build Tools
    environment script and no `link.exe`, so an MSVC lane cannot pass on that
    fresh runner without external image changes.
- Repo-owned remediation:
  - Windows smoke/release now use the Rust `x86_64-pc-windows-gnu` host triple;
  - `scripts/ci-windows.ps1` installs that Rust host via rustup when needed;
  - the wrapper downloads LLVM-MinGW under the Buildkite/ctx tool cache and
    wires Cargo, `cc`, `c++`, and `ar` to the bundled MinGW compiler tools for
    bundled SQLite builds, avoiding Visual Studio and `/tmp`.
- Remaining external evidence gap:
  - commit and push the GNU toolchain remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows GNU Download Follow-Up

- Build 48:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/48>
- Branch/head:
  `work-record` / `893439e3c923de926738ead2d5c21d86484fa105`
- Outcome at remediation time:
  - PASS: Linux and macOS smoke/release lanes completed, along with the
    pipeline contract, fmt, docs, cargo check, clippy, cargo test, examples,
    Bazel, and FreeBSD blocker artifact;
  - IN PROGRESS: Windows smoke reached the intended `x86_64-pc-windows-gnu`
    Rust bootstrap and began downloading the GNU toolchain, proving the lane was no longer
    blocked by the missing MSVC `link.exe`;
  - RISK: the Windows log stayed silent at the `Invoke-WebRequest` GNU toolchain download
    line for multiple polling intervals, leaving no bounded retry/timeout or
    completion evidence.
- Repo-owned remediation:
  - `scripts/ci-windows.ps1` now disables PowerShell progress rendering and
    routes rustup, GNU toolchain, and optional Visual Studio Build Tools downloads through
    a `Download-File` helper;
  - the helper prefers `curl.exe` with redirects, retries, connect timeout, and
    a bounded max runtime, writes through a temporary `.download` file, verifies
    non-empty output, and logs downloaded byte counts;
  - the GNU toolchain and external Buildkite tool-cache strategy are
    unchanged.
- Local validation before pushing:
  - `./scripts/check-buildkite-pipeline.sh`: PASS;
  - `bash -n scripts/check.sh scripts/ci-common.sh scripts/release-dry-run.sh scripts/check-buildkite-pipeline.sh scripts/buildkite/macos_agent_pre_command.sh`:
    PASS;
  - PowerShell execution remains validated by the next Windows Buildkite lane
    because `pwsh`/`powershell` are unavailable on this Linux host.
- Remaining external evidence gap:
  - commit and push the download hardening;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows LLVM-MinGW Follow-Up

- Build 49:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/49>
- Branch/head:
  `work-record` / `8e7803cd82210b8f5721cd00fabac5f46e43f714`
- Outcome:
  - Windows smoke bootstrapped Rust GNU and reached compilation, then failed
    linking the first proc-macro build because the Zig linker wrapper could not
    find the Windows GNU `msvcrt`/MinGW import libraries.
- Repo-owned remediation:
  - replace the Zig wrapper with LLVM-MinGW, which ships the Windows GNU
    compiler, linker, archiver, and import libraries needed by Rust GNU and the
    bundled SQLite C build;
  - keep all downloads and extracted tools under the Buildkite/ctx tool cache.
- Remaining external evidence gap:
  - commit and push the LLVM-MinGW remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows LLVM-MinGW Libgcc Follow-Up

- Build 51:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/51>
- Branch/head:
  `work-record` / `c19d3952a4279823d59923d82cb738cc9f463f01`
- Outcome:
  - Windows smoke downloaded/extracted LLVM-MinGW and entered compilation;
  - FAIL: linking proc-macro build scripts failed because Rust GNU still passes
    `-lgcc` and `-lgcc_eh`, while LLVM-MinGW uses compiler-rt and does not ship
    those GCC compatibility archive names.
- Repo-owned remediation:
  - replace LLVM-MinGW with `w64devkit-x64-2.8.0.7z.exe`, a GCC-based MinGW
    package that includes `libgcc` and `libgcc_eh`;
  - wire Windows GNU `CC`, `CXX`, `AR`, and Cargo linker environment to
    `gcc.exe`, `g++.exe`, and `ar.exe` from the extracted w64devkit `bin`
    directory.
- Remaining external evidence gap:
  - commit and push the w64devkit remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows w64devkit Extraction Follow-Up

- Build 53:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/53>
- Branch/head:
  `work-record` / `d5b232ba5ada9d93abc2af8c877cb13629a2d7ab`
- Outcome:
  - Windows smoke downloaded `w64devkit-x64-2.8.0.7z.exe` successfully;
  - FAIL: extraction completed with exit code 0, but the script only searched
    child directories for `bin\gcc.exe`; if the archive extracts `bin` at the
    extraction root, discovery misses it.
- Repo-owned remediation:
  - include the extraction root itself in the w64devkit compiler discovery
    candidates before searching nested directories.
- Remaining external evidence gap:
  - commit and push the extraction discovery remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows 7zr Extraction Follow-Up

- Build 55:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/55>
- Branch/head:
  `work-record` / `0c2f2232d8f187e1a1005b5725fbceefae946b1f`
- Outcome:
  - Windows smoke still failed during w64devkit extraction/discovery; the
    self-extracting archive path exited successfully but did not produce a
    discoverable `bin\gcc.exe`.
- Repo-owned remediation:
  - download the official `7zr.exe` console extractor into the same tool cache;
  - use `7zr.exe x` to extract the w64devkit self-extracting archive explicitly
    before compiler discovery.
- Remaining external evidence gap:
  - commit and push the 7zr extraction remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.

## 2026-06-23 Buildkite Windows w64devkit 7zr Follow-Up

- Build 55:
  <https://buildkite.com/luca-king/ctx-public-release-verification/builds/55>
- Branch/head:
  `work-record` / `0c2f2232d8f187e1a1005b5725fbceefae946b1f`
- Outcome:
  - Windows smoke reused the cached `w64devkit-x64-2.8.0.7z.exe`;
  - FAIL: executing the self-extracting archive still produced no detectable
    `bin\gcc.exe` under the extraction directory, so the issue is extraction,
    not only compiler discovery.
- Repo-owned remediation:
  - download the standalone `7zr.exe` extractor into the same Buildkite/ctx
    tool cache;
  - extract `w64devkit-x64-2.8.0.7z.exe` with `7zr x ... -y` before compiler
    discovery.
- Remaining external evidence gap:
  - commit and push the 7zr extraction remediation;
  - trigger and monitor a fresh public Buildkite run proving Windows smoke and
    Windows release dry-run.
