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
and background self-upgrade will not apply; use the same tool or manual process
that installed ctx to upgrade it.

## Binary Lifecycle Handoff

`ctx setup` enables a persistent background daemon. Before any package manager,
manual installer, or source-install command replaces or removes the executable,
run the currently installed executable:

```bash
ctx daemon disable --prepare-uninstall --format=json
```

This is an installation-wide handoff, even when `CTX_DATA_ROOT` or
`--data-root` selects a custom root. It disables and quiesces every registered
daemon root, removes the singleton native supervisor from the canonical root,
releases owner locks and endpoints, and retains the executable and history
data. Do not replace or remove the executable unless the command exits
successfully and its JSON receipt reports all of these fields:

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
ctx daemon enable
ctx setup
```

For an uninstall, omit those post-install commands. History data remains until
you deliberately remove the selected data roots.

## Release Assets

Stable releases publish prebuilt binaries on GitHub Releases:

| Platform | Asset |
| --- | --- |
| Linux x64 | `ctx-linux-x64` |
| Linux ARM64 | `ctx-linux-aarch64` |
| macOS Apple Silicon | `ctx-macos-arm64` |
| macOS Intel | `ctx-macos-x64` |
| Windows x64 | `ctx-windows-x64.exe` |
| FreeBSD x64 | `ctx-freebsd-x64` |

Each release also publishes `SHA256SUMS` for the binary assets. Releases that
ship ctx-managed dynamic ONNX Runtime assets for the local semantic preview may
also include sidecar archives named `ctx-onnxruntime-<platform>.tar.gz` on
Unix-like platforms and `ctx-onnxruntime-windows-x64.zip` on Windows. The
official installer reads signed release metadata and installs those runtime
assets automatically when present; direct unmanaged installs should follow the
release notes for any required runtime sidecar placement.

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

Official Linux release binaries are checked to require no newer than glibc
2.35 and are built from pinned Ubuntu 22.04 container inputs rather than the
runner's host libraries. Local semantic search is opt-in and supported on every
public release platform through a separately installed ONNX Runtime sidecar, so
the CLI binary keeps its baseline CPU and ABI contract. The macOS binaries
currently target macOS 13 or newer.

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

For source builds from a checkout:

```bash
cargo build -p ctx --release
cargo install --path crates/ctx-cli
```

Source builds are unmanaged. They do not use the official release metadata or
installer-managed upgrade path. Run the binary lifecycle handoff before
`cargo install` overwrites an existing ctx executable or before deleting a
source-installed executable.
