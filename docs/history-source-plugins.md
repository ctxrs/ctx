# History Source Plugins

History source plugins let third-party tools make their local histories
searchable in ctx without ctx owning their storage schemas.

The narrow waist is:

1. A local manifest declares one or more history sources.
2. ctx invokes the declared command only during explicit import.
3. The command writes `ctx-history-jsonl-v1` records to stdout.
4. ctx validates and imports that stream atomically.
5. ctx passes the previous source cursor back on the next run.

Plugins are command-line adapters, not an in-process ABI and not a hosted plugin
store. Plugin authors own their native JSONL, SQLite, or API reads. ctx owns the
manifest, cursor handoff, validation, import, and search index.

## Install And Discover

Put a manifest at one of:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- any directory or manifest file listed in `CTX_HISTORY_PLUGIN_PATH`;
- any directory or manifest file listed in `CTX_PLUGIN_PATH`.

`ctx sources` and `ctx sources --json` list plugin sources without executing
their commands.

Manifest example:

```json
{
  "schema_version": 1,
  "name": "dorkos",
  "display_name": "DorkOS history",
  "version": "0.1.0",
  "history_sources": [
    {
      "id": "default",
      "provider_key": "dorkos",
      "source_id": "default",
      "source_format": "dorkos-claude-jsonl-v1",
      "enabled": true,
      "command": ["ctx-history-source-dorkos", "export"],
      "timeout_seconds": 300
    }
  ]
}
```

`name`, `id`, `provider_key`, and `source_id` must be stable lowercase ASCII
identifiers. `command` is an argv array; ctx does not run it through a shell.

`enabled: true` means `ctx import --all` may run that source. Explicit imports
can run a discovered source even when it is not enabled.

## Import

```bash
ctx import --history-source dorkos
ctx import --plugin dorkos
ctx import --history-source dorkos/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --plugin-manifest ./ctx-history-plugin.json
ctx import --all
ctx import --history-source hermes --reset-cursor
```

Selectors can match plugin name, source id, `plugin/source`, `provider_key`, or
`provider_key/source_id`, but they must resolve to exactly one source before ctx
executes a command. Prefer `plugin/source` when a machine has multiple plugins.

`--history-source-manifest` is a development path: it adds that manifest for the
current command without installing it. With no selector, ctx imports sources
from the supplied manifest path.

`--reset-cursor` withholds the previous cursor and sets
`CTX_HISTORY_FULL_RESCAN=1`. The plugin should emit a fresh `source.cursor.after`
checkpoint if the rescan succeeds.

`ctx setup` and search refresh do not execute plugins in this version. Run
`ctx import --history-source <id>` or `ctx import --all` to catch up
plugin-backed sources before searching.

## Runtime Environment

ctx sets these variables before invoking a plugin command:

- `CTX_DATA_ROOT`
- `CTX_HISTORY_PLUGIN=1`
- `CTX_HISTORY_PLUGIN_NAME`
- `CTX_HISTORY_PLUGIN_MANIFEST`
- `CTX_HISTORY_SOURCE`, such as `dorkos/default`
- `CTX_HISTORY_SOURCE_ID`
- `CTX_HISTORY_PROVIDER_KEY`
- `CTX_HISTORY_SOURCE_FORMAT`
- `CTX_HISTORY_CURSOR_STREAM`
- `CTX_HISTORY_MACHINE_ID`
- `CTX_HISTORY_FULL_RESCAN`, `1` or `0`
- `CTX_HISTORY_CURSOR`, when a previous cursor exists and is small enough for
  inline environment handoff
- `CTX_HISTORY_CURSOR_JSON`, same value as `CTX_HISTORY_CURSOR` when set
- `CTX_HISTORY_CURSOR_FILE`, a temporary file containing the cursor

Use `CTX_HISTORY_CURSOR_FILE` for large native cursor maps. The file exists only
while the plugin process runs and is the reliable cursor handoff path.

The plugin must write only `ctx-history-jsonl-v1` JSONL to stdout. Progress and
diagnostics belong on stderr. If the command exits nonzero or stdout is invalid,
ctx imports nothing from that run and does not advance the cursor.

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
- local machine id

On the next import, ctx passes the stored `cursor.after.cursor` value back in
the runtime environment. This keeps native cursor design inside the provider
adapter:

- file appenders can use byte offsets;
- SQLite stores can use row ids;
- split stores can use JSON maps keyed by session id, file path, or direction.

Every plugin run should emit a `source` record matching the manifest
`provider_key`, `source_id`, and `source_format`. ctx rejects mismatches before
writing imported rows.

## Adapter Shapes

The local research checkouts showed four different storage models, which is why
ctx should not maintain native adapters for them.

### DorkOS

DorkOS currently derives history from Claude SDK JSONL files under
`~/.claude/projects/<slug>/*.jsonl`. A DorkOS plugin should read those files by
byte offset and use a cursor like:

```json
{"files":{"/home/me/.claude/projects/x/session.jsonl":{"offset":12345,"size":13000,"mtimeMs":1780000000000}}}
```

The plugin can enrich events with DorkOS metadata from `~/.dork/dork.db`, but
the transcript source is still the Claude JSONL file.

### OpenClaw

OpenClaw currently has session metadata under
`~/.openclaw/agents/<agentId>/sessions/sessions.json` and transcript JSONL
files beside it. A plugin should use OpenClaw's session accessor where possible,
resolve transcript paths, and cursor by byte offset:

```json
{"backend":"openclaw-file","transcripts":{"/home/me/.openclaw/agents/a/sessions/s.jsonl":{"offset":456,"size":900,"lastRecordId":"rec-2"}}}
```

If OpenClaw flips storage to SQLite, the OpenClaw-owned plugin can keep the same
ctx stdout contract while changing its native reader.

### Hermes

Hermes Agent stores canonical history in `~/.hermes/state.db`. A Hermes plugin
should read `sessions` and `messages` read-only, order by `messages.id`, and
cursor by the maximum message row id:

```json
{"message_id":1234}
```

Session metadata-only changes may need a second cursor if Hermes exposes a
reliable session update high-water mark.

### NanoClaw

NanoClaw uses a central `data/v2.db` plus per-session inbound and outbound
SQLite databases under `data/v2-sessions/<agent_group_id>/<session_id>/`.
Inbound messages use even `seq` values and outbound messages use odd `seq`
values. A generic NanoClaw plugin can cursor by per-session sequence:

```json
{"sessions":{"sess-abc":42,"sess-def":8}}
```

Provider-specific NanoClaw plugins can instead read mounted provider state, such
as Claude JSONL, when they need full internal tool/thinking events.

## Minimal Plugin Pseudocode

```python
import json, os, sqlite3, sys

cursor = json.loads(os.environ.get("CTX_HISTORY_CURSOR_JSON") or "{}")
after_message_id = cursor.get("message_id", 0)
db = sqlite3.connect(os.path.expanduser("~/.hermes/state.db"))

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
