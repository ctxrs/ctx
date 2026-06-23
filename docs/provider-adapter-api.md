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
- `work_record_capture::PiSessionJsonlAdapter`

## Security gates for new provider paths

Provider transcript import and hook capture cross a privacy boundary from
provider-owned storage into the ctx data root. Before public docs upgrade a
provider claim, the provider worker must add:

- provider-specific redaction corpus cases for new sensitive fields;
- malformed-input and replay/idempotency tests for the source format;
- raw-retention notes for transcripts, command output, images, attachments,
  and local object payloads;
- threat-model updates for the new import, hook, or wrapper boundary;
- release/CI evidence when the claim is `supported-live` or otherwise depends
  on real provider behavior.

Until those artifacts exist, keep the provider at `fixture-only`,
`detected-unsupported`, or `blocked`, or describe an explicit narrow import
path with its missing fidelity dimensions.

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
- Shared sanitization is heuristic and should not be treated as a
  general-purpose sanitizer for arbitrary provider transcript fields.
