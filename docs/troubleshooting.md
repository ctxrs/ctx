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

In automatic indexing mode, the default background search asks the persistent
daemon to refresh while serving the latest published generation. In manual
mode, ordinary search and `--refresh background` intentionally use only that
published generation and do not start or wake a process.

Check the current mode and background health:

```bash
ctx status
ctx index mode
```

Request an authoritative refresh explicitly:

```bash
ctx import --all
ctx search "the missing phrase"
# Or refresh before this search:
ctx search "the missing phrase" --refresh wait
```

Use `ctx import --resume --format json` when you want output to mark the run as an
idempotent rescan. In manual mode, explicit import and `--refresh wait` may
start a finite Core worker and wait for it to publish. `--refresh off` never
starts or wakes one. Run `ctx index mode auto` to return to automatic indexing;
when no process-level override disables it, that command installs or repairs
supervision and starts persistent background indexing.

## Background Indexing Is Not Running

Run:

```bash
ctx status
ctx index mode auto
```

`ctx status` includes daemon and supervisor health. Reapplying auto mode
reconciles the lifecycle to the effective mode; if an override still selects
manual mode, the command reports that instead of starting a daemon.
For advanced foreground diagnosis, `ctx daemon run` blocks in the current
terminal and does not change the configured indexing mode.

After upgrading to `0.10.x` or newer, a refresh can take longer once because ctx
marks older provider import cache rows pending and reimports them to populate
touched-file metadata and local transcript text.

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

Automatic upgrade is on by default for an official installer-managed binary.
Auto indexing with the full daemon profile uses the persistent daemon for
checks. Manual indexing, source-refresh-only mode, ordinary foreground commands,
MCP, and finite workers perform no automatic upgrade work. Explicit
`ctx upgrade` remains available. To opt out persistently, run:

```bash
ctx upgrade disable
```

or for one process:

```bash
CTX_UPGRADE_AUTO=off ctx search "query"
```

The shared automatic scheduler state is stored beside the managed executable in
`.ctx.upgrade-state.json`; checks do not write to foreground stdout or stderr.
Finite Core workers do not perform upgrade maintenance.

## Semantic Search Is Not Ready

Continuous semantic indexing requires auto mode. Enable it and inspect current
health with:

```bash
ctx index mode auto
ctx semantic enable --wait
ctx semantic status
```

Lexical search remains available while embeddings build. Hybrid search begins
using both lexical and semantic evidence when coverage is ready.

In manual mode, there is no background semantic maintenance. After enabling
semantic search, run an explicit semantic or nonzero-weight hybrid search with
`--refresh wait` to prepare the selected executor when needed and reconcile the
semantic projection for that request's pinned Core generation.

For a URL executor, `ctx semantic status` shows the local selection but does not
use the token or probe the endpoint. Verify that the URL follows the HTTPS or
literal-loopback HTTP policy, that remote processes receive
`CTX_SEMANTIC_EMBEDDING_TOKEN`, and that the server implements the
[V1 executor contract](semantic-executors.md#v1-http-protocol). If the endpoint
identity changed, rerun `ctx semantic enable --executor URL` to accept it and
rebuild the derived semantic index. ctx does not silently retry with the
built-in executor; hybrid may still return lexical results while reporting why
semantic evidence was unavailable.

## Store Problems

Find the active root:

```bash
ctx status
```

The default is `~/.ctx`. Check permissions and available disk space. Treat the
database and logs as private local history when collecting diagnostics.
