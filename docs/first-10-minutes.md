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

## 2. Set Up And Index

```bash
ctx setup
ctx status --json
```

`ctx setup` creates local storage, discovers supported provider history,
catalogs Codex sessions, imports discovered native provider sources, and
optimizes the local search index. It does not execute history-source plugin
commands. The default root is `~/.ctx`. Use a temporary root for trials:

```bash
ctx --data-root /tmp/ctx-first-10 setup
```

## 3. Check Sources

```bash
ctx sources
ctx sources --json
```

Expect rows for supported local import providers such as Codex, Pi,
Antigravity, Claude, OpenCode, OpenClaw, Hermes, Gemini, Cursor, Copilot CLI,
and Factory AI Droid. NanoClaw and AstrBot can appear as preview rows when ctx
can discover their local project or SQLite paths. A row with `exists: false`
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
ctx import --provider hermes --path ~/.hermes/state.db
ctx import --provider nanoclaw --path /path/to/nanoclaw-project
ctx import --provider astrbot --path /path/to/data/data_v4.db
ctx import --provider shelley --path ~/.config/shelley/shelley.db
```

Preview providers such as NanoClaw and AstrBot are explicit-import only. Use
`ctx import --provider nanoclaw` or `ctx import --provider astrbot` when
discovery finds the desired source, or add `--path` to target a specific source.
They are not included in `ctx import --all` or the default pre-search refresh
until their storage contracts are promoted.

After upgrading from an older ctx version, the first refresh or import can
re-read previously indexed provider transcripts once so the local index includes
current touched-file metadata and unredacted local transcript text.

## 5. Search

```bash
ctx search "build failure" --limit 5
ctx search "build failure" --term checksum --term release --limit 5
```

`--limit` is capped at `200`. Search defaults to `--refresh auto`, which
best-effort refreshes discovered native provider sources and enabled auto
history-source plugins before querying; use `--refresh off` to search only the
existing index.

Inside Codex, ctx excludes the active session tree by default when it can
identify it, so your current prompt and subagents do not dominate results. Add
`--include-current-session` when that is what you want to search.

Copy ctx-owned IDs from the result and inspect the hit or transcript:

```bash
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id>
ctx locate event <ctx-event-id>
```

Use citations from `ctx search` or `ctx show` when the retrieved material
affects an answer or implementation. Add `--json` only when a script or `jq`
needs exact fields.

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
- Import fails on a file: rerun with `--json` and inspect the per-source
  `failed` count.
- Search returns no results: confirm `ctx status` shows indexed items, then
  widen the query or remove filters.
- Citation source missing: ctx can still return indexed text, but the raw
  provider file is unavailable at the stored path.
- Upgrade says unmanaged install: reinstall with the official installer if you
  want signed self-upgrades, or keep managing the binary with your package
  manager/source checkout.
