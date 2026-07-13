# Antigravity Native History Fixture v1

This fixture mirrors the public-safe portion of Antigravity CLI local history:

```text
brain/<conversation-id>/.system_generated/logs/transcript_full.jsonl
```

The JSONL files are synthetic and sanitized. They preserve the native field
names used by persisted Antigravity transcripts, including `step_index`,
`source`, `type`, `status`, `created_at`, `content`, `thinking`, and
`tool_calls`. Tool call `args` are kept as JSON objects so the importer can
prove typed argument preservation without storing real local history.

Coverage:

- `agy-success`: user input, assistant response, and typed tool call args.
- `agy-resume`: multi-step resumed conversation.
- `agy-malformed`: import with one rejected malformed JSONL line.
- `agy-future`: unknown future event type and sensitive marker fixture.
