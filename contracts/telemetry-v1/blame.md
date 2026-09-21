# Ordinary Blame telemetry

Blame uses one `operation_completed@1` terminal with `operation: "blame"` and
`surface: "cli"` or `"mcp"`. CLI and MCP adapters attach facts to the existing
operation terminal. They do not emit a second event. The terminal outcome
describes completion including the final output boundary; a successful query's
facts survive a later presentation, serialization, write, or flush failure.
An evaluated `none` result is a successful query.

The producer accepts only these typed, content-free Blame properties:

| Property | Values |
| --- | --- |
| `blame_target_kind` | `file`, `commit`, `pull_request` |
| `blame_request_kind` | `first_request`, `continuation` |
| `blame_query_duration_bucket` | Existing measured duration buckets; absent when unmeasured |
| `blame_result_state` | `proven`, `possible`, `conflicting`, `none` |
| `blame_result_count_bucket` | Existing count buckets for the rendered result's `coverage.evaluated` |
| `blame_freshness` | `current`, `stale_committed` |
| `blame_has_more` | Boolean from the rendered result |
| `blame_failure_class` | `invalid_request`, `source`, `repository`, `stale`, `ambiguous`, `corruption`, `cancelled`, `output`, `other` |
| `blame_failure_phase` | `setup`, `query`, `presentation`, `output` |
| `blame_output_served` | Boolean observed at the final output boundary |

The four result properties are present together when a query produced a
validated result. Failure class and phase are present together. Unobserved
facts remain absent. The target kind does not include a path, commit, PR
identifier, repository, selector, or cursor. There are no raw counts, exact
query durations, result bodies, citations, error messages, account/access
states, machine hashes, or client signing material in this sidecar. The CLI
retains its ordinary `output` property; MCP retains its ordinary method/tool
and response diagnostics.

Blame follows the existing analytics consent, random profile/data-root IDs,
bounded local outbox, and daemon-only upload path. These are shared
pseudonymous analytics IDs; Blame is not independently anonymous. Optional
recording or delivery failure must not fail the product operation. Local usage
definition 3 remains separate: its Blame result class is `not_applicable`, its
result count is zero, CLI output bytes are unavailable, and MCP retains its
existing delivered response-byte measurement. The remote evaluated-count
bucket does not change that local definition.

The Worker accepts ordinary Blame without an installation proof using the
ordinary profile/data-root envelope, queues `telemetry_row`, and inserts into
the existing `ctx.telemetry_event` table. There is no new identity, table,
proof, or queue format. Deploy receiver acceptance before releasing the new
producer: an older receiver rejects the new CLI operation/properties and
ordinary client handling treats most 4xx responses as permanent rejection.

Historical signed `pro_host` Blame V1/V2 and materialization proofs remain
required on their existing paths, including queued receipts. Old bare MCP
Blame terminals remain accepted without inventing missing measurements. There
is no identity crosswalk from those proof-derived subjects to ordinary IDs.

Raw server telemetry has no automatic expiration under the current SQL
retention policy. The 30-day bound is local outbox expiry, not server retention.
Permanent diagnostic aggregates omit identity. The public Worker owns the
receiver, queue, database adapter, and schema/test prerequisites in
[`services/telemetry-worker`](../../services/telemetry-worker/README.md).

[`blame_operation_completed.valid.json`](fixtures/blame_operation_completed.valid.json)
contains authored CLI/MCP examples consumed by both Rust serialization tests
and Worker admission/SQL tests; it contains no captured user history.
