# Getting Started

ctx indexes local agent history so an agent can search previous sessions before
it repeats work.

## 1. Install The CLI

```bash
curl -fsSL https://ctx.rs/install | sh
```

The Unix installer requires `curl` and OpenSSL to verify signed release
metadata. On Windows, use `irm https://ctx.rs/install.ps1 | iex`.

On Unix, the installer places `ctx` in `${CTX_BIN_DIR:-$HOME/.local/bin}`. If
that directory is not already on `PATH`, the installer adds an idempotent ctx
PATH snippet to your shell startup file and prints the command to use for the
current shell session. On Windows, the installer places `ctx.exe` in
`$HOME\.local\bin` by default, adds that directory to the user `Path`, and
updates the current PowerShell session. Use `sh -s -- --no-modify-path` on Unix,
`-NoModifyPath` on Windows, or set `CTX_INSTALL_NO_MODIFY_PATH=1` when you want
to manage `PATH` yourself.

The install script installs `ctx`, runs the bundled agent-history skill
installer, and runs `ctx setup` so discovered local history is indexed before it
exits. If daemon maintenance has been explicitly enabled, that setup run can
also start the ctx-owned background daemon for native-history freshness and
semantic catch-up. The skill installer opens an agent picker when interactive;
otherwise it installs the universal `~/.agents/skills` copy plus detected
agent-specific folders for tools that need them. Use `sh -s -- --no-setup` on
Unix, or set `CTX_INSTALL_NO_SETUP=1` on Windows, for install-only CI or
packaging flows. Install-only mode also skips skill setup unless you explicitly
pass a skill option.

To keep installer setup but opt out of setup daemon autostart, use
`sh -s -- --no-daemon` on Unix, `-NoDaemon` on Windows, or set
`CTX_INSTALL_NO_DAEMON=1`.

To skip only the skill step, use `--no-skill` on Unix or `-NoSkill` on Windows,
or set `CTX_INSTALL_NO_SKILL=1`. To target agent-specific skill folders during
install, use `--skill-agent codex`, repeat `--skill-agent`, or use
`--all-skill-agents`; Windows exposes the same controls as `-SkillAgent` and
`-AllSkillAgents`.

When working from source, use `cargo build -p ctx` or
`cargo install --path crates/ctx-cli`.

For GitHub release binaries, mise, Homebrew, and source builds, see
[Package Managers And Unmanaged Installs](unmanaged-installs.md).

## 2. Set Up And Index

```bash
ctx setup
ctx status
```

Setup creates the configured ctx data root, initializes SQLite, discovers known
provider history paths, inventories local history sources, imports discovered
native provider sources, optimizes the local search index, and prints next
steps. It does not write `config.toml` for implicit defaults and does not
execute history-source plugin commands. The default data root is `~/.ctx`. The
daemon is disabled by default during the prerelease; enable it with
`ctx daemon enable` when you want daemon-owned background maintenance. Use
`ctx setup --no-daemon` for a one-run opt-out after daemon maintenance is
enabled. `ctx setup --catalog-only` and `ctx setup --json` do
not autostart daemon maintenance.

Use a different root when testing:

```bash
ctx --data-root /tmp/ctx-demo setup
CTX_DATA_ROOT=/tmp/ctx-demo ctx status
```

Setup does not write to source repositories, call model APIs, download embedding
models, or require API keys while semantic search is disabled. If daemon and
semantic search are explicitly enabled, daemon maintenance may acquire the local
ONNX Runtime asset and embedding model needed for the installed platform.
Daemon maintenance is local-only; cloud sync remains disabled and reports
`network_allowed: false`. Official
installer-managed binaries can run a signed background auto-upgrade check after
later successful non-JSON commands other than `ctx status`; that updater does
not collect provider history.

## 3. See Available Sources

```bash
ctx sources
ctx sources --json
```

`sources` checks known provider locations on the current machine. Today it
reports supported Codex, Pi, Antigravity, Claude, OpenCode, Kilo Code, Gemini,
Cursor, Zed, Copilot CLI, Factory AI Droid, Warp, and other supported local
history paths. JSON rows include
`status` and `importable`; `status: "empty"` means the default location exists
but no provider-specific transcript files were found there, and
`status: "unknown"` means the bounded transcript probe hit its scan budget.

