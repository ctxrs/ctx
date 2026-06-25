# Release Supply Chain

ctx release evidence is split into classes so CI output is not mistaken for a
public release approval.

## Evidence Classes

Contract fixture evidence is generated for script self-tests. It uses fake
artifacts with real checksums and must set `self_test_fixture=true` plus
`evidence_class=contract_fixture`. It can prove that the certificate verifier
works, but it cannot approve a release.

Host artifact dry-run evidence is produced on one runner from a local release
build. It proves that the runner can build a ctx binary, write a manifest, and
record checksums. It is not multi-platform release proof.

Multi-platform artifact proof requires separate evidence for each install
target. A production release requires proof for `linux-x64`, `macos-arm64`,
`macos-x64`, `windows-x64`, and `freebsd-x64`, or an explicit
manager-approved release exception that names the missing target and reason.
Each platform proof must include both the staged artifact manifest and a
packaged artifact runtime smoke result. The smoke installs or extracts the
exact staged artifact into a temporary bin directory, then runs `ctx --version`,
`ctx setup`, `ctx import`, `ctx search`, `ctx doctor`, and `ctx validate`
against the checked-in fixture data.

FreeBSD is a first-class release target, not an optional stretch target. The
public Buildkite pipeline includes a native `freebsd-x64` lane that builds and
tests ctx on FreeBSD, writes the dry-run manifest, runs packaged artifact
runtime smoke, and exports artifact, checksum, and smoke evidence for
`x86_64-unknown-freebsd`. Contract self-tests may still emit a `freebsd-x64`
blocker fixture, but real release evidence does not need a manager-approved
release exception when the native manifest, metadata, and artifact smoke are
present.

R2 staging evidence proves only that the object layout and upload plan are
well-formed. Normal CI does not upload objects, move channels, or expose public
install instructions.

Dependency advisory/license evidence is written by
`scripts/release-supply-chain-proof.sh`. The script inventories `Cargo.lock`
with `cargo metadata --locked`, checks that every package declares license
metadata, and records whether advisory proof came from `cargo audit` or a
manager-supplied audit artifact. If advisory tooling is unavailable, the JSON
must say `blocked_manual_required`; it must not claim a pass.

SBOM, provenance, signature, and notarization evidence use the same script and
the same rule: generated artifacts may be supplied through explicit environment
paths, otherwise the evidence is a non-publishing manual blocker. A public
release cannot treat missing signing or SBOM infrastructure as success.

## Release Blockers

Signing, notarization, SBOM, and provenance are external blockers. Public
release approval requires configured credentials, approved policy, generated
artifacts, and verification instructions for each item.

Dependency advisory proof, license inventory review, SBOM publication,
provenance publication, signature verification, notarization where applicable,
and R2 upload/readback proof are required-before-public-release evidence.
Contract fixtures and blocked manual lanes are useful for CI shape checks, but
they do not approve a release.

The completion certificate remains non-publishing until all blockers are
replaced by explicit pass evidence or by a manager-approved release exception
that is recorded in the release evidence. A contract fixture certificate must
never be used as approval for public artifacts.
