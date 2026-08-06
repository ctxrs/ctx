# Bazel Development

Bazel is the authoritative Rust build and test graph. Cargo manifests and
`Cargo.lock` remain dependency metadata for `crate_universe`; they do not define
a second routine build path.

Use the repository-owned entry points for every normal build or test:
`scripts/bazelw` for direct Bazel commands, `scripts/check.sh` for named suites,
and `scripts/bazel-affected.sh` for affected tests. Do not invoke raw `bazel`;
that bypasses the worktree/cache policy owned by the wrapper.

## Fast Linux loop

Start with the narrowest real Bazel test that covers the change:

```bash
scripts/bazelw build //crates/ctx-cli:ctx --config=dev-linux
scripts/bazelw test //crates/ctx-history-index:unit_tests --config=dev-linux
scripts/bazelw test //crates/ctx-protocol:unit_tests //crates/ctx-sdk:unit_tests --config=dev-linux
scripts/bazelw test //crates/ctx-cli:unit_tests --config=dev-linux
```

The two Rust SDK unit-test targets above are their authoritative test path.
The Go SDK is modeled by `rules_go` targets such as
`//sdks/go:go_sdk_tests`; Bazel downloads Go 1.22.12 through its repository
cache instead of reinstalling it in the CI wrapper. The Buildkite wrapper uses
`.buildkite-cache/bazel-repository` as its stable hosted cache-volume mount
contract. Cross-job reuse requires that path to be configured as a Buildkite
cache volume; without the mount, it is an isolated job-local cache and a hosted
cache miss downloads the pinned SDK again. Language-agnostic contract and
other non-Rust SDK checks remain in `//:sdk_contract_checks`.

`dev-linux` uses `fastbuild`, Rust `debuginfo=0`, and the content-pinned mold
archive declared in `MODULE.bazel`. `dev-debug` deliberately has a different
key, enables debug information, and uses the platform linker. `test`, `ci`, and
`release` likewise have distinct configuration keys.

Routine checks must be `bazel test` targets or `test_suite` targets so successful
results can be cached. `bazel run` is for tools, generators, and explicit manual
operations, not ordinary test evidence.

## Repository-owned configuration

The checked-in `.bazelversion`, `.bazelrc`, module graph, and wrapper are the
machine-portable configuration. A contributor must not need `~/.bazelrc`,
`~/.cargo/config.toml`, or another machine-local dotfile.

The wrapper prefers `${CTX_BAZEL_SPACIOUS_ROOT:-/mnt/ctx-perf}/ctx-bazel` when
that location is writable and has at least 5 GiB and 50,000 free inodes, and
otherwise uses
`${XDG_CACHE_HOME:-$HOME/.cache}/ctx/bazel`. Override the choice explicitly
when needed:

```bash
export CTX_BAZEL_CACHE_ROOT=/mnt/shared/ctx-bazel
scripts/bazelw test //crates/ctx-cli:unit_tests --config=test
```

`CTX_BAZEL_SPACIOUS_MIN_FREE_KIB` and
`CTX_BAZEL_SPACIOUS_MIN_FREE_INODES` tune only automatic spacious-root
selection. `CTX_BAZEL_CACHE_ROOT` is an explicit operator choice and is never
silently redirected.

The layout is:

- `output-roots/bazel-<version>/<workspace-hash>`: one output-user-root per
  canonical worktree
- `bazel-<version>/repository-cache`: shared immutable downloads
- `bazel-<version>/action-test-cache`: shared content-addressed action and test
  results
- `bazel-<version>/sandboxes/<workspace-hash>`: worktree-isolated sandboxes

`CTX_BAZEL_SANDBOX_BASE` can move a worktree's sandbox. CI may set
`BAZEL_OUTPUT_USER_ROOT` explicitly while retaining the same repository and
action caches.
`CTX_BAZEL_REPOSITORY_CACHE` and `CTX_BAZEL_ACTION_CACHE` override those two
shared cache paths independently. Never point two worktrees at the same output
root; only content-addressed caches are shared.

The shared action/test disk cache is garbage-collected in the background after
the Bazel server becomes idle. Defaults are 100 GiB and 30 days; override them
for a particular host with `CTX_BAZEL_DISK_CACHE_MAX_SIZE` and
`CTX_BAZEL_DISK_CACHE_MAX_AGE`. Repository downloads and per-worktree output
roots are separate from that bounded action cache.

Servers stop after 600 idle seconds by default. An explicit
`scripts/bazelw shutdown` also removes that worktree's disposable default
sandbox base while retaining its output root and shared caches for a warm
restart.

The wrapper derives normal jobs from one quarter of available CPUs, caps that
value at eight, constrains it by detected memory, defaults concurrent local
test targets to two, and caps default Rust test threads at four. Override the
resource policy for one task with `CTX_BAZEL_JOBS`,
`CTX_BAZEL_LOCAL_CPU_RESOURCES`, `CTX_BAZEL_LOCAL_RAM_RESOURCES`,
`CTX_BAZEL_LOCAL_TEST_JOBS`, and `CTX_TEST_THREADS`, or pass Bazel flags through
the wrapper:

