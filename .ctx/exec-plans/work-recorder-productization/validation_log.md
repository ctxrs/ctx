# Work Recorder Productization Validation Log

Updated: 2026-06-22T19:17:18-05:00

## 2026-06-22 Baseline Public Branch Check

- Command: `./scripts/check.sh`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head at start: `work-record` / `4c60fe8`
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets`;
  - `cargo test --workspace --all-targets`;
  - 10 CLI integration tests passed;
  - 1 report unit test passed;
  - 4 store unit tests passed.
- Notes: this is the slim public Work Recorder branch, not the prior large ADE
  workspace.

## 2026-06-22 First Integrated Slice Checks

- Command:
  `bash -n scripts/check.sh scripts/bazel-test.sh scripts/release-dry-run.sh scripts/ci-common.sh`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS
- Notes: verified shell syntax for new resource-safe scripts.

- Command: `git diff --check`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets --locked`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --all-targets --locked -- --test-threads 1`;
  - Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
    installed.
- Notes:
  - `TMPDIR=/var/tmp/ctxwr` avoided the `/tmp` pressure seen in child workers.
  - Test coverage after core-type expansion: 10 CLI integration tests, 2 core
    unit tests, 1 report unit test, and 4 store unit tests passed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS
- Artifacts:
  - `target/ctx-artifacts/release-dry-run/manifest.json`;
  - `target/ctx-artifacts/release-dry-run/checksums.sha256`;
  - `target/ctx-artifacts/release-dry-run/timings.json`.

## 2026-06-22 Foundation Re-Review Fix Checks

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `b7abdca`
- Outcome: PASS after updating the import atomicity test to expect the new
  deterministic preflight `record not found` error instead of a SQLite FK error.
- Coverage:
  - 11 CLI integration tests passed;
  - 4 core unit tests passed;
  - 9 store unit tests passed;
  - core/store doc-tests passed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `b7abdca`
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets --locked`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --all-targets --locked -- --test-threads 1`;
  - Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
    installed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh && git diff --check`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `b7abdca`
- Outcome: PASS
- Artifacts:
  - `target/ctx-artifacts/release-dry-run/manifest.json`;
  - `target/ctx-artifacts/release-dry-run/checksums.sha256`;
  - `target/ctx-artifacts/release-dry-run/timings.json`.

## 2026-06-22 Archive Payload Fix Checks

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `77d227f`
- Outcome: PASS
- Coverage:
  - 11 CLI integration tests passed;
  - 4 core unit tests passed;
  - 9 store unit tests passed;
  - core/store doc-tests passed;
  - store archive round-trip now asserts full artifact payload content survives
    export/import while evidence rows expose safe previews.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `77d227f`
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets --locked`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --all-targets --locked -- --test-threads 1`;
  - Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
    installed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh && git diff --check`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `77d227f`
- Outcome: PASS
- Artifacts:
  - `target/ctx-artifacts/release-dry-run/manifest.json`;
  - `target/ctx-artifacts/release-dry-run/checksums.sha256`;
  - `target/ctx-artifacts/release-dry-run/timings.json`.

## Environment Notes

- Root filesystem has available space; `/tmp` was comparatively full. Use
  `TMPDIR=/var/tmp/ctxwr` or another disk-backed temp root for cargo-heavy work.
- Do not run broad Cargo checks from multiple agents concurrently on this host.
  Use the repo's resource-capped scripts with low job counts and disk-backed
  temp space.

## 2026-06-22 Root CLI And Store Foundation Checks

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx --locked -- --test-threads 1`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS
- Coverage:
  - 11 CLI integration tests passed;
  - root commands covered;
  - hidden `ctx workspace ...` and `ctx work ...` compatibility aliases covered.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets --locked`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --all-targets --locked -- --test-threads 1`;
  - 11 CLI integration tests, 2 core unit tests, 1 report unit test, and 8 store
    unit tests passed;
  - Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
    installed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Outcome: PASS
- Artifacts:
  - `target/ctx-artifacts/release-dry-run/manifest.json`;
  - `target/ctx-artifacts/release-dry-run/checksums.sha256`;
  - `target/ctx-artifacts/release-dry-run/timings.json`.

Future entries must include:

- exact command;
- worktree/repo;
- start/end timestamp;
- outcome;
- failure mode if any;
- whether the command was local, Buildkite, or staging.

## 2026-06-22 Capture Spool Integration Checks

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p work-record-capture --lib -- --test-threads 1`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / capture merge with uncommitted archive-schema compatibility fix
- Outcome: PASS
- Coverage:
  - 3 capture unit tests passed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx capture -- --test-threads 1`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / capture merge with uncommitted archive-schema compatibility fix
- Outcome: PASS
- Coverage:
  - 2 capture CLI integration tests passed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all && git diff --check`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / capture merge with uncommitted archive-schema compatibility fix
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets --locked`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --all-targets --locked -- --test-threads 1`;
  - 13 CLI integration tests, 3 capture unit tests, 4 core unit tests, 1 report
    unit test, and 9 store unit tests passed;
  - Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
    installed.

## 2026-06-22 Foundation Review Fix Checks

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 cargo test -p ctx -p work-record-core -p work-record-store -- --test-threads 1`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `eb0d8f9`
- Outcome: PASS
- Coverage:
  - 11 CLI integration tests passed;
  - 3 core unit tests passed;
  - 9 store unit tests passed;
  - core/store doc-tests passed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 BAZEL_JOBS=2 ./scripts/check.sh all`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `eb0d8f9`
- Outcome: PASS
- Coverage:
  - `cargo fmt --all -- --check`;
  - `cargo check --workspace --all-targets --locked`;
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`;
  - `cargo test --workspace --all-targets --locked -- --test-threads 1`;
  - Bazel lane recorded `skipped` because neither `bazel` nor `bazelisk` is
    installed.

- Command:
  `TMPDIR=/var/tmp/ctxwr CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=1 ./scripts/release-dry-run.sh`
- Repo/worktree:
  `/home/daddy/code/ctx-multi-repo-workspace/worktrees/ctx/work-record-product`
- Branch/head:
  `work-record` / uncommitted changes on `eb0d8f9`
- Outcome: PASS
- Artifacts:
  - `target/ctx-artifacts/release-dry-run/manifest.json`;
  - `target/ctx-artifacts/release-dry-run/checksums.sha256`;
  - `target/ctx-artifacts/release-dry-run/timings.json`.
