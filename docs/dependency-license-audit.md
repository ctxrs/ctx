# Dependency License Audit

Before cutting an external release, generate a dependency license report from
the exact lockfile used for the release build and review any new licenses.

The executable lane is `scripts/release-supply-chain-proof.sh`. It writes
`artifacts/buildkite/supply-chain/dependency-advisory-license-audit.json` with
the `Cargo.lock` checksum, `cargo metadata --locked` package inventory, license
metadata coverage, and advisory-audit status.

If `cargo audit` or equivalent approved advisory evidence is unavailable, the
artifact must record `blocked_manual_required` and `manager_approval_required`;
that is a truthful blocker, not a pass. The release completion certificate
requires this artifact so a real release evidence tree fails when dependency
advisory/license evidence is missing.
