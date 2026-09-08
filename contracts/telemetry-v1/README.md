# Public telemetry v1

ctx emits only five durable, content-free event families:

- `operation_completed@1`
- `provider_refresh_completed@1`
- `runtime_observation@1`
- `analytics_delivery_observation@1`
- `install_stage@1`

On the wire, each family is represented by `event_name` plus
`event_version: 1`. Producers use closed enums and typed payloads; arbitrary
property maps are not part of the producer API. The batch fixtures show valid
event envelopes, not an exhaustive list of every operation-specific property.
The `install_stage@1` fixture is the exact standalone hosted endpoint body.

Every batch event has a UUIDv4 `event_id`, a minute-rounded `occurred_at`, a
closed `surface`, a closed `outcome`, and a coarse `duration_bucket`.
Identity-bearing batch events do not carry exact duration milliseconds.
`surface` is one of `cli`, `mcp`, or `daemon`.
`duration_bucket` is one of `unknown`, `lt_100ms`, `lt_1s`, `lt_5s`,
`lt_30s`, `lt_2m`, `lt_10m`, `lt_1h`, or `gte_1h`.
Official hosted installs may attach the install-attempt identifier to these
batch events only for the first seven days after installation. The producer
omits the bridge when the marker timestamp is missing, malformed, future-dated,
or at least seven days old; managed upgrades preserve the original timestamp.

Each outbound batch contains at most 50 events. A pending capability snapshot is
attached only to events in the first batch and is acknowledged after that batch
is durably handed to the bounded local outbox. A failed local handoff leaves the
claim unacknowledged.

`operation_completed@1` records one terminal event for an eligible operation.
`provider_refresh_completed@1` records one completed aggregate for every
observed provider and trigger. Provider names are the complete
`CaptureProvider` vocabulary; producers do not suppress low-usage providers.
`providers-v1.json` is the versioned machine-readable vocabulary for those
wire names. Current entries must exactly match `CaptureProvider`; retired
entries remain reserved so a name is never silently reused.
One provider-neutral aggregate is also permitted for an authoritative
all-provider publication when the global receipt cannot truthfully attribute a
provider or coarse per-run work; that event omits `provider` and all work
buckets instead of guessing them or serializing default zeroes.
Its decision fields are closed:

- `refresh_result`: `complete`, `partial`, or `failure`;
- `core_result`: `no_op`, `complete`, `partial`, `failure`, or `unknown`;
- `failure_scope`: `none`, `record`, `source`, `system`, `mixed`, or `unknown`;
- `failure_type`: `none`, `record_rejection`, `unsupported_schema`,
  `not_found`, `permission`, `source_database`, `malformed_source`, `store`,
  `worker_panic`, `system_io`, `system`, `other`, `mixed`, or `unknown`;
- `failure_code`: `none` or one closed structured Core terminal code from the
  public producer enum; and
- `retryable`: a boolean copied from the same structured terminal receipt.

Optional workload measurements are limited to bucketed records and logical
bytes. Per-provider duration is independently bucketed; a
multi-provider refresh never copies one aggregate duration into every provider
event. Daemon terminals may additionally report only whether a successor is
pending and, for failures, whether the previous generation was retained.
`runtime_observation@1` is reserved for low-frequency lifecycle and liveness
observations. Daemon `ready` and jittered 23–24-hour `liveness` observations may
carry one best-effort storage sidecar. No other runtime operation carries it,
and collection failure omits the affected group without changing daemon,
refresh, publication, or delivery behavior.

