# Building and releasing ctx

A public checkout builds the complete ctx CLI, including Blame. No companion,
private repository, account, activation or production credential is needed for
normal source/package builds. Rust 1.95 or newer is required; the retained
release toolchain is Rust 1.97.1. Workspace Cargo and Bazel builds include the
same default product.

```sh
cargo build --locked --release -p ctx --bin ctx
cargo install --path crates/ctx-cli --locked
scripts/bazelw build //crates/ctx-cli:ctx --config=release
```

Use the native compiler and platform development tools for your host. The five
distribution targets are Linux x64/arm64, macOS arm64/x64 and Windows x64.
Windows release construction retains `x86_64-pc-windows-gnu`; a local MSVC
build is not the signed release artifact.

## macOS: use an installed Apple SDK

Install Xcode or Apple's Command Line Tools through Apple's normal distribution
and accept their license. No ctx SDK archive or signing identity is required
for a native source build. Use the SDK selected by the installed developer
toolchain:

```sh
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export MACOSX_DEPLOYMENT_TARGET=13.0
# Run on the corresponding host; both target names are supported.
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo +1.97.1 build --locked --release -p ctx --bin ctx \
  --target aarch64-apple-darwin
# On an Intel Mac:
cargo +1.97.1 build --locked --release -p ctx --bin ctx \
  --target x86_64-apple-darwin
```

The source and build recipes are public; Apple supplies the licensed compiler
and SDK. Do not publish Apple's SDK archive. The signed Linux cross factory
additionally pins one SDK transport in `contracts/release-factory-inputs-v1.json`.
That transport is an optional cross-construction input, not a requirement for
the installed-SDK native route above. Native builds do not claim byte identity
with the cross factory or carry a release signature.

## Signed five-target factory

`scripts/release/build-public-candidate-on-linux.sh` builds all five binaries
from one clean public commit on Ubuntu 24.04 x86_64. It retains the Linux glibc
2.28 ceiling, macOS deployment target 13.0, and Windows GNU ABI. It signs and
notarizes both macOS artifacts and applies Azure Authenticode signing to Windows
before computing final checksums, candidate manifests, SBOMs and notices.

Required tools are the pinned Rust toolchain and target libraries,
cargo-zigbuild 0.23.0, Java 11+, LLVM tools and the tools listed by the factory.
Zig 0.15.2, rcodesign 0.29.0 and Jsign 7.5 come from checksum-pinned public
upstreams. Official construction additionally uses an offline OSV database and
scanner and the licensed SDK archive whose size/hash match the public contract.
Signing credentials are injected only at existing platform-signing boundaries.
They are not inherited by Cargo or dependency build scripts.

Place compilation, SDK extraction and scratch on a disk with enough free space.
`--work-dir` moves those outputs and the default tool cache; `--output-dir`
selects the final candidate. Set `CARGO_HOME` and `RUSTUP_HOME` to appropriately
provisioned disk caches when the default home filesystem is constrained.

```sh
scripts/release/build-public-candidate-on-linux.sh \
  --source-commit "$(git rev-parse HEAD)" \
  --work-dir /path/on/build-disk/ctx-factory \
  --output-dir /path/on/build-disk/ctx-candidate \
  --macos-sdk /path/to/licensed/SDK.archive
```

`--diagnostic-unsigned` constructs selected local diagnostics without signing
credentials; it never emits a promotable completion. `--targets linux-x64` or
another subset avoids SDK/signing requirements for unselected platforms. Use
`--jobs` and `--build-parallelism` to bound compilation. Final executables must
fit the 128 MiB limit of existing updaters.

## Construction, checks and packaging

The public Buildkite artifact matrix first runs normal CI using
`scripts/buildkite-public-ci.sh --mode=ci`, then the single public factory.
Normal CI emits `normal-ci.json` only after actual checks succeed for the source
commit. The factory does not depend on a private product job or source pin.

Staging uses `scripts/stage-github-release-assets.sh`. The selected policy
`CTX_RELEASE_VALIDATION_POLICY=factory-only-human-override-v1` requires normal CI,
sealed candidate identity, all platform signatures/notarization and complete
SBOM/notices while recording nightly, release-tier and unselected native
execution as `not_run`. It never creates passing execution receipts for omitted
checks. The `native-receipts-required-v1` selection additionally consumes the
five actual exact-byte native receipts. Set `CTX_RELEASE_CI_RECEIPT` to the
source-bound normal-CI receipt. Buildkite selects native receipts only when its
native smoke matrix is explicitly enabled.

`scripts/assemble-github-release-assets.sh` combines the staged executable set
with five ONNX Runtime transports. Supply `CTX_RELEASE_AUTHORITY_DIR` and the
reviewed `CTX_RELEASE_HANDOFF_SHA256`. Assembly verifies the source/artifact
handoff, runtime digests and macOS runtime signatures. The same validation
policy controls whether actual macOS CLI/runtime execution receipts are
required. It preserves 21 GitHub files: five executables, five SBOMs, five notice
files, five runtime archives and `SHA256SUMS`. A separate assembly receipt binds
those final digests and records actual coverage. The full nine-archive Semantic
handoff remains part of hosted publication.

Public publication helpers live in `scripts/release/`. Metadata preparation and
package construction need no production service credentials. Signing and upload
are later operations with injected authority; immutable object verification
precedes advancing the current release pointer. Legacy signed envelopes project
the same unified executable into both historical filenames for incoming older
updaters. Both source fields identify the same public commit. New installations
and updates use one executable. The original v1/1.3.2 feed remains frozen.

To prepare the retained older-client projection, author the existing six-field
`ctx-managed-pair-release-candidate` JSON with the release name, stable channel,
legacy matrix SHA-256 and the next reviewed rollback generation. Then run:

```sh
node scripts/release/release-manifest.mjs \
  --candidate /path/to/candidate.json \
  --factory-dir /path/to/ctx-candidate \
  --candidate-manifest-handoff /path/to/github-release-authority \
  --candidate-handoff-sha256 REVIEWED_HANDOFF_SHA256 \
  --target-matrix contracts/release-targets-v1.json \
  --output-dir /path/to/new-projection --preflight-only
```

At signing time, omit `--preflight-only` and supply the release key through
stdin in an isolated signer environment. The existing public helper writes the
same executable to both historical content-addressed filenames and signs the
unchanged envelope schema. No private commit is an input.

The retained `stage-runtime-transport-handoff.mjs` verifies and stages five
runtime transports against that publication. The
`publish-hosted-managed-pair-stable.sh --preflight-only` route then validates the
projection, runtime handoff, all nine Semantic archives and independently bound
candidate handoff before preparing metadata. Its ordinary publishing route
signs metadata and uploads/readbacks immutable objects before changing v2's
pointer. Credentials may be injected as the existing `CTX_RELEASE_R2_*` and
metadata-key variables; the operational Infisical lookup is only a fallback
at that later boundary. Neither service authentication nor signing keys are
needed to compile or package ctx locally.

After publication, `scripts/release/release-contract.sh` checks signed metadata,
public source identity, candidate digests and hosted artifact readbacks. Its
receipt explicitly leaves native installer execution and stock-client upgrade
proof as `not_run`; those require their own real executions. The fixed stable
URL proof with an unmodified 1.4 client happens only after pointer publication.
