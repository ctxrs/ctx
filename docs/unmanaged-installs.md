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
metadata. The macOS and Windows binaries are not currently Developer ID
signed/notarized or Authenticode-signed; a checksum verifies bytes against the
release metadata, but it is not OS-native application signing.

Official Linux release binaries are checked to require no newer than glibc
2.35 and are built from pinned Ubuntu 22.04 container inputs rather than the
runner's host libraries. The public release matrix is currently lexical-only on
every platform; a future semantic build must not raise the baseline CPU or ABI
contract without an explicit compatibility decision. The macOS binaries
currently target macOS 13 or newer.

For pinned installs, GitHub release asset URLs use this pattern:

```text
https://github.com/ctxrs/ctx/releases/download/vVERSION/ASSET
```

For example:

```text
https://github.com/ctxrs/ctx/releases/download/v0.24.0/ctx-linux-x64
https://github.com/ctxrs/ctx/releases/download/v0.24.0/SHA256SUMS
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

On macOS, choose the asset for your CPU and verify it with `shasum`:

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
mise use -g 'github:ctxrs/ctx[bin=ctx]@0.24.0'
```

mise owns upgrades for this install. Re-run `ctx integrations install skills` after upgrading
when you want to refresh the bundled agent skill.

## Homebrew

The ctx org maintains a Homebrew tap:

```bash
brew install ctxrs/tap/ctx
```

Homebrew owns upgrades for this install. Run `ctx integrations install skills` and
`ctx setup` after installing.

## Source Builds

For source builds from a checkout:

```bash
cargo build -p ctx --release
cargo install --path crates/ctx-cli
```

Source builds are unmanaged. They do not use the official release metadata or
installer-managed upgrade path.
