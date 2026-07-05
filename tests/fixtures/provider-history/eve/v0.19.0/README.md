# Eve v0.19.0 Workflow Local Fixture

This fixture mirrors Eve local development storage backed by
`@workflow/world-local@5.0.0-beta.22`.

- `.workflow-data/streams/runs/wrun_evefixture.json` maps the workflow run to
  the default user stream.
- `.workflow-data/streams/chunks/*.bin` uses the world-local chunk shape:
  one EOF marker byte, then length-prefixed Workflow frames.
- Each frame is a `devl` serialized `Uint8Array` whose bytes are Eve message
  stream NDJSON from `encodeMessageStreamEvent`.

The events are sanitized and deterministic. They exercise user, assistant,
tool-call, and tool-output imports without requiring Eve runtime credentials.
