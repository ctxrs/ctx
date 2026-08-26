# Testing Taxonomy

Public verification has three graph-discovered modes. All normal commands use
repository-owned wrappers; direct `bazel` and broad raw Cargo commands are not
routine entry points.

## Modes

| Mode | Purpose |
| --- | --- |
| `ci` | Merge gate: native clippy plus deterministic format, policy, SDK, unit, contract, bounded integration, packaging, and content checks. |
| `nightly` | `ci` plus performance sanity, serialized upgrade acceptance, persistent-daemon soak, and injected crash/ENOSPC qualification. |
| `release` | `nightly`, named explicitly for release-candidate qualification. |

Each named mode first builds `//...` with `--config=ci`, whose checked-in
configuration inherits the strict Clippy aspect with `-Dwarnings`. It then
discovers tests from `//...` and runs them with the deterministic test
configuration, without applying the lint aspect a second time.

An untagged test runs in `ci`, `nightly`, and `release` by default.
`tier-nightly` moves an expensive test out of `ci`; `tier-release` moves a test
out of both `ci` and `nightly`; and Bazel's standard `manual` tag excludes an
explicit operation or harness from all three modes. Tier tags belong on leaf
tests, are mutually exclusive, and are checked against the live graph. There
is no maintained allowlist of ordinary tests, so a newly added untagged test
cannot be orphaned from CI.
Release mode additionally uses the exact-clean-candidate Rust crate-size
preflight.

During editing, run the smallest owning test and then the affected selector.
Build-graph changes, unresolved comparison bases, selector failures, and
unmapped changes fail closed to the complete default-CI graph;
the full merge gate remains `scripts/check.sh --mode=ci`. Use `nightly` or
`release` for performance sanity and the serialized upgrade, daemon-soak, and
fault-injection qualification that is too expensive for each source change.
Network-dependent, external, platform-native, and manual checks remain separate.

## Commands

```bash
scripts/check.sh --mode=ci
scripts/check.sh --mode=nightly
scripts/check.sh --mode=release
scripts/check.sh --mode=ci --force-rerun
scripts/bazel-affected.sh origin/main
```

`--force-rerun` passes `--cache_test_results=no` only to Bazel test actions;
shared compilation and repository caches remain available.

Use direct Bazel targets when a narrower check is enough:

```bash
scripts/bazelw test //:docs_check --config=test
scripts/bazelw test //crates/ctx-cli:native_providers_tests --config=test
scripts/bazelw test //sdks/go:go_sdk_tests --config=test
scripts/bazelw test //:package_audit_release --config=release
```

Routine labels must be real Bazel tests or test suites. `bazel run` remains
appropriate for a generator or explicit operation, but not for an ordinary
check whose successful result should be reusable.

All default public tests must be hermetic. They must not require API keys,
network access, provider accounts, hidden model calls, or writes into source
repositories.

## Cache reuse and diagnostics

Bazel reuses a passing result when its declared source, graph, configuration,
toolchain, platform, and target inputs are identical. Do not force reruns merely
to make a test execute again. Rerun when an input differs, the affected selector
fails or cannot classify a change, or a flake is being investigated.

The checked-in CI configuration deliberately does not inherit per-job CI
identifiers into every test action. Tests that need CI-shaped inputs must
declare stable values or fixtures locally; otherwise each Buildkite job would
invalidate the entire test cache.

When comparison with Cargo is necessary to diagnose Bazel parity, use
`scripts/cargo-diagnostic.sh <cargo arguments...>`. It bounds build jobs to one
quarter of available CPUs (maximum eight), bounds default Rust test threads to
four, and uses `debug=0` unless `CTX_CARGO_DIAGNOSTIC_DEBUG=1`. Cargo diagnostic
output does not replace the owning Bazel test.

`scripts/cargo-fixit.sh` is the only supported mutating compiler-repair path;
the strict Bazel Clippy aspect remains the read-only merge gate.
`scripts/cargo-shear.sh` runs the pinned offline dependency-hygiene test
`//:cargo_shear_check`, which is part of routine CI.

## Upgrade Compatibility

Importer identity, cursor, dedupe-key, and source-root changes must be tested as
upgrades, not only as fresh imports. A regression test for such a change starts
from the oldest relevant stored record shape, opens it through the current
schema migrations, changes or appends provider content, and imports again. It
must prove that logical session and event counts remain stable, existing ctx IDs
still resolve, genuinely new events are retained once, and cross-source
sessions remain distinct.

Keep these upgrade fixtures sanitized and hermetic. When a release changes a
stored identity input, add the compatibility test in the same change; a
fresh-home idempotency test alone cannot prove migration safety.

When identity code is shared across providers, cover each distinct historical
storage shape established by the compatibility audit rather than duplicating
the same test for every provider label.
