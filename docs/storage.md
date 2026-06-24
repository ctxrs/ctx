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

`CTX_DATA_ROOT` or a command-specific data-root option may point ctx somewhere
else. The configured root is the root itself; ctx does not append another
product directory.

## What SQLite Stores

The SQLite store may contain:

- provider and source metadata;
- source file paths and import cursors;
- session IDs and event IDs;
- timestamps and working-directory or repository metadata when known;
- normalized user, assistant, tool, command, and lifecycle event text;
- bounded command or tool-output previews;
- FTS-indexable text required for search;
- citations and offsets or line/cursor metadata when available.

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
If a raw source path moves or is deleted, `ctx show` and `ctx context` should
still return indexed text and clearly mark the source as unavailable.

## Index Lifecycle

Find the active ctx root before destructive maintenance:

```bash
ctx status
```

The default root is `~/.ctx`. If you set `CTX_DATA_ROOT` or pass a data-root
option, use that root in the commands below instead.

Re-import or update the index:

```bash
ctx import --all
ctx import --resume
ctx import --path ~/.codex/sessions
```

Current adapters are safe to re-run. They rescan sources idempotently and keep
source paths or cursors when available.

Remove a source:

```bash
$EDITOR ~/.ctx/config.toml
```

The current CLI does not add provider source entries to `config.toml`; default
provider locations are discovered each time and explicit `--path` imports are
not remembered as future defaults. If you maintain local source entries in the
config, remove those entries. To remove data already indexed from that source,
rebuild the index and import only the sources you still want.

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

This removes ctx's local index, config, objects, and logs for the default root.
It does not remove provider-owned history such as `~/.codex/sessions`.

## Privacy Truth

No local search index can be considered share-safe by default. Indexed prompts,
code, commands, file paths, and output previews may contain credentials,
customer data, private repository names, or proprietary design notes.

Recommended handling:

- keep `~/.ctx` out of source repos;
- do not share SQLite databases or logs;
- review JSON output before sharing it outside the machine;
- delete or reinitialize the local store when working on shared machines;
- use provider filters and token budgets to limit agent context to relevant
  material.

## Network Behavior

Core setup, source discovery, import, search, and context commands are local
filesystem operations. The tools you ran inside the original agent sessions may
have used the network according to their own configuration; ctx indexing those
sessions does not change that history.
