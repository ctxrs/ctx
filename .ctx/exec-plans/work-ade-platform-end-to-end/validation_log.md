# Validation Log

Record commands, timestamps, exit status, resource caps, and notable warnings.

## Baseline Already Observed Before This Plan

- Web typecheck, lint, test, and build passed on the current branch before this
  expanded plan was written.
- Buildkite/Bazel shifted-left schema/config tests passed on the current branch
  before this expanded plan was written.
- Full Rust workspace tests passed through `core/scripts/dev/cargo-safe.sh` with
  memory/job/thread caps before this expanded plan was written.

These baseline results must be rerun after subsequent implementation phases.

## Phase 0 Focused Validation

- After `5dc809d`:
  - `pnpm -C core/apps/web test -- src/pages/workbenchShell/WorkbenchPageShellView.test.tsx src/pages/workbenchShell/WorkbenchTemplates.test.tsx src/workbench/persistence.test.ts src/workbench/store.template.test.ts src/utils/workbenchStoreLayout.test.ts src/pages/workbenchShell/agentWorkProjection.test.ts`
  - Result: passed, 6 files / 32 tests.
- After `729d953`:
  - `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-store agent_work`
  - Result: passed, 11 tests.
- After `399b29e`:
  - `scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-daemon duplicate_plugin_ids_are_load_errors_and_not_registered`
  - Result: passed, 1 test. Existing daemon warnings remained warnings only.
- After `6a36194`:
  - `bash -n core/scripts/dev/cargo-safe.sh core/scripts/dev/check-local.sh`
  - Result: passed.

## Workbench Template Visual Slice

- Before commit:
  - `CTX_E2E_BROWSER=chromium CTX_E2E_BROWSER_CHANNEL=chrome CTX_E2E_DISABLE_VIDEO=1 pnpm -C core/apps/web exec playwright test -c playwright.visual.config.ts e2e/visual-workbench-templates.spec.ts --grep "Classic template desktop-wide"`
  - Result: passed, 1 test. Used system Chrome because this host lacks the
    Playwright-managed WebKit browser for this Ubuntu image; disabled video
    because the matching Playwright-managed ffmpeg binary is also absent.
- Before commit:
  - `CTX_E2E_BROWSER=chromium CTX_E2E_BROWSER_CHANNEL=chrome CTX_E2E_DISABLE_VIDEO=1 pnpm -C core/apps/web exec playwright test -c playwright.visual.config.ts e2e/visual-workbench-templates.spec.ts`
  - Result: passed, 14 tests / 39.8s, one Playwright worker.
- Before commit:
  - `pnpm -C core/apps/web test -- src/pages/workbenchShell/WorkbenchPageShellView.test.tsx src/pages/workbenchShell/WorkbenchTemplates.test.tsx src/workbench/persistence.test.ts src/workbench/store.template.test.ts src/utils/workbenchStoreLayout.test.ts src/pages/workbenchShell/agentWorkProjection.test.ts`
  - Result: passed, 6 files / 32 tests.
- Before commit:
  - `pnpm -C core/apps/web typecheck`
  - Result: passed.

## Plugin SDK Slice

- Before commit:
  - `pnpm -C core install --lockfile-only`
  - Result: passed. Lockfile-only metadata refresh; no package tree install.
    Existing deprecation warnings from transitive dependencies remained
    warnings only.
- Before commit:
  - `pnpm -C core/packages/plugin-sdk test`
  - Result: passed, 10 Node tests. This command built the package with `tsc -p
    tsconfig.json` and ran tests against generated `dist` output.
- Before commit:
  - `pnpm -C core/packages/plugin-sdk typecheck`
  - Result: passed.
- Before commit:
  - `.buildkite/run-bazel.sh test //core/packages/plugin-sdk:unit_tests //core/packages/plugin-sdk:typecheck`
  - Result: passed, 2 Bazel targets. Initial failures exposed missing
    repo-level `tsconfig.json` runfiles wiring; fixed by adding a root
    `tsconfig_json` `js_library`.

## Work CLI Slice

