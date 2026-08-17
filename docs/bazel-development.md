# Bazel Development

Bazel is the authoritative Rust build and test graph. Cargo manifests and
`Cargo.lock` remain dependency metadata for `crate_universe`; they do not define
a second routine build path.

Use the repository-owned entry points for every normal build or test:
`scripts/bazelw` for direct Bazel commands, `scripts/check.sh` for named suites,
and `scripts/bazel-affected.sh` for affected tests. Do not invoke raw `bazel`;
that bypasses the worktree/cache policy owned by the wrapper.

The repository pins Bazel 9.2.0 in `.bazelversion`. The wrapper resolves
Bazelisk before a direct Bazel binary, passes the pin through `USE_BAZEL_VERSION`,
and fails closed unless the selected launcher reports exactly that version. If
Bazelisk is not already installed, bootstrap the checked-in launcher into the
worktree tool cache with:

```bash
CTX_BOOTSTRAP_BAZELISK=1 scripts/bazelw version
```

This bootstrap is explicit so offline builds do not unexpectedly access the
network. Buildkite enables the same bootstrap path in its isolated job tool
environment. Bazel is used for reproducible development, tests, and
qualification; public CLI candidates are constructed by the Linux factory.

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
unmapped changes, and selector failures all expand to `//:ci_tests`, the
complete deterministic test suite. The complete public merge gate remains
`scripts/check.sh --mode=ci`.
The named check first builds `//...` under `--config=ci`; that configuration
inherits the strict Clippy aspect and `-Dwarnings`. It then runs the owning
`//:ci_tests` or `//:nightly_tests` suite under the
deterministic test configuration, so test execution does not reapply the lint
aspect.
Performance sanity, serialized auto-upgrade acceptance, persistent-daemon soak,
and process/fault injection run in `nightly` and `release`; they are
intentionally outside the per-change `ci` loop.

The affected command uses pinned bazel-diff, an ephemeral detached base
worktree, a commit-keyed cached base hash, and complete target-graph hashes for
both graphs. BUILD, `.bzl`, module, lock, and configuration changes select the full
`//:ci_tests` suite. A diff/query/filter failure or a changed file with no mapped
test also fails closed to `//:ci_tests`. Non-routine external, manual, network,
platform, stress, and release targets stay outside affected execution.

`//:repository_policy_check` reads the live Git and Cargo workspaces on every
invocation. It enforces fixed physical-line limits and discovers every Cargo
package, target, local dependency, and Bazel owner without a copied package
inventory. The target is intentionally uncached so a newly added package or
source file cannot be hidden by an incomplete declared-input list. Production
source, including generated source, is limited to 1,000 physical lines; test
source is limited to 1,500. Bazel declaration files are exempt, `.bzl` source
is not, and there is no exception or grandfather list.

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

## Public release candidates

Bazel remains the development, test, and release-qualification authority. It
does not construct the downloadable CLI binaries. One Ubuntu 24.04 x86_64
factory cross-builds all five public binaries from the same clean commit and
writes one candidate directory. The same script runs directly on a matching
host or inside an equivalent VM, container, or Buildkite image:

```bash
scripts/release/build-public-candidate-on-linux.sh \
  --source-commit "$(git rev-parse HEAD)" \
  --macos-sdk /private/path/MacOSX.sdk.tar.gz \
  --output-dir target/public-cli-artifacts
```

The factory pins Rust, Zig, cargo-zigbuild, rcodesign, and Jsign. Official mode
also requires Java 11 or newer, the offline OSV inputs, the existing five Apple
signing values, and the three Azure Artifact Signing identity values. The
Authenticode verifier always uses Java 11 source mode. The factory signs the
Windows binary and signs/notarizes both macOS binaries from Linux before
sealing the candidate. `--diagnostic-unsigned` is available for local
cross-build diagnostics, but its output is explicitly non-releasable.

