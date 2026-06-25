# Testing Taxonomy

ctx validation modes are named by the decision they support. Agents should pick
the smallest mode that answers the question in front of them, then escalate when
the change touches wider contracts, release proof, platforms, provider import,
or performance.

The product boundary does not expand in any mode: ctx remains a search-only,
local CLI for existing local agent history. Setup, import, search, context,
doctor, and validate must not require API keys or product network calls.

## Modes

| Mode | Purpose | When agents should run it |
| --- | --- | --- |
| `fast` | Tight local feedback for ordinary implementation work. | Run during development after narrow code, script, or docs changes when a quick signal is enough before continuing. |
| `presubmit` | Default pre-merge confidence for normal changes. | Run before handing off a complete change that affects product behavior, docs contracts, scripts, or tests. |
| `production` | Required production gate for the search-only local product. | Run for changes that affect CLI behavior, storage, providers, privacy, security, Buildkite, release gating, or any shared validation path. |
| `release_contract` | Non-publishing release contract proof using fixtures, schemas, generated metadata, certificate self-tests, and missing-evidence rejection checks. | Run when release scripts, release docs, certificate logic, metadata formats, provider lane definitions, or release-gate wiring changes. |
| `release` | Real release artifact proof. | Run only when actual platform artifacts, checksums, install verification, and release evidence are being produced or certified. It is stronger than `release_contract`, not an alias for it. |
| `nightly` | Broad scheduled validation outside the critical development path. | Run from scheduled automation or by request when slow checks are useful but should not block ordinary iteration. |
| `perf` | Performance benchmarks and regression checks. | Run before accepting search, indexing, storage, ranking, or dependency changes that could materially alter runtime, memory, or index size. |
| `provider_live` | Opt-in proof that ctx can import provider history and retrieve search/context from it. | Run manually when provider import behavior changes, explicit local history paths are available, or credential-gated generated histories are requested. It must use redacted aggregate artifacts only and must not execute provider CLIs. |
| `platform` | Operating-system and install proof beyond the default Linux gate. | Run before claiming support for platform-specific packaging, install, shell, filesystem, or worker behavior. |
| `manual` | Explicitly selected checks requiring local resources, external workers, or human review. | Run only when a mode or target says it is manual. Keep it out of default wildcard, production, and release-contract runs unless explicitly requested. |

Canonical Bazel suites should use `//:<mode>` names. Compatibility aliases may
exist for older callers: `//:production_hardening` for `//:production`,
`//:release_candidate` for `//:release_contract`, and `//:manual_external` for
`//:manual`. Agents should prefer the canonical names once they are present.

Script entry points should use matching check modes, for example
`./scripts/check.sh --mode=production`, with extra Bazel arguments after `--`
when needed.

## Escalation Rules

- Docs-only changes should run the docs check, and should escalate to
  `presubmit` when examples, CLI flags, support claims, or release/security
  contracts change.
- CLI, storage, provider parser, search ranking, privacy, or security changes
  should run `production`.
- Release-certificate, release-metadata, R2 layout, platform-blocker, or
  provider-lane-definition changes should run `release_contract`.
- Claims about produced binaries, checksums, install commands, or platform
  availability require `release` artifact proof.
- Provider-live validation is never implied by `production` or
  `release_contract`; it requires explicit local-history or generated-history
  opt-in.
- Generated OpenRouter provider-live validation may use runner credential and
  endpoint configuration before import to create temporary synthetic histories.
  The credential must not be passed to `ctx`, generated raw histories must not
  be published as artifacts, and setup, import, search, context, status, doctor, and validate
  remain local filesystem operations with no product network dependency.
- Performance-sensitive changes should add `perf` to the normal gate instead of
  replacing correctness checks.
- The search performance gate is manual and non-default. Run
  `bazel test //:search_perf_bench --config=ci --test_output=all` when search,
  context, storage, import, or indexing changes need measured evidence. It
  builds a synthetic local corpus with at least 10k provider events, records
  import/search/context timings, p50/p95 samples, SQLite footprint bytes, and
  threshold pass/fail checks in `synthetic-search-perf.json`.
- For explicit slow evidence, run the same target with
  `--test_env=CTX_SEARCH_PERF_SLOW=1` or
  `--test_env=CTX_SEARCH_PERF_EVENTS=100000`. Slow-mode evidence is not part of
  default CI and should be requested only when the extra runtime is useful.

## Release Proof Classes

`release_contract` proves that release contracts are internally consistent. It
can validate fixture evidence trees, generated manifests, schema fields,
certificate behavior, lane definitions, documentation requirements, and the
truthfulness contract that real release evidence is rejected when required
artifacts, checksums, platform install evidence, or FreeBSD proof/manager
exception evidence are missing. It does not prove that final platform artifacts
exist, that checksums match real downloads, or that install verification passed
on target platforms.

`release` is the real artifact proof class. It must be backed by produced
artifacts, checksums, platform install verification, and the evidence needed to
certify those artifacts. A passing `release_contract` run is necessary release
hygiene, but it must not be described as a completed release. The `//:release`
gate therefore includes the release-contract suite and a real evidence
certificate verifier. If the required external platform artifacts or evidence
tree are absent, `//:release` must fail with missing-evidence errors instead of
falling back to contract fixtures.

## Remote Cache Posture

This phase uses local execution by default.

- Do not introduce RBE.
- Do not configure a remote cache by default.
- Prefer local Bazel and tool caches that stay within the CI worker or
  developer machine.
- Reconsider cache-only remote caching only if Buildkite p50 exceeds 12
  minutes, Buildkite p95 exceeds 20 minutes for two consecutive weeks, or
  multi-platform release proof exceeds 25 minutes because of repeated
  cacheable work.

If those thresholds are met, the first reconsideration should be cache-only:
remote execution remains out of scope. Any cache-only proposal needs explicit
secret handling, cache key, retention, opt-out, and measured benefit notes
before it becomes a default.
