# History Source Plugins

History source plugins let local tools export their history into ctx without
ctx owning those tools' storage schemas. The stable 1.0 route is intentionally
narrow: a user explicitly selects one manifest source, the source command emits
`ctx-history-jsonl-v1`, and the CLI publishes that provider export through the
normal daemon-owned source-backed generation path.

ctx does not load plugin code in-process and does not provide a second history
store for plugins. The managed provider export remains the exact-body
authority; the source-backed index is a derived search structure.

## Install And Discover

Put a manifest at one of:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- any directory or manifest file listed in `CTX_HISTORY_PLUGIN_PATH`.

`ctx sources` and `ctx sources --format json` discover manifests without
executing commands. A valid source is reported as `available`, `importable:
true`, and `import_mode: explicit_source_backed`. Its `history_source` is the
filterable `provider_key/source_id` route identity; `plugin_source` is the
`plugin/source` import-selection alias. Invalid manifests are listed as
non-importable `history_source_plugin` rows with their validation error.

Manifest example:

```json
{
  "schema_version": 1,
  "name": "example-agent",
  "display_name": "Example Agent history",
  "version": "1.0.0",
  "history_sources": [
    {
      "id": "default",
      "provider_key": "example-agent",
      "source_id": "default",
      "source_format": "example-agent-sqlite-v1",
      "enabled": true,
      "refresh": "manual",
      "command": ["example-agent-to-ctx", "export"],
      "timeout_seconds": 300
    }
  ]
}
```

`name`, `id`, `provider_key`, and `source_id` must be stable lowercase ASCII
identifiers. `command` is an argv array; ctx never runs it through a shell.
`working_dir` may be absolute or relative to the manifest directory. Explicit
`env` entries are supported after manifest validation.

`enabled` and `refresh` remain discovery metadata in the 1.0 explicit route.
They do not opt a plugin into `ctx import --all`, `ctx setup`, or automatic
pre-search execution.

## Explicit Import

Select exactly one source:

```bash
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
ctx import --history-source example-agent/default --reset-cursor
```

Selectors match either `plugin/source` or `provider_key/source_id` and must
resolve to exactly one source before ctx executes anything. Bare plugin names,
source ids, and provider keys are rejected because values such as `default`
are commonly reused.

`--history-source-manifest` adds a development manifest for the current
command. Without `--history-source`, the supplied manifest path must resolve to
exactly one source.

The 1.0 plugin route does not execute plugins from `ctx import --all` or
automatic search refresh. Run an explicit plugin import when its native
history changes, then search the published generation with refresh disabled or
with the normal provider refresh behavior.

## Source-Backed Publication

An accepted import has one authority path:

1. ctx runs the selected command with bounded stdout, stderr, and runtime.
2. The importer validates one `ctx-history-jsonl-v1` manifest and source
   record, source identity, cursor identity, record references, and parent
   relationships.
3. Incremental records are merged idempotently into a private managed
   provider-export JSONL source under
   `$CTX_DATA_ROOT/history-source-plugin-sources`.
4. ctx registers that file as the explicit custom
   `ctx_history_jsonl_v1` source route.
5. the normal source-refresh endpoint publishes a fresh source-backed
   generation, or returns an authoritative no-op receipt;
6. only after that receipt does ctx commit the plugin cursor.

Cold imports, appends, rewrites, resets, and no-ops all use this path. There is
no fallback to the old Store database or to a synthetic `NativePath` body.
`ctx show ... --content complete` rehydrates exact event content from the
managed provider export and fails closed if that source no longer verifies.

After import, `--history-source` uses the canonical
`provider_key/source_id` route identity:

```bash
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
```

These filters imply `--provider custom`; combining them with another provider
is an error. When plugin/source differs from provider_key/source_id, use the
latter for search; the former remains the explicit import selector.

## Runtime Environment

ctx clears the inherited environment, restores a small allowlist, applies the
manifest `env` object, and sets:

- `CTX_DATA_ROOT`
- `CTX_HISTORY_PLUGIN=1`
- `CTX_HISTORY_PLUGIN_NAME`
- `CTX_HISTORY_PLUGIN_MANIFEST`
- `CTX_HISTORY_SOURCE`, such as `example-agent/default`
- `CTX_HISTORY_SOURCE_ID`
- `CTX_HISTORY_PROVIDER_KEY`
- `CTX_HISTORY_SOURCE_FORMAT`
- `CTX_HISTORY_CURSOR_STREAM`
- `CTX_HISTORY_MACHINE_ID`
- `CTX_HISTORY_FULL_RESCAN`, `1` or `0`
- `CTX_HISTORY_CURSOR`, when a previous cursor is small enough for inline
  handoff
- `CTX_HISTORY_CURSOR_FILE`, a private temporary file when a previous cursor
  exists

The inherited allowlist covers `PATH`, home and user names, locale variables,
temporary-directory variables, and XDG config/data/cache/state roots.

The command must write only `ctx-history-jsonl-v1` JSONL to stdout. Diagnostics
belong on stderr. stdout is capped at 64 MiB, stderr at 256 KiB, and
`timeout_seconds` defaults to 300 seconds. A nonzero exit, timeout, oversized
output, malformed stream, identity mismatch, or reference error fails before
source registration and does not advance the cursor.

## Cursor Contract

The plugin owns the cursor string. It can be a byte offset, row id, JSON map,
or opaque native token. ctx binds the cursor to:

- `provider_key`
- `source_id`
- `source_format`
- the local ctx machine identity

Every run must emit a source record matching the manifest identity. When a
previous cursor exists, a `source.cursor.before` value, if emitted, must match
the supplied cursor and `CTX_HISTORY_CURSOR_STREAM`.
`source.cursor.after` advances the private sidecar only after daemon
publication. Transient cursor fields are not used as event-body storage.

`--reset-cursor` withholds the old cursor and sets
`CTX_HISTORY_FULL_RESCAN=1`. A reset run must emit a fresh
`source.cursor.after` checkpoint; otherwise ctx rejects it rather than risking
reuse of stale state.

## Minimal Exporter Shape

```python
import json, os

print(json.dumps({
    "record_type": "manifest",
    "schema_version": "ctx-history-jsonl-v1",
}))
print(json.dumps({
    "record_type": "source",
    "source_id": os.environ["CTX_HISTORY_SOURCE_ID"],
    "provider_key": os.environ["CTX_HISTORY_PROVIDER_KEY"],
    "source_format": os.environ["CTX_HISTORY_SOURCE_FORMAT"],
    "cursor": {
        "after": {
            "stream": os.environ["CTX_HISTORY_CURSOR_STREAM"],
            "cursor": "{\"message_id\":1234}",
            "observed_at": "2026-07-01T12:00:00Z",
        }
    },
}))

# Emit stable session, event, file_touch, and edge records for this increment.
```

The complete record schema is documented in
`docs/custom-history-import-format.md`.
