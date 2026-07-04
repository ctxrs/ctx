# Reasonix v0.53.2 Provider-History Fixture

Primary source: `reasonix@0.53.2` resolves to GitHub repository
`esengine/DeepSeek-Reasonix`, tag `v0.53.2` (`b307987c0bb86ebee80b0d058ed92de75419ad8e`).

Source anchors:

- `src/memory/session.ts` lines 1 and 80-86 place append-only session JSONL under
  `~/.reasonix/sessions/<sanitizeName(session)>.jsonl`.
- `src/memory/session.ts` lines 22-28 define session sidecars:
  `.events.jsonl`, `.meta.json`, `.pending.json`, `.plan.json`, and `.jsonl.bak`.
- `src/memory/session.ts` lines 159-189 load session JSONL records whose parsed object
  has `role`; lines 192-200 append `ChatMessage` JSONL records.
- `src/types.ts` lines 31-41 define `ChatMessage` roles and fields including
  `content`, `tool_call_id`, `tool_calls`, and `reasoning_content`.
- `src/adapters/event-sink-jsonl.ts` lines 7-20 write
  `<session>.events.jsonl` event records; `src/adapters/event-source-jsonl.ts`
  lines 36-52 read records whose parsed object has string `type`.
- `src/core/events.ts` lines 8-128 and 216-224 define event `id`, `ts`, `turn`,
  `type`, user/model/tool/file/plan/error fields, usage, and cost fields.
- `src/transcript/log.ts` lines 7-49 define explicit transcript records and `_meta`;
  lines 98-116 write transcript JSONL; lines 125-151 parse `_meta` and records with
  `ts`, `turn`, `role`, and `content`.

These fixtures are sanitized. Paths use `/workspace/...` or relative file names, and
token/cost values are tiny non-sensitive numbers. The session JSONL shape is generated
from the exported session/transcript APIs where practical; sidecar examples are
faithful to the tagged TypeScript event interfaces without requiring paid/auth setup.
