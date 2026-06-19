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