Buildkite runs this command once, uploads the resulting directory, and fans the
exact bytes out to Linux x64, Linux arm64, macOS arm64, macOS x64, and Windows
x64. Those jobs execute native smoke and signature checks; they never rebuild
the candidate. Bazel remains available for hermetic development and
qualification checks, but it is not a public CLI construction path.

Each `.build-info.json` is canonical, timestamp-free JSON. It binds the exact
artifact, clean source commit, Cargo lock, target, Rust/Zig/cargo-zigbuild
versions, factory recipe, and macOS SDK digest where applicable. It records
static inspection in the factory and deliberately leaves native runtime proof
to the five fan-out jobs.

With no mode flag, the staging helper validates and stages the five CLI
binaries plus their SBOMs and notices. Semantic models and runtime transports
are constructed and handed off by the separate Semantic release graph; they
are not accepted by the Core GitHub staging command. When the factory runs
with `--skip-runtimes`, Core staging requires its aggregate Core completion
marker, not per-platform Linux runtime completion identities.

Aggregate staging also writes a separate release-authority handoff directory;
it does not add those files to the GitHub Release asset set or `SHA256SUMS`.
The handoff retains all five canonical per-target candidate manifests and their
digest sidecars. It also retains the Windows executable and candidate evidence
under their exact construction names (`ctx.exe`, `ctx.exe.build-info.json`,
`ctx.exe.cdx.json`, `ctx.exe.size.json`, and
`ctx.exe.third-party-notices.txt`). The remaining leaves are exact copies of
the 15-entry Core `SHA256SUMS`, `ctx-release-factory.json`, and
`ctx-core.release-complete.json`, plus the canonical
`ctx-core-github-handoff.json` document and its checksum. `release_bundle.py`
publishes this fresh, exact 20-file directory atomically without replacement.

The canonical handoff document records the clean source commit and the exact
name, size, and SHA-256 of all five candidate manifests, `SHA256SUMS`, the
factory manifest, and the factory completion marker. Its digest is the
aggregate Core GitHub authority identifier. The production verifier therefore
accepts only the complete handoff and an independently obtained expected
digest of the exact canonical `ctx-core-github-handoff.json` bytes:

```bash
python3 -I scripts/release-sbom.py verify-release \
  --handoff-dir target/github-release-authority \
  --expected-handoff-sha256 HEX_DIGEST
```

The command requires the exact 20-file inventory and canonical handoff schema.
It verifies all five candidate-manifest records and digest sidecars, the exact
15-entry Core `SHA256SUMS` order and its factory-byte bindings, the canonical
factory/completion identities and their mutual file bindings, and the retained
Windows artifact, build information, SBOM, size report, and notices byte for
byte through the Windows candidate. It prints the authenticated handoff
digest. It does not sign, attest, authenticate, or select that digest. The
integrating authority must supply the expected digest independently of the
handoff; neither `ctx-core-github-handoff.json.sha256` nor any candidate digest
sidecar is authority by itself.

Pass an explicit third directory when staging for a release build:

```bash
scripts/stage-github-release-assets.sh \
  target/public-cli-artifacts \
  target/github-release-assets \
  target/github-release-authority
```

The staging command consumes the factory-authored build information directly.
Mac signing is mandatory in official mode; unsigned diagnostic factory output
cannot be staged as a candidate.

## Platform boundary

Release construction requires one Linux x86_64 host, the pinned Rust targets,
Zig, and a private macOS SDK. Native runners are validation authorities only.
The Windows binary uses the dedicated `x86_64-pc-windows-gnu` Cargo graph and
is executed later on the Windows x64 validation host.

FreeBSD is not a release route. On FreeBSD, build the normal
`//crates/ctx-cli:ctx --config=release` target from source. The pinned
`rules_rust` host-detection patch, FreeBSD Rust toolchain, and checked-in lock
factor remain part of that best-effort source path. FreeBSD source
compatibility does not block prebuilt releases. Host-Cargo builds remain
diagnostic rather than authoritative.