The filesystem group is all-or-none:
`filesystem_total_bytes_bucket`, `filesystem_available_bytes_bucket`, and
`filesystem_available_fraction_bucket`. Available bytes are bytes available to
the caller on the filesystem containing the data root. The Core group contains
`core_active_logical_bytes_bucket` and
`core_certified_source_bytes_bucket`; it also contains
`core_logical_amplification_bucket` when certified source bytes are nonzero.
Active logical bytes are the checked sum of the current active generation's
authenticated Tantivy `meta.json` and active artifact lengths. Managed
bookkeeping, inactive generations, semantic indexes, and physical allocation
are excluded. When both available filesystem bytes and a nonzero active Core
size are known, `filesystem_available_to_active_core_ratio_bucket` describes
capacity for one additional active-Core-sized logical generation. It is not a
migration-success prediction. Exact byte and ratio values remain local.

Storage byte buckets use binary units and lower-inclusive, upper-exclusive
boundaries: `0`, `lt_100mb`, `100mb-1gb`, `1gb-5gb`, `5gb-10gb`,
`10gb-25gb`, `25gb-50gb`, `50gb-100gb`, `100gb-250gb`, `250gb-500gb`,
`500gb-1tb`, `1tb-2tb`, `2tb-5tb`, and `5tb+`. Available-fraction buckets are
`0`, `lt_5pct`, `5pct-10pct`, `10pct-20pct`, `20pct-40pct`, `40pct-60pct`,
and `60pct+`. Core logical-amplification buckets are `lt_0_10x`,
`0_10x-0_25x`, `0_25x-0_35x`, `0_35x-0_50x`, `0_50x-1x`, `1x-2x`, and
`2x+`. Available-to-active-Core buckets are `lt_0_5x`, `0_5x-1x`,
`1x-1_25x`, `1_25x-2x`, `2x-4x`, and `4x+`. Exact boundaries enter the
higher bucket.

`analytics_delivery_observation@1` carries exactly bucketed queue
depth, retry attempts, dropped count, oldest queued age, and one closed failure
class. It never contains an endpoint, response, request body, or raw error.
`install_stage@1` is produced only by the hosted shell and
PowerShell installers and records one closed installer stage/status pair with
coarse platform, architecture, and script-family fields.

Payloads must not contain raw history, prompts, responses, SQL or search
queries, result content, source bodies, paths, target values, repository names,
command output, raw error strings, secrets, or credentials. Counts, byte sizes,
text lengths, and durations are bucketed before serialization.
Payloads also exclude source/session/record IDs, provider keys, source formats,
locators, cursors, exact timestamps, permanent ingestion-engine labels, and
free-form failure or rewrite reasons. Exact CPU time, resident memory, worker
counts, preparation bytes, Store receipts, and journal sizes are never
serialized; unavailable runtime dimensions are omitted rather than inferred.

Foreground CLI and MCP producers perform no telemetry network I/O. They
serialize each eligible event once, preserving its UUIDv4 event ID in the exact
batch body, and durably append that body to an owner-private,
cross-process-locked local outbox. If the persistent daemon is disabled or
absent, entries remain local until later delivery or bounded expiry.

The enabled persistent daemon is the sole network uploader. On startup, active
wakes, and periodic cycles it briefly locks and snapshots at most 10 entries,
releases the state lock before HTTP, then re-locks to reconcile exact outbox
entry IDs. Only a final 2xx response removes an accepted entry. A crash after
server acceptance therefore replays the unchanged event IDs, which the server
treats idempotently. Network failures, HTTP 408/429, and 5xx responses retry
under persisted capped exponential backoff with jitter; bounded `Retry-After`
can extend that delay. Other permanent HTTP rejections are dropped. Daemon
shutdown appends terminal events without starting another upload.

Delivery failures are coalesced into one closed
`analytics_delivery_observation@1` only after ordinary delivery recovers;
failure to deliver that health event does not recursively create another one.
The outbox binds each entry to a one-way endpoint fingerprint and is bounded to
128 entries, 2 MiB total, 512 KiB per entry, and 30 days. An explicit analytics
opt-out removes it the next time ctx opens analytics state, before any drain.
Malformed or oversized owner-private state resets to an empty valid outbox and
later reports one `local_io` drop after recovery; unsafe paths or permissions
still fail closed.
