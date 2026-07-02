# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
output previews, and local paths. Treat it as private until a user reviews and
redacts it.

Command result JSON currently uses `schema_version: 1`. Progress-event JSON is
stderr progress output and does not include `schema_version`.

## Setup

```bash
ctx setup --json
```

Writes local storage and returns:

- `schema_version`;
- `data_root`;
- `database_path`;
- `config_path`;
- `mode`, either `ready` or `catalog_only`;
- `indexed_items`;
- `sources`;
- `catalog`;
- `catalog_sources`;
- `import`;
- `network_required: false`;
- `repo_writes: false`.

`import.ran` is true for the default setup path and false for
`ctx setup --catalog-only`. When it runs, `import.totals` and `import.sources`
use the same shape as `ctx import --json`.

## Status

```bash
ctx status --json
```

Reads local storage state and returns:

- `schema_version`;
- `initialized`;
- `data_root`;
- `database_path`;
- `config_path`;
- `indexed_items`;
- `indexed_sources`;
- `cataloged_sessions`;
- `indexed_catalog_sessions`;
- `pending_catalog_sessions`;
- `failed_catalog_sessions`;
- `stale_catalog_sessions`;
- `local_only: true`.

## Sources

```bash
ctx sources --json
```

Returns:

- `schema_version`;
- `sources[]`.

Each source includes:

- `provider`;
- `path`;
- `exists`;
- `source_format`;
- `status`;
- `import_support`;
- `native_import`;
- `importable`;
- `raw_retention`;
- `unsupported_reason`.

`status` is `available`, `empty`, `unknown`, `missing`, or `unsupported`.
`import_support` is `native` or `unsupported`. `native_import` is a boolean
derived from `import_support == "native"`. `importable` is true only when the
source is both available and natively importable. `unknown` means the bounded
provider-specific transcript probe hit its scan budget before proving the
source available or empty. `unsupported_reason` is a string for unsupported,
empty, or unknown rows and otherwise null.

## Import

```bash
ctx import --json
```

Writes the local SQLite index and returns:

- `schema_version`;
- `resume`;
- `resume_mode`;
- `totals`;
- `sources[]`.

`totals` and each source row include file, byte, session, event, edge, skipped,
and failed counts. `resume_mode` is currently `idempotent_rescan` when
`--resume` is passed and `normal_scan` otherwise.

## Progress

```bash
ctx setup --progress json
ctx import --progress json
ctx import --json --progress json
```

`--progress json` writes newline-delimited progress objects to stderr for
`setup` and `import`. It does not change command result stdout. This means
`ctx setup --json --progress json` and `ctx import --json --progress json`
write the command result object to stdout and zero or more progress objects to
stderr.

Each progress object includes:

- `type: "ctx_progress"`;
- `operation`, currently `setup` or `import`;
- `phase`;
- `message`;
- `completed_bytes`;
- `total_bytes`;
- `percent`;
- `elapsed_seconds`;
- `eta_seconds`, nullable when no estimate is available or the operation is
  complete;
- `completed_files`, nullable;
- `total_files`, nullable;
- `imported_events`, nullable;
- `done`.

Progress events are operational status events, not durable result records.
Consumers should key on `type` and `operation`, ignore unknown fields, and read
the final command result from stdout when `--json` is present.

## Show

```bash
ctx show session <ctx-session-id> --format json
ctx show event <ctx-event-id> --format json
```

Writes nothing and returns:

- `schema_version`;
- `item_type`, either `session_transcript` or `event_window`;
- `mode` for session transcripts;
- `format`;
- `session` for session output;
- `event` for event output;
- `source`;
- `events[]`.

`session` includes the ctx-owned `item_id`, `provider`, and
`provider_session_id` when known. `event` and `events[]` rows include
`ctx_event_id`, `ctx_session_id`, `sequence`, `event_type`, `role`,
`occurred_at`, `source`, `cursor`, `text` or `preview`, and
`redaction_state`.

