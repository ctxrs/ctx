# Optional typed failure reasons

These optional closed strings extend telemetry v1 without changing existing
event names, surfaces, operations, outcomes, failure codes, retry policy, or
the six coverage reasons and five partial-source failure classes. The typed
Rust event API and serializer enforce this contract.

## Refresh

`refresh_failure_reason` is allowed only on a failed daemon
`provider_refresh_completed` with `refresh_result=failure`, the existing valid
`refresh_failure_stage`/`refresh_failure_kind` pair, `failure_code`, and
`retryable`. The existing stage identifies admission, execution, verification,
or finalization; no finer filesystem operation is inferred.

| Reason | Permitted kind | Typed evidence |
| --- | --- | --- |
| `io_not_found` | io, index, provider | I/O NotFound |
| `io_permission_denied` | io, index, provider | I/O PermissionDenied |
| `io_storage_full` | io, index, provider | I/O StorageFull |
| `io_read_only_filesystem` | io, index, provider | I/O ReadOnlyFilesystem |
| `io_out_of_memory` | io, index, provider | I/O OutOfMemory |
| `io_timed_out` | io, index, provider | I/O TimedOut |
| `route_output_limit` | provider | Shared route output reservation exceeds its limit |
| `route_scratch_limit` | provider | Shared route scratch reservation exceeds its limit |
| `index_memory_limit` | index | IndexMemoryTooSmall |
| `index_scratch_limit` | index | VerificationScratchLimitExceeded |
| `index_writer_invariant` | index | WriterInvariant |

A route may preserve an I/O kind before converting its error to local display
text. Admission aggregates report a reason only if every failure has the same
known reason. Combined primary/cleanup diagnostics are retained only when both
are known and identical, regardless of route severity. Dominant route class and
retry policy remain unchanged. Accounting overflow is not capacity exhaustion.
Unknown, missing, malformed, and future reasons are omitted on local recovery.
The diagnostic is hidden until the terminal record is durable, and survives
terminal persistence retry and restart without recapturing.

String-only `source_refresh_failed`, `source_refresh_admission_failed`, and
`malformed_source` reports may still have no reason. In particular, no provider,
path, error message, SQL text/code, record, or generic failure code establishes
a specific cause. This extension does not diagnose every SQLite, parser, or
provider invariant failure.

## Delivery

`delivery_failure_reason` is allowed only on a failed
`analytics_delivery_observation`, preserving surface `cli` and operation
`outbox`.

| Failure class | Closed reasons |
| --- | --- |
| transport | `request_dns`, `request_connect`, `request_timeout`, `request_io`, `response_status_408`, `response_body_timeout`, `response_body_io` |
| local_io | `file_open`, `file_write`, `file_flush`, `outbox_corrupt`, `outbox_expired`, `outbox_capacity`, `outbox_clock`, `outbox_oversized` |

Request DNS/connect reasons use ureq's typed transport kind, taking precedence
over nested I/O. Other request I/O uses its typed kind; only TimedOut means
request_timeout. Successful HTTP status followed by a failed body read is
response_body_timeout for TimedOut, otherwise response_body_io. HTTP 408 is
separate. File values identify the failing append/write/flush call, not an OS
cause. Outbox values identify corrupt-state recovery, expiry, capacity
eviction, clock normalization, or oversized-batch rejection. Unknown transport
kinds, including TLS errors without a permitted typed cause, remain absent.

The optional reason follows the existing last ordinary failure class and
failure-sequence acknowledgement. Persistence/restart retain it; delivery of
an ordinary payload makes it reportable. Health-event delivery failures do not
create recursive observations. A newer failure replaces the old reason even
when the newer reason is unknown. Final zero-queue successful recovery carries
no reason. The authoritative v3 envelope retains exactly the old fields, so
coexisting older clients can read and rewrite it without losing queued payloads.
Optional reasons live separately in one owner-private, 64 KiB-bounded sidecar
with at most 128 root records, under the existing outbox lock. It binds the
exact queue bytes and file generation to each root's failure sequence/class.
Queue persistence completes first; sidecar updates are best-effort, not a
second queue or transaction journal. Missing, stale, corrupt, unsafe, or
unreadable metadata omits reasons; sidecar write failures never fail or reset
the ordinary queue. Purge discards the sidecar. An older writer's replacement,
even with identical bytes, invalidates it. Unsupported file-identity platforms
omit this optional evidence. Once a recovery event is queued, its existing
payload retains the reason normally.

Outbox limits and retry/durability rules are unchanged. Diagnostics cannot
report an outbox I/O error that prevents the outbox itself from being persisted.
No raw error text, stack, endpoint, path, source record, or identifier is added.
