# Provider Storage Proof Notes

These notes capture providers that were researched while adding native
IDE/application storage imports.

## Kiro CLI

- Source evidence: official Kiro CLI docs state chat sessions are auto-saved
  after every conversation turn, keyed by directory path, with UUID session IDs,
  and stored in a local SQLite database under the Kiro CLI home area.
- Source evidence: official Kiro CLI configuration docs state `KIRO_HOME`
  overrides `~/.kiro` for agents, prompts, skills, steering, settings, and
  sessions; this proves the home override for Kiro-managed session files, but
  not a separate SQLite DB override.
- Direct binary proof: running the official Kiro CLI 2.10.0 Linux binary with a
  temporary `HOME`, `XDG_DATA_HOME`, and `KIRO_HOME` created
  `$XDG_DATA_HOME/kiro-cli/data.sqlite3` and `KIRO_HOME/settings/cli.json`.
- Direct DB proof from that generated DB: tables include `conversations`,
  `conversations_v2`, `history`, `auth_kv`, `state`, `migrations`, and
  `extracted_kas_versions`; `conversations_v2` columns are `key`,
  `conversation_id`, `value`, `created_at`, and `updated_at`, with primary key
  `(key, conversation_id)`.
- Payload proof: upstream issue reports query `json_extract(value, '$.history')`
  against `conversations_v2`, and the `kiro-history` package parser reads
  `value.history[]` entries with `user.content.Prompt.prompt`,
  `assistant.Response.content`, and `assistant.ToolUse.{content,tool_uses}`.
- Gap: a recent upstream issue and third-party tooling also point to newer
  `~/.kiro/sessions/cli` JSON/JSONL event logs. This pass imports only the
  proven SQLite DB format as `kiro_cli_sqlite`.

## Autohand Code

- Source evidence: upstream `src/constants.ts` defines `AUTOHAND_HOME` as the
  overrideable base directory with a default of `~/.autohand`, and
  `AUTOHAND_PATHS.sessions` as `AUTOHAND_HOME/sessions`.
- `src/session/SessionManager.ts` creates each session directory, writes
  `metadata.json`, appends messages to `conversation.jsonl`, loads the same
  JSONL file, and maintains `sessions/index.json`.
- `src/session/types.ts` defines `SessionMetadata` fields including
  `sessionId`, `createdAt`, `lastActiveAt`, `projectPath`, `projectName`,
  `model`, `messageCount`, `status`, `client`, `importedFrom`, and `branch`.
- `src/session/types.ts` defines `SessionMessage` as JSONL records with
  `role`, `content`, `timestamp`, optional `toolCalls`, `name`,
  `tool_call_id`, and `_meta`.
- `tests/session/SessionManager.test.ts` asserts that created sessions contain
  both `metadata.json` and `conversation.jsonl`.
- `ctx` imports this shape as `autohand_code_sessions_jsonl`.

## CodeBuddy

- Source evidence: WayLog `shayne-snap/WayLog` commit
  `6939033b7a39326fbdc249e28e6aa12461db1f09`,
  `src/services/readers/codebuddy-reader.ts` and
  `src/utils/platform-paths.ts`.
- Default storage roots are `~/Library/Application Support/CodeBuddyExtension/Data`
  on macOS, `%LOCALAPPDATA%/CodeBuddyExtension` on Windows, and `~/.codebuddy`
  on Linux.
- The reader recursively finds directories named `history`; project folders are
  keyed by `md5(projectPath)`, project `index.json` lists conversations, session
  `index.json` lists message IDs, and `messages/<id>.json` stores each message.
- Message `message` fields may be stringified JSON; text content can be a string
  or an array of `{type:"text", text}` blocks.
- `ctx` imports this shape as `codebuddy_history_json` and keeps the provider
  files read-only. Schema confidence is based on WayLog plus sanitized fixtures,
  not official CodeBuddy documentation.

## Aider Desk

- Source evidence: Aider Desk repository commit
  `01ad9d3ab75998fbc79c6decca5896ba6ef294aa`.
- Path evidence: `src/main/constants.ts` defines `AIDER_DESK_DIR` as
  `process.env.AIDER_DESK_DIR || ".aider-desk"` and derives
  `AIDER_DESK_TASKS_DIR` as `<aider-dir>/tasks`.
- Runtime evidence: `src/main/task/context-manager.ts` persists task context to
  `<project>/<AIDER_DESK_DIR>/tasks/<taskId>/context.json`.
- Type evidence: `packages/common/src/types/context.ts` defines
  `ContextMessage` roles, content parts including `text`, `image`, `file`,
  `reasoning`, `tool-call`, and `tool-result`, plus usage and prompt-context
  metadata.
