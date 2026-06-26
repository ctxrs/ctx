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

No session text, prompts, transcripts, or indexed snippets are sent by ctx by
default.

## Provider-Owned Data

ctx does not own provider homes. Import reads from configured or discovered
locations and records enough information to search and cite imported material.
If a raw source path moves or is deleted, `ctx show` and `ctx search` can still
return indexed text and should mark source availability when that information
is known.

## Command Read/Write Behavior

This table describes core command effects. It excludes the optional first-party
analytics marker described under network behavior.

| Command | Reads | Writes |
| --- | --- | --- |
| `ctx setup` | provider transcript files and home path metadata for source discovery | data root, `work.sqlite`, `config.toml`, and SQLite index |
| `ctx status` | data root metadata and existing SQLite store | none |
| `ctx sources` | known provider paths under the user's home | none |
| `ctx import` | provider transcript files and path metadata | data root, `config.toml` if missing, and SQLite index |
| `ctx list` | SQLite index | none |
| `ctx show` | SQLite index | none |
| `ctx locate` | SQLite index and raw source path metadata | none |
| `ctx export` | SQLite index | selected `--out` path when provided |
| `ctx search` | provider transcript files, path metadata, and SQLite index | SQLite index for newly discovered history |
| `ctx doctor` | SQLite index and data root metadata | none |
| `ctx validate` | SQLite index | none |

Setup, import, and search do not require source repository writes, model APIs,
API keys, or remote accounts.

## Default Config

`ctx setup` creates `~/.ctx/config.toml` when the default root is used, or
`config.toml` under the configured data root when `CTX_DATA_ROOT` or
`--data-root` points elsewhere. Existing config files are left in place.

The day-1 generated config is:

```toml
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

Core indexing work uses local filesystem and SQLite operations. The tools that
originally produced provider transcripts may have used the network according to
their own configuration; ctx indexing those transcripts does not repeat that
behavior.

First-party analytics are default-on and may create `install.json` and send
coarse product metadata. They do not send session text, prompts, transcripts,
search queries, result snippets, source paths, repository or branch names,
native session IDs, command text, command output, or raw IP addresses.

Analytics may include:

- a generated install identifier that is hashed server-side;
- ctx version, OS, architecture, command name, success state, and duration
  bucket;
- JSON-output and option booleans such as whether a search used filters;
- bucketed counts such as indexed sessions, import totals, result counts, and
  validation finding counts;
- provider identifiers such as `codex` or `claude` when selected as filters;
- coarse Cloudflare-derived geography such as country, region, colo, ASN, and
  AS organization.

To disable analytics, add:

```toml
[analytics]
enabled = false
```

Equivalent environment opt-outs are `CTX_ANALYTICS_OFF=1`,
`CTX_DISABLE_ANALYTICS=1`, or `CTX_ANALYTICS_ENABLED=false`. Use an opt-out when
a strict local-only no-network mode is required.
