# JSON Contracts

ctx JSON is for local agents and scripts. It can include prompts, command
output previews, and local paths. Treat it as private until a user reviews and
redacts it.

All JSON commands currently use `schema_version: 1`.

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
- `raw_retention`.

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

## Live Provider E2E Artifacts

```bash
scripts/release-provider-live-e2e-lanes.sh run codex
scripts/release-provider-live-e2e-lanes.sh run pi
scripts/release-provider-live-e2e-lanes.sh run-selected
```

These manual artifacts are not raw `ctx` command output. They are redacted
summaries written only after explicit native local-history opt-in.

`live-e2e.json` returns:

- `schema_version`;
- `kind`;
- `publishing: false`;
- `provider` and `display_name` for provider runs;
- `status`, such as `passed`, `skipped`, `blocked`, or `failed`;
- `evidence_class: "manual_opt_in_local_history"` for passing Codex/Pi
  local-history provider runs;
- `provider_command_execution: false`;
- `api_key_env_passed_to_ctx: false`;
- `temporary_ctx_data_root: true` for passing provider runs;
- redaction flags for raw transcripts, snippets, queries, source paths, and raw
  ctx command output;
- aggregate import counts;
- aggregate search result counts;
- aggregate health counts and booleans;
- git commit, branch, and generated timestamp.

The selected runner writes a root `live-e2e.json` with selected, passed,
skipped, blocked, and failed provider counts, plus per-provider artifacts in
subdirectories.

The artifacts must not include raw transcripts, snippets, queries, API keys, or
raw source paths. Missing opt-in writes `skipped`; fixture-only providers write
`blocked`.

## Compatibility Limits

`list --json` currently includes `id` as an alias for `item_id` because it was
part of the early local output. New agents should prefer `item_id`.
