# ctx telemetry Worker

This public package owns the active telemetry receiver, durable Queue admission
and consumption, Neon SQL adapter, health checks, and their tests. Its source
and test prerequisites require no private checkout. First-party source inherits
the repository's Apache-2.0 license. `private: true` in `package.json` only
prevents accidental npm publication.

The existing ordinary analytics envelope now accepts CLI/MCP `blame` on both
`/functions/v1/analytics` and `/functions/v1/telemetry`. It uses the same random
profile/data-root IDs, opt-out, local outbox, daemon uploader, `telemetry_row`
queue format, and `ctx.telemetry_event` table as other ordinary operations.
The [public contract](../../contracts/telemetry-v1/blame.md) defines its closed
measurements. An evaluated `none` result remains a success; a failed final
output preserves successful query facts without claiming delivery.

Deploy receiver acceptance before the 1.5 producer release. Current receivers
reject the new CLI operation and ordinary Blame properties, and permanent 4xx
responses can drop client outbox events. Deployment is an operator action;
normal tests never send an event to a deployed service. WorkOS/Stripe servers
and old account records are independent of this Worker and can remain running.

## Compatibility and reporting

`blame-installation-proof.ts`, `blame-product-contract.ts`, and
`blame-product-receipt.ts` serve legacy signed `pro_host` V1/V2 clients. Their
proof validation and Queue receipts remain intact, as does legacy signed
materialization ingestion. They provide no product activation authority for
1.5. Historical client envelopes, including old bare MCP Blame, remain
accepted with their original missing measurements.

The protected `ctx_product_health.blame_weekly` and `blame_summary` views and
`ctx.blame_product_event` source cover **legacy signed clients only**. Label
existing dashboards accordingly before 1.5 producer release. Ordinary 1.5
Blame reporting must select `ctx.telemetry_event` with
`event_name = 'operation_completed'`, `properties->>'operation' = 'blame'`, and
`surface in ('cli', 'mcp')`, using the usual environment/traffic-class filters.
Old MCP rows without sidecars must remain distinguishable from measured 1.5
rows. Do not join old proof-derived subjects to ordinary IDs or present the
legacy view as total Blame usage. Failure reporting must keep generic operation
outcome, query result, and final `blame_output_served` separate.

Current SQL disables automatic raw product-telemetry deletion. Raw accepted
events retain pseudonymous identity; permanent diagnostics are identity-free.
The client's 30-day local outbox expiry is not server retention. Nothing in
this source transfer deletes raw history, retires Queue receipts, or changes
commercial account state.

## Local validation

Use the repository's declared package dependencies and shared package runner.
The owning Bazel labels are:

- `//services/telemetry-worker:package_test` — admission, legacy proofs,
  Queue/adapter behavior, operator-script tests, and TypeScript checking.
- `//services/telemetry-worker:maintenance_test` — focused existing health and
  scheduled-maintenance behavior.
- `//services/telemetry-worker:blame_postgres_test` — the Rust-asserted ordinary
  fixture through HTTP, Queue decode/consume, real SQL insertion, replay, and
  the existing typed SQL identity constraint.
- `//services/telemetry-worker:maintenance_postgres_test` — existing SQL
  maintenance behavior with isolated synthetic data.

PostgreSQL targets require a declared PostgreSQL 16+ runner prerequisite and
fail when it is missing. They initialize a unique temporary database/socket and
remove it afterward. The source/schema inputs are self-contained; PostgreSQL
is an external runner prerequisite, not a source-built hermetic toolchain.
Apply shared-host governor/lab admission rules before executing heavy checks.
Ordinary Rust serialization lives in
`//crates/ctx-client-observability:unit_tests`; the existing analytics policy,
identity, and local-usage contracts retain consent and delivery coverage.

## Operator configuration

`wrangler.toml` contains nonsecret route, Queue, rate-limit and schedule
configuration. Supply runtime database URLs, identity HMAC key, and Cloudflare
API credentials through secret bindings; they are not source files. Existing
operational secrets remain private even though all service source is public.
`config/health-monitor.json` retains the monitor definitions with an example
notification email. Set the intended notification destination in operator
configuration before applying a monitor change; no monitor was changed by
moving this source. The candidate, Queue, DLQ/redrive, and deployment scripts
remain explicit operator actions and are not normal test/release campaigns.

See [schema/README.md](schema/README.md) for retained migrations and the bounded
test schema. The selected legacy proof fixture is authored test material and
contains a public verification key and signature, never a production secret.
