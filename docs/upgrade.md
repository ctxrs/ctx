# Upgrade

`ctx upgrade` checks and applies signed ctx CLI releases for binaries installed
by the official hosted installer.

```bash
ctx upgrade status
ctx upgrade status --format json
ctx upgrade check
ctx upgrade check --format json
ctx upgrade --dry-run
ctx upgrade
ctx upgrade disable
ctx upgrade enable
```

The installer writes a sidecar marker next to the binary, such as
`~/.local/bin/ctx.install.json`, recording the managed install path, platform,
version, channel, binary SHA-256, metadata URL, and artifact URL. Source builds,
`cargo install`, package-manager installs, copied binaries, and mismatched
sidecars are treated as unmanaged and will not self-upgrade.
`ctx upgrade status --format json` also lists every `ctx` binary found on `PATH` and
warns when another binary shadows the managed install.

Managed automatic upgrades are on by default (`upgrade.auto = "apply"`).
Signed release metadata must also allow automatic application. The enabled
long-lived daemon is the sole authority that checks cadence/backoff, downloads,
stages, and initiates an automatic replacement. Foreground commands and MCP
never schedule an upgrade or spawn a background upgrade process.
Machine-readable foreground commands also do not start or nudge the daemon.
When `daemon.enabled = false`, there is no automatic network check, download,
or apply; explicit `ctx upgrade` remains available.

Use `CTX_UPGRADE_AUTO=off` for a process-level opt-out. For a persistent opt-out,
run `ctx upgrade disable`; it writes `upgrade.auto = "off"` in `config.toml`.
Run `ctx upgrade enable` to restore `upgrade.auto = "apply"`. `ctx status` and
`ctx doctor` report the effective mode after config and process overrides.

## Fix upgrade diagnostics

If a diagnostic says another `ctx` shadows the managed executable on `PATH`,
put the managed install directory before the reported shadowing directory and
restart the shell. On POSIX shells, `command -v -a ctx` shows the resolution
order; in PowerShell, use `Get-Command ctx -All`.

An absent install marker is normal for a source build or package-manager
install and leaves ctx unmanaged. If a marker exists but is malformed,
unsupported, path-mismatched, or does not match the binary hash, reinstall with
the official installer instead of editing the sidecar:

```bash
curl -fsSL https://ctx.rs/install | sh
```

```powershell
irm https://ctx.rs/install.ps1 | iex
```

Manual `ctx upgrade` verifies signed release metadata, explicit self-upgrade
policy, artifact SHA-256, the current managed install marker, and the staged
binary's `ctx --version` output before replacing the installed binary.

When local semantic search is explicitly enabled, the same signed release
metadata may carry the semantic asset catalog. ctx verifies the metadata
signature before it accepts any catalog URL, archive hash, expanded-size limit,
or per-file hash. Downloads are streamed with role-specific byte limits, and
archive extraction accepts only the signed regular-file inventory. Semantic
search remains off by default; a disabled semantic configuration neither
selects nor downloads these assets.

The selected catalog entry pairs one exact model with one local backend:
ONNX Runtime 1.27.0 for portable CPU execution, WindowsML 2.1.74 with DirectML
on Windows, or the pinned Linux x86_64 CUDA 12 runtime including its CUDA and
cuDNN user-space libraries. Apple silicon uses the signed Core ML bundle.
The CUDA package still requires a compatible NVIDIA driver from the host.

Model and runtime publication participates in the existing upgrade transaction
and recovery journal. A failed publication rolls back the previous paths.
Running `ctx upgrade` again at the same CLI version repairs a missing or
hash-mismatched selected semantic asset from the signed catalog without
changing the CLI version.

On Windows, replacement may be scheduled by a helper that finishes after the
running `ctx.exe` exits; JSON reports `status: "scheduled"` and
`applied: false` until replacement completes.

One scheduler state, `.ctx.upgrade-state.json`, and one replacement transaction
journal live beside the managed executable. The executable-adjacent
`.ctx.install.lock` coordinates all data roots sharing that installation.
`ctx upgrade status` reads the scheduler state and shows failed-check details.
Upgrade metadata checks do not send provider transcript text, search queries,
result snippets, source paths, repository names, or command output.
