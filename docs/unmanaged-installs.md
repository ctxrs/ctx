# Package Managers And Unmanaged Installs

The official installer is the recommended way to install ctx. It installs the
CLI, installs the bundled agent-history skill, runs initial setup, and writes
the installer marker used by `ctx upgrade` and background self-upgrade.

Use an unmanaged install when you want to manage the binary yourself. This page
is for users who prefer a direct release binary, mise, Homebrew, or a source
build.

After any unmanaged install, run:

```bash
ctx integrations install skill
ctx setup
```

Unmanaged installs do not write the official installer marker. `ctx upgrade`
and background self-upgrade will not apply; their effective automatic-upgrade
mode is `off` without writing an opt-out to `config.toml`. `ctx upgrade check`
can still report available releases without upgrade locks or scheduler state.
Use the same tool or manual process that installed ctx to upgrade it.

## Binary Lifecycle Handoff

Automatic indexing is the default, so `ctx setup` may enable a persistent
background daemon. Before any package manager, manual installer, or
source-install command replaces or removes the executable, run the currently
installed executable immediately before that operation:

```bash
ctx daemon disable --prepare-uninstall --format=json
```

This hidden compatibility command is reserved for the installation-wide
uninstall handoff; it is not the public indexing-mode control. Use
`ctx index mode auto` or `ctx index mode manual` for normal indexing
configuration. Daemon lifecycle and supervisor coordination is unified under
the canonical `~/.ctx` root. The handoff applies even when `CTX_DATA_ROOT` or
`--data-root` selects a custom history root. It disables and quiesces every
registered daemon root, removes the singleton native supervisor, releases
owner locks and endpoints, and retains the executable and history data. Do not
replace or remove the executable unless the command exits successfully and its
JSON receipt reports all of these fields:

```json
{
  "ok": true,
  "scope": "installation",
  "installation_quiescent": true,
  "supervisor_removed": true,
  "owner_lock_released": true,
  "endpoint_released": true,
  "coordination_state_removed": true,
  "binary_retained": true
}
```

The receipt proves that the installation is quiescent when the command
finishes; it is not a persistent block on future ctx launches. Proceed directly
to the serialized package-manager or installer operation, and do not run ctx
again until replacement completes.

If handoff fails, keep the installed executable in place so the command can be
retried. ctx never falls back to a PID-only or process-name kill.

After an upgrade or reinstall, restore the normal unmanaged installation:

```bash
ctx integrations install skill
ctx index mode auto
ctx setup
```

For an uninstall, omit those post-install commands. History data remains until
you deliberately remove the selected data roots.

## Convert An Unmanaged Install To A Managed Install

The hosted installer will not silently adopt a binary installed by a package
manager, copied from a release, or built from source. A ctx executable without
the hosted-install marker remains owned by the tool or process that installed
it. `ctx upgrade enable` rejects that install before writing config and points
to this conversion procedure. The hosted installer stops if the executable
occupies its selected binary directory.

To convert safely:

