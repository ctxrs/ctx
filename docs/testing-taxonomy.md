# Testing Taxonomy

Public verification starts with fast local confidence and expands as needed.
All normal commands use repository-owned wrappers; direct `bazel` and broad raw
Cargo commands are not routine entry points.

## Modes

| Mode | Purpose |
| --- | --- |
| `fast` | Native Rust formatting/smoke, target inventory, public docs, CLI contracts, and package-surface audit. |
| `smoke` | `fast` plus a fresh-home CLI flow and basic provider fixture smoke. |
| `presubmit` | `fast` plus deterministic native Rust unit and bounded integration tests. |
| `ci` | Buildkite gate: native clippy, `presubmit`, SDK packaging, and release/content audits. |
| `nightly` | `ci` plus serialized upgrade acceptance, persistent-daemon soak, and injected crash/ENOSPC qualification. |
| `release` | The same complete qualification graph as `nightly`, named explicitly for candidate receipts. |

During editing, run the smallest owning test and then the affected selector.
Use `presubmit` when the build graph changes or selection is uncertain, and
`ci` for the ordinary hosted public check. Use `nightly` or `release` for the
serialized upgrade, daemon-soak, and fault-injection qualification that is too
expensive for each source change. Network-dependent, external,
platform-native, and manual checks remain separate.

## Commands

```bash
scripts/check.sh --mode=fast
scripts/check.sh --mode=smoke
scripts/check.sh --mode=presubmit
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
scripts/bazelw test //:fresh_home_e2e --config=test
scripts/bazelw test //:provider_fixture_e2e --config=test
scripts/bazelw test //:local_transcript_oracle --config=test
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
