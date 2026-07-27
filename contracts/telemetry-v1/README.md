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

`operation_completed@1` records one terminal event for an eligible operation.
`provider_refresh_completed@1` records a completed refresh summary.
`runtime_observation@1` is reserved for low-frequency lifecycle and liveness
observations. `install_stage@1` is produced only by the hosted shell and
PowerShell installers and records one closed installer stage/status pair with
coarse platform, architecture, and script-family fields.

Payloads must not contain raw history, prompts, responses, SQL or search
queries, result content, source bodies, paths, target values, repository names,
command output, raw error strings, secrets, or credentials. Counts, byte sizes,
text lengths, and durations are bucketed before serialization.
