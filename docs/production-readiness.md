# Production Readiness

Production readiness for ctx means the local search product is understandable,
bounded, and testable.

## Product Readiness

- Public docs describe only local provider import and search.
- A fresh agent can initialize storage, import supported local history, search,
  and inspect cited results from the docs.
- Provider support is truthful and machine-readable.
- Limitations are documented next to the happy path.

## Security Readiness

- Command read/write behavior is documented.
- Core setup/import/search are local operations.
- JSON output is private by default.
- Raw provider ownership is clear.
- Redaction limits are explicit.

## Contract Readiness

- CLI examples match implemented flags.
- JSON fields used by agents are documented.
- Remaining compatibility names are treated as opaque implementation details.
- Source citation and source availability semantics are documented.

## Validation Readiness

Validation readiness is mode-based. Use
[`docs/testing-taxonomy.md`](testing-taxonomy.md) as the source of truth for
which gate an agent should run.

- `fast` is for tight local feedback while implementing.
- `presubmit` is the normal handoff gate for finished work.
- `production` is the required readiness gate for search-only local behavior,
  privacy, security, provider fixtures, docs contracts, and Buildkite wiring.
- `release_contract` proves release fixtures, schemas, metadata, lane
  definitions, certificate contracts, and deterministic rejection of missing
  real release evidence. It is not proof of real release artifacts.
- `release` is reserved for produced platform artifacts, checksums, install
  verification, and release evidence.
- `provider_live`, `platform`, `perf`, `nightly`, and `manual` are explicit
  escalation modes. They do not run by default unless the selected mode says so.

For docs-only validation, use:

```bash
bash scripts/check-docs.sh
jq empty docs/provider-support-matrix.json
```

When Bazel owns the docs gate, run:

```bash
bazel test //:docs_check --config=ci
```

Do not use direct Cargo checks for docs-only validation unless the execution plan
is updated to require them through Bazel.

## Release Proof Boundary

Production readiness can include release-contract checks, but those checks are
not artifact certification. `release_contract` should fail when fixture release
evidence, metadata, certificate inputs, provider lane definitions, or required
release docs are missing or inconsistent. It should not be used to claim that
real platform binaries, checksums, or install verification exist.

`release` is the artifact-proof mode. It requires produced platform artifacts
and verification evidence. Until that evidence exists, install docs should keep
source-build instructions only and release status should remain explicitly
blocked or staging-plan-only. The `release` gate validates real evidence and
must fail when required platform artifacts, checksums, or release certificate
inputs are missing; `release_contract` asserts that strict failure mode with a
missing-evidence contract test, but remains the fixture-based CI contract gate
and is not a substitute.

## Remote Cache Posture

The default posture is local execution with local tool caches only.

- No RBE is part of this phase.
- No default remote cache is part of this phase.
- Reconsider cache-only remote caching only if Buildkite p50 exceeds 12
  minutes, Buildkite p95 exceeds 20 minutes for two consecutive weeks, or
  multi-platform release proof exceeds 25 minutes with repeated cacheable work.

Remote execution remains out of scope even if those thresholds are crossed.
