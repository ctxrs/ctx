# Public telemetry v1 fixtures

The four unsuffixed event fixtures are the canonical common set replayed by the
Worker test as versions 1.0.0, 1.0.1, and 1.0.2. The three releases emitted
identical bytes for these shapes, so Git retains one copy rather than a
per-release mirror.

`analytics_delivery_observation.valid.json` is the current privacy-observability
candidate rather than a historical 1.0.x fixture. It contains only bucketed
queue/retry/drop/age facts and a closed failure class.

`provider_refresh_completed.privacy-observability.valid.json` is the matching
current candidate with the structured failure code and retryability pair. The
unsuffixed provider fixture remains byte-accurate for historical replay.

`runtime_observation_storage.valid.json` mirrors the public daemon storage
candidate. The unsuffixed runtime fixture remains the released pre-storage
shape and proves that all storage sidecars stay optional at ingestion.

`provider_refresh_completed.typed-v1.valid.json` preserves the prior typed-v1
provider-refresh shape. `search_operation_completed.v1.0.2.valid.json` retains
the distinct 1.0.2 Search shape after `include_subagents` stopped being emitted;
the field remains optional at ingestion so both the old and new released shapes
stay accepted.

The public `contracts/telemetry-v1/providers-v1.json` file is the provider-name
authority. The Worker keeps the bounded provider vocabulary it needs for
admission without a candidate-source mirror or release-time hash comparison.

The remaining files cover focused retained compatibility:
- `provider_refresh_completed.corpus-stock-v1.1.1.valid.json` preserves the
  bounded corpus-stock fields briefly emitted by public main while it still
  reported 1.1.1. They were removed before 1.2.0, but deployed source builds
  can continue sending them until upgraded.
- `provider_refresh_completed.v1.2.0.valid.json` is the simplified provider-
  refresh shape released by 1.2.0, including only bucketed records and logical
  bytes and no retired content-evidence field.
- `mcp_unknown_request.v1.1.1.valid.json` preserves the released MCP unknown-
  request encoding: no tool name was present, so the operation and tool are
  `missing` while the bounded method dimension is `unknown`. The same public
  producer shape remains current in 1.2.0.
- `list_events_operation_completed.v1.0.0.valid.json` preserves the CLI
  list-events completion shape released in 1.0.0 and still emitted in 1.2.0,
  including the distinct `events` target kind.
- `list_events_broken_pipe_operation_completed.v1.0.0.valid.json` preserves
  the released count-omitted success receipt emitted when list-events output
  closes early.
- `mcp_query_events.v1.0.0.valid.json` preserves the MCP query-events
  completion shape released in 1.0.0 and still emitted in 1.2.0.
- Retired CLI and MCP SQL values are absent from that current matrix. A separate
  Worker test retains acceptance coverage for already-emitted historical SQL
  events; this is ingestion compatibility only and does not describe a current
  ctx command, MCP tool, or local relational projection.
- `upgrade-v026.valid.json` exhausts every reachable manual and daemon-owned
  terminal upgrade shape from the final public producer, including all upgrade
  failure buckets. `upgrade-v026.invalid.json` pins impossible relationships,
  cross-operation fields, raw prose, and invalid attempt identities that remain
  rejected. The Worker validates the producer's bounded attempt-id syntax,
  persists only a domain-separated HMAC, and never stores the raw value.
- `row-flow-candidate.valid.json` embeds the expanded provider golden exactly,
  pairs it with the exact blame properties asserted by
  `blame_properties_are_closed_bucketed_and_content_free` in public
  `crates/ctx-cli/src/analytics/pro.rs`, and carries the closed Search
  concentration and daemon-refresh terminal shapes exercised by the current
  public producer tests. It contains no generic query fields or handwritten SQL
  aggregate projection. This fixture proves local Worker-to-insert composition.
