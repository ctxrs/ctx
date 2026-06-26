# Getting Started

ctx indexes local agent history so an agent can search previous sessions before
it repeats work.

## 1. Install The CLI

```bash
curl -fsSL https://cli.ctx.rs/install | sh
```

The install script installs `ctx` and runs `ctx setup` so discovered local
history is indexed before it exits. Use `sh -s -- --no-setup` for install-only
CI or packaging flows.

When working from source, use `cargo build -p ctx` or
`cargo install --path crates/ctx-cli`.

## 2. Set Up And Index

```bash
ctx setup
ctx status
```

Setup creates the configured ctx data root, initializes SQLite, writes
`config.toml` when missing, discovers known provider history paths, catalogs
Codex sessions, imports discovered sources, optimizes the local search index,
and prints next steps. The default data root is `~/.ctx`.

Use a different root when testing:

```bash
ctx --data-root /tmp/ctx-demo setup
CTX_DATA_ROOT=/tmp/ctx-demo ctx status
```

Setup does not write to source repositories, call model APIs, require API keys,
or start a background process.

## 3. See Available Sources

```bash
ctx sources
ctx sources --json
```

`sources` checks known provider locations on the current machine. Today it
reports supported Codex, Pi, Antigravity, Claude, OpenCode, Gemini, Cursor,
Copilot CLI, and Factory AI Droid local history paths.

## 4. Re-Run Or Target Imports

```bash
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --provider cursor
ctx import --path ~/.codex/sessions
ctx import --resume --json
```

Setup already imports discovered sources. Use `ctx import` to repair, re-run,
resume, or target a specific provider/path. Current importers rescan sources
idempotently and skip or replace unchanged indexed rows. The `--resume` flag is
reported as `idempotent_rescan`; it does not yet mean every provider has a
native cursor-resume API.

When `--path` is used without `--provider`, ctx treats the path as Codex format.

## 5. Search

```bash
ctx search "failed migration"
ctx search "failed migration" --json
ctx show event <ctx-event-id> --window 3
ctx show session <ctx-session-id> --mode lite
```

Use `ctx_event_id` with `ctx show event` when you need a hit plus surrounding
events. Use `ctx_session_id` with `ctx show session` when you need the
transcript. Search also accepts filters such as `--provider`, `--repo`,
`--since`, `--event-type`, `--file`, `--primary-only`, `--include-subagents`,
and `--limit`.

## 6. Use Search JSON For Agents

```bash
ctx search "failed migration" --json
ctx show event <ctx-event-id> --format json
ctx show session <ctx-session-id> --mode lite --format json
```

Search JSON is the supported machine-readable retrieval API. It contains cited
snippets and source metadata, but it is retrieved source material rather than
generated analysis.
