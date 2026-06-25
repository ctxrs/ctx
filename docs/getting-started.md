# Getting Started

ctx indexes local agent history so an agent can search previous sessions before
it repeats work.

## 1. Build The CLI

```bash
cargo build -p ctx
cargo install --path crates/ctx-cli
```

The source build is the documented install path until release artifacts and
verification instructions exist.

## 2. Create Local Storage

```bash
ctx setup
ctx status
```

Setup creates the configured ctx data root, initializes SQLite, writes
`config.toml` when missing, discovers known provider history paths, and prints
next steps. The default data root is `~/.ctx`.

Use a different root when testing:

```bash
ctx --data-root /tmp/ctx-demo setup
CTX_DATA_ROOT=/tmp/ctx-demo ctx status
```

Setup does not write to source repositories, call model APIs, require API keys,
or start a background process. First-party analytics are disabled by default
and send coarse command metadata or create `install.json` only when explicitly
enabled in `config.toml` or env.

## 3. See Available Sources

```bash
ctx sources
ctx sources --json
```

`sources` checks known provider locations on the current machine. Today it
reports supported Codex, Pi, Claude, OpenCode, Gemini, Cursor, Copilot CLI, and
Factory AI Droid local history paths, plus detection-only rows for unsupported
Antigravity and Amp locations when relevant.

## 4. Import History

```bash
ctx import --all
ctx import --provider codex
ctx import --provider pi
ctx import --provider cursor
ctx import --path ~/.codex/sessions
ctx import --resume --json
```

Imports are explicit and safe to re-run. Current importers rescan sources
idempotently and skip or replace unchanged indexed rows. The `--resume` flag is
reported as `idempotent_rescan`; it does not yet mean every provider has a
native cursor-resume API.

When `--path` is used without `--provider`, ctx treats the path as Codex format.

## 5. Search

```bash
ctx search "failed migration"
ctx search "failed migration" --json
ctx show <item-uuid>
```

Use result IDs with `ctx show` when you need surrounding events. Search also
accepts filters such as `--provider`, `--repo`, `--since`, `--event-type`,
`--file`, `--primary-only`, `--include-subagents`, and `--limit`.

## 6. Use Search JSON For Agents

```bash
ctx search "failed migration" --json
ctx show <item-uuid> --json
```

Search JSON is the supported machine-readable retrieval API. It contains cited
snippets and source metadata, but it is retrieved source material rather than
generated analysis.
