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
indexing and independent Pro materialization advance separately. It does not
execute history-source plugin commands. The default root is `~/.ctx`.
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
Antigravity, Claude, OpenCode, Kilo Code, OpenClaw, Hermes, Gemini, Cursor,
Zed, Copilot CLI, Factory AI Droid, and Warp Terminal restoration SQLite.
NanoClaw is supported for explicit project paths; AstrBot appears as supported
when a bounded `data_v4.db` source exists. Warp is supported from documented
local `warp.sqlite` paths. A row with
`exists: false`
means ctx knows the default path but did not find local history there. A JSON
row with `status: "empty"` means the path exists but no provider-specific
transcript files were found. A row with `status: "unknown"` means the bounded
transcript probe hit its scan budget.

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
ctx import --provider hermes --path ~/.hermes/state.db
ctx import --provider nanoclaw --path /path/to/nanoclaw-project
ctx import --provider astrbot --path /path/to/data/data_v4.db
ctx import --provider shelley --path ~/.config/shelley/shelley.db
ctx import --provider continue --path ~/.continue/sessions
ctx import --provider openhands --path ~/.openhands
ctx import --provider mimocode
ctx import --provider codebuddy --path ~/.codebuddy
```

NanoClaw is explicit-import only. Use `ctx import --provider nanoclaw` when
discovery finds the desired source, or add `--path` to target a specific source.
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
serves the active Core generation while daemon maintenance requests a Core
refresh and semantic catch-up when enabled; use `--refresh off` for a read-only
query of the active Core generation.

Inside Codex, ctx excludes the active session tree by default when it can
identify it, so your current prompt and subagents do not dominate results. Add
`--include-current-session` when that is what you want to search.

Copy ctx-owned IDs from the result and inspect the hit or transcript:

```bash
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
```

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
