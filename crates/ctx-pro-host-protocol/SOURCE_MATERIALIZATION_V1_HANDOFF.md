# Source-backed Pro materialization V1 handoff

The public `ctx-pro-host-protocol` crate and its generated inventory are the
wire authority. A helper must negotiate `source_materialization` and the exact
Protocol V1 fingerprint
`1859dda144965823c18a700c0a2d423b74fb2c8a5917144895420234f3062047`.
The legacy output inventory and `OutputNativeCursor` are not authority for this
feed.

## Exact exchanges

Messages use the existing `{ "kind": ..., "body": ... }` representation.

| Host tag | Request | Helper tag | Response |
| --- | --- | --- | --- |
| `begin_source_manifest` | `BeginSourceManifestRequest { manifest }` | `source_manifest_began` | `SourceManifestBegan { core_generation_id, materializer_revision, progress, replayed }` |
| `prepare_source` | `PrepareSourceRequest { core_generation_id, source, certified_revision_sha256, materializer_revision, disposition, expected_prior }` | `source_prepared` | `SourcePrepared { core_generation_id, progress, replayed }` |
| `materialize_source_page` | `MaterializeSourcePageRequest { core_generation_id, expected_prior, next_frontier, terminal, records }` | `source_page_materialized` | `SourcePageMaterialized { core_generation_id, progress, accepted_records, materialized_facts, replayed }` |
| `delete_source` | `DeleteSourceRequest { core_generation_id, removal, expected_prior }` | `source_deleted` | `SourceDeleted { core_generation_id, source, removed_source_epoch, replayed }` |
| `finish_source_manifest` | `FinishSourceManifestRequest { manifest, expected_progress }` | `source_manifest_finished` | `SourceManifestFinished { receipt, replayed }` |

`SourceManifest` contract version is 1. It carries the committed Core generation,
retained `CertifiedSource` values, and explicit `SourceRemoval` values. A removal
pairs one `CertifiedSourceDeletion` with the exact complete
`CertifiedSourceInventory` that verifies it.

## Compare-and-swap semantics

- Begin validates and selects the exact manifest generation. It returns the
  helper's one materializer revision and sorted durable progress, including
  stale source lineages that may require explicit deletion and progress from an
  older materializer revision that must be rewritten.
- Prepare is a per-lineage CAS. `new_source` has no prior and returns epoch 1 at
  no frontier. `resume` returns the exact supplied prior. `rewrite` requires a
  prior, atomically invalidates that source epoch's derived graph state, and
  returns prior epoch + 1 at no frontier. No other source changes.
- A page CAS compares every field of `expected_prior`: exact source descriptor,
  epoch, certified revision, frontier, materializer revision, and terminal bit.
  The helper atomically derives graph state and commits `next_progress()`.
  A mismatched prior does not mutate state. An exact replay may return the same
  acknowledgement with `replayed: true`; different content for the same CAS is
  rejected.
- Delete compares the exact prior and verifies the manifest's
  `CertifiedSourceDeletion`/complete-inventory pair. It atomically removes only
  that source epoch's derived graph state and progress. Absence from a manifest
  is never deletion evidence.
- Finish is one bounded CAS containing both the exact manifest and all expected
  terminal progress. It publishes a receipt only when every retained source
  matches its certificate revision and certified terminal frontier, all
  removals were explicitly applied, and no unaccounted progress remains.

Core is already published before these exchanges. Any provider, helper, or CAS
failure leaves Core untouched and Pro retryable from its independently committed
progress.

## Transient detector input and bounds

`SourceRecord` carries stable event/session IDs, a typed source locator, direct,
root, parent, provider-session and agent relationships, optional repository
context, record metadata, and normalized message/command/result facts. Command
and result facts preserve call IDs; results preserve outcome, exit code, and
duration. Root and parent session IDs may belong to other source lineages.
Content is canonical base64 and must be discarded as a full body after private
page handling; durable state is identities, locators, progress, and derived
graph facts.

The enforced maxima are 100,000 manifest entries, removals, inventory sources,
and progress entries; 4,096 records per page; 256 facts and 4,096 touched files
per record; and 16 MiB decoded transient content per page. Manifests encode to at
most 16 MiB and page/control DTOs to at most 24 MiB, leaving safe envelope
headroom under the 32 MiB framed payload limit. All request and response
`validate()` methods must run before mutation or acknowledgement.
