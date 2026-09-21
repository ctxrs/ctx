# Telemetry schema sources

These selected existing migrations own the Worker-facing receipt, diagnostic,
retention, and reporting definitions. They were transferred as source, with no
production data or private repository history. They are historical migrations,
not a new 1.5 database migration and not an instruction to replay them against
a deployed database. Ordinary 1.5 Blame needs no new table or SQL constraint.

| Files | Retained ownership |
| --- | --- |
| `0038`, `0043` | Legacy signed Blame receipt/table and protected summaries |
| `0044`, `0044a` | Disabled raw deletion, permanent diagnostics, and ordinary typed event constraint |
| `0045`, `0048` | Health snapshots and delivery recovery diagnostics |
| `0046`, `0050`, `0051` | Received-date materialization, indexes, and serialized maintenance |
| `0047` | Event collision receipts |
| `0049` | Existing daemon-storage analytics |

The current production schema already supplies these objects. This directory
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
