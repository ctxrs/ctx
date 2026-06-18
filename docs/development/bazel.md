# Bazel And Buildkite Gates

The unified ctx repository exposes repo-level Bazel validation gates from the
workspace root while the source tree still lives under `core/`.

Buildkite is the repository CI entrypoint. The checked-in pipeline at
`.buildkite/pipeline.yml` calls these same repo-level Bazel labels through
`.buildkite/run-bazel.sh`, which bootstraps pnpm dependencies and then invokes
the lockfile-pinned Bazelisk binary from `core/node_modules`.

GitHub Actions workflows are intentionally not used for this repository. The
root `//:buildkite_config_test` target fails if `.github/workflows` contains
workflow files or if the Buildkite pipeline stops naming the required gates.
When `buildkite-agent` is installed, that same guard also runs
`buildkite-agent pipeline upload --dry-run --reject-secrets` so Buildkite's own
pipeline parser validates the checked-in YAML before heavier Bazel work starts.

The Buildkite order follows the same shift-left taxonomy used by the original
ctx monorepo: source/config contracts first, build-graph analysis second,
source-family gates next, browser E2E after web/unit confidence, and release
artifact proof at the tail.

Run these labels from the repository root:

```bash
bazel test //:presubmit
bazel test //:all-rust
bazel test //:all-web
bazel test //:schemas
bazel test //:e2e-premerge
bazel test //:release
bazel build //:release-artifacts
```

Gate contents:

- `//:presubmit` delegates to `//core:presubmit` and runs the current Rust
  unit/doc/bin validation surface, web lint/typecheck/unit checks, schema
  checks, the required premerge web E2E suite, and the Buildkite configuration
  test.
- `//:all-rust` delegates to `//core:all-rust` and adds the existing
  `ctx-http` integration suites to the Rust validation surface.
- `//:all-web` delegates to `//core:all-web` and runs web lint, typecheck,
  Playwright runtime-script tests, Vitest shards, and package unit tests.
- `//:schemas` delegates to `//core:schemas` and runs the checked-in schema and
  generated-binding validation targets that currently exist.
- `//:e2e-premerge` delegates to `//core:e2e-premerge` and runs the required
  premerge Playwright suite.
- `//:release` delegates to `//core:release` and runs `presubmit` plus the
  required release Playwright suite.
- `//:release-artifacts` delegates to `//core:release-artifacts` and builds the
  currently wired release artifact targets.

These are compatibility gates around the current layout. They intentionally do
not flatten `core/` into the repository root.

The gates only reference targets that have BUILD coverage today. Rust crates
under `core/crates/` without `BUILD.bazel` files are not included until those
packages gain Bazel targets.

Local execution requires a Bazel launcher compatible with `.bazelversion`.