1. Run the [binary lifecycle handoff](#binary-lifecycle-handoff) with the
   currently installed unmanaged executable and verify its successful JSON
   receipt.
2. Use the current package manager or manual process to move or remove that
   executable. Do not remove it before the handoff succeeds.
3. Rerun the hosted installer so it can create a new managed installation and
   marker. On Linux or macOS:

   ```bash
   curl -fsSL https://ctx.rs/install | sh
   ```

   On Windows:

   ```powershell
   irm https://ctx.rs/install.ps1 | iex
   ```

Instead of removing the unmanaged executable, you may select a different empty
`BinDir` for the hosted installer. Make sure `Path` resolves `ctx` to the
installation you intend to use; the two binaries remain separate installs, and
the hosted installer does not assume ownership of the unmanaged one.

If ctx reports that an existing hosted-install marker is malformed or does not
match its executable, use the same lifecycle handoff before moving or removing
both the executable and invalid marker. Then rerun the hosted installer, or
choose a different empty binary directory. Do not overwrite an inconsistent
pair in place.

## Release Assets

Stable releases publish prebuilt binaries on GitHub Releases:

| Platform | Asset |
| --- | --- |
| Linux x64 | `ctx-linux-x64` |
| Linux ARM64 | `ctx-linux-aarch64` |
| macOS Apple Silicon | `ctx-macos-arm64` |
| macOS Intel | `ctx-macos-x64` |
| Windows x64 | `ctx-windows-x64.exe` |

Each stable release also publishes `SHA256SUMS` and the dynamic ONNX Runtime
dependency used by the built-in semantic executor:
`ctx-onnxruntime-<platform>.tar.gz` on Unix-like platforms and
`ctx-onnxruntime-windows-x64.zip` on Windows. The official installer reads
signed release metadata and installs the matching runtime automatically. A
direct-release install has no installer, so it must provision that runtime
itself. Until it does, the built-in executor has no ONNX Runtime to load and
`ctx semantic status` reports `failed` with the loader's reason.

The hosted installer and managed-upgrade path verify signed ctx release
metadata. Beginning with ctx 0.25.0, official macOS CLI binaries and the
executable code in their ONNX Runtime sidecars are Developer ID signed with
hardened runtime compatibility and notarized by Apple. Release construction
also verifies those exact signed bytes with strict `codesign`, a Developer ID
cryptographic attestation, and the published checksums. Each standalone CLI is
executed from an exact-byte copy on native macOS. Headless release jobs do not
simulate Finder's interactive first-open quarantine prompt, and `spctl`
app-bundle classification is not used for standalone Mach-O files. The runtime
dylib requires Accepted notarization and pinned signature/attestation,
then a native packaged semantic smoke proves dyld loading. The final macOS
runtime `tar.gz` is separately authorized by a Developer ID statement binding
the archive, nested dylib, release role, native provenance, and source commit.
Windows
binaries and ONNX Runtime DLLs remain unsigned by Authenticode; signed release
metadata and checksums authenticate their bytes, but they are not OS-native
application signatures.

On macOS, verify an installed official release binary's integrity and Apple
trust, then inspect its signing identity with:

```bash
codesign --verify --strict --verbose=4 "$(command -v ctx)"
spctl --assess --verbose=4 --type install "$(command -v ctx)"
codesign -d --verbose=4 "$(command -v ctx)" 2>&1 | grep -E '^(Authority|TeamIdentifier)='
```

If a package manager installed a wrapper or source build instead of the
official release binary, run these commands against a downloaded
`ctx-macos-arm64` or `ctx-macos-x64` release asset.

Official Linux release binaries are checked to require no newer than glibc
2.28 and are constructed by the pinned Ubuntu 24.04 x86_64 factory rather than
depending on a runner's host libraries. The factory can run directly on an
Ubuntu 24.04 host or in an equivalent Ubuntu 24.04 VM/container/Buildkite
image. Semantic search is opt-in on the prebuilt platforms. Its built-in
executor uses a separately installed runtime sidecar, so the CLI binary keeps
its baseline CPU and ABI contract. The macOS binaries currently target macOS 13
or newer.

For pinned installs, GitHub release asset URLs use this pattern:

```text
https://github.com/ctxrs/ctx/releases/download/vVERSION/ASSET
```

For example:

```text
https://github.com/ctxrs/ctx/releases/download/v0.26.0/ctx-linux-x64
https://github.com/ctxrs/ctx/releases/download/v0.26.0/SHA256SUMS
```

### Provision The CPU Runtime

The CPU ONNX Runtime is the baseline. The built-in executor cannot embed
anything without it, so a direct-release install provisions it first. Which
runtime an archive carries is read from the archive itself, so no backend has
to be named:

```bash
scripts/build-onnxruntime-sidecar.sh linux-x64
ctx semantic runtime install \
  --archive target/public-cli-artifacts/ctx-onnxruntime-linux-x64.tar.zst \
  --sha256 "$(cat target/public-cli-artifacts/ctx-onnxruntime-linux-x64.tar.zst.sha256)"
ctx semantic enable
```

The published `ctx-onnxruntime-<platform>` release asset is the same archive,
so `--archive` also accepts a downloaded one. The installed layout is the one
the loader searches:

```text
${CTX_RUNTIME_DIR:-<data-root>/runtime}/onnxruntime/1.27.0/linux-x64/lib/libonnxruntime.so
```

Unpacking that asset by hand into the same layout still loads, and
`CTX_ONNXRUNTIME_DYLIB` still points the loader at an absolute library path.
What an install adds is verification: with no environment override set, a
digest-verified install is preferred over a plain unpacked layout.

Use `--backend cpu` on `ctx semantic runtime status` to report exactly this
runtime; a bare `ctx semantic runtime status` reports every backend this build
can install locally, plus whether this machine has an accelerator it could use.

`ctx-onnxruntime-windows-x64.zip` is a zip. `ctx semantic runtime install`
reads `.tar.zst` archives only, so Windows CPU runtimes remain hosted-installer
only; a direct-release install on Windows unpacks that zip into the layout
above by hand. macOS ships `.tar.zst` and installs normally. Core ML on macOS
is a separate path: it is an execution provider of the CPU runtime supplied by
the OS, not an installable accelerator sidecar, so macOS has no accelerator
backend to provision.

### Provision A CUDA Runtime For GPU Semantic Search

GPU execution is an opt-in addition on top of the CPU runtime. `ctx semantic
runtime status` says so directly on a machine with an NVIDIA GPU: it reports the
detected accelerator and, until its runtime is installed, the install step. The
CUDA runtime is not a published release asset; build it from the pinned public
inputs and install it with the digest it produced:

```bash
scripts/build-onnxruntime-sidecar.sh linux-x64-cuda12
ctx semantic runtime install \
  --archive target/public-cli-artifacts/ctx-onnxruntime-linux-x64-cuda12.tar.zst \
  --sha256 "$(cat target/public-cli-artifacts/ctx-onnxruntime-linux-x64-cuda12.tar.zst.sha256)"
ctx semantic enable
```

```text
${CTX_RUNTIME_DIR:-<data-root>/runtime}/onnxruntime/1.27.0/linux-x64-cuda12/lib/libonnxruntime.so
```

The build writes the archive plus `<archive>.sha256` and `<archive>.asset.json`
into `target/public-cli-artifacts` by default. `--sha256` is optional when
`<archive>.sha256` sits next to the archive; ctx reads it from there. For every
backend, ctx verifies the archive digest, then reads which runtime the archive
carries by matching its files against the runtime contract compiled into the
binary, then verifies the size and SHA-256 of every extracted file against that
contract, and records the install as `manager: ctx-local-operator` with
`metadata_trust: operator-pinned-digest`. Nothing is installed from bytes it
cannot verify, and the backend is never taken from the archive's name or from
the `.asset.json` sidecar, neither of which the digest covers. Passing
`--backend` asserts the expected runtime instead of selecting one: a mismatch
fails naming both the requested and the detected runtime.

`ctx semantic runtime status` reports each installed runtime, its trust tier,
and its file count. `CTX_ONNXRUNTIME_DYLIB`, `ORT_DYLIB_PATH`, and
`CTX_ONNXRUNTIME_DIR` cannot select an accelerator runtime: it is loaded only
from a provisioned runtime root carrying its `ctx-runtime-install.json`
manifest.

The CUDA package carries its pinned CUDA 12 and cuDNN user-space libraries. The
NVIDIA driver stays host-provided, so the host still needs a driver new enough
for CUDA 12. WSL2 is supported: ctx detects the GPU through `/dev/dxg` and the
WSL driver libraries under `/usr/lib/wsl/lib`, which the Windows host driver
provides. Without a usable GPU, semantic search stays on the CPU runtime.

Windows ML is the Windows accelerator backend. Its sidecar is a zip too, so it
is hosted-installer only; `ctx semantic runtime install` refuses it naming that
installer.

## Direct GitHub Download

On Linux, choose the asset for your CPU:

```bash
curl -fL -O https://github.com/ctxrs/ctx/releases/latest/download/ctx-linux-x64
curl -fL -O https://github.com/ctxrs/ctx/releases/latest/download/SHA256SUMS
grep '  ctx-linux-x64$' SHA256SUMS | sha256sum -c -
mkdir -p ~/.local/bin
install -m 0755 ctx-linux-x64 ~/.local/bin/ctx
```

Use `ctx-linux-aarch64` in the commands above on Linux ARM64.

For ctx 0.25.0 and later on macOS, choose the Developer ID signed and notarized
asset for your CPU and verify its release checksum with `shasum`:

```bash
curl -fL -O https://github.com/ctxrs/ctx/releases/latest/download/ctx-macos-arm64
curl -fL -O https://github.com/ctxrs/ctx/releases/latest/download/SHA256SUMS
grep '  ctx-macos-arm64$' SHA256SUMS | shasum -a 256 -c -
mkdir -p ~/.local/bin
install -m 0755 ctx-macos-arm64 ~/.local/bin/ctx
```

For Windows x64, download `ctx-windows-x64.exe` and `SHA256SUMS`, verify the
file hash, then place it on `Path` as `ctx.exe`.

## mise

mise can install ctx directly from GitHub Releases:

```bash
mise use -g 'github:ctxrs/ctx[bin=ctx]@latest'
```

For a pinned install, replace `latest` with a release version:

```bash
mise use -g 'github:ctxrs/ctx[bin=ctx]@0.26.0'
```

mise owns upgrades for this install. Run the binary lifecycle handoff above
before asking mise to replace or remove ctx, then run the post-upgrade commands
after the new executable is installed.

## Homebrew

The ctx org maintains a Homebrew tap:

```bash
brew install ctxrs/tap/ctx
```

Homebrew owns upgrades for this install. Run the binary lifecycle handoff above
before `brew upgrade` or `brew uninstall`; after an upgrade, run the
post-upgrade commands above.

## Source Builds

FreeBSD is source-only: ctx does not publish a FreeBSD GitHub Release binary,
serve one through the hosted installer, or provide managed self-upgrades there.
FreeBSD source compatibility is maintained on a best-effort basis and does not
block a release.

From a checkout, use the repository's authoritative Bazel build target:

```bash
scripts/bazelw build //crates/ctx-cli:ctx --config=release
install -d "$HOME/.local/bin"
install -m 0755 bazel-bin/crates/ctx-cli/ctx "$HOME/.local/bin/ctx"
```

Source builds are unmanaged. They do not use the official release metadata or
installer-managed upgrade path. Run the binary lifecycle handoff before
the `install` command overwrites an existing ctx executable or before deleting
a source-installed executable. The repository pins its Rust toolchain and
includes the FreeBSD host/toolchain support used by this build; the wrapper
requires the repository's pinned Bazel version and Python 3.11 to be available.
