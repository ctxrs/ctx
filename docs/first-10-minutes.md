# First 10 Minutes

This path gets a fresh human or agent from an empty ctx root to a first cited
search result.

## 1. Confirm The Binary

```bash
ctx status
```

If ctx is not installed, build it from this checkout:

```bash
cargo build -p ctx
cargo install --path crates/ctx-cli
```

## 2. Initialize Local Storage

```bash
ctx setup
ctx status --json
```

The default root is `~/.ctx`. Use a temporary root for trials:

```bash
ctx --data-root /tmp/ctx-first-10 setup
```

## 3. Check Sources

```bash
ctx sources
ctx sources --json
```

Expect rows for supported local import providers such as Codex, Pi, Claude,
OpenCode, Gemini, Cursor, Copilot CLI, and Factory AI Droid. Antigravity and
Amp may appear as detection-only unsupported rows. A row with `exists: false`
means ctx knows the default path but did not find local history there.

## 4. Import

```bash
ctx import --all
```

If no sources exist, pass an explicit path:

```bash
ctx import --provider codex --path ~/.codex/sessions
ctx import --provider pi --path ~/.pi/sessions.jsonl
ctx import --provider cursor --path ~/.cursor/projects
```

## 5. Search

```bash
ctx search "build failure" --limit 5
ctx search "build failure" --limit 5 --json
```

Copy an item ID from the result and inspect it:

```bash
ctx show <item-uuid>
```

Use citations from `ctx search --json` or `ctx show` when the retrieved material
affects an answer or implementation.

## Failure Paths

- No sources listed: this machine may not have supported local provider
  history. Use `ctx import --path` only for a known supported format.
- Import fails on a file: rerun with `--json` and inspect the per-source
  `failed` count.
- Search returns no results: confirm `ctx status` shows indexed items, then
  widen the query or remove filters.
- Citation source missing: ctx can still return indexed text, but the raw
  provider file is unavailable at the stored path.
