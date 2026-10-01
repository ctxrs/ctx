# Telemetry schema sources

These migrations own the Worker-facing receipt, diagnostic, retention, and
reporting definitions. The historical definitions were transferred as source,
with no production data or private repository history. They are not an
instruction to replay established migrations against a deployed database.
Ordinary 1.5 Blame needs no new table or SQL constraint.

| Files | Retained ownership |
| --- | --- |
| `0038`, `0043` | Legacy signed Blame receipt/table and protected summaries |
| `0044`, `0044a` | Disabled raw deletion, permanent diagnostics, and ordinary typed event constraint |
| `0045`, `0048` | Health snapshots and delivery recovery diagnostics |
| `0046`, `0050`, `0051` | Received-date materialization, indexes, and serialized maintenance |
| `0047` | Event collision receipts |
| `0049` | Existing daemon-storage analytics |
| `0052` | Daily materializer sort memory |
| `0053` | Bounded occurrence-time rollout aggregates |

These migrations target the established telemetry schema. This directory
does not bootstrap unrelated business/account tables or reproduce the entire
historical database. SQL migrations retain their role/ownership preconditions.
Deploying the unchanged adapter against the established schema remains an
operator responsibility.

`../test/support/postgres.mjs` extracts only the existing synthetic `BASE_SCHEMA`
and isolated PostgreSQL helpers. It registers no imported private test suite and
contains no live rows. The maintenance test applies `0044`, `0046`, `0051`, and
`0050`; the focused ordinary Blame test applies actual `0044` and `0044a` before
executing the real adapter's INSERT/collision SQL as the ingest role. Fixture
roles/tables model the necessary prerequisites, not a production data dump.

Legacy signed Blame summaries exclude the new ordinary CLI/MCP events. The
[service reporting guidance](../README.md#compatibility-and-reporting) describes
the required dashboard labels and ordinary-event source; there is no identity
crosswalk or license check for the new producer.

`0053` adds `ctx.analytics_rollout_window(environment, from, until, versions)`
against the established restricted `ctx.analytics_canonical_telemetry_events`
view. Apply it as `ctx_migration`; only `ctx_analytics_readonly` receives
EXECUTE. It changes no event rows, classification rules, reader table grants,
materialization, or Worker behavior. Its isolated `rollout_postgres_test` uses
the retained canonical definition with authored synthetic inputs and verifies
that timestamp bounds reach the index scan before classification.

The half-open occurrence window must be positive, finite and at most 24 hours.
A null version array includes all versions; an empty array matches none. Retain
the caller's statement timeout. `version` rows count observed profiles and data
roots per producer version across the requested window. `event` rows break down
the same population by outcomes and optional upgrade/refresh fields; their
distinct counts overlap. Neither count describes unique machines or all
installations, and neither should be summed across versions or time slices to
deduplicate identities. Legacy installation counts remain separate.

Explicit `upgrade_status='applied'` with `upgrade_applied=true` differs from
`up_to_date`, successful checks and missing old-producer fields. The version is
the producer's version, not the upgrade destination. Missing diagnostics remain
null. Refresh source failures can accompany successful partial refreshes;
terminal failure reason coverage must use the appropriate failure denominator.
Delivery observations are outside the canonical view and this aggregate.
Late commits and current classification overrides can change a past window;
results are not a completeness watermark or proof of fleet-wide recovery.


`0054_product_telemetry_windows.sql` adds reader-only
`ctx.analytics_product_health_window` and
`ctx.analytics_product_measurements_window`. Both take environment, finite
positive half-open occurrence bounds of at most 24 hours, and an optional version
array. Null versions means all; an empty array matches none. Production uses
canonical eligible public traffic; staging uses explicitly labelled synthetic
qualification. These functions expose no identity keys or arbitrary properties.
`cohort` contains only fixed closed diagnostic fields. Profile/root counts are
observed grains, not machine counts. Summary receipts and measured bucket
frequencies cannot be summed as exact invocation/token totals or dollar savings.
Use `count_unit`, eligible/measured counts and each summary's denominator buckets.

Apply as `ctx_migration` after the canonical telemetry view and reader role
exist; preserve the existing `0053` contract. The migration adds functions and
ACLs only. The isolated `//services/telemetry-worker:product_postgres_test`
exercises HTTP, Queue, PostgreSQL writes and these restricted reads.
