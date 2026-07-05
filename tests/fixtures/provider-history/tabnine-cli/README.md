# Tabnine CLI Provider-History Fixture

This fixture mirrors the Tabnine CLI 0.25.1 chat recording shape observed from
the official installer bundle fetched from
`https://console.tabnine.com/update/cli/installer.mjs`.

Source/runtime evidence:

- The installer created `~/.tabnine/agent/.bundles/0.25.1` and
  `~/.tabnine/agent/projects.json` in a scratch `HOME`.
- The bundle's `Storage` class uses `~/.tabnine/agent/tmp/<project-id>` and a
  slug registry in `projects.json`; older hash directories are migrated.
- `ChatRecordingService` appends JSONL under
  `tmp/<project-id>/chats/session-<timestamp>-<sessionid8>.jsonl`.
- The first JSONL record carries `sessionId`, `projectHash`, `startTime`,
  `lastUpdated`, `kind`, and optional `directories`.
- Message records carry `id`, `timestamp`, `type`, and `content`; assistant
  records use `type: "tabnine"` while legacy Gemini-family records may use
  `type: "gemini"`.
- Subagent records are placed under `chats/<parent-session-id>/<agent>.jsonl`
  with `kind: "subagent"`.
- `/chat save <tag>` writes `checkpoint-<encodedTag>.json` in the project temp
  directory; checkpoints are resumable state, not the primary transcript source.

The fixture is hand-written from that bundle schema because a no-auth Tabnine
CLI runtime probe initialized project temp storage but did not progress far
enough to create a chat JSONL transcript.