## 4. Re-Run Or Target Imports

```bash
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --provider cursor
ctx import --provider zed
ctx import --provider codex --path ~/.codex/sessions
ctx import --resume --json
ctx import --provider pi --path ./pi-session.jsonl --partial
```

Setup already imports discovered sources. Use `ctx import` to repair, re-run,
resume, or target a specific provider/path. Current importers rescan sources
idempotently and skip or replace unchanged indexed rows. The `--resume` flag is
reported as `idempotent_rescan`; it does not yet mean every provider has a
native cursor-resume API. Imports are source-atomic by default; use `--partial`
only when you want valid rows from a malformed source to commit while row
failures are reported.

When daemon maintenance is enabled, `ctx import` can start the same ctx-owned
background daemon profile after the foreground import finishes.
The daemon refreshes native history within local budgets and, when semantic is
enabled, may acquire the local embedding model and perform semantic catch-up.
Use `ctx import --no-daemon` for a one-run opt-out. JSON import output does not
autostart daemon maintenance.

After upgrading an older data root to `0.10.x` or newer, the first refresh or import may
re-read previously indexed provider transcripts once. That rebuilds search
content with touched-file metadata and local/private transcript text.

Native provider `--path` imports require `--provider`. Custom JSONL imports use
`--format ctx-history-jsonl-v1 --path <file>` instead.

## 5. Search

```bash
ctx search "failed migration"
ctx search "failed migration" --term sqlite --term rollback
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
```

Lexical search treats words in a multi-word query as alternatives and ranks
results matching more of those words ahead of partial matches. Repeated
`--term` values merge additional queries or keywords into the same result set.

Use `ctx_event_id` with `ctx show event` when you need a hit plus surrounding
events. Use `ctx_session_id` with `ctx show session` when you need the
transcript. Commands accept full ctx IDs or unambiguous ID prefixes of at least
eight hex characters. Search also accepts filters such as `--provider`,
`--workspace`, `--since`, `--event-type`, `--file`, `--include-subagents`,
`--include-current-session`, `--term`, `--limit`, and
`--refresh background|off|wait`.
`--limit` is capped at `200`.
Search defaults to `--refresh background`, which serves existing indexes while
daemon maintenance refreshes lexical and semantic indexes when enabled. Use
`--refresh wait` for foreground text refresh, or `ctx import --all` for an
explicit import catch-up.

When ctx runs inside Codex, search excludes the active Codex session tree by
default when it can identify it. Use `--include-current-session` if the current
session or its subagent work is the history you want to search. Use
`--refresh off` when you need a strictly read-only query over the existing ctx
index.

Semantic and hybrid search read existing local sidecar coverage. With semantic
enabled and default background refresh, search may start the configured daemon
so the daemon-owned query service can embed the query; `--refresh off` skips
that autostart. Search does not run semantic catch-up or download embedding
models. Hybrid uses semantic evidence only after coverage is complete and dirty
work is drained; until then it falls back to lexical search with a structured
reason. Explicit semantic search can query partial coverage for diagnostics,
but reports a local error when the model cache is missing or the semantic worker
is actively indexing.

## 6. Use JSON For Scripts

```bash
ctx search "failed migration" --json | jq '.results[0].ctx_event_id'
ctx show event <ctx-event-id> --format json
ctx show session <ctx-session-id> --format json
```

Default text output is usually better for agent reading. Search JSON is the
supported machine-readable retrieval API for scripts and exact field
extraction. It contains cited snippets and source metadata, but it is retrieved
source material rather than generated analysis.

## 7. Built-In Docs And Upgrades

```bash
ctx docs search "file path"
ctx docs show cli-reference
ctx docs man --print ctx
ctx upgrade status
ctx upgrade check
```

`ctx docs` reads embedded public docs from the installed binary. Agents should
prefer `ctx docs search` and `ctx docs show` over man pages; man pages are
available for human shell use.

`ctx upgrade` works for official installer-managed binaries. Source builds,
`cargo install`, package-manager installs, and copied binaries are treated as
unmanaged and will not self-upgrade. Use `ctx upgrade disable` or
`CTX_UPGRADE_OFF=1` to disable managed background auto-upgrade.