- Type evidence: `packages/common/src/types/common.ts` defines `TaskContext`
  with `contextMessages` and `contextFiles`; the V1-to-V2 migration normalizes
  older tool-call arguments/results into the current input/output shape.
- `ctx` imports this shape as `aider_desk_task_context_json`, using read-only
  cwd/ancestor discovery only when a project has task context files.
- Caveat: ctx imports task context snapshots and optional task settings, not the
  Aider Desk application usage database.

## iFlow CLI

- Source evidence: the `iflow-ai-iflow-cli-0.5.19.tgz` bundle resolves
  `IFLOW_HOME` first and otherwise falls back to the user's `.iflow` directory.
- Bundle storage helpers build project history paths under
  `<iflow-home>/projects/<sanitized-project-path>/session-<uuid>.jsonl`.
- Bundle JSONL record construction includes fields observed in sanitized
  fixtures: `uuid`, `parentUuid`, `sessionId`, `timestamp`, `type`,
  `message.role`, `message.content`, `cwd`, `gitBranch`, `toolUseResult`,
  `isCompactSummary`, `compressionInfo`, and `isMeta`.
- Supplemental checkpoint files under `.iflow/tmp` are treated as non-canonical;
  `ctx` imports the resumable project `session-*.jsonl` transcripts as
  `iflow_cli_session_jsonl_tree`.

## Kode

- Source evidence: `@shareai-lab/kode@2.2.1` declares repository
  `shareAI-lab/kode`.
- Bundle path evidence: `dist/chunk-HGY32KZM.js` builds project history under
  `<kode-base>/projects/<sanitized-cwd>/<sessionId>.jsonl`, using
  `cwd.replace(/[^a-zA-Z0-9]/g, "-")` for the project directory.
- Sidechain evidence: the same bundle writes agent logs as
  `agent-<agentId>.jsonl` in the project directory and appends records with
  JSONL serialization.
- Config evidence: `dist/chunk-4OKQLS3L.js` resolves `KODE_CONFIG_DIR`, then
  `CLAUDE_CONFIG_DIR`, then `~/.kode`; the legacy global config file is
  commonly `~/.kode.json`.
- Schema evidence: Kode records include `type`, `uuid`, `parentUuid`,
  `sessionId`, `cwd`, `timestamp`, `message`, optional `toolUseResult`,
  `isSidechain`, and `agentId`; helper code also appends `summary`,
  `custom-title`, `tag`, and `file-history-snapshot` rows.
- `ctx` imports this shape as `kode_session_jsonl_tree`, including
  `agent-<id>.jsonl` sidechains as child sessions.

## Neovate

- Source evidence: `@neovate/code@0.28.5` declares repository
  `neovateai/neovate-code`.
- Type evidence: `dist/index.d.ts` defines `NormalizedMessage` as a message with
  `type: "message"`, `timestamp`, `uuid`, `parentUuid`, and optional metadata,
  and defines `Paths` with `globalConfigDir`, `globalProjectDir`,
  `projectConfigDir`, `fileHistoryDir`, and `getSessionLogPath`.
- Bundle path evidence: `dist/index.mjs` stores global projects under
  `~/.neovate/projects/<sanitized-cwd>`, resolves normal session IDs to
  `<globalProjectDir>/<sessionId>.jsonl`, and keeps file-history sidecars under
  `<globalProjectDir>/file-history`.
- Logger evidence: the bundled JSONL logger appends normalized message rows and
  snapshot rows; the request logger writes request logs under
  `<globalProjectDir>/requests/<requestId>.jsonl`.
- `ctx` imports this shape as `neovate_session_jsonl_tree` and excludes
  `requests/` and `file-history/` JSONL sidecars from primary session import.

## ForgeCode

- Source evidence: Forge repository commit
  `b06194fef8ee7bdad9a5cc3a4e30fa4f761deb51`.
- Database evidence:
  `crates/forge_repo/src/database/migrations/2025-09-12-065405_create_conversations_table/up.sql`
  creates `conversations(conversation_id, title, workspace_id, context,
  created_at, updated_at)`, and
  `crates/forge_repo/src/database/migrations/2025-10-16-000000_add_metrics_to_conversations/up.sql`
  adds `metrics`.
- DTO evidence:
  `crates/forge_repo/src/conversation/conversation_record.rs` serializes
  `context` as the conversation DTO with text, tool, and image message variants,
  tool calls/results, usage, tool metadata, and generation options.
- Metrics evidence: the same DTO file serializes `metrics` with
  `files_changed` and `files_accessed`, including line deltas, tool name, and
  content hash where available.