```bash
CTX_BAZEL_JOBS=4 \
CTX_BAZEL_LOCAL_CPU_RESOURCES=4 \
CTX_BAZEL_LOCAL_RAM_RESOURCES=8192 \
CTX_BAZEL_LOCAL_TEST_JOBS=2 \
CTX_TEST_THREADS=2 \
  scripts/bazelw test //crates/ctx-history-search:unit_tests --config=test
```

The values above are an example, not required machine defaults. Prefer the
wrapper defaults unless the host or concurrent workload needs an override.

Hosts may opt into a generic external build governor by placing an executable
at `${XDG_CONFIG_HOME:-$HOME/.config}/ctx/build-governor` or setting the
absolute `CTX_HOST_BUILD_GOVERNOR` path. Build-capable and other non-light
commands are invoked as `<governor> bazel <command> -- <exact Bazel argv>`;
query, informational, and shutdown commands bypass admission. The wrapper
fails with status 125 when a configured governor is not executable. The
governor sets `CTX_HOST_BUILD_GOVERNOR_ACTIVE=1` for the managed command so
nested wrapper calls do not reacquire a lease. The wrapper accepts that marker
only when its lease ID matches the current systemd lease cgroup; a forged marker
or an explicitly empty governor path fails with status 125. A governed host
defaults Bazel jobs and local CPU resources to 16 while preserving explicit
resource overrides.

## Optional remote execution

The repository defines one opt-in REAPI profile: `--config=ctx-reapi`. It sends
every remotely eligible spawn action to the configured executor and disables
local fallback. Connection and authentication settings remain external to the
repository and must be supplied by the Bazel invocation or machine
configuration.

After those external settings are available, use the profile with an ordinary
wrapper command:

```bash
scripts/bazelw test //crates/ctx-cli:unit_tests --config=ctx-reapi
```

## Focused, affected, and complete checks

```bash
scripts/check.sh --mode=ci
scripts/check.sh --mode=nightly
scripts/check.sh --mode=release
scripts/bazel-affected.sh origin/main
```

Run focused tests repeatedly while editing, then run the affected selector
against the comparison base. Build-configuration changes, uncertain ownership,
unmapped changes, and selector failures all expand to `ci`, the complete public
repository check.
Performance sanity, serialized auto-upgrade acceptance, persistent-daemon soak,
and process/fault injection run in `nightly` and `release`; they are
intentionally outside the per-change `ci` loop.

The affected command uses pinned bazel-diff, an ephemeral detached base
worktree, a commit-keyed cached base hash, and complete target-graph hashes for
both graphs. BUILD, `.bzl`, module, lock, and configuration changes select the full
`ci` suite. A diff/query/filter failure or a changed file with no mapped test
also fails closed to `ci`. Non-routine external, manual, network,
platform, stress, and release targets stay outside affected execution.

`tools/bazel/rust-target-inventory.json` records native ownership for every
Cargo production, binary, example, build-script, and integration-test target.
Its test fails whenever a manifest target is added without a Bazel label.

Bazel automatically reuses successful results when their declared inputs,
configuration, toolchain, platform, and target are unchanged. Use
`scripts/check.sh --force-rerun` only when deliberately checking for a flake;
it reruns test actions without deleting compiled outputs.

Do not inherit changing CI job identifiers through global `--test_env`.
CI-shaped behavior belongs in target-local stable fixtures so an otherwise
identical test action remains reusable across jobs.

## Bounded Cargo diagnostics

Cargo is an exceptional diagnostic or ecosystem-parity path, not a second
normal workflow. When a Bazel failure specifically requires Cargo comparison,
use the bounded repository escape hatch:

```bash
scripts/cargo-diagnostic.sh check -p ctx-history-search
scripts/cargo-diagnostic.sh test -p ctx-history-search
CTX_CARGO_DIAGNOSTIC_DEBUG=1 scripts/cargo-diagnostic.sh test -p ctx-history-search
```

The wrapper defaults Cargo build jobs to one quarter of available CPUs, capped
at eight, caps default Rust test threads at four, uses
`target/cargo-diagnostic`, and disables development/test debug information
unless `CTX_CARGO_DIAGNOSTIC_DEBUG=1`. A Cargo result diagnoses parity; it does
not replace the owning Bazel test.

Do not revive a parallel Rust development wrapper or enable `sccache` by
default. Either change would require separate measured evidence and an explicit
repository policy change.

## Bazel release candidates

The public Core release routes compile and package the target-configured
`//crates/ctx-cli:ctx --config=release` graph. They do not invoke Cargo,
publish, or promote. Each route declares its artifact, pinned rustc,
`Cargo.lock`, target matrix, and target platform as non-overridable runfiles.
The packager verifies the binary's stamped source commit, `Cargo.lock` digest,
and Rust target identity, requires a clean matching checkout, applies the
existing format/ABI and native candidate-smoke hooks, uses the existing macOS
signing hooks when signing is required, and installs the artifact, `.sha256`,
`.version`, and `.build-info.json` files without replacing existing leaves.

