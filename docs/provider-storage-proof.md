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
