# First 10 Minutes

This path gets a fresh human or agent from an empty ctx root to a first cited
search result.

## 1. Confirm The Binary

```bash
ctx status
```

If ctx is not installed:

```bash
curl -fsSL https://ctx.rs/install | sh
```

The Unix installer requires `curl` and OpenSSL to verify signed release
metadata. On Windows, use `irm https://ctx.rs/install.ps1 | iex`.

The hosted installer runs the bundled agent-history skill installer and
`ctx setup` by default. The skill step opens an agent picker when interactive
and otherwise installs the universal skill copy plus detected agent-specific
folders. Use `--no-setup` only for install-only automation; it also skips skill
setup unless you pass an explicit skill option.

## 2. Set Up And Index

```bash
ctx setup
ctx status --format json
```

`ctx setup` creates local storage, discovers supported provider history,
inventories local history sources, imports discovered native provider sources,
and publishes an immutable Core/Tantivy search generation. Optional semantic
indexing advances separately. It does not execute history-source plugin
commands. The default root is `~/.ctx`.

Use a temporary root for trials:

```bash
ctx --data-root /tmp/ctx-first-10 setup
```

## 3. Check Sources

```bash
ctx sources
ctx sources --format json
```

Expect rows for supported local import providers such as Codex, Pi,
Antigravity, Claude Code, OpenCode, Kilo Code, OpenClaw, Hermes Agent, Gemini,
Cursor, Zed, GitHub Copilot, Factory AI Droid, and Warp Terminal restoration
SQLite.
NanoClaw is supported from an exact project CWD or official launchd/systemd
service registration; AstrBot appears as supported when a bounded `data_v4.db`
source exists. Warp is supported from documented local `warp.sqlite` paths. A
row with
`exists: false`
means ctx knows the default path but did not find local history there. A JSON
row with `status: "empty"` means the path exists but no provider-specific
transcript files were found. A row with `status: "unknown"` means bounded
discovery could not decide safely, for example because of a scan budget, I/O
failure, or an authentication/encryption boundary. Inspect `status_reason` and
`unsupported_reason` for the typed and human-readable diagnostics.

Hermes `state.db` appears as native and importable. On Linux, a non-root ctx
process with the certified read-only live-WAL path makes new sessions and
appended records converge on native-watch and search refreshes. Otherwise the
incremental attempt defers without copying the provider database. Structural
edits, deletions, and deferred increments reconcile in roughly 60–80 minutes
with a healthy daemon, or on `ctx import --provider hermes` or
`ctx import --all`.

## 4. Re-Run Or Target Imports

```bash
ctx import --all
```

Setup already imports discovered auto-importable sources. Use `ctx import` when
you want to repair, re-run, resume, or pass an explicit path:

```bash
ctx import --provider codex --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/agent/sessions
ctx import --provider cursor --path ~/.cursor/projects
ctx import --provider zed --path ~/.local/share/zed/threads/threads.db
ctx import --provider nanoclaw --path /path/to/nanoclaw-project
ctx import --provider astrbot --path /path/to/data/data_v4.db
ctx import --provider shelley --path ~/.config/shelley/shelley.db
ctx import --provider continue --path ~/.continue/sessions
ctx import --provider openhands --path ~/.openhands
ctx import --provider mimocode
ctx import --provider codebuddy --path ~/.codebuddy
```

NanoClaw participates in ordinary automatic discovery when the exact CWD is a
project store or an official service registration pins a valid checkout. Add
`--path` to target a specific unregistered NanoClaw project.
AstrBot `data_v4.db` sources are imported by `ctx import --all` and pre-search
refresh when they live in bounded default locations, and still support explicit
`--path` imports.

After upgrading from an older ctx version, the first refresh or import can
perform a one-time provider reimport so Core includes current touched-file
metadata and local transcript text.

## 5. Search

```bash
ctx search "build failure" --limit 5
ctx search "build failure" --term checksum --term release --limit 5
```

`--limit` is capped at `200`. Search defaults to `--refresh background`, which
serves the active Core generation while automatic indexing requests background
refresh and semantic catch-up when enabled. Manual indexing serves only the last
published generation in background mode. Use `ctx index mode auto` or
`ctx index mode manual` to change the mode, `--refresh wait` for an authoritative
Core refresh, or `--refresh off` for a query that never starts or wakes a
process.

Direct CLI searches automatically exclude the current session tree for Codex,
DeepSeek Harness, Grok Build, Pi, Claude Code, Goose, Hermes, Shelley, Qwen
Code, and Mux when the current session can be identified unambiguously. If
detection is unsupported or ambiguous, ctx fails open and leaves the history
included. Add `--include-current-session` to restore the automatically
excluded tree. Repeat `--exclude-session <ctx-uuid-or-unambiguous-prefix>` to
exclude exact named sessions; it conflicts with `--session`. MCP searches do
not automatically exclude the caller's session.

Copy ctx-owned IDs from the result and inspect the hit or transcript:

```bash
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
```

Semantic search is optional; see [Retrieval backends](search.md#retrieval-backends)
for setup and readiness behavior.

Use citations from `ctx search` or `ctx show` when the retrieved material
affects an answer or implementation. Add `--format json` only when a script or
`jq` needs exact fields such as `provider_session_id`; for Codex, that field is
the resume UUID.

## 6. Local Help And Upgrade Status

```bash
ctx docs search "upgrade"
ctx docs show search
ctx upgrade status
```

`ctx docs` is embedded in the binary for humans and agents. `ctx upgrade status`
shows whether the current binary is managed by the official installer, eligible
for signed self-upgrades, and shadowed by another `ctx` binary on `PATH`.

## Failure Paths

- No sources listed: this machine may not have supported local provider
  history. Use `ctx import --provider <provider> --path <path>` only for a
  known supported native provider format.
- Import fails on a file: rerun with `--format json` and inspect the per-source
  `failed` count.
- Search returns no results: confirm `ctx status` shows indexed items, then
  widen the query or remove filters.
- A saved citation no longer resolves: rerun the search against the active Core
  generation and use the current ctx-owned IDs.
- Upgrade says unmanaged install: reinstall with the official installer if you
  want signed self-upgrades, or keep managing the binary with your package
  manager/source checkout.