## Locate

```bash
ctx locate session <ctx-session-id> --format json
ctx locate event <ctx-event-id> --format json
```

Writes nothing and returns provenance metadata:

- `schema_version`;
- `item_type`, either `session_location` or `event_location`;
- `ctx_session_id`;
- `ctx_event_id` for event output;
- `provider`;
- `provider_session_id` when known;
- `source`;
- `resume`.

`source` includes `path`, `cursor`, `exists`, `source_id`, and
`source_format` when known. `resume` includes provider cursor or import resume
metadata when available.

## Transcript Artifacts

```bash
ctx show session <ctx-session-id> --mode full --format json --out transcript.json
```

With `--out`, writes the requested transcript artifact to that path and prints
nothing on success. Without `--out`, stdout is the requested transcript
artifact. JSON and JSONL artifact rows use the same ctx-owned ID fields as
`show`.

## Search

```bash
ctx search <query>|--term <term>|--file <path> --json
```

Returns:

- `schema_version`;
- `query`;
- `filters`;
- `freshness`;
- `generated_at`;
- `results[]`;
- `pagination`;
- `truncation`.

Each result can include:

- `ctx_event_id` for event hits;
- `ctx_session_id` when known;
- `provider_session_id`;
- `event_seq`;
- `title`;
- `snippet`;
- `rank`;
- `result_scope`, either `session` for a session-level result or `event` for an
  event-level result;
- `session_importance` for default session results;
- `more_matches_in_session` for default session results;
- `provider`;
- `timestamp`;
- `cwd`;
- `source_path`;
- `source_exists`;
- `cursor`;
- `why_matched`;
- `citations[]`;
- `suggested_next_commands[]`;
- `visibility`.

`why_matched[]` can include text, metadata, or touched-file reasons. A touched
file match is backed by normalized touched-file storage and can appear when
search uses `--file <path>` or when file-path metadata contributes to ranking.
`citations[]` can cite sessions, events, files, or source metadata depending on
which indexed item produced the match.

Search JSON is local/private by default and is not share-safe or redacted for
external publication.

`freshness` describes the pre-search refresh attempt:

- `mode`, one of `auto`, `off`, or `strict`;
- `status`, such as `completed`, `skipped`, `no_sources`,
  `skipped_large_index`, or `failed`;
- `source_count`;
- `totals`, using the same import total fields as `ctx import --json`;
- `error`, present when refresh failed but results were still served.

`suggested_next_commands` can include `ctx show event`, `ctx show session`,
`ctx search ... --session <ctx-session-id>`, `ctx locate event`, and
`ctx locate session` command strings when the required ctx IDs are known.

When ctx can identify the active Codex provider session through
`CODEX_THREAD_ID`, search filters include `exclude_provider_session` and omit
that active session tree by default. Passing `--include-current-session` removes
that filter.

## SQL

```bash
ctx sql "SELECT COUNT(*) AS sessions FROM ctx_sessions" --json
ctx sql --file query.sql --format json
```

Runs one read-only SQL statement against the existing local SQLite index and
returns:

- `schema_version`;
- `item_type: "sql_result"`;
- `read_only: true`;
- `share_safe: false`;
- `columns[]`, ordered selected column names;
- `rows[]`, ordered arrays matching `columns[]`;
- `returned_rows`;
- `truncated.rows`;
- `truncated.values`;
- `limits.max_rows`;
- `limits.max_columns`;
- `limits.max_value_bytes`;
- `limits.max_sql_bytes`;
- `limits.timeout_ms`;
- `elapsed_ms`.

Scalar SQL values are encoded as JSON nulls, numbers, or strings when they fit
the configured value cap. Truncated text values are encoded as objects with
`type: "text"`, `value`, `bytes`, and `truncated: true`. Blob values are
encoded as objects with `type: "blob"`, `bytes`, `preview_hex`, and
`truncated`.

