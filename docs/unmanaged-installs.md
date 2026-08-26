# Package Managers And Unmanaged Installs

The official installer is the recommended way to install ctx. It installs the
CLI, installs the bundled agent-history skill, runs initial setup, and writes
the installer marker used by `ctx upgrade` and background self-upgrade.

Use an unmanaged install when you want to manage the binary yourself. This page
is for users who prefer a direct release binary, mise, Homebrew, or a source
build.

After any unmanaged install, run:

```bash
ctx integrations install skills
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
installed executable:

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

If handoff fails, keep the installed executable in place so the command can be
retried. ctx never falls back to a PID-only or process-name kill.

After an upgrade or reinstall, restore the normal unmanaged installation:

```bash
ctx integrations install skills
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
dependency used by local semantic search: `ctx-onnxruntime-<platform>.tar.gz`
on Unix-like platforms and `ctx-onnxruntime-windows-x64.zip` on Windows. The
official installer reads signed release metadata and installs the matching
runtime automatically; direct unmanaged installs should follow the release
notes for runtime sidecar placement.

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
image. Local semantic search is opt-in on the prebuilt
platforms and uses a separately installed runtime sidecar, so the CLI binary
keeps its baseline CPU and ABI contract. The macOS binaries currently target
macOS 13 or newer.

For pinned installs, GitHub release asset URLs use this pattern:

```text
https://github.com/ctxrs/ctx/releases/download/vVERSION/ASSET
```

For example:

```text
https://github.com/ctxrs/ctx/releases/download/v0.26.0/ctx-linux-x64
https://github.com/ctxrs/ctx/releases/download/v0.26.0/SHA256SUMS
```

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