- Location evidence: ForgeCode uses `FORGE_CONFIG` as the base directory when
  set, otherwise legacy `~/forge` when present, otherwise `~/.forge`; the
  conversation DB is `<base>/.forge.db`.
- `ctx` imports this shape as `forgecode_sqlite`, using read-only SQLite
  discovery only when the `conversations` table exists.
- Caveat: ForgeCode stores conversation messages as a mutable context JSON
  snapshot rather than append-only message rows, so ctx uses message array
  indexes for event cursors and retains capped raw context/metrics JSON for DTO
  fields that are not explicitly normalized yet.

## Mistral Vibe

- Source evidence: Mistral Vibe repository commit
  `474a0e4055b210a60c39ed0c89458d904b7f6a7b`.
- Path evidence: `vibe/core/paths/_vibe_home.py` resolves `VIBE_HOME` first
  and otherwise defaults to `~/.vibe`; its session log directory is
  `VIBE_HOME/logs/session`.
- Configuration evidence: `vibe/core/config/models.py` defines
  `SessionLoggingConfig.save_dir`, defaulting to the session log directory and
  expanding configured paths.
- Runtime evidence: `vibe/core/session/session_logger.py` creates session
  directories named from a session prefix, timestamp, and short id, writes
  `meta.json`, and appends transcript rows to `messages.jsonl`.
- Loader evidence: `vibe/core/session/session_loader.py` treats `meta.json` and
  `messages.jsonl` as the required files for a loadable saved session.
- Type evidence: `vibe/core/types.py` defines session metadata plus LLM message
  fields including role, content, reasoning content, tool calls, tool call id,
  message id, images, and usage-adjacent metadata.
- `ctx` imports this shape as `mistral_vibe_session_jsonl_tree`, using
  read-only discovery only when a session directory contains both required
  files.
- Caveat: message rows do not consistently include per-row timestamps, so ctx
  uses row timestamps when available and otherwise falls back to deterministic
  session/import timestamps.

## Mux

- Source evidence: `coder/mux` tag `v0.27.0`, commit
  `2658ea088d00d4186e8614d04a411a0d2a24aa10`.
- Path evidence: `src/common/constants/paths.ts` resolves `MUX_ROOT` first and
  otherwise defaults to `~/.mux`; sessions are under
  `<mux-root>/sessions/<workspaceId>/chat.jsonl`.
- Runtime evidence: `src/node/services/historyService.ts` defines
  `CHAT_FILE = "chat.jsonl"` and `PARTIAL_FILE = "partial.json"`, writes each
  message as a JSONL row with `workspaceId`, and stages in-progress messages in
  `partial.json`.
- Type evidence: `src/common/orpc/schemas/message.ts` defines `MuxMessage` with
  `id`, `role`, `parts[]`, optional `createdAt`, and rich `metadata` including
  `historySequence`, `timestamp`, `model`, `usage`, `providerMetadata`,
  `muxMetadata`, and `partial`.
- Merge evidence: Mux merges `partial.json` by `metadata.historySequence`,
  replacing an existing row only when the partial has more `parts`; otherwise it
  appends or inserts by sequence.
- Subagent evidence: archived child transcripts live under parent session
  directories in `subagent-transcripts/<childTaskId>/chat.jsonl`, with optional
  `partial.json` and `subagent-transcripts.json` index metadata.
- `ctx` imports this shape as `mux_session_jsonl_tree`, using read-only
  discovery for `chat.jsonl` and optional `partial.json` under `MUX_ROOT` or
  `~/.mux/sessions`.
- Caveat: the checked-in fixture is sanitized rather than runtime-generated
  because producing a real Mux chat requires model/auth setup; the fixture is
  schema-backed by the source anchors in
  `tests/fixtures/provider-history/mux/v0.27.0/README.md`.

## Reasonix

- Source evidence: `reasonix@0.53.2` resolves to `esengine/DeepSeek-Reasonix`
  tag `v0.53.2` (`b307987c0bb86ebee80b0d058ed92de75419ad8e`).
- Path evidence: `src/memory/session.ts` defines session files under
  `~/.reasonix/sessions/<sanitizeName(session)>.jsonl`.
- Sidecar evidence: the same module defines `.events.jsonl`, `.meta.json`,
  `.pending.json`, `.plan.json`, and `.jsonl.bak` as known session sidecars,
  and rename/delete operations move those sidecars with the base JSONL.
- Session evidence: `src/types.ts` defines `ChatMessage` rows with `role`,
  `content`, tool-call fields, and `reasoning_content`; `session.ts` appends
  those messages as JSONL and loads parsed records that have `role`.
