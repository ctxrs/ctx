# FreeBSD Release Worker

FreeBSD is a first-class release target for ctx. It is not out of scope and is
not optional for a production release.

## Required Worker

- Buildkite queue label: `freebsd-x64`
- Target triple: `x86_64-unknown-freebsd`
- Required tools: Bash, Git, Rust stable, Cargo, and `sha256sum`, `shasum`, or
  FreeBSD `sha256`
- Expected lane shape: build the ctx CLI on the native worker, write the release
  dry-run manifest, run packaged artifact runtime smoke, and export the
  artifact, checksum, and smoke evidence

## Buildkite Lane

The public Buildkite pipeline contains `freebsd-native-release-proof`, routed to
`queue=freebsd-x64` with `os=freebsd` and `arch=x86_64` agent tags. The lane
fails closed unless `rustc -vV` reports `host: x86_64-unknown-freebsd`.

The lane runs native cargo tests and then:

```bash
CTX_RELEASE_PLATFORM=freebsd-x64 \
CTX_RELEASE_TARGET_TRIPLE=x86_64-unknown-freebsd \
CTX_EXPECT_HOST_TRIPLE=x86_64-unknown-freebsd \
CTX_ARTIFACT_DIR=artifacts/buildkite/release-dry-run/freebsd-x64 \
./scripts/release-dry-run.sh

CTX_RELEASE_PLATFORM=freebsd-x64 \
CTX_RELEASE_TARGET_TRIPLE=x86_64-unknown-freebsd \
CTX_EXPECT_HOST_TRIPLE=x86_64-unknown-freebsd \
CTX_RELEASE_DRY_RUN_DIR=artifacts/buildkite/release-dry-run/freebsd-x64 \
CTX_ARTIFACT_DIR=artifacts/buildkite/release-artifact-smoke/freebsd-x64 \
./scripts/release-artifact-smoke.sh freebsd-x64
```

The expected evidence is:

- `artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json`
- `artifacts/buildkite/release-dry-run/freebsd-x64/ctx-release-metadata.env`
- `artifacts/buildkite/release-dry-run/freebsd-x64/checksums.sha256`
- `artifacts/buildkite/release-dry-run/freebsd-x64/ctx-0.1.0-x86_64-unknown-freebsd`
- `artifacts/buildkite/release-artifact-smoke/freebsd-x64/artifact-smoke.json`
- `artifacts/buildkite/release-artifact-smoke/freebsd-x64/artifact-smoke.md`

## Release Requirement

A production release requires `freebsd-x64` proof alongside `linux-x64`,
`macos-arm64`, `macos-x64`, and `windows-x64`. If the native worker is still
absent, the release evidence must include an explicit manager-approved release
exception that names `freebsd-x64`, explains the temporary risk, and keeps the
certificate from implying full platform proof.

## Blocker Status

The contract self-test may still generate a FreeBSD blocker fixture to prove the
certificate rejects missing platform proof. Real release evidence does not need
that blocker when the native manifest, metadata, and artifact smoke above are
present. If `queue=freebsd-x64` is not provisioned or cannot run the lane, that
is an infrastructure blocker, not release proof.
