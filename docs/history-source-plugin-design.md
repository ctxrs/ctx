# History Source Plugin Design

This document records the shipped 1.0 architecture for explicitly selected
history-source plugins.

## Goal And Boundary

Third-party tools can expose local history without ctx learning each tool's
native JSONL, SQLite, or API schema. The plugin owns native reads and cursor
meaning. ctx owns:

- bounded manifest discovery and validation;
- explicit single-source selection;
- bounded local command execution;
- validation of the existing `ctx-history-jsonl-v1` format;
- a private managed provider-export source;
- registration of that source with the custom source-backed route;
- cursor commit after authoritative daemon publication.

This does not add an in-process ABI, remote marketplace, hosted plugin store,
installation manager, arbitrary executable format, automatic plugin
scheduling, or `ctx import --all` plugin execution. A manifest command is local
code running with the current user's operating-system permissions.

## Public Manifest Contract

A manifest is JSON named `ctx-history-plugin.json` and can be discovered from:

- `$CTX_DATA_ROOT/plugins/<plugin>/ctx-history-plugin.json`;
- entries in `CTX_HISTORY_PLUGIN_PATH`;
- an explicit `--history-source-manifest` path.

The schema is:

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
      "source_format": "example-agent-sqlite-v1",
      "enabled": true,
      "refresh": "manual",
      "command": ["example-agent-to-ctx", "export"],
      "working_dir": ".",
      "env": {
        "EXAMPLE_AGENT_PROFILE": "default"
      },
      "timeout_seconds": 300
    }
  ]
}
```

`schema_version`, `name`, `history_sources[].id`, `source_format`, and
`command` are required. `provider_key` defaults to `name`; `source_id` defaults
to `id`; `enabled` defaults to `false`; `refresh` defaults to `manual`; and
`timeout_seconds` defaults to 300 seconds and is clamped to at least one.

Identifiers are at most 128 bytes, start with a lowercase ASCII letter or
digit, and contain only lowercase ASCII letters, digits, `.`, `_`, or `-`.
Commands are argv arrays and are never interpreted by a shell.

## Stream And Identity Contract

The only supported command output is `ctx-history-jsonl-v1`. A run emits
exactly one manifest record, exactly one source record, and zero or more
session, event, file-touch, and edge records.

The source record must match the manifest's `provider_key`, `source_id`, and
`source_format`. Optional `machine_id` and cursor stream values must match the
values supplied by ctx. Sessions and dependent records must remain inside that
source identity; missing sessions, invalid event/touch references, and cyclic
parents fail validation.

Normalized rows remain internally bounded to provider `custom`. Exporter-owned
`provider_key`, `source_id`, `source_format`, session IDs, and plugin identity
metadata remain available for filtering and display.

## Publication State Machine

```text
discover + select
        |
        v
execute bounded command
        |
        v
validate complete delta
        |
        v
merge private provider-export JSONL under route lock
        |
        v
upsert explicit custom ctx_history_jsonl_v1 route
        |
        v
daemon-owned source-backed refresh + terminal receipt
        |
        v
commit cursor sidecar
```

The managed JSONL source is the exact-body authority. Source-backed indexes are
derived from it, and complete-content hydration returns to it. Neither the old
Store database nor a `NativePath` locator participates in plugin import or body
recovery.

The managed merge uses stable record identities. New records append, identical
records are no-ops, replacements produce rewrites, and `--reset-cursor`
replaces the managed snapshot with a validated full rescan. Cursor data is
stored separately so cursor-only progress does not manufacture a new search
generation.

If daemon publication fails after the snapshot is staged, the cursor remains
unchanged. Retrying is safe because the merge is idempotent and publication is
again required before cursor commit.

## User Reachability

`ctx sources` performs discovery only. Valid rows report
`explicit_source_backed` importability, the canonical filterable
`history_source` (`provider_key/source_id`), and the `plugin_source`
import-selection alias. Invalid or oversized manifests report `invalid` and
cannot execute.

Production execution is reached through:

```bash
ctx import --history-source plugin/source
ctx import --history-source-manifest ./ctx-history-plugin.json
```

Selection must resolve to one source before execution. The resulting generation
supports canonical `--history-source provider_key/source_id`, plus
`--provider-key` and `--source-id` custom-history filters.
`ctx show ... --content complete` hydrates from the managed provider export.

The `enabled` and `refresh` fields are retained for manifest compatibility and
discovery output, but 1.0 does not schedule plugin commands during search,
setup, daemon maintenance, or `ctx import --all`.

## Failure And Trust Model

The route fails closed on:

- missing, oversized, malformed, or unsupported manifests;
- ambiguous selection;
- spawn errors, nonzero exit, timeout, or oversized stdout/stderr;
- malformed or oversized JSONL records;
- manifest/source duplication or identity mismatch;
- invalid session, event, touch, or edge references;
- cursor identity mismatch;
- managed-source verification or daemon-publication failure.

stdout is limited to 64 MiB, stderr to 256 KiB, individual JSONL lines to
16 MiB, cursor values to 1 MiB, and managed snapshots to 1 GiB. Commands receive
closed stdin and a cleared, allowlisted environment plus explicit manifest
variables. Private route, snapshot, lock, cursor, and temporary cursor files
receive restrictive local permissions where the platform supports them.

These are process and data guardrails, not a sandbox. Installing a plugin means
choosing to run its local command.