- Event evidence: `src/adapters/event-sink-jsonl.ts` appends typed events to
  `<session>.events.jsonl`; `src/core/events.ts` defines user/model/tool/file,
  plan, usage, cost, and error event fields.
- Transcript evidence: `src/transcript/log.ts` defines an explicit transcript
  JSONL format with `_meta`, `turn`, `role`, `content`, tool, error, usage,
  and cost fields.
- `ctx` imports this shape as `reasonix_session_jsonl_tree`, reading base
  session JSONL plus adjacent event/meta/pending/plan sidecars read-only.
- Caveat: base session `ChatMessage` rows do not carry timestamps, so ctx uses
  event/transcript timestamps when present and otherwise falls back to the
  deterministic import timestamp.

## Terramind

- Source evidence: `npm view terramind@0.2.91` reports repository
  `terramind-io/ide`, homepage `https://terramind.com`, and CLI bin
  `terramind.cjs`.
- Path evidence: the published `terramind-0.2.91.tgz` bundle resolves the CLI
  app data root through `getCliUserDataPath()` using the app name `Nucleus`
  (`Nucleus Dev` when `NUCLEUS_DEV` is set), then `getDatabasePath()` appends
  `data/agents.db`. On Linux that is `$XDG_CONFIG_HOME/Nucleus/data/agents.db`
  or `~/.config/Nucleus/data/agents.db`; macOS uses
  `~/Library/Application Support/Nucleus/data/agents.db`; Windows uses
  `%APPDATA%/Nucleus/data/agents.db`.
- Schema evidence: the bundled Drizzle schema and migrations create
  `projects`, `chats`, `sub_chats`, and `tool_outputs`; `sub_chats.messages`
  is JSON text, and `tool_outputs.full_output` stores split large tool output.
- Runtime evidence: bundled runner code parses and stringifies
  `sub_chats.messages` JSON arrays and stores large tool outputs through the
  `tool_outputs` table.
- `ctx` imports this shape as `terramind_agents_sqlite`, using read-only SQLite
  discovery only when the `projects`, `chats`, and `sub_chats` tables exist.
- Caveat: a temporary-home `npx terramind@0.2.91 list --chats` probe did not
  complete without interactive setup, so the fixture is a sanitized SQLite DB
  built from the package-backed schema rather than a live-generated chat.

## OpenHands

- Source evidence: OpenHands `get_default_persistence_dir()` checks
  `OH_PERSISTENCE_DIR`, then legacy `FILE_STORE_PATH`, then `~/.openhands`.
- The filesystem event service stores events as JSON with
  `event.model_dump_json(indent=2)`.
- Conversation paths are
  `<persistence>/<user_id>/v1_conversations/<conversation-id-hex>/<event-id-hex>.json`.
- `ctx` imports this shape as `openhands_file_events` without OpenHands runtime
  dependencies.

## Zed

- Source evidence: zed-industries/zed commit
  `e3b73c6b30cdc09e820823fe44542b89850d4be1`.
- `crates/agent/src/db.rs` creates the agent thread database at
  `<Zed data dir>/threads/threads.db`, creates and migrates the `threads`
  table, and saves `SerializedThread { thread: DbThread, version }` into
  `threads.data` with `data_type = 'zstd'`.
- Table shape observed in source:
  `threads(id, summary, updated_at, data_type, data, parent_id, folder_paths, folder_paths_order, created_at)`.
- `crates/agent/src/db.rs` loads both current `zstd` rows and legacy `json`
  rows; ctx mirrors that behavior and caps decompressed JSON size.
- `crates/agent/src/thread.rs` defines the externally tagged `DbThread.messages`
  variants (`User`, `Agent`, `Resume`, `Compaction`) and agent/user content
  variants used by the importer.
- `crates/paths/src/paths.rs` resolves the Linux data dir through
  `$XDG_DATA_HOME/zed` or `~/.local/share/zed`.
- `ctx` imports this shape as `zed_threads_sqlite`.
- Caveat: `DbThread` messages do not carry per-message timestamps, so ctx uses
  thread-level `updated_at` for imported event timestamps.

## Void

- Source evidence: Void stores chat threads through VS Code/Electron application
  storage.
- Current key: `void.chatThreadStorageII`; older keys:
  `void.chatThreadStorage` and `void.chatThreadStorageI`.
- Values are JSON-serialized chat thread/message structures in application
  storage.
- Gap: native import needs a small, proven reader for the VS Code/Electron
  storage location and key formats, plus fixtures covering all three keys.
