# Release Install

The finished-product installer flow is metadata driven. Users download an
installer script as a local file, inspect it if needed, and run it against
release metadata that pins every artifact name and SHA-256 checksum.

Do not document or publish a `curl ... | sh` command. The supported shell
pattern is a local launch:

```sh
curl -fsSLO https://github.com/ctxrs/ctx/releases/download/vX.Y.Z/install.sh
curl -fsSLO https://github.com/ctxrs/ctx/releases/download/vX.Y.Z/ctx-release-metadata.env
bash install.sh --metadata ./ctx-release-metadata.env
```

PowerShell follows the same local launch model:

```powershell
Invoke-WebRequest -Uri https://github.com/ctxrs/ctx/releases/download/vX.Y.Z/install.ps1 -OutFile install.ps1
Invoke-WebRequest -Uri https://github.com/ctxrs/ctx/releases/download/vX.Y.Z/ctx-release-metadata.env -OutFile ctx-release-metadata.env
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Metadata .\ctx-release-metadata.env
```

The installers reject insecure metadata URLs, non-HTTPS artifact URLs,
placeholder checksums, unsupported platforms, and artifact names that contain
path traversal. They download the selected artifact to a temporary directory,
verify SHA-256 before installation, and then copy only the verified `ctx`
binary into the chosen bin directory.

## Metadata

Release metadata uses `release/install/ctx-release-metadata.env.template` as
the schema reference. Release dry-runs generate a host-specific metadata file
beside the manifest and checksum artifact. Real releases must replace every
placeholder checksum with the SHA-256 digest of the final published artifact.

Required keys:

- `CTX_RELEASE_SCHEMA_VERSION=1`
- `CTX_RELEASE_VERSION`
- `CTX_RELEASE_BASE_URL`
- `CTX_RELEASE_ARTIFACT_<platform>`
- `CTX_RELEASE_SHA256_<platform>`

Supported platform keys are `linux_x64`, `macos_arm64`, `macos_x64`,
`windows_x64`, and `freebsd_x64`. FreeBSD remains blocked until native release
evidence exists.