Build and package with the same release configuration:

```bash
scripts/bazelw run //:ctx_release_linux_x64 --config=release -- \
  --build-info /secure/ctx-linux-x64.build-info.json \
  --output-dir target/public-cli-artifacts
```

The route labels are `//:ctx_release_linux_x64`,
`//:ctx_release_linux_arm64`, `//:ctx_release_macos_arm64`,
`//:ctx_release_macos_x64`, `//:ctx_release_windows_x64`, and
`//:ctx_release_freebsd_x64`. Their distribution names come only from
`contracts/release-targets-v1.json`; the packager preserves the raw
construction names consumed by `scripts/stage-github-release-assets.sh`.

Linux release lanes use the tracked native builder rather than hand-authoring
the Linux `--build-info` input:

```bash
source_commit="$(git rev-parse --verify HEAD^{commit})"
scripts/release/build-linux-bazel-release.sh \
  --platform linux-x64 \
  --source-commit "${source_commit}" \
  --output-dir /secure/build/ctx-linux-x64
```

The command requires a clean checkout at exactly `source_commit`, native host
and Docker architectures, and the pinned offline advisory inputs named in its
usage. It builds the matching Bazel route with compilation networking
disabled, stages the complete CLI/runtime/evidence bundle beside the final
destination, seals it, smokes those final candidate bytes once, verifies the
seal, and atomically commits the directory without replacement. Private debug
symbols use the same no-replace publication rule when requested. The command
never signs, uploads, deploys, or updates a release channel.

`ctx.build-info.json` is canonical, timestamp-free JSON. It binds the exact
artifact, clean source commit, 0.26.0 source and executable versions,
`Cargo.lock`, release target matrix, `MODULE.bazel`, `MODULE.bazel.lock`,
Bazel version, configured Rust toolchain, builder recipe, and immutable
builder/runtime/inspector image IDs. The producer writes it only after the
pinned static-ABI and native-runtime gates pass. The release packager
reconstructs those source/version/target/toolchain bindings from its declared
runfiles and rejects any changed or non-canonical build-info bytes.

With no mode flag, the staging helper validates and stages the six CLI binaries
paired with the six legacy runtime transports. `--with-semantic` preserves
those pairs and adds the ten exact Semantic assets. Linux inputs must carry
their sealed per-platform completion identities; Semantic assets are instead
validated directly through their manifests, checksums, and canonical catalog.

Aggregate staging also writes a separate release-authority handoff directory;
it does not add those files to the GitHub Release asset set or `SHA256SUMS`.
The handoff retains all six canonical per-target candidate manifests and their
digest sidecars. It also retains the Windows executable and candidate evidence
under their exact construction names (`ctx.exe`, `ctx.exe.build-info.json`,
`ctx.exe.cdx.json`, `ctx.exe.size.json`, and
`ctx.exe.third-party-notices.txt`), plus exact copies of `SHA256SUMS` and the
Windows runtime archive. `release_bundle.py` publishes this fresh, exact
19-file directory atomically without replacement.

After the final `SHA256SUMS` exists, the Windows manifest is finalized to bind
the literal `SHA256SUMS` and
`ctx-onnxruntime-windows-x64.zip` names, their exact bytes and SHA-256 values,
and the exact `lib/onnxruntime.dll` name, size, and SHA-256 value. Its digest
sidecar is convenience input and is not authority. The production verifier
accepts only the complete handoff and an independently obtained expected
manifest digest:

```bash
python3 -I scripts/release-sbom.py verify-release \
  --handoff-dir target/github-release-authority \
  --expected-manifest-sha256 HEX_DIGEST
```

The command requires the exact handoff inventory, verifies the release-bound
manifest and every Windows input it names, and prints the verified digest. It
does not sign, attest, authenticate, or select the expected digest. The
authority integrating this interface must supply that digest independently of
the handoff and its sidecars.

Pass an explicit third directory when staging for a release build:

```bash
scripts/stage-github-release-assets.sh \
  target/public-cli-artifacts \
  target/github-release-assets \
  target/github-release-authority
```

Linux must also pass `--build-info PATH` from the pinned Ubuntu 22.04 builder.
For Linux x64 staging dogfood, the tracked builder above owns that producer and
passes its output to the generic packager. Other Linux release lanes must
provide equivalently validated builder-authored evidence. Set
`CTX_MACOS_RELEASE_SIGNING=required` on trusted macOS release workers; signing
remains optional for unsigned local qualification candidates.

## Platform boundary

Construction requires the corresponding Bazel Rust/C++ toolchains and native
runners. The Windows route selects a dedicated
`x86_64-pc-windows-gnu` target graph; it does not reuse or relabel the normal
MSVC graph, and its native authority must provide the contracted MinGW linker
and runtime. The stable FreeBSD route name fails closed until the separately
owned crate/toolchain graph repair lands. Host-Cargo builds remain diagnostic
rather than authoritative.