- Before commit:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http agent_work_cli`
  - Result: failed after compiling all `ctx-http` test targets; 11 passed / 1
    failed in the `ctx` binary unit tests. Failure was a case-sensitive
    assertion in the new import-stub diagnostic test, not a product failure.
- Before commit:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http --bin ctx agent_work_cli`
  - Result: passed, 12 tests. Conservative cap: MemoryMax=24G, one cargo job,
    one test thread.
- Before commit:
  - `.buildkite/run-bazel.sh test //core/crates/ctx-http:bin_tests_root_help //core/crates/ctx-http:bin_tests_agent_work_help //core/crates/ctx-http:bin_tests_agent_work_schema`
  - Result: failed to build because the Bazel `ctx` binary target was missing
    its explicit `@crates//:serde_json` dependency. Cargo had already passed
    because the Cargo manifest had the dependency.
- Before commit:
  - `.buildkite/run-bazel.sh test --jobs=2 //core/crates/ctx-http:bin_tests_root_help //core/crates/ctx-http:bin_tests_agent_work_help //core/crates/ctx-http:bin_tests_agent_work_schema`
  - Result: passed, 3 Bazel smoke targets. The rerun used `--jobs=2` to reduce
    local memory pressure.

## Work CLI Review-Hardening Slice

- Before commit:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http --bin ctx agent_work_cli`
  - Result: passed, 16 tests. Conservative cap: MemoryMax=24G, one cargo job,
    one test thread.
- Before commit:
  - `.buildkite/run-bazel.sh test --jobs=2 //core/crates/ctx-http:bin_tests_root_help //core/crates/ctx-http:bin_tests_agent_work_help //core/crates/ctx-http:bin_tests_agent_work_schema`
  - Result: passed, 3 Bazel smoke targets. The rerun used `--jobs=2` to reduce
    local memory pressure.

## Store-Backed Work CLI Slice

- Before commit:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http --bin ctx agent_work_cli`
  - Result: passed, 17 tests. Conservative cap: MemoryMax=24G, one cargo job,
    one test thread.
- Before commit:
  - `.buildkite/run-bazel.sh test --jobs=2 //core/crates/ctx-http:bin_tests_root_help //core/crates/ctx-http:bin_tests_agent_work_help //core/crates/ctx-http:bin_tests_agent_work_schema`
  - Result: passed, 3 Bazel smoke targets. First run exposed missing explicit
    deps on the Bazel `ctx` binary target for `ctx-store`, `ctx-http-auth`,
    `directories`, `serde`, and `uuid`; fixed before rerun.

## Plugin Contribution Collision Slice

- Before commit:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-daemon duplicate_`
  - Result: passed, 6 tests. This covered the new duplicate provider,
    command, and UI surface tests plus existing duplicate-name tests. Existing
    daemon unused-import/dead-code warnings remained warnings only.
- Before commit:
  - `.buildkite/run-bazel.sh test --jobs=2 //core/crates/ctx-daemon:unit_tests_daemon`
  - Result: passed, 1 Bazel daemon unit target. Existing daemon warnings
    remained warnings only.

## Plugin Last-Good Reload Slice

- Worker validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-daemon daemon::plugins::tests::`
  - Result: passed, 25 tests. Conservative cap: MemoryMax=24G, one cargo job,
    one test thread. Existing unrelated daemon warnings remained warnings only.
- Follow-up worker validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-daemon invalid_last_good_reload`
  - Result: passed, 3 tests. This added explicit duplicate runtime ID coverage.
- Manager validation:
  - `cargo fmt --manifest-path core/Cargo.toml --all`
  - Result: passed after applying standard Rust formatting.
- Manager validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-daemon daemon::plugins::tests::`
  - Result: passed, 26 tests. Conservative cap: MemoryMax=24G, one cargo job,
    one test thread. Existing unrelated daemon warnings remained warnings only.
- Manager validation:
  - `.buildkite/run-bazel.sh test --jobs=2 //core/crates/ctx-daemon:unit_tests_daemon`
  - Result: passed, 1 Bazel daemon unit target.

## Harness Starter Hooks Slice

