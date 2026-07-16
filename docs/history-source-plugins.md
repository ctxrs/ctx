# History Source Plugins

History source plugins let local tools make their histories searchable in ctx
without ctx owning their storage schemas.

A plugin integration works like this:

1. A local manifest declares one or more history sources.
2. ctx invokes enabled auto-refresh commands during search refresh, or any
   selected source during explicit import.
3. The command writes `ctx-history-jsonl-v1` records to stdout.
4. The stream is checked and imported as one batch.
5. ctx passes the previous source cursor back on the next run.

Plugins run as local commands. ctx does not load plugin code in-process or
operate a plugin store. Plugin authors own their native JSONL, SQLite, or API
reads. ctx owns the manifest, cursor handoff, validation, import, and search
index.

## Install And Discover

Put a manifest at one of:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- any directory or manifest file listed in `CTX_HISTORY_PLUGIN_PATH`.

`ctx sources` and `ctx sources --json` list plugin sources without executing
their commands. Invalid installed manifests are listed as non-importable
`history_source_plugin` rows so authors can diagnose broken local config.

Manifest example:

```json
{
  "schema_version": 1,
  "name": "example-agent",
  "display_name": "Example Agent history",
  "version": "0.1.0",
  "history_sources": [
    {
      "id": "default",
      "provider_key": "example-agent",
      "source_id": "default",
      "source_format": "example-agent-sqlite-v1",
      "import_revision": 1,
      "enabled": true,
      "refresh": "auto",
      "command": ["example-agent-to-ctx", "export"],
      "timeout_seconds": 300
    }
  ]
}
```

`name`, `id`, `provider_key`, and `source_id` must be stable lowercase ASCII
identifiers. `command` is an argv array; ctx does not run it through a shell.
`import_revision` is an optional positive integer and defaults to `1`. Bump it
when the source parser changes in a way that invalidates the previous cursor;
the new revision receives a distinct cursor stream.

`enabled: true` means `ctx import --all` may run that source. `refresh: auto`
means `ctx search` may run it during the normal pre-search refresh. Explicit
imports can run a discovered source even when it is not enabled or is marked
`refresh: manual`.

## Import

```bash
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --all
ctx import --history-source example-agent/default --reset-cursor
```

Selectors match `plugin/source` or `provider_key/source_id`, and must resolve
to exactly one source before ctx executes a command. ctx does not accept bare
plugin names, bare source ids, or bare provider keys because many integrations
use ids like `default`.

`--history-source-manifest` is a development path: it adds that manifest for the
current command without installing it. With no selector, ctx imports sources
from the supplied manifest path.

`--reset-cursor` withholds the previous cursor and sets
`CTX_HISTORY_FULL_RESCAN=1`. The plugin should emit a fresh `source.cursor.after`
checkpoint if the rescan succeeds; ctx rejects reset runs that do not emit a
new after checkpoint so an old cursor cannot be reused accidentally.

`ctx setup` does not execute plugins. `ctx search` defaults to
`--refresh background` and lets daemon maintenance refresh discovered plugin
sources when they are both `enabled: true` and `refresh: auto`. If daemon
maintenance is disabled, foreground background refresh applies a short runtime
cap so search can serve the existing index promptly. `--refresh off` never runs
plugins, and `--refresh wait` fails if an auto plugin refresh fails while using
the normal configured timeout. `ctx import` also uses the normal configured
timeout. Plugin refresh is incremental because ctx passes the previously stored
source cursor before invoking the command.

Search can be limited to a custom history source after import:

```bash
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
ctx search "release notes" --source-format example-agent-sqlite-v1
```

These filters imply `--provider custom`; combining them with another provider is
an error.

## Runtime Environment

ctx sets these variables before invoking a plugin command:

- `CTX_DATA_ROOT`
- `CTX_HISTORY_PLUGIN=1`
- `CTX_HISTORY_PLUGIN_NAME`
- `CTX_HISTORY_PLUGIN_MANIFEST`
- `CTX_HISTORY_SOURCE`, such as `example-agent/default`
- `CTX_HISTORY_SOURCE_ID`
- `CTX_HISTORY_PROVIDER_KEY`
- `CTX_HISTORY_SOURCE_FORMAT`
- `CTX_HISTORY_IMPORT_REVISION`
- `CTX_HISTORY_CURSOR_STREAM`
- `CTX_HISTORY_MACHINE_ID`
- `CTX_HISTORY_FULL_RESCAN`, `1` or `0`
- `CTX_HISTORY_CURSOR`, when a previous cursor exists and is small enough for
  inline environment handoff
