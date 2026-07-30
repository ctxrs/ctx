# History Source Plugins

History source plugins let local tools export their history into ctx without
ctx owning those tools' storage schemas. The stable 1.0 route is intentionally
narrow: a manifest identifies a durable provider-owned
`ctx-history-jsonl-v1` file, and the CLI registers that file with the normal
daemon-owned source-backed generation path.

ctx does not load plugin code in-process and does not provide a second history
store for plugins. The declared provider file remains the exact-body authority;
the source-backed index is a derived search structure. Command-only exporters
are discoverable but unsupported in 1.0 because command stdout is not a durable
provider source.

## Install And Discover

Put a manifest at one of:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- any directory or manifest file listed in `CTX_HISTORY_PLUGIN_PATH`.

`ctx sources` and `ctx sources --format json` discover manifests without
executing commands. A durable regular-file source is reported as `available`,
`importable: true`, and `import_mode: explicit_source_backed`. Its
`history_source` is the filterable `provider_key/source_id` route identity;
`plugin_source` is the `plugin/source` import-selection alias. Command-only
compatibility sources are reported as `unsupported` and never importable.
Invalid manifests are listed as non-importable `history_source_plugin` rows
with their validation error.

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
      "source_format": "example-agent-jsonl-v1",
      "path": "/path/owned/by/example-agent/history.jsonl",
      "enabled": true,
      "refresh": "manual"
    }
  ]
}
```

`name`, `id`, `provider_key`, and `source_id` must be stable lowercase ASCII
identifiers. `path` may be absolute or relative to the manifest directory and
must identify a regular provider-owned file. Its `ctx-history-jsonl-v1` source
record must match the declared `provider_key`, `source_id`, and
`source_format`.

Legacy manifests may still declare a `command` argv array so users receive a
typed compatibility diagnostic. They are not importable, are never executed by
`ctx sources`, and are never copied into ctx-owned storage. A source cannot
declare both `path` and command runtime options.

`enabled` and `refresh` remain discovery metadata in the 1.0 explicit route.
They do not opt a plugin into `ctx import --all`, `ctx setup`, or automatic
pre-search execution.

## Explicit Import

Select exactly one source:

```bash
ctx import --history-source example-agent/default
ctx import --history-source-manifest ./ctx-history-plugin.json
```

Selectors match either `plugin/source` or `provider_key/source_id` and must
resolve to exactly one source before ctx executes anything. Bare plugin names,
source ids, and provider keys are rejected because values such as `default`
are commonly reused.

`--history-source-manifest` adds a development manifest for the current
command. Without `--history-source`, the supplied manifest path must resolve to
exactly one source.

The 1.0 plugin route does not execute plugins from `ctx import --all` or
automatic search refresh. Run an explicit plugin import after configuring its
durable path; the persistent daemon then watches and refreshes the registered
provider source normally. `--reset-cursor` is invalid because ctx owns no
plugin cursor.

## Source-Backed Publication

An accepted import has one authority path:

1. The CLI validates that the selected manifest identifies a regular
   provider-owned file.
2. A bounded header check verifies the `ctx-history-jsonl-v1` schema and exact
   declared source identity without copying the body.
3. ctx registers that same file as the explicit custom
   `ctx_history_jsonl_v1` source route.
4. The normal source-refresh endpoint publishes a fresh source-backed
   generation, or returns an authoritative no-op receipt.

Cold imports, appends, rewrites, replacements, and no-ops all use the shared
custom JSONL source-family path. There is no fallback to the old Store database,
synthetic `NativePath` body, command-output snapshot, or local content pack.
`ctx show ... --content complete` rehydrates exact event content from the
provider-owned file and fails closed if that source no longer verifies.

After import, `--history-source` uses the canonical
`provider_key/source_id` route identity:

```bash
ctx search "release notes" --history-source example-agent/default
ctx search "release notes" --provider-key example-agent --source-id default
```

These filters imply `--provider custom`; combining them with another provider
is an error. When plugin/source differs from provider_key/source_id, use the
latter for search; the former remains the explicit import selector.

## Minimal Durable Source Shape

```python
import json

print(json.dumps({
    "record_type": "manifest",
    "schema_version": "ctx-history-jsonl-v1",
}))
print(json.dumps({
    "record_type": "source",
    "source_id": "default",
    "provider_key": "example-agent",
    "source_format": "example-agent-jsonl-v1",
}))

# Append stable session, event, file_touch, and edge records to the provider file.
```

The complete record schema is documented in
`docs/custom-history-import-format.md`.
