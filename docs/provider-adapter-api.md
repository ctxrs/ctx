# Provider Adapter API

Provider adapters convert local provider transcript files into normalized
sessions and events for indexing.

See [`provider-import-policy.md`](provider-import-policy.md) for the canonical
content policy, storage-family taxonomy, and fixture expectations for native
agent-history importers.

Adapters should provide:

- provider ID and source format;
- stable source identity and cursor information;
- session IDs and event IDs;
- event type, timestamp, role, text, and metadata when known;
- touched-file metadata when tool calls, outputs, or native provider fields
  expose file paths;
- bounded diagnostic previews for failed or timed-out tool/command output;
- source path plus cursor or line information for citations;
- clear errors for malformed or unsupported input.

Adapters must be read-only with respect to provider-owned files. They should
prefer structured provider formats over ad hoc text scraping and must document
which fields become searchable.
