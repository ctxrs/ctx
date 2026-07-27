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
scripts/bazelw test //crates/ctx-history-search:unit_tests --config=dev-linux
scripts/bazelw test //crates/ctx-protocol:unit_tests //crates/ctx-sdk:unit_tests --config=dev-linux
scripts/bazelw test //:native_rust_smoke --config=dev-linux
```

The two SDK unit-test targets above are the authoritative Rust SDK test path;
the language-agnostic contract and non-Rust SDK checks remain in
`//:sdk_contract_checks`.

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
that location is writable and otherwise uses
`${XDG_CACHE_HOME:-$HOME/.cache}/ctx/bazel`. Override the choice explicitly
when needed:

```bash
export CTX_BAZEL_CACHE_ROOT=/mnt/shared/ctx-bazel
scripts/bazelw test //:native_rust --config=test
```

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

## Focused, affected, and complete checks

```bash
scripts/check.sh --mode=fast
scripts/check.sh --mode=presubmit
scripts/check.sh --mode=ci
scripts/bazel-affected.sh origin/main
```

Run focused tests repeatedly while editing, then run the affected selector
against the comparison base. Use `presubmit` when the change
touches build configuration, affected selection expands or fails closed, or
ownership is uncertain. Use `ci` for the complete public repository check.

The affected command uses pinned bazel-diff, an ephemeral detached base
worktree, a commit-keyed cached base hash, and complete-content hashes for both
graphs. BUILD, `.bzl`, module, lock, and configuration changes select the full
presubmit suite. A diff/query/filter failure or a changed file with no mapped
test also fails closed to presubmit. Non-routine external, manual, network,
platform, stress, and release targets stay outside affected execution.

`tools/bazel/rust-target-inventory.json` records native ownership for every
Cargo production, binary, example, build-script, and integration-test target.
Its test fails whenever a manifest target is added without a Bazel label.

Bazel automatically reuses successful results when their declared inputs,
configuration, toolchain, platform, and target are unchanged. Use
`scripts/check.sh --force-rerun` only when deliberately checking for a flake;
it reruns test actions without deleting compiled outputs.

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

The staging helper has three closed release-set modes. With no mode flag it
stages the six CLI binaries paired with the six legacy runtime transports.
`--native-candidate` stages exactly six CLI/native-runtime pairs (12
`SHA256SUMS` entries), preserving the five Unix `.tar.gz` runtime transports
and selecting `ctx-windowsml-windows-x64.zip` for Windows. Its Windows runtime
proof must identify the `windows-ml` backend, bind that exact archive digest,
and record a passed Semantic contract canary.
`--with-semantic` preserves the legacy pairs and adds the ten Semantic assets
for 22 checksum entries. These modes are separate; mode flags cannot be
combined.

Linux must also pass `--build-info PATH` from the pinned Ubuntu 22.04 builder.
The generic packager validates that evidence against the exact artifact,
source SHA, `Cargo.lock`, and target matrix but cannot author builder image or
authoritative runtime provenance itself. Set
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
