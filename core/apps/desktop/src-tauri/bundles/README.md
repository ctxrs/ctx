# Bundled Harnesses + Runtimes

This directory is packaged into the desktop app as a resource bundle. Release builds can drop
prebuilt harness binaries and runtimes here, along with a `manifest.json` that describes them.

## Layout (v1)

Recommended structure (paths are relative to this folder):

- `manifest.json`
- `runtime_lock.v1.json`
- `runtime_lock.v2.json`
- `providers/<provider-id>/<os>/<arch>/...`
- `runtimes/node/<os>/<arch>/...`
- `runtimes/python/<os>/<arch>/...`
- `runtimes/avf-linux-guest/<os>/<arch>/...`

The `manifest.json` is the source of truth for locating bundled assets. Paths in the manifest are
relative to this folder unless absolute.

`runtime_lock.v1.json` is retained for migration compatibility.
`runtime_lock.v2.json` is the active lock schema for profile-aware runtime preflight.
Desktop dev launch validates lock + manifest (and generates `runtime_manifest.effective.json`) before starting Tauri.
For parity desktop launches, required targets are explicit and release-blocking:
- `macos/aarch64`
- `linux/aarch64`
- `linux/x86_64`

## Manifest schema (v1)

```jsonc
{
  "version": 1,
  "generated_at": "2026-01-29T00:00:00Z",
  "providers": [
    {
      "id": "codex",
      "protocol": "crp",         // acp | crp
      "version": "1.0.0",
      "os": "macos",             // linux | macos | windows
      "arch": "aarch64",         // aarch64 | x86_64
      "sha256": "...",
      "command": "providers/codex/macos/aarch64/codex-crp",
      "args": []
    }
  ],
  "runtimes": [
    {
      "id": "node",
      "version": "24.12.0",
      "os": "macos",
      "arch": "aarch64",
      "sha256": "...",
      "root": "runtimes/node/macos/aarch64/node-v24.12.0",
      "bin": "bin/node",
      "npm_cli": "lib/node_modules/npm/bin/npm-cli.js"
    },
    {
      "id": "python",
      "version": "3.13.11",
      "os": "macos",
      "arch": "aarch64",
      "sha256": "...",
      "root": "runtimes/python/macos/aarch64/cpython-3.13.11+20251217-aarch64-apple-darwin",
      "bin": "bin/python3"
    },
    {
      "id": "avf-linux-guest",
      "version": "ubuntu-noble-arm64-deadbeef",
      "os": "macos",
      "arch": "aarch64",
      "sha256": "...",
      "root": "runtimes/avf-linux-guest/macos/aarch64/ubuntu-noble-arm64-deadbeef",
      "bin": "rootfs.raw"
    }
  ]
}
```

Notes:
- `protocol` is required for harnesses to distinguish ACP vs CRP binaries.
- `sha256` is included for auditing; v1 uses it for metadata only.
- `command`, `root`, `bin`, and `npm_cli` can be absolute paths, but relative paths are preferred.

## Public source builds

The public export includes the bundle manifest and lock files needed by the checked-in
desktop source tree. Maintainer bundle generation and official release artifact
promotion are outside the public source-build path.

## Build-time troubleshooting

Common setup checks:

```bash
docker version
docker buildx version
docker buildx ls
```

Common failures and fixes:
- `docker buildx ls` shows unhealthy builders: restart Docker Desktop, then re-run.
- `Cannot load builder` or `context deadline exceeded`: ensure Docker daemon is running and `docker buildx inspect --bootstrap` succeeds.
