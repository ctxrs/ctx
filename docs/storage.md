# Storage And Privacy

ctx stores search indexes locally. Treat the ctx data root like private source
history.

## Local Layout

Default root:

```text
~/.ctx/
  work.sqlite
  config.toml
  logs/
```

`CTX_DATA_ROOT` or `--data-root` may point ctx somewhere else. The configured
root is used directly; ctx does not append another directory.

## What SQLite Stores

The SQLite store may contain:

- provider and source metadata;
- source file paths and import cursors when available;
- session IDs and event IDs;
- timestamps and working-directory metadata when known;
- normalized user, assistant, tool, command, and lifecycle event text;
- bounded command or tool-output previews;
- FTS-indexable text required for search;
- citations and offsets or line/cursor metadata when available;
- compatibility rows used by the current search implementation.

If text is searchable, assume a copy or normalized form exists in SQLite. Raw
provider transcript files may still remain in provider-owned locations such as
`~/.codex/sessions`, but the searchable parts are local ctx data too.

## What ctx Avoids By Default

The current CLI avoids copying unbounded stdout, stderr, binary artifacts, image
payloads, and provider-private blobs into SQLite. When a provider transcript has
large raw payloads, ctx should store a bounded preview plus a citation back to
the raw source path when available.

## Provider-Owned Data

ctx does not own provider homes. Import reads from configured or discovered
locations and records enough information to search and cite imported material.
If a raw source path moves or is deleted, `ctx show` and `ctx search` can still
return indexed text and should mark source availability when that information
is known.

## Command Read/Write Behavior

| Command | Reads | Writes |
| --- | --- | --- |
| `ctx setup` | home path metadata for source discovery | data root, `work.sqlite`, `config.toml` |
| `ctx status` | data root metadata, existing SQLite store | nothing intentional |
| `ctx sources` | known provider paths under the user's home | nothing |
| `ctx import` | provider transcript files and path metadata | data root, `config.toml` if missing, SQLite index |
| `ctx list` | SQLite index | nothing |
| `ctx show` | SQLite index | nothing |
| `ctx search` | SQLite index | nothing |
| `ctx doctor` | SQLite index and data root metadata | nothing intentional |
| `ctx validate` | SQLite index | nothing intentional |

Setup, import, and search do not require network access or source repository
writes.

## Default Config

`ctx setup` creates `~/.ctx/config.toml` when the default root is used, or
`config.toml` under the configured data root when `CTX_DATA_ROOT` or
`--data-root` points elsewhere. Existing config files are left in place.

The day-1 generated config is:

```toml
[updates]
channel = "stable"
auto_update = true
```

Analytics are enabled by default when `[analytics] enabled` is absent. To opt
out, add:

```toml
[analytics]
enabled = false
```

Automatic update checks use the `stable` channel by default. These checks report
availability only; ctx does not replace its own binary until signed release
manifest verification ships. To disable the checks, add:

```toml
[updates]
auto_update = false
```

## Index Lifecycle

Find the active ctx root before destructive maintenance:

```bash
ctx status
```

The default root is `~/.ctx`. If you set `CTX_DATA_ROOT` or pass `--data-root`,
use that root in the commands below instead.

Re-import or update the index:

```bash
ctx import --all
ctx import --resume
ctx import --path ~/.codex/sessions
```

Current adapters are safe to re-run. They rescan sources idempotently and keep
source paths or cursors when available.

Remove a source from future imports:

```bash
$EDITOR ~/.ctx/config.toml
```

The current CLI does not add provider source entries to `config.toml`; default
provider locations are discovered each time and explicit `--path` imports are
not remembered as future defaults. To remove already indexed data, rebuild the
index and import only the sources you still want.

Reset and rebuild the index:

```bash
rm -f ~/.ctx/work.sqlite ~/.ctx/work.sqlite-wal ~/.ctx/work.sqlite-shm
ctx setup
ctx import --all
```

This removes the local SQLite index and recreates it from provider history. It
does not delete raw provider transcript files.

Inspect storage size:

```bash
du -sh ~/.ctx
du -h ~/.ctx/work.sqlite*
ctx status --json
```

Delete all ctx data:

```bash
rm -rf ~/.ctx
```

This removes ctx's local index, config, and logs for the default root. It does
not remove provider-owned history such as `~/.codex/sessions`.

## Privacy Truth

No local search index can be considered share-safe by default. Indexed prompts,
code, commands, file paths, and output previews may contain credentials,
customer data, private repository names, or proprietary design notes.

Recommended handling:

- keep `~/.ctx` out of source repositories;
- do not share SQLite databases or logs;
- review JSON output before sharing it outside the machine;
- delete or reinitialize the local store when working on shared machines;
- use provider filters and result limits to keep agent retrieval focused on
  relevant material.

## Network Behavior

Core setup, source discovery, import, and search commands are local filesystem
operations. The tools that originally produced provider transcripts may have
used the network according to their own configuration; ctx indexing those
transcripts does not repeat that behavior. Analytics sends coarse command
metadata unless disabled. Update checks use the release endpoint only for
`ctx update` and the throttled status/doctor/validate auto-update path.
