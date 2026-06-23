# Provider Adapter API

Provider workers should use the shared ctx provider path instead of
writing directly to store tables.

## Common flow

1. Parse provider-native files, logs, hooks, or wrapper output in a provider
   adapter that implements `work_record_capture::ProviderCaptureAdapter`.
2. Normalize every session/event row into
   `work_record_core::ProviderCaptureEnvelope`.
3. Persist through
   `work_record_capture::import_normalized_provider_captures(...)`.
4. Update the matching row in `docs/provider-support-matrix.json`.

Reference implementations in this branch:

- `work_record_capture::ProviderFixtureJsonlAdapter`
- `work_record_capture::CodexHistoryJsonlAdapter`

## Required envelope fields

- `provider`: current stored provider identity.
- `source.source_format`: stable parser/import format name.
- `source.trust`: whether the data came from a provider-native export, wrapper,
  fixture, or synthetic path.
- `source.raw_retention`: whether raw local data is retained by path reference,
  metadata only, local object, or not at all.
- `source.redaction_boundary`: where raw content must be sanitized before
  leaving the local product.
- `source.cursor`: checkpoint stream/value for incremental import.
- `session.provider_session_id`: stable external session id.
- `event.provider_event_index` plus `event.provider_event_hash`: shared event
  idempotency tuple.

## Guarantees from the shared importer

- idempotent session/event replay for the same provider session tuple;
- parent/child session edge materialization;
- sync-cursor persistence in `sync_cursors`;
- secret-shape sanitization for provider payload and metadata before store;
- consistent source/session/event sync metadata for dashboard/export/report
  consumers.

## Current limits

- Artifact descriptors can travel in the normalized envelope metadata, but this
  branch does not yet materialize provider objects into the artifact table.
- New providers that need a first-class stored provider id may still need the
  capture/store enums extended in their worker branch.
