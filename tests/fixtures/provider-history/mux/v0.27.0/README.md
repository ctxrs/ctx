# Mux v0.27.0 Provider History Proof

This fixture is sanitized, but it mirrors the native Mux v0.27.0 history
schema and layout from primary source code in `coder/mux`.

Generating a real transcript without paid/auth setup was not feasible here: Mux
sessions are produced by the application while model-backed chats run. The
schema is nevertheless anchored to the tagged upstream sources below.

- `MUX_ROOT` defaults to `~/.mux`, and sessions live under
  `sessions/<workspaceId>/chat.jsonl`:
  https://github.com/coder/mux/blob/v0.27.0/src/common/constants/paths.ts#L65-L109
- `ConfigManager.getSessionDir(workspaceId)` resolves to
  `<sessionsDir>/<workspaceId>`:
  https://github.com/coder/mux/blob/v0.27.0/src/node/config.ts#L1495-L1500
- `HistoryService` names `CHAT_FILE = "chat.jsonl"` and
  `PARTIAL_FILE = "partial.json"`, writes `chat.jsonl` as JSONL, and stores
  `workspaceId` with each message:
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/historyService.ts#L95-L123
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/historyService.ts#L1089-L1167
- `partial.json` is a staged Mux message with `metadata.partial = true`; Mux
  merges it by `metadata.historySequence`, replacing an existing row only when
  the partial has more parts:
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/historyService.ts#L855-L895
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/historyService.ts#L951-L1057
- `MuxMessage` has `id`, `role`, `parts[]`, optional `createdAt`, and rich
  `metadata` including `historySequence`, `timestamp`, `model`, `usage`,
  `providerMetadata`, `muxMetadata`, and `partial`:
  https://github.com/coder/mux/blob/v0.27.0/src/common/orpc/schemas/message.ts#L14-L150
- Subagent transcripts are archived under the parent session directory in
  `subagent-transcripts/<childTaskId>/chat.jsonl` plus optional `partial.json`,
  with an index in `subagent-transcripts.json`:
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/subagentTranscriptArtifacts.ts#L11-L65
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/workspaceService.ts#L957-L1065
- Mux readers skip malformed chat JSONL lines and best-effort read corrupt
  partial files:
  https://github.com/coder/mux/blob/v0.27.0/src/node/orpc/router.ts#L410-L520
  https://github.com/coder/mux/blob/v0.27.0/src/node/services/analytics/etl.ts#L527-L542

