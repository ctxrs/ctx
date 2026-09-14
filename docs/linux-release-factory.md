# Linux release factory

The public release has one construction path. A clean Ubuntu 24.04 x86_64
checkout (direct, VM, container, or Buildkite image) builds the five supported
CLI binaries, signs the Windows binary with Azure Artifact Signing, signs and
notarizes the two macOS binaries, and emits all
checksums and release evidence into one directory. Native hosts validate those
exact bytes afterwards.

## Inputs

- A clean checkout at the requested source commit.
- Rust 1.97.1 with the five release targets installed.
- A complete macOS SDK directory or archive supplied with `--macos-sdk` or
  `CTX_MACOS_SDK_ROOT`. The SDK is an unredistributed private build input.
- Offline OSV scanner/database inputs in official mode.
- Java 11 or newer for the checksum-pinned Jsign client and the Linux
  Authenticode verifier. The verifier is always compiled in Java 11 source
  mode, including on newer factory JDKs.
- Infisical access at the narrow macOS and Windows signing boundaries in
  official mode. Build and dependency subprocesses never receive Apple or
  Azure signing credentials.

The host contract is recorded in `contracts/release-factory-inputs-v1.json`:
Ubuntu 24.04, x86_64, authority
`ctx-release-factory-ubuntu24-x86_64-v1`. A container or VM is optional; it is
useful when the caller is not already on Ubuntu 24.04, but it does not create a
different construction path.

Zig 0.15.2, rcodesign 0.29.0, and Jsign 7.5 are downloaded from
checksum-pinned upstream archives into `target/release-toolchain`.
cargo-zigbuild 0.23.0 must already be installed;
`CTX_RELEASE_ALLOW_TOOL_INSTALL=1` permits the factory to install that pinned
version. Jsign calls the `ctxsignkimmy/ctx-public-release` Public Trust profile
directly from Linux and records exact-byte Authenticode evidence beside
`ctx.exe` before any factory checksum or manifest is written.

## Commands

An official candidate:

```bash
scripts/release/build-public-candidate-on-linux.sh \
  --source-commit "$(git rev-parse HEAD)" \
  --macos-sdk /private/path/MacOSX.sdk.tar.gz
```

An unsigned, non-promotable local diagnostic:

```bash
scripts/release/build-public-candidate-on-linux.sh \
  --source-commit "$(git rev-parse HEAD)" \
  --macos-sdk /private/path/MacOSX.sdk.tar.gz \
  --diagnostic-unsigned
```

The factory refuses a dirty checkout, an unsupported official host, an
unpinned toolchain, a missing or unexpected SDK archive, or missing official
credentials. It builds targets with bounded parallelism; use `--jobs` and
`--build-parallelism` to tune a local machine.

## Native validation

Buildkite's five native jobs download artifacts from
`public-cli-linux-factory` and invoke:

```bash
scripts/validate-public-cli-factory-artifact.sh \
  PLATFORM target/public-cli-artifacts target/public-cli-native-smoke/PLATFORM
```

This is an exact three-argument, Core-only interface. Companion binaries and
managed-pair envelopes are not inputs to the public factory validator or its
five native lanes. The validator checks the factory checksum, executes the
native candidate, and checks that validation did not mutate it. macOS
additionally performs strict native codesign verification. Windows requires
`Get-AuthenticodeSignature` to accept the exact factory bytes and match the
factory certificate evidence. Semantic runtime smokes run in their independent
graph and do not consume Core factory artifacts. Only after all five Core jobs
pass does the staging job assemble GitHub assets.

Bazel remains available for hermetic development and qualification checks. It
does not construct or publish public CLI candidates; the Linux factory is the
single public construction authority.