- Worker validation:
  - `git diff --check`
  - `git diff --check HEAD~1..HEAD`
  - Result: passed on worker branch `ctx/harness-hooks-20260619`.
- Manager integration:
  - `git cherry-pick b6ca1d93022338564f1bb8e0cf353f59d5d601ec`
  - Result: passed; integrated as `725edbf`.

## Local Cargo Safety Lock Slice

- Manager validation:
  - `bash -n core/scripts/dev/cargo-safe.sh core/scripts/dev/check-local.sh`
  - Result: passed.
- Manager validation:
  - `CTX_CARGO_USE_CGROUP=0 CTX_CARGO_LOCK_PATH=/tmp/ctx-cargo-safe-smoke.lock core/scripts/dev/cargo-safe.sh --version`
  - Result: passed; wrapper waited on the host Cargo lock and invoked Cargo
    under the low-I/O/nice runner.
- Reviewer validation:
  - `bash -n core/scripts/dev/cargo-safe.sh`
  - harmless `CTX_CARGO_BIN=/bin/true` wrapper smoke checks
  - Result: passed; reviewer found no blockers.
- Follow-up manager validation after adding the ionice availability probe:
  - `bash -n core/scripts/dev/cargo-safe.sh core/scripts/dev/check-local.sh`
  - `CTX_CARGO_USE_CGROUP=0 CTX_CARGO_LOCK_PATH=/tmp/ctx-cargo-safe-smoke.lock core/scripts/dev/cargo-safe.sh --version`
  - Result: passed.
- Manager validation:
  - `git diff --check`
  - Result: passed.

## Workbench Plugin Contribution Projection Slice

- Worker validation:
  - `pnpm --dir core/apps/web exec vitest run src/pages/workbenchShell/pluginWorkbenchContributionProjection.test.ts`
  - Result: passed, 1 file / 8 tests.
- Reviewer validation:
  - `pnpm vitest run src/pages/workbenchShell/pluginWorkbenchContributionProjection.test.ts`
  - Result: passed on worker branch.
- Manager integration:
  - `git cherry-pick 887981f01cad986df547eb3d5b7589d308af4a57 5ca66aa24da459cf7b08b87ceb778e13b65e3829`
  - Result: passed; integrated as `2174364` and `de4488f`.
- Manager validation:
  - `pnpm --dir core/apps/web exec vitest run src/pages/workbenchShell/pluginWorkbenchContributionProjection.test.ts`
  - Result: passed, 1 file / 8 tests.
- Manager validation:
  - `git diff --check HEAD~2..HEAD`
  - Result: passed.

## Transactional Work Import Slice

- Worker validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-store import_agent_work_records`
  - Result: passed, 7 tests. Covered atomic import, idempotency, provenance
    handling, rollback on later failure, and cross-workspace ID collision
    rejection.
- Worker validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-store validate_agent_work_import_records`
  - Result: passed, 1 test. Covered dry-run validation rollback after a
    successful batch.
- Worker validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http --bin ctx import`
  - Result: passed, 4 tests. Covered import round trip, workspace mismatch,
    dry-run relational validation, and rollback after a later contribution
    failure.
- Worker validation:
  - `CTX_CARGO_MEMORY_MAX_GIB=24 CTX_CARGO_JOBS=1 CTX_RUST_TEST_THREADS=1 scripts/dev/cargo-safe.sh test --manifest-path Cargo.toml --locked -p ctx-http --bin ctx capture_returns_actionable_not_implemented_diagnostic`
  - Result: passed, 1 test. Confirms capture remains diagnostic-only.
- Worker hygiene:
  - `cargo fmt --manifest-path core/Cargo.toml --all -- --check`
  - `git diff --check`
  - Result: passed.
- Reviewer:
  - Locke (`019ee1a7-a59e-7f61-9ceb-004013b408f1`) found no blockers and
    confirmed no hosted/team/sync/capture implementation entered the slice.
  - Reviewer residual gaps for dry-run relational validation and direct
    cross-workspace collision coverage were fixed before integration.
- Manager integration:
  - `git cherry-pick 68692be`
  - Result: passed; integrated as `0fd4576`.
