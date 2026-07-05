# Storage And Privacy

ctx stores search indexes locally. Treat the ctx data root like private source
history.

## Local Layout

Default root:

```text
~/.ctx/
  work.sqlite
  config.toml
  upgrade-state.json
  upgrade.lock
  logs/
    upgrade.log
```

`CTX_DATA_ROOT` or `--data-root` may point ctx somewhere else. The configured
root is used directly; ctx does not append another directory.

Official installer-managed binaries also have a sidecar next to the installed
binary, for example:

```text
~/.local/bin/ctx
~/.local/bin/ctx.install.json
```

The sidecar is outside the ctx data root because it describes ownership of the
installed executable, not indexed provider history.

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

Provider-specific sensitive handles should stay out of normalized metadata when
they are not needed for local search. For example, the Warp SQLite importer
records only boolean presence for Warp server conversation tokens and does not
copy token values from `agent_conversations.conversation_data`.

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
| `ctx sources` | known provider paths under the user's home and local history-source plugin manifests | none |
| `ctx import` | provider transcript files and path metadata, the explicit custom history JSONL file passed with `--format ctx-history-jsonl-v1 --path`, or stdout from an explicit history-source plugin command | data root, `config.toml` if missing, and SQLite index |
| `ctx show` | SQLite index | selected `--out` path for `show session` when provided |
| `ctx locate` | SQLite index and raw source path metadata | none |
| `ctx search` | native provider transcript files, path metadata, enabled auto history-source plugin stdout, and SQLite index | SQLite index for newly discovered native provider or plugin history |
| `ctx sql` | existing SQLite index only | none |
| `ctx docs` | embedded documentation in the binary | selected topic `--out` path for `ctx docs show --out` or selected `--out` directory for `ctx docs man --out` |
| `ctx upgrade` | signed release metadata and installed binary/sidecar metadata | installed binary for manual upgrade, install sidecar, `upgrade-state.json`, `upgrade.lock`, and `logs/upgrade.log` |
| `ctx doctor` | SQLite index and data root metadata | none |

Setup, import, and search do not require source repository writes, model APIs,
API keys, or remote accounts.

## Default Config

`ctx setup` creates `~/.ctx/config.toml` when the default root is used, or
`config.toml` under the configured data root when `CTX_DATA_ROOT` or
`--data-root` points elsewhere. Existing config files are left in place.

The day-1 generated config is:

```toml
[upgrade]
auto = "apply"
channel = "stable"
interval_hours = 24
```

`upgrade.auto = "apply"` only takes effect for official installer-managed
binaries with a valid install sidecar. Unmanaged installs do not self-upgrade.
Set `auto = "off"` or use `ctx upgrade disable` to disable background
auto-upgrade for the configured data root.

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
ctx import --provider codex --path ~/.codex/sessions
ctx import --format ctx-history-jsonl-v1 --path ./history.jsonl
ctx import --history-source example-agent/default
```

Current adapters are safe to re-run. They rescan sources idempotently and keep
source paths or cursors when available.
Custom history JSONL imports follow the same v1 lifecycle: ctx rescans the
explicit file, upserts already-imported records, stores supplied source cursor
metadata under ctx-owned custom cursor streams, and preserves event native
cursors. History-source plugins receive the previous stored cursor on each
explicit import and stream the same JSONL format to stdout. Failed plugin runs
do not advance cursors. Explicit file paths and plugin manifests are not added
to `config.toml` or treated as fixed provider homes.

## Upgrade Reindexing

When an existing `0.8.x` or `0.9.x` data root is opened by `0.10.x` or newer, ctx keeps
the SQLite database and migrates it in place. The migration rebuilds derived
search projections and marks prior provider import cache rows pending so the
next normal refresh can re-read original provider transcripts.

This is a one-time reimport, not a destructive wipe. It is needed because older
indexes can lack touched-file metadata or can contain text that was sanitized
before storage. If the original provider transcript files still exist, refresh
replaces those old rows with current local/private transcript text. If source
files were deleted or moved, ctx can still return indexed text from SQLite but
cannot reconstruct text that was already stored as a placeholder.

Remove a source from future imports:

```bash
$EDITOR ~/.ctx/config.toml
```

The current CLI does not add provider source entries to `config.toml`; default
provider locations are discovered each time and explicit `--path` imports are
not remembered as future defaults. Custom history JSONL paths are also
one-shot explicit imports. To remove already indexed data, rebuild the index and
import only the sources you still want.

## SQL Inspection

`ctx sql` is a read-only advanced inspection command for cases normal search
does not express, such as exact counts, joins, audits, and one-off scripts. It
opens the existing SQLite store in read-only mode, rejects writes, rejects
multiple statements, enforces row/column/value caps, and times out long-running
queries. It also applies SQLite runtime limits to bound SQL text and generated
value allocation. It does not initialize or migrate the store; run `ctx
status`, `ctx setup`, or `ctx import` first when a schema migration is required.

Stable read-only views are the preferred compatibility surface:

- `ctx_sessions`;
- `ctx_events`;
- `ctx_files_touched`;
- `ctx_sources`.

Run `ctx docs show sql` for view schemas, examples, limits, and output formats.
Internal tables remain local and queryable, but they are implementation details
and can change across versions. SQL output is private local history by default.

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
The persisted `safe_preview` redaction state and `safe_preview_text` search
columns are legacy local-index names for searchable previews; they do not mean
the stored text has been redacted for sharing.

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

Official installer-managed binaries can contact the signed release metadata
endpoint for `ctx upgrade` and for background auto-upgrade checks after
successful normal commands. These checks are skipped for JSON commands, MCP,
`ctx docs`, `ctx sql`, `ctx upgrade`, CI, unmanaged installs, and process-level
opt-outs such as `CTX_UPGRADE_OFF=1` or `CTX_DISABLE_AUTO_UPGRADE=1`. Upgrade
metadata checks do not send provider transcript text, search queries, result
snippets, source paths, repository names, or command output.

First-party analytics are default-on and may create `install.json` plus a
separate device identity file in OS user state, then send coarse product
metadata. They do not send session text, prompts, transcripts, search queries,
result snippets, source paths, repository or branch names, native session IDs,
command text, command output, usernames, hostnames, raw IP addresses, or
hardware-derived machine fingerprints.

Analytics may include:

- generated random install and device identifiers that are hashed server-side;
- ctx version, OS, architecture, command name, success state, and duration
  bucket;
- JSON-output and option booleans such as whether a search used filters;
- bucketed counts such as indexed sessions, import totals, result counts, and
  validation finding counts;
- bucketed search query length and term count, but not query content;
- provider identifiers such as `codex` or `claude` when selected as filters;
- coarse Cloudflare-derived geography such as country, region, colo, ASN, and
  AS organization.

The install identifier lives in `install.json` under the configured ctx data
root and represents that local index. The device identifier is a random UUID
created only when analytics are enabled and an event is sent; it lives outside
the ctx data root in OS user state, such as `$XDG_STATE_HOME/ctx/device.json` or
`~/.local/state/ctx/device.json` on Linux.

`ctx sql` and MCP do not send first-party analytics events.

To disable analytics, add:

```toml
[analytics]
enabled = false
```

Equivalent environment opt-outs are `CTX_ANALYTICS_OFF=1`,
`CTX_DISABLE_ANALYTICS=1`, or `CTX_ANALYTICS_ENABLED=false`. Use an opt-out when
a strict local-only no-network mode is required.
