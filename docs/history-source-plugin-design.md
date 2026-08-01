# History Source Plugin Design

This document records the shipped 1.0 architecture for explicitly selected
history-source plugins.

## Goal And Boundary

Third-party tools can expose local history without ctx learning each native
storage schema. The plugin or provider owns a durable
`ctx-history-jsonl-v1` file. ctx owns:

- bounded manifest discovery and validation;
- explicit single-source selection;
- validation of the durable path, container schema, and source identity;
- registration of that provider-owned path with the custom source-backed route;
- daemon-owned import and Core generation publication.

This does not add an in-process ABI, marketplace, hosted plugin store,
installation manager, automatic plugin scheduling, command-output body store,
or `ctx import --all` plugin execution.

## Public Manifest Contract

A manifest is JSON named `ctx-history-plugin.json` and can be discovered from:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- entries in `CTX_HISTORY_PLUGIN_PATH`;
- an explicit `--history-source-manifest` path.

The source contract is:

```json
{
  "schema_version": 1,
  "name": "example-agent",
  "display_name": "Example Agent",
  "version": "1.0.0",
  "history_sources": [
    {
      "id": "default",
      "display_name": "Example local history",
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

`schema_version`, `name`, `history_sources[].id`, `source_format`, and either
`path` or `command` are required. `provider_key` defaults to `name`;
`source_id` defaults to `id`; `enabled` defaults to `false`; and `refresh`
defaults to `manual`.

Identifiers are at most 128 bytes, start with a lowercase ASCII letter or
digit, and contain only lowercase ASCII letters, digits, `.`, `_`, or `-`.
Relative paths resolve from the manifest directory. A durable path cannot also
declare command runtime options.

Command-only manifests remain parseable so existing installations receive a
stable compatibility diagnostic. They are reported as `unsupported`,
`importable: false`, and are never executed or copied into ctx storage.

## Identity And Publication Contract

The provider-owned file contains exactly one `ctx-history-jsonl-v1` manifest
record, one or more source/session/event records, and optional file-touch and
edge records. The selected source record must match the manifest's
`provider_key`, `source_id`, and `source_format`.

```text
discover + select
        |
        v
validate provider-owned path + bounded identity header
        |
        v
upsert explicit custom ctx_history_jsonl_v1 route
        |
        v
daemon-owned source-backed refresh + terminal receipt
```

Core is the imported-content authority used by search and presentation.
Tantivy and relational projections are disposable derivatives. Neither a
`NativePath` body nor a command-output snapshot participates in query-time
presentation.

The shared custom JSONL route handles cold builds, no-ops, appends, rewrites,
replacement, deletion, source certification, and crash-safe publication.
`--reset-cursor` is invalid because ctx owns no plugin cursor.

## User Reachability

`ctx sources` performs discovery only. Durable rows report
`explicit_source_backed` importability and their provider path. Command-only
rows report a typed unsupported reason without executing the command.

Production registration is reached through:

```bash
ctx import --history-source plugin/source
ctx import --history-source-manifest ./ctx-history-plugin.json
```

Selection must resolve to one source. Search filters use the canonical
`provider_key/source_id` identity from the provider file. Once imported,
`ctx show` reads the normalized imported content from the active Core generation.

## Failure And Trust Model

The route fails closed on:

- missing, oversized, malformed, or ambiguous manifests;
- a missing, symlinked, nonregular, or data-root-overlapping provider path;
- simultaneous `path` and command runtime declarations;
- malformed or oversized JSONL records;
- manifest/source identity mismatch;
- invalid record references;
- source mutation during certification or publication;
- daemon-publication failure.

Manifest discovery is read-only. Import writes only the ctx-owned route catalog
and disposable projections; it never rewrites or copies the provider file.
