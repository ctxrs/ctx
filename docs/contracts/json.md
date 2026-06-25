# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
output previews, and local paths. Treat it as private until a user reviews and
redacts it.

Command result JSON currently uses `schema_version: 1`. Progress-event JSON is
stderr telemetry and does not include `schema_version`.

## Setup

```bash
ctx setup --json
```

Writes local storage and returns:

- `schema_version`;
- `data_root`;
- `database_path`;
- `config_path`;
- `sources`;
- `network_required: false`;
- `repo_writes: false`.

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
- `local_only: true`.

## Sources

```bash
ctx sources --json
```

Writes nothing and returns:

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
- `raw_retention`;
- `unsupported_reason`.

`status` is `available`, `missing`, or `unsupported`. `import_support` is
`native`, `normalized_developer_only`, or `unsupported`. `native_import` is a
boolean derived from `import_support == "native"`. `unsupported_reason` is a
string for unsupported rows and otherwise null.

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
`ctx import --json --progress json` writes the import result object to stdout
and zero or more progress objects to stderr.

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

Progress events are operational telemetry, not durable result records. Consumers
should key on `type` and `operation`, ignore unknown fields, and read the final
command result from stdout when `--json` is present.

## List

```bash
ctx list --json
```

Writes nothing and returns:

- `schema_version`;
- `items[]`.

Items include:

- `id`, a compatibility alias for `item_id`;
- `item_id`, the opaque identifier to pass to `ctx show`;
- `item_type`, such as `agent_history` or `session`;
- fields available for that indexed item.

Session rows can include `provider`, `external_session_id`, `agent_type`,
`role`, `is_primary`, `status`, `started_at`, `ended_at`, `source_id`,
`source_path`, and `source_exists`.

## Show

```bash
ctx show <item-uuid> --json
```

Writes nothing and returns:

- `schema_version`;
- `item`;
- `events[]` for sessions and indexed items;
- `sessions[]` for indexed items.

`show --json` does not serialize raw store row shapes. Events are projected to
local/private previews with `event_id`, `item_id`, `item_type`, `session_id`,
`sequence`, `event_type`, `role`, `occurred_at`, `source_id`, `source_path`,
`source_exists`, `cursor`, `preview`, and `redaction_state`.

## Search

```bash
ctx search [query] --json
```

Writes nothing and returns:

- `schema_version`;
- `query`;
- `filters`;
- `generated_at`;
- `results[]`;
- `pagination`;
- `truncation`;
- `share_safe: false`.

Each result can include:

- `item_id`, the opaque item identifier used with `ctx show`;
- `item_type`, such as `agent_history`;
- `session_id`;
- `event_id`;
- `event_seq`;
- `title`;
- `snippet`;
- `rank`;
- `provider`;
- `timestamp`;
- `cwd`;
- `source_path`;
- `source_exists`;
- `cursor`;
- `why_matched`;
- `citations[]`;
- `links`;
- `visibility`.

## Citation Fields

Citations can include:

- `item_id`;
- `item_type`;
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

## Doctor And Validate

```bash
ctx doctor --json
ctx validate --json
```

Both commands read local storage and return findings:

- `doctor`: `schema_version`, `ok`, `findings`;
- `validate`: `schema_version`, `valid`, `findings`.

## Provider Smoke

Provider smoke tests call normal `ctx` commands with temporary local storage and
static fixtures. Their output is ordinary command JSON covered by the command
schemas above; there is no separate provider artifact schema in the public CLI.

## Compatibility Limits

`list --json` currently includes `id` as an alias for `item_id` because it was
part of the early local output. New agents should prefer `item_id`.
