# Troubleshooting

## ctx: Command Not Found After Install

On Unix, the hosted installer places `ctx` in
`${CTX_BIN_DIR:-$HOME/.local/bin}`. If that directory was not already on
`PATH`, the installer updates your shell startup file and prints the command to
use immediately. Existing shells do not inherit startup-file edits
automatically, so open a new terminal or run:

```bash
export PATH="$HOME/.local/bin:$PATH"
ctx status
```

On Windows, the hosted installer places `ctx.exe` in `$HOME\.local\bin` by
default, adds that directory to the user `Path`, and updates the current
PowerShell session. If `ctx` is still unavailable, open a new PowerShell window
or run:

```powershell
$env:Path = "$HOME\.local\bin;$env:Path"
ctx status
```

If you installed with `--no-modify-path`, `-NoModifyPath`, or
`CTX_INSTALL_NO_MODIFY_PATH=1`, add the install directory to `PATH` yourself.

## No Sources Found

Run:

```bash
ctx sources --format json
```

Confirm the provider keeps history on this machine and pass an explicit path if
needed:

```bash
ctx import --provider codex --path ~/.codex/sessions
```

## Search Misses Recent Work

Re-run import:

```bash
ctx import --all
ctx search "the missing phrase"
```

Use `ctx import --resume --format json` when you want output to mark the run as an
idempotent rescan.

After upgrading to `0.10.x` or newer, a refresh can take longer once because ctx marks
older provider import cache rows pending and re-reads source transcripts to
populate touched-file metadata and local transcript text.

If the raw provider file moved, indexed text may still be searchable, but source
citations should report that the raw path is unavailable.

## JSON Consumer Fails

Run the same command without `--format json` to inspect warnings, then run:

```bash
ctx doctor --format json
```

Check the command contract in [contracts/json.md](contracts/json.md), including
whether the field is documented as nullable or compatibility-only.

## Upgrade Problems

Run:

```bash
ctx upgrade status
ctx upgrade check
```

Self-upgrade requires an official installer-managed binary and matching
`ctx.install.json` sidecar. Source builds, `cargo install`, copied binaries,
package-manager installs, and binaries whose SHA-256 no longer matches the
sidecar are intentionally unmanaged.

Daemon-owned automatic upgrade is on by default for an official
installer-managed binary while the daemon is enabled. To opt out persistently,
run:

```bash
ctx upgrade disable
```

or for one process:

```bash
CTX_UPGRADE_AUTO=off ctx search "query"
```

The automatic scheduler state is stored beside the managed executable in
`.ctx.upgrade-state.json`; checks should not write to foreground stdout or
stderr. With `daemon.enabled = false`, no automatic check occurs.

## Store Problems

Find the active root:

```bash
ctx status
```

The default is `~/.ctx`. Check permissions and available disk space. Treat the
database and logs as private local history when collecting diagnostics.