- `CTX_HISTORY_CURSOR_FILE`, a temporary file containing the cursor

Use `CTX_HISTORY_CURSOR_FILE` for large native cursor maps. The file exists only
while the plugin process runs and is the reliable cursor handoff path.

The plugin must write only `ctx-history-jsonl-v1` JSONL to stdout. Progress and
diagnostics belong on stderr. If the command exits nonzero or stdout is invalid,
ctx imports nothing from that run and does not advance the cursor.
stdout is capped at 64 MiB per run and stderr at 256 KiB, so plugins should emit
incremental batches from the supplied cursor instead of full historical dumps
during normal refresh.

Plugin commands receive a limited inherited environment by default: `PATH`,
`HOME`, basic locale variables, temporary-directory variables, and XDG data or
config homes. Put provider-specific environment values in the manifest `env`
object instead of relying on the parent shell.

## Cursor Contract

The plugin controls the cursor string. It may be a number, an opaque token, or a
JSON string. ctx stores it under a stable custom stream derived from:

- `provider_key`
- `source_id`
- `source_format`
- `import_revision`, when greater than the backward-compatible default of `1`

The local machine id is stored separately with the cursor so multiple machines
can import the same custom source without overwriting each other's progress.

On the next import, ctx passes the stored `cursor.after.cursor` value back in
the runtime environment. This keeps native cursor design inside the provider
adapter:

- file appenders can use byte offsets;
- SQLite stores can use row ids;
- split stores can use JSON maps keyed by session id, file path, or direction.

Every plugin run should emit a `source` record matching the manifest
`provider_key`, `source_id`, and `source_format`. ctx rejects mismatches before
writing imported rows.

## Common Storage Shapes

Use the cursor format that matches your native storage. ctx treats it as an
opaque string and passes it back on the next run.

### Append-Only Files

For one JSONL transcript per session, read each file from the last imported byte
offset and store a cursor keyed by path:

```json
{"files":{"/home/me/.example-agent/sessions/a.jsonl":{"offset":12345,"size":13000,"mtimeMs":1780000000000}}}
```

If a file shrinks or its fingerprint changes, rescan that file from the
beginning and emit the same stable session and event IDs.

### SQLite

For a local database with monotonic message IDs, read rows above the previous
high-water mark and advance the cursor to the largest imported ID:

```json
{"message_id":1234}
```

Use a second field if session metadata has its own reliable update marker:

```json
{"message_id":1234,"session_updated_at":"2026-07-01T12:00:00Z"}
```

### Split Stores

Some tools keep session metadata in one place and transcripts somewhere else.
Use a cursor map for each moving part:

```json
{"sessions_version":17,"transcripts":{"/home/me/.example-agent/transcripts/a.jsonl":{"offset":456,"size":900}}}
```

The plugin can change how it reads native storage later without changing the ctx
manifest or stdout contract.

### Local APIs Or Commands

If the tool already has an export command or local API, call that API and store
its sync token:

```json
{"sync_token":"opaque-provider-token"}
```

## Minimal Plugin Pseudocode

```python
import json, os, pathlib, sqlite3, sys

cursor_text = os.environ.get("CTX_HISTORY_CURSOR")
if not cursor_text and os.environ.get("CTX_HISTORY_CURSOR_FILE"):
    cursor_text = pathlib.Path(os.environ["CTX_HISTORY_CURSOR_FILE"]).read_text()
cursor = json.loads(cursor_text or "{}")
after_message_id = cursor.get("message_id", 0)
db = sqlite3.connect(os.path.expanduser("~/.example-agent/state.db"))

print(json.dumps({"record_type": "manifest", "schema_version": "ctx-history-jsonl-v1"}))
print(json.dumps({
    "record_type": "source",
    "source_id": os.environ["CTX_HISTORY_SOURCE_ID"],
    "provider_key": os.environ["CTX_HISTORY_PROVIDER_KEY"],
    "source_format": os.environ["CTX_HISTORY_SOURCE_FORMAT"],
    "cursor": {
        "after": {
            "stream": os.environ["CTX_HISTORY_CURSOR_STREAM"],
            "cursor": json.dumps({"message_id": after_message_id}),
            "observed_at": "2026-07-01T12:00:00Z"
        }
    }
}))

for row in db.execute("SELECT id, session_id, role, content, timestamp FROM messages WHERE id > ? ORDER BY id", (after_message_id,)):
    # Emit session records as needed, then event records with stable event_index.
    pass
```
