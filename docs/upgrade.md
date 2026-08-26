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
Signed release metadata must also allow automatic application. With automatic
indexing and the full daemon profile, the persistent daemon drives automatic
checks. When that full driver is absent—manual indexing or
`daemon.mode = "source-refresh-only"`—successful commands in the `setup`,
`index`, `sources`, `import`, `show`, `list`, `locate`, `search`,
`integrations`, and `doctor` families perform a cheap post-output cadence hint
and, only when a check may be due, launch a detached automatic-upgrade worker.
The worker reloads policy and uses the same executable-scoped scheduler state,
lock, backoff, signed metadata, daemon handoff, and replacement transaction as
the daemon.

The foreground hint does not acquire the upgrade lock, parse or hash the
installed binary, access the network, or change command output. The `status`,
`stats`, `docs`, MCP, `daemon`, `upgrade`, and companion command families do
not launch a worker, and finite Core indexing workers never own upgrade
maintenance. Failed commands and failed output delivery do not launch one.
Output format does not change these rules.

Use `ctx index mode` to inspect the indexing mode. `ctx index mode auto`
restores automatic indexing and its persistent daemon when no process-level
override disables it; `ctx index mode manual` stops the daemon and removes its
supervision without disabling managed automatic upgrades.

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
install and leaves ctx unmanaged. The hosted installer will not silently adopt
that executable. A marker that is malformed, unsupported, path-mismatched, or
does not match the binary hash leaves the executable and marker as an
inconsistent pair; do not edit or overwrite the sidecar in place.

Before moving or removing either an unmanaged executable or an inconsistent
executable/marker pair, run the lifecycle handoff with the currently installed
executable:

```bash
ctx daemon disable --prepare-uninstall --format=json
```

Continue only after the command succeeds and its JSON receipt reports a
quiescent installation. Then move or remove the unmanaged executable, or both
members of the inconsistent pair, and rerun the platform-correct hosted
installer. On Linux or macOS:

```bash
curl -fsSL https://ctx.rs/install | sh
```

On Windows:

```powershell
irm https://ctx.rs/install.ps1 | iex
```

Alternatively, after the successful handoff, choose a different empty binary
directory (`BinDir` on Windows) and make sure `PATH` resolves `ctx` to the
intended installation. See `ctx docs show unmanaged-installs` for the complete
conversion procedure and required receipt fields.

Manual `ctx upgrade` verifies signed release metadata, explicit self-upgrade
policy, artifact SHA-256, the current managed install marker, and the staged
binary's `ctx --version` output before replacing the installed binary.

The production binary fixes release metadata under
`https://cli.ctx.rs/functions/v1/releases/<channel>/`, derives the detached
signature URL from that metadata URL, verifies with its embedded release public
key, and accepts artifact URLs only under the compiled
`https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/` authority.
Config files and process environment variables cannot replace those origins or
the verification key. A key or authority change therefore requires a new ctx
binary, not a shell-profile or config-file change.

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