Use stable `ctx_*` views for scripts when possible: `ctx_sessions`,
`ctx_events`, `ctx_files_touched`, and `ctx_sources`. Internal tables remain
queryable for advanced local inspection but are not the preferred compatibility
surface.

## MCP Tool Results

`ctx mcp serve` exposes read-only MCP tools over stdio for status, sources,
search, SQL, showing sessions, and showing events. Tool results include
`structuredContent` JSON using the same private local fields as CLI JSON. MCP
output may include absolute paths, source metadata, snippets, and transcript
text, and the MCP host may log or forward it.

MCP search does not refresh or import provider history. It also excludes the
active Codex session tree by default when `CODEX_THREAD_ID` is set; pass
`include_current_session: true` to opt back in.

The MCP `sql` tool uses the same `sql_result` JSON contract as `ctx sql
--json`, always read-only.

## Docs

```bash
ctx docs list --json
ctx docs search <query> --json
ctx docs show <topic> --format json
```

`ctx docs list --json` returns:

- `schema_version`;
- `topics[]`.

Each topic includes `id`, `title`, `audience`, `summary`, `tags`, and
`source_path`.

`ctx docs search <query> --json` returns:

- `schema_version`;
- `query`;
- `results[]`.

Each result uses the topic fields above and adds `score`.

`ctx docs show <topic> --format json` returns one topic object plus:

- `schema_version`;
- `body`, containing the embedded markdown source.

Docs JSON is generated from embedded static docs and does not read provider
history or SQLite.

## Upgrade

```bash
ctx upgrade --json
ctx upgrade --dry-run --json
ctx upgrade check --json
ctx upgrade status --json
```

`ctx upgrade` and `ctx upgrade check` return:

- `schema_version`;
- `command`, either `upgrade` or `upgrade_check`;
- `ok`;
- `status`, such as `available`, `up_to_date`, `dry_run`, `applied`, or
  `scheduled`;
- `message`;
- `current_version`;
- `latest_version`;
- `update_available`;
- `channel`;
- `platform`;
- `metadata_url`;
- `artifact_url`;
- `install_path`;
- `managed`;
- `applied`;
- `dry_run`;
- `warnings[]`.

`ctx upgrade status --json` returns:

- `schema_version`;
- `command: "upgrade_status"`;
- `state`;
- `install`.

`state` is the last local upgrade-state object when present, or
`status: "never_checked"`. `install.managed` is true only when the running
binary has a matching official installer sidecar. Unmanaged installs report
`managed: false` and a `reason`.

Background upgrade checks do not write JSON to stdout. They write
`upgrade-state.json` and `logs/upgrade.log` under the ctx data root. Windows
self-upgrade can report `scheduled` with `applied: false` while a helper waits
for the running `ctx.exe` to exit and then replaces the binary and sidecar.

## Citation Fields

Citations can include:

- `item_id`;
- `item_type`;
- `ctx_event_id`;
- `ctx_session_id`;
- `label`;
- `time`;
- `provider`;
- `session_id`;
- `event_seq`;
- `source_path`;
- `source_exists`;
- `cursor`.

`source_exists: false` means indexed text is available but the raw source
was not present at the stored path when checked.

## Doctor

```bash
ctx doctor --json
```

Reads local storage and returns findings:

- `schema_version`;
- `ok`;
- `findings`.

## Provider Smoke

Provider smoke tests call normal `ctx` commands with temporary local storage and
static fixtures. Their output is ordinary command JSON covered by the command
schemas above; there is no separate provider artifact schema in the public CLI.

## Compatibility Limits

Compatibility `item_id`, `id`, `session_id`, and `event_id` fields can remain
in some outputs. New integrations should prefer ctx-owned `ctx_session_id` and
`ctx_event_id` where present, and should treat provider-owned IDs as metadata
unless an explicit provider lookup flag is present.
