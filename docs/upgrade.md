# Upgrade

`ctx upgrade` checks and applies signed ctx CLI releases for binaries installed
by the official hosted installer.

```bash
ctx upgrade status
ctx upgrade status --json
ctx upgrade check
ctx upgrade check --json
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
`ctx upgrade status --json` also lists every `ctx` binary found on `PATH` and
warns when another binary shadows the managed install.

Official installer-managed installs default to background auto-upgrade after
successful normal commands when signed release metadata explicitly allows
auto-upgrade. Background checks never run for `--json` commands, MCP,
`ctx docs`, `ctx sql`, `ctx upgrade`, CI, unmanaged installs, or process-level
opt-outs. They write state and logs under the ctx data root and do not write to
stdout or stderr.

Use `CTX_UPGRADE_OFF=1` or `CTX_DISABLE_AUTO_UPGRADE=1` for process-level
opt-out, or `ctx upgrade disable` to write `upgrade.auto = "off"` in
`config.toml`. Use `ctx upgrade enable` to restore managed background
auto-upgrade for installer-managed binaries.

Manual `ctx upgrade` verifies signed release metadata, explicit self-upgrade
policy, artifact SHA-256, the current managed install marker, and the staged
binary's `ctx --version` output before replacing the installed binary.

On Windows, replacement may be scheduled by a helper that finishes after the
running `ctx.exe` exits; JSON reports `status: "scheduled"` and
`applied: false` until replacement completes.

Background checks write `upgrade-state.json` and `logs/upgrade.log` under the
ctx data root. `ctx upgrade status` reads that local state. Upgrade metadata
checks do not send provider transcript text, search queries, result snippets,
source paths, repository names, or command output.
