# Public telemetry v1

ctx emits only four durable, content-free event families:

- `operation_completed@1`
- `provider_refresh_completed@1`
- `runtime_observation@1`
- `install_stage@1`

On the wire, each family is represented by `event_name` plus
`event_version: 1`. Producers use closed enums and typed payloads; arbitrary
property maps are not part of the producer API. The first three fixtures show
valid batch event envelopes, not an exhaustive list of every operation-specific
property. The `install_stage@1` fixture is the exact standalone hosted endpoint
body.

Every batch event has a UUIDv4 `event_id`, a minute-rounded `occurred_at`, a
closed `surface`, a closed `outcome`, and a coarse `duration_bucket`.
Identity-bearing batch events do not carry exact duration milliseconds.
`surface` is one of `cli`, `mcp`, `pro_host`, or `daemon`.
`duration_bucket` is one of `unknown`, `lt_100ms`, `lt_1s`, `lt_5s`,
`lt_30s`, `lt_2m`, `lt_10m`, `lt_1h`, or `gte_1h`.
Official hosted installs may attach the install-attempt identifier to these
batch events only for the first seven days after installation. The producer
omits the bridge when the marker timestamp is missing, malformed, future-dated,
or at least seven days old; managed upgrades preserve the original timestamp.

Each outbound batch contains at most 50 events. A pending capability snapshot is
attached only to events in the first batch and is acknowledged after that batch
succeeds. A later batch failure does not undo that successful acknowledgement;
failure of the snapshot-bearing batch leaves the claim unacknowledged.

`operation_completed@1` records one terminal event for an eligible operation.
`provider_refresh_completed@1` records one completed aggregate for every
observed closed provider/source-mode pair. Provider names are the complete
`CaptureProvider` vocabulary; producers do not suppress low-usage providers.
Its decision fields are closed:

- `content_evidence`: `none`, `accepted`, `mixed`, or `unknown`;
- `work_kind`: `no_op`, `fresh`, `append`, `rewrite`, `truncate`, `replace`,
  `retire`, or `mixed`; omitted when the runtime did not retain enough evidence
  to classify changed work truthfully;
- `refresh_result`: `complete`, `partial`, or `failure`;
- `core_result`: `no_op`, `complete`, `partial`, `failure`, or `unknown`;
- `canonical_pro_result` and `output_pro_result`: `not_requested`,
  `unavailable`, `no_op`, `complete`, `partial`, `behind`, `failure`, or
  `unknown`;
- `failure_scope`: `none`, `record`, `source`, `system`, `mixed`, or `unknown`;
- `failure_type`: `none`, `record_rejection`, `unsupported_schema`,
  `not_found`, `permission`, `source_database`, `malformed_source`, `store`,
  `worker_panic`, `system_io`, `system`, `other`, `mixed`, or `unknown`.

Refresh counts include bucketed sources, source files, sessions, events, edges,
skips, rejections, failures, retired records, and source bytes. Counts retain
large-store resolution through `1m+`; bytes retain resolution through `100gb+`.
`retired_records_bucket` is omitted when the runtime did not retain an exact
aggregate. Per-provider duration is independently bucketed; a multi-provider
refresh never copies one aggregate duration into every provider event.
`runtime_observation@1` is reserved for low-frequency lifecycle and liveness
observations. `install_stage@1` is produced only by the hosted shell and
PowerShell installers and records one closed installer stage/status pair with
coarse platform, architecture, and script-family fields.

Payloads must not contain raw history, prompts, responses, SQL or search
queries, result content, source bodies, paths, target values, repository names,
command output, raw error strings, secrets, or credentials. Counts, byte sizes,
text lengths, and durations are bucketed before serialization.
Payloads also exclude source/session/record IDs, provider keys, source formats,
locators, cursors, exact timestamps, permanent ingestion-engine labels, and
free-form failure or rewrite reasons.
