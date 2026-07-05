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

## Amp Thread Export

- Source evidence: `@ampcode/cli@0.0.1783181941-g187572` exposes
  `amp threads export <threadIDOrURL>`, `amp threads markdown`, and
  `amp threads raw`.
- Export-handler evidence: the bundled CLI export path loads the requested
  thread through the same thread loader used by markdown/raw views and serializes
  the returned thread JSON. The markdown renderer consumes `id`, `created`,
  optional `title`/`agentMode`, and message blocks including visible text,
  `tool_use`, `tool_result`, `summary`, and `manual_bash_invocation`.
- `ctx` imports this official explicit export shape as
  `amp_threads_export_json` only when the user supplies a file with
  `ctx import --provider amp --path <export.json>`.
- Fixture provenance: `tests/fixtures/provider-history/amp/threads-export/redacted-thread.json`
  is a sanitized package/source-backed fixture. This environment had no `amp`
  binary on `PATH` and no Amp/OpenRouter/Anthropic key environment variables, so
  no safe live generated export fixture was available.
- Caveat: ctx does not discover or parse `~/.config/amp`,
  `$XDG_CACHE_HOME/amp/logs/cli.log`, account/auth files, or remote Amp thread
  history. Default discovery remains unclaimed until Amp proves durable local
  transcript storage distinct from operational logs.

## Dexto

- Package evidence: public npm packages `dexto@1.9.1`,
  `@dexto/storage@1.9.1`, and `@dexto/agent-management@1.9.1`.
- Storage evidence: `@dexto/storage/dist/database/sqlite-store.js` creates
  `kv_store` and `list_store`; `dist/database/schemas.js` requires a SQLite
  `path` and notes CLI enrichment provides the per-agent path.
- Path evidence:
  `@dexto/agent-management/dist/config/config-enrichment.js` fills missing
  SQLite storage paths by calling `getDextoPath("database", ...)` with an
  `<agentId>.db` filename.
- Path evidence: `@dexto/core/dist/utils/path.js` maps global CLI resources to
  `~/.dexto/<type>` and Dexto project resources to `<project>/.dexto/<type>`,
  so the enriched database roots are `~/.dexto/database` and project
  `.dexto/database`.
- `ctx` imports this shape as `dexto_sqlite`, scanning bounded
  `.dexto/database/*.db` roots only when a SQLite file has the Dexto
  `kv_store`/`list_store` schema; explicit Dexto DB paths remain supported.
- Caveat: `~/.dexto/logs`, agents, blobs, `.env`, config files, and arbitrary
  cache paths are not history sources. Custom `storage.database.path` values
  outside the default roots still require explicit `--path`.

## Lingma

- Source evidence: WayLog `shayne-snap/WayLog` commit
  `6939033b7a39326fbdc249e28e6aa12461db1f09`,
  `src/services/readers/lingma-reader.ts`
  (`https://raw.githubusercontent.com/shayne-snap/WayLog/6939033b7a39326fbdc249e28e6aa12461db1f09/src/services/readers/lingma-reader.ts`).
- Default storage paths proven by that reader are
  `~/.lingma/vscode/sharedClientCache/cache/db/local.db` and
  `~/.lingma/vscode-insiders/sharedClientCache/cache/db/local.db`.
- The reader queries SQLite table
  `chat_record(session_id, request_id, chat_prompt, summary, error_result,
  gmt_create, extra)` where `chat_prompt` is nonempty.
- WayLog labels the Lingma reader summaries-only and notes original answers may
  be encrypted. `ctx` therefore imports user `chat_prompt` values plus assistant
  `summary` or meaningful `error_result` values with partial/summary-only
  fidelity, not as full assistant transcripts.
- Alibaba public docs say Qoder CN is the renamed Lingma product line, and the
  official Qoder CN VSIX/package identity remains `Alibaba-Cloud.tongyi-lingma`.
  That ties Qoder CN to the same extension-local `~/.lingma/.../local.db`
  contract, so `ctx` accepts `qoder-cn` and `qoder_cn` as aliases for the Lingma
  importer.
- Separate Qoder IDE homes such as `~/.qodercn` or `~/.qoder-cn` remain
  unclaimed because no source-backed local transcript schema was found for those
  paths. Source anchors:
  `https://www.alibabacloud.com/help/zh/lingma/product-overview/introduction-of-lingma`
  and
  `https://www.alibabacloud.com/help/en/lingma/product-overview/qoder-cn-update-log`.

## Qoder

- Source evidence: official Qoder Hooks docs define the Transcript file format
  and state that the transcript is an automatically generated session log at
  `~/.qoder/projects/<project>/transcript/<session-id>.jsonl`, with one JSON
  object per line and complete session interaction records.
- Schema evidence: those docs define common line fields `type`, `sessionId`,
  `uuid`, `timestamp`, `cwd`, `message`, and `data`; documented record types
  are `session_meta`, `user`, `assistant`, and `progress`.
- Message evidence: documented user records include plain string prompts and
  `tool_result` content blocks with `toolUseResult`; documented assistant
  records include `text` and `tool_use` content blocks with tool `name`,
  `input`, and `id`.
- Extraction evidence: the docs publish `jq` examples for user questions,
  assistant text replies, tool calls, failed tool results, session mode, and
  hook trigger records from the same transcript file.
- `ctx` imports this shape as `qoder_transcript_jsonl_tree`, using read-only
  discovery under `~/.qoder/projects` or explicit transcript
  file/directory/project paths.
- Caveat: ctx imports only the official transcript JSONL tree. It does not parse
  Qoder VS Code/Electron state databases, message indexes, or encrypted
  application logs. Qoder CN remains covered separately as a Lingma alias.
- Source anchor: `https://docs.qoder.com/extensions/hooks`.

## CodeArts Agent

- Source evidence: the current Visual Studio Marketplace page for
  `HuaweiCloud.vscode-codebot` identifies CodeArts Agent as a VS Code extension
  and documents editor/chat features such as R&D Q&A, slash commands, chat-panel
  shortcuts, and selected-code actions.
- Package evidence: VSIX `HuaweiCloud.vscode-codebot` v26.6.0, SHA256
  `394f54ba999bdab8095f9bdd3ccd28ce5e0df3ab804911703efdbe1a329a2d34`,
  resolves `~/.codeartsdoer` and routes current session operations through an
  agent-kernel client.
- Storage evidence: the kernel store is an OpenCode-derived SQLite family under
  `~/.codeartsdoer/.../opencode.db`, while older/cache evidence also shows VS
  Code app-data paths such as `User/chat_sessions/<session>/chat_baseInfo.json`,
  `User/chat_sessions/<session>/messages_<n>.json`, and `claw_session`.
- `ctx` imports this native shape as `codearts_agent_kernel_sqlite` from
  `~/.codeartsdoer/vscode-data/opencode.db`,
  `~/.codeartsdoer/codearts-data/opencode.db`,
  `$XDG_DATA_HOME/codeartsdoer/opencode.db`,
  `~/.local/share/codeartsdoer/opencode.db`, or an explicit DB path. The
  native-auto claim is for the kernel SQLite DB, not the legacy JSON cache.
- Caveat: ctx does not import older/private VS Code cache JSON such as
  `User/chat_sessions` or `claw_session`; those remain unclaimed until fixtures
  prove they are durable and complete.
- Source anchor:
  `https://marketplace.visualstudio.com/items?itemName=HuaweiCloud.vscode-codebot`.

## Code Studio

- Source evidence: Syncfusion Code Studio docs describe an AI-powered IDE with
  Ask, Edit, Agent, Plan, checkpoint, hooks, custom agents, and Agent Skills
  features.
- Artifact evidence: Syncfusion Code Studio v2.0.4 artifact SHA256
  `603991729cbb154e1bbc1a12d292b389895e1c2fb6f07b25bc1cda4fe14fa13b`
  backs the local session-store SQLite shape.
- Docs evidence: `syncfusion-content/syncfusion-code-studio-docs` commit
  `de44c7b35fc30ece937ff85d113b5b360a3e3955` documents sessions, agent
  features, settings, hooks, and skill locations including `.codestudio/agents`,
  `~/.codestudio/agents`, `.codestudio/skills`, and `~/.codestudio/skills`.
- `ctx` tracks this intended native shape as `codestudio_session_store_sqlite`,
  with native-auto discovery for Code Studio app-data
  `User/globalStorage/session-store.db` files and explicit DB paths. Discovery
  requires the session-store SQLite tables/columns before auto-importing.
- Caveat: `.codestudio/skills`, `.codestudio/agents`, and settings files remain
  configuration evidence only; debug logs are not imported; the importer claim
  is limited to the session-store SQLite DB.
- Source anchors:
  `https://help.syncfusion.com/code-studio/getting-started/overview` and
  `https://help.syncfusion.com/code-studio/reference/configure-properties/usersettings`.

## Zencoder

- Exporter evidence: `jverre/opik-chat-history` commit
  `5e7380933564d4fe1084d0e6f48f0e49e43e45ea` reads Zencoder chat history from a
  local `zencoder-chat` folder with `sessions.json` plus
  `sessions/<session-id>.json`.
- Extension evidence: Open VSX `ZencoderAI.zencoder` v3.63.9002 constants
  confirm the extension `zencoder-chat` storage root and `sessions.json`
  persisted-chat anchors used for local chat/session history.
- `ctx` tracks this native-auto shape as `zencoder_chat_sessions_json_tree`.
  Default discovery covers common VS Code-family app-data roots under
  `User/globalStorage/ZencoderAI.zencoder/zencoder-chat`, and explicit
  `ctx import --provider zencoder --path <zencoder-chat>` remains supported.
- Caveat: `~/.zencoder` remains npx skill/config evidence; native import reads
  the proven `zencoder-chat` session tree, not arbitrary extension caches.

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

## Jazz

- Source evidence: the published `jazz-ai@0.12.5` package bundle resolves
  `JAZZ_HOME` first and otherwise falls back to `~/.jazz`.
- Bundle storage evidence: the history helper resolves the history directory as
  `<jazz-home>/history` and writes per-agent files at
  `<history>/<agentId>.json`.
- Schema evidence: the writer persists a top-level object with `agentId` and
  `conversations`; each conversation includes `conversationId`, `title`,
  `agentId`, `startedAt`, `endedAt`, `messageCount`, and `messages`.
- Retention evidence: the package writer prepends the latest conversation and
  slices the retained list to the package's current maximum, so ctx imports only
  conversations still present in the history JSON file.
- `ctx` imports this shape as `jazz_history_json`, using read-only discovery
  under `JAZZ_HOME/history` or `~/.jazz/history`.

## Windsurf

- Source evidence: official Windsurf Cascade Hooks docs at
  `https://docs.windsurf.com/windsurf/cascade/hooks` redirect to Devin Desktop
  Cascade Hooks docs.
- Hook evidence: `post_cascade_response_with_transcript` writes JSONL
  transcript files to `~/.windsurf/transcripts/{trajectory_id}.jsonl`.
- Schema evidence: the docs examples show one JSON object per line with `type`,
  `status`, and payloads such as `user_input.user_response`,
  `planner_response.response`, and `code_action.path`/`new_content`.
- Privacy evidence: the docs warn transcripts may include file contents,
  command outputs, tool arguments, search results, rules, and conversation
  history, and that exact step structure may change.
- `ctx` imports this shape as
  `windsurf_cascade_hook_transcript_jsonl_tree`, from explicit file/directory
  paths or native default discovery under `~/.windsurf/transcripts`.
- Boundary: support is limited to official hook transcript JSONL. Hook
  transcripts are opt-in and usually capture sessions after hook setup, so ctx
  does not backfill earlier sessions from other Windsurf storage.
- Caveat: ctx does not parse the private `~/.codeium/windsurf/cascade` cache or
  guessed VS Code state databases.

## Pochi

- Source evidence: Pochi's `packages/livekit/src/livestore/default-schema.ts`
  defines LiveStore materialized state tables for `tasks`, `messages`, and
  `files`.
- Message type evidence: `packages/livekit/src/livestore/types.ts` defines UI
  messages with `role` plus typed `parts`, including text and tool parts.
- CLI storage evidence: `packages/cli/src/livekit/store.ts` enables filesystem
  LiveStore sync only when `POCHI_LIVEKIT_SYNC_ON` is set and stores state under
  `~/.pochi/storage/<storeId>/state<schemaHash>@6.db`.
- `ctx` imports this shape as `pochi_livestore_state_sqlite`, reading synced
  `~/.pochi/storage` roots when present, explicit SQLite file paths, or
  explicit directories containing `state*.db` files read-only.
- This is native-auto only for bounded LiveStore state SQLite paths:
  `~/.pochi/storage/**/state*.db` and explicit state DB file/directory paths.
  Default discovery requires a read-only `tasks`/`messages` schema probe. `ctx`
  does not read `config.jsonc` and does not attempt VS Code OPFS extraction.

## OpenLoaf

- Source evidence: the public [OpenLoaf repository](https://github.com/OpenLoaf/OpenLoaf)
  documents a local-first app with data rooted in the user's OpenLoaf
  directories.
- Path evidence: [`packages/config/src/openloaf-paths.ts`](https://raw.githubusercontent.com/OpenLoaf/OpenLoaf/main/packages/config/src/openloaf-paths.ts)
  confirms the root directory `~/.openloaf`, the SQLite file name
  `openloaf.db`, and the data root `~/OpenLoafData`.
- Session path evidence: [`packages/api/src/services/chatSessionPaths.ts`](https://raw.githubusercontent.com/OpenLoaf/OpenLoaf/main/packages/api/src/services/chatSessionPaths.ts)
  defines `chat-history` and `messages.jsonl`, with global, project
  `.openloaf/chat-history`, and board-scoped chat-history directory
  conventions.
- Message schema evidence: [`apps/server/src/ai/services/chat/repositories/chatFileStore.ts`](https://raw.githubusercontent.com/OpenLoaf/OpenLoaf/main/apps/server/src/ai/services/chat/repositories/chatFileStore.ts)
  writes optional `session.json` plus StoredMessage JSONL records containing
  `id`, `parentMessageId`, `role`, `messageKind`, `parts`, `metadata`, and
  `createdAt`.
- `ctx` imports this shape as `openloaf_chat_jsonl_tree`, using read-only
  discovery under `~/.openloaf/chat-history` and bounded
  `~/OpenLoafData/projects` project chat-history roots. Explicit imports also
  accept a project `.openloaf/chat-history`, session directory, or
  `messages.jsonl` file.
- npx-skills detects the agent id `loaf`; ctx maps `loaf` and `openloaf` CLI
  aliases to the canonical provider `openloaf`. The native importer does not
  treat `~/.loaf` as a source-backed history path.

## Auggie / Augment CLI

- Source evidence: the published `@augmentcode/auggie@0.32.0` package bundle
  contains the Augment CLI implementation.
- Official Augment CLI docs describe the npm-installed `auggie` command,
  `--augment-cache-dir` defaulting to `~/.augment`, resumable sessions,
  `auggie session list`, session sharing, and `--dont-save-session`.
- Bundle storage evidence: `SessionManager` resolves the session directory as
  `<augment-cache-dir>/sessions`, and `SessionStore` stores each session as
  `<session_id>.json`.
- Schema evidence: package code writes JSON objects with `sessionId`, `created`,
  `modified`, `workspaceId`, optional workspace path metadata, `agentState`, and
  `chatHistory`. Each `chatHistory` item includes an `exchange`, completion
  flags, sequence/timestamp metadata, changed-file arrays, history-summary
  markers, and a source marker.
- Exchange evidence: remote session materialization maps backend exchanges into
  `exchange.request_message`, `exchange.response_text`, and `exchange.request_id`.
  Current package code also carries node arrays; ctx imports recognized text-node
  content when present.
- Direct no-auth probe evidence: running Auggie 0.32.0 with a temporary
  `--augment-cache-dir` created the cache/session directory but could not
  produce full transcript JSON without Augment login. The repository fixture is
  therefore minimized from the verified package schema rather than invented from
  a live private account.
- `ctx` imports this shape as `auggie_session_json`, using read-only discovery
  under `~/.augment/sessions` or an explicit session JSON/session directory.
- Caveat: ctx indexes user/assistant request and response text only. Richer tool,
  changed-file, and IDE/application history semantics remain unclaimed until a
  public contract or fixture proves them.

## Devin CLI ATIF Export

- Official CLI evidence: the Devin CLI command reference documents
  `devin --export [PATH]` and states that it exports the conversation to a file
  after each turn in ATIF format. The same page shows both default-path and
  explicit-file examples.
- Official changelog evidence: the 2026.5.26-0 stable changelog entry added the
  `--export` flag for ATIF conversation history. A later stable entry says ATIF
  exports include richer per-step transcript details and timing metrics.
- Schema evidence: the public ATIF RFC defines a JSON root with
  `schema_version`, `agent`, and `steps`; step records carry message/source data
  and optional tool/observation-like details.
- `ctx` imports this shape as `devin_atif_json` only through an explicit
  user-supplied path: `ctx import --provider devin --path <atif-file-or-dir>`.
  A directory import recursively scans `.json` files; an explicit file path is
  validated by ATIF content even if a temporary fixture path has no `.json`
  suffix.
- Fixture provenance: `tests/fixtures/provider-history/devin/atif/export.json`
  is synthetic, minimized from the public ATIF root/step contract and Devin CLI
  export docs. It is not copied from a Devin account, Devin cloud history, local
  Devin config, login state, or a private user session.
- Caveat: ctx does not discover or parse `~/.config/devin`, cloud/object-store
  state, login/account paths, or any default local Devin history location. Those
  paths remain unclaimed until a public local storage contract or safe generated
  export fixture proves them.

## Firebender

- Registry evidence: `vercel-labs/skills` maps the `firebender` agent id to
  `~/.firebender/skills`.
- Source evidence: the public Firebender 1.0.10 JetBrains Marketplace plugin
  artifact (`updateId=1045537`) contains `ChatHistoryDatabaseService`, which
  decodes and opens a project-local `.idea/firebender/chat_history.db` SQLite
  database.
- Schema evidence: the decoded database service creates `chat_sessions` with
  `id`, `name`, `created_at`, `updated_at`, `deleted_at`, `messages_json`, and
  `metadata_json`.
- Message evidence: serialized Firebender message/content classes carry
  `role`, `content`, `tool_calls`, `tool_call_id`, `type`, and `text` field
  names that match the fixture's `messages_json` shape.
- `ctx` imports this shape as `firebender_chat_history_sqlite`, using explicit
  DB paths, explicit project-root paths, and current-project ancestor discovery
  for `.idea/firebender/chat_history.db`.
- Caveat: no global `~/.firebender` chat transcript file is claimed; the proven
  chat store is project-local.

## Eve

- Source evidence: the published `eve@0.19.0` package documents durable
  sessions and replayable streams; local development and self-deployed
  `eve start` use Workflow local-world storage by default.
- Workflow local-world evidence: `@workflow/world-local@5.0.0-beta.22`
  resolves the data directory from `WORKFLOW_LOCAL_DATA_DIR` or `.workflow-data`
  and stores runs, stream maps, chunks, steps, events, and hooks as local JSON
  and binary files.
- Storage evidence: local stream maps are written under
  `.workflow-data/streams/runs/<runId>.json`, and stream chunks are written
  under `.workflow-data/streams/chunks/<streamName>-<chunkId>.bin`.
- Eve runtime evidence: the Eve workflow entry obtains a default workflow
  writable stream, and turn steps write `encodeMessageStreamEvent(...)` records
  to that parent writable. The message protocol encodes each event as NDJSON.
- Encoding evidence: current Workflow local-world stream chunks prepend a
  one-byte EOF marker; data chunks then contain length-prefixed Workflow frames.
  Eve message bytes are wrapped as `devl`-serialized `Uint8Array` payloads that
  decode back to NDJSON stream events such as `message.received`,
  `message.completed`, `actions.requested`, and `action.result`.
- `ctx` imports this shape as `eve_workflow_data_streams`, using conservative
  discovery from `WORKFLOW_LOCAL_DATA_DIR` and current-project
  `.workflow-data` only when stream maps and matching chunks exist. Explicit
  import also accepts a `.workflow-data` directory or project root.
- Caveat: `.eve/` contains build/runtime artifacts and dev-server state, not
  the durable transcript source. ctx does not import `.eve/` as history.
- Caveat: ctx imports completed/default Eve message stream events and skips
  incremental deltas to avoid duplicate transcript text. Encrypted or
  non-devalue Workflow stream chunks remain unclaimed until source proof and
  fixtures exist.

## Junie

- Source-backed storage evidence: the public Claudescope Junie connector
  documents and reads `~/.junie/sessions/index.jsonl` plus
  `~/.junie/sessions/session-*/events.jsonl`, and its implementation says it
  was built against real Junie on-disk data from build `1892.22`.
- Event-shape evidence: Claudescope parses top-level `UserPromptEvent` rows and
  `SessionA2uxEvent` wrappers around nested `agentEvent` blocks including
  `LlmResponseMetadataEvent`, `TerminalBlockUpdatedEvent`,
  `ViewFilesBlockUpdatedEvent`, `FileChangesBlockUpdatedEvent`, and
  `ResultBlockUpdatedEvent`.
- JetBrains release evidence: the downloaded Junie release jar contains matching
  event classes such as `UserPromptEvent`, `SessionA2uxEvent`,
  `LlmResponseMetadataEvent`, and file/result block events, though the cloned
  first-party installer repo does not document the `index.jsonl` path itself.
- `ctx` imports this shape as `junie_session_events_jsonl_tree`, using
  `JUNIE_SESSIONS_DIR`, `JUNIE_HOME/sessions`, `~/.junie/sessions`, or explicit
  sessions/session paths. Discovery follows `index.jsonl` and rejects unsafe
  `sessionId` path segments.
- Caveat: Junie's local file schema is not first-party documented and may drift.
  ctx imports the UI event stream blocks that exist in `events.jsonl`; it does
  not read attachment image paths or project memory directories.

## Antigravity

- CLI source evidence: existing ctx support imports Antigravity CLI brain transcript
  JSONL files from `~/.gemini/antigravity-cli/brain`.
- IDE source evidence: official Antigravity IDE hook docs expose a
  `transcriptPath` and document IDE transcript JSONL under
  `~/.gemini/antigravity-ide/brain/<conversationId>/.system_generated/logs/transcript.jsonl`.
- CLI/IDE bridge evidence: official Antigravity CLI conversation docs state the
  CLI can import desktop/IDE conversations, supporting the shared transcript
  family rather than a separate opaque IDE database.
- `ctx` imports this shape as `antigravity_cli_transcript_jsonl_tree`, using
  read-only discovery for both CLI and official IDE brain roots. The npx skills
  `~/.gemini/antigravity` skill/config path is not treated as a transcript root.

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

## Command Code

- Source evidence: the published `command-code@0.41.1` package writes project
  history under `~/.commandcode/projects/<sanitized-cwd>/<sessionId>.jsonl`.
- Bundle evidence: session records include `id`, `sessionId`, optional
  `parentId`, `role`, `content`, `timestamp`, `gitBranch`, and `metadata`;
  sidecars include `<sessionId>.meta.json`, `<sessionId>.checkpoints.jsonl`,
  and prompt/checkpoint JSONL files.
- `ctx` imports this shape as `command_code_session_jsonl_tree`, using read-only
  discovery under `~/.commandcode/projects`.
- The importer requires a `sessionId` row, imports `parentId` as a child-session
  edge, and skips prompt/checkpoint sidecars. Non-session JSONL files found
  while scanning a project tree are skipped so valid sessions still import.
- Privacy note: ctx stores local transcript text and path references in its
  local history store only. External reporting must not include transcript paths,
  usernames, repository paths, message content, or file bodies.

## Rovo Dev

- Source evidence: Atlassian Rovo Dev CLI session management docs describe the
  sessions directory under `~/.rovodev/sessions`, with per-session
  `session_context.json` and `metadata.json` files.
- Schema evidence: public Rovo Dev readers parse `session_context.json`
  `message_history[]` entries with message roles/kinds and part arrays, while
  `metadata.json` carries title, workspace path, session IDs, and fork/parent
  metadata.
- `ctx` imports this shape as `rovodev_session_json_tree`, using read-only
  discovery under `~/.rovodev/sessions`.
- The importer accepts a sessions root, a single session directory, or a
  `session_context.json` file; it imports message text, tool-use/tool-result
  parts, cwd metadata, and parent-session metadata when present.
- Privacy note: Rovo Dev metadata can contain local workspace paths. Those paths
  stay local and are not part of external reporting.

## Cortex Code

- Source evidence: Snowflake Cortex Code CLI and ACP docs document local
  conversations under `~/.snowflake/cortex/conversations`, including
  `<sessionId>.json` snapshots and `<sessionId>.history.jsonl` append-only
  history sidecars.
- Schema evidence: public Cortex Code replay tooling parses snapshot fields
  such as `title`, `session_id`, `created_at`, `last_updated`,
  `connection_name`, and `working_directory`, plus history rows with `role`,
  `user_sent_time`, and content blocks such as `text`, `thinking`, `tool_use`,
  and `tool_result`.
- `ctx` imports this shape as `cortex_code_conversations_json`, using read-only
  discovery under `~/.snowflake/cortex/conversations`.
- The importer groups snapshot and `.history.jsonl` sidecar files by session ID.
  History JSONL rows are preferred; snapshot `history`/`messages` arrays are
  used as fallback when no sidecar rows exist.
- Privacy note: Cortex Code conversation files can include local working
  directories, SQL context, and tool outputs. ctx keeps imported data local and
  external reporting must remain coarse/provider-level only.

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

## Deep Agents

- Official docs evidence: Deep Agents Code data-location docs list sessions at
  `~/.deepagents/.state/sessions.db` as a read/write SQLite checkpoint database
  and input history at `~/.deepagents/.state/history.jsonl` as JSON lines.
- Source evidence: Deep Agents Code repository commit
  `bf5e551399565136e35628e046e0ad6cede6df3f`.
- Path evidence: `libs/code/deepagents_code/model_config.py` defines
  `DEFAULT_CONFIG_DIR` as `~/.deepagents` and `DEFAULT_STATE_DIR` as
  `~/.deepagents/.state`.
- Checkpointer evidence: `libs/code/deepagents_code/sessions.py` resolves
  `sessions.db` under `DEFAULT_STATE_DIR` and opens it with
  `AsyncSqliteSaver.from_conn_string`.
- Schema evidence: `langgraph-checkpoint-sqlite` `AsyncSqliteSaver.setup`
  creates `checkpoints(thread_id, checkpoint_ns, checkpoint_id,
  parent_checkpoint_id, type, checkpoint, metadata)` and
  `writes(thread_id, checkpoint_ns, checkpoint_id, task_id, idx, channel, type,
  value)`.
- Fixture evidence: `tests/fixtures/provider-history/deepagents/v1/sessions.db`
  was generated with `AsyncSqliteSaver` and sanitized LangChain messages, without
  API credentials.
- `ctx` imports this shape as `deepagents_sessions_sqlite`, using read-only
  discovery only when both `checkpoints` and `writes` tables exist.
- Caveat: ctx indexes decoded chat messages only from root
  `writes.channel = 'messages'`; it does not index checkpoint state blobs or the
  separate `history.jsonl` input-history file.

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

## CodeMaker CLI (insufficient proof)

- npx skills evidence: `skills@1.5.14` registers the `codemaker` target with
  `skillsDir = ".codemaker/skills"`,
  `globalSkillsDir = "~/.codemaker/skills"`, and install detection based only
  on `~/.codemaker`.
- Public source evidence: the `codemakerai/codemaker-cli` repository reads and
  writes `~/.codemaker/config` for API configuration and then calls the
  CodeMaker SDK for generation/refactor/fix commands.
- Negative evidence: source search found no durable local session,
  conversation, chat, transcript, SQLite, or JSONL history store.
- Decision: ctx should not add a native `codemaker` importer from
  `~/.codemaker` until a newer product or fixture proves transcript storage.

## inference.sh / belt (insufficient proof)

- npx skills evidence: `skills@1.5.14` registers the `inference-sh` target with
  `skillsDir = ".inferencesh/skills"`,
  `globalSkillsDir = "~/.inferencesh/skills"`, and install detection based only
  on `~/.inferencesh`.
- Package evidence: the current `infsh@0.0.2` npm package is a stub that tells
  users to install `@inferencesh/belt`; `@inferencesh/belt@1.14.10` is a
  wrapper that downloads/runs the `belt` binary.
- Public docs evidence: inference.sh CLI docs describe skills, knowledge,
  cloud tasks, chats, and agent runtime commands, but they do not document a
  stable local transcript file path or schema under `~/.inferencesh`.
- Decision: ctx should not add a native `inference-sh` importer from
  `~/.inferencesh`; future support needs an official export/API/plugin
  contract or a source-backed local store.

## Moxby (insufficient proof)

- npx skills evidence: `skills@1.5.14` registers the `moxby` target with
  `skillsDir = ".moxby/skills"`, `globalSkillsDir = "~/.moxby/skills"`, and
  install detection based only on `~/.moxby`.
- Public product evidence: Moxby describes a local desktop/browser workspace,
  coding-agent harnessing for Claude Code or Codex subscriptions, chat/session
  features, and line-level reasoning/history.
- Local storage lead: local inspection found SQLite chat database proof for
  Moxby, so the product is no longer just a web-account guess. The proof is not
  yet enough to implement because ctx still lacks the exact stable app-data
  path, a documented or fixture-backed schema, and a sanitized real fixture.
- Gap: no public source-backed app-data path, SQLite/IndexedDB schema, export
  contract, or sanitized real fixture was found for durable local transcripts.
- Decision: Moxby remains an unknown native-history target for ctx. If a real
  fixture proves the app-data SQLite storage, it may become a candidate desktop
  storage importer.

## MCPJam (webapp/object-store boundary)

- npx skills evidence: `skills@1.5.14` registers the `mcpjam` target with
  skill installation rooted at `~/.mcpjam`.
- Boundary evidence: current evidence points to a stateless CLI plus backend or
  object-store activity boundary, not to a durable local transcript file under
  the skill home.
- Decision: MCPJam stays `webapp-boundary` until it provides an exporter,
  history-source plugin, documented local transcript store, or managed import
  API.

## Ona (webapp/object-store boundary)

- npx skills evidence: `skills@1.5.14` registers the `ona` target with skill
  installation rooted at `~/.ona`.
- Boundary evidence: no stable local transcript path or schema is proven. The
  best current lead is a support bundle or API-backed managed import contract,
  not direct local file discovery.
- Decision: Ona stays `webapp-boundary` until an official support-bundle/API
  import path or sanitized local fixture proves a bounded history format.

## Replit (webapp/object-store boundary)

- npx skills evidence: `skills@1.5.14` registers `replit` through project
  markers such as `.replit`.
- Boundary evidence: the project marker identifies a Replit project, but it is
  not a local agent-history transcript contract. Agent activity remains tied to
  the project/cloud boundary.
- Decision: Replit stays `webapp-boundary`; future support should use an
  exporter, history-source plugin, or explicit cloud/project export contract.

## Zenflow (webapp/object-store boundary)

- npx skills evidence: `skills@1.5.14` maps Zenflow into the Zencoder skill
  home surface, but that home is not itself a proven Zenflow transcript store.
- Local storage lead: native local `db.sqlite` evidence makes Zenflow a
  plausible native-preview candidate.
- Repro attempt: Zenflow Desktop 2.3.1 Linux was run in an isolated
  `ZENFLOW_DATA_DIR`. It created `db.sqlite` with transcript-shaped tables
  (`tasks`, `chats`, `execution_processes`, `execution_process_logs`,
  `execution_process_normalized_logs`, `assistant_sessions`, `attachments`),
  and no-auth API probes could create task/chat/process rows plus synthetic
  `error_message` log patches. They did not produce user/assistant/tool log
  rows; custom no-auth executor runs left empty `raw_logs/*.jsonl` sidecars.
- Gap: ctx still needs a sanitized real task fixture, or equivalent
  source-backed row proof, showing user, assistant, tool-call, and tool-output
  records before adding a native reader.
- Decision: Zenflow remains `webapp-boundary` for the npx ledger until those
  proofs exist; users should prefer an exporter or underlying provider import
  where available.

## AstrBot (native-preview)

- npx skills evidence: `skills@1.5.14` registers `astrbot` under `~/.astrbot`,
  matching AstrBot's packaged desktop root.
- Upstream source evidence: `AstrBotDevs/AstrBot` v4.26.4
  (`25cbd41e08eb7e678b0ab3f5e6c972581bd0589f`) derives the root from
  `ASTRBOT_ROOT`, packaged desktop `~/.astrbot`, or cwd, and sets
  `DB_PATH = <root>/data/data_v4.db`.
- Schema evidence: upstream SQLModel tables include `conversations` with
  `content` JSON for OpenAI-formatted LLM messages and
  `platform_message_history` with `platform_id`, `user_id`, `sender_id`,
  `sender_name`, JSON `content`, and optional `llm_checkpoint_id`. ctx's
  sanitized fixture at `tests/fixtures/provider-history/astrbot/v1/data/data_v4.db`
  matches those tables and imports/searches idempotently.
- Write-path evidence: upstream WebChat/OpenAPI/live-chat services insert user
  and bot message records into `platform_message_history`, and LLM request
  handling writes the model-facing context into `conversations.content`.
- Blocker: upstream's own conversation model documentation still says WebChat
  history stores all messages, while other platform chats do not store
  non-LLM replies because those are considered stored on their platforms. I did
  not find a general IM-platform write path that records complete raw user and
  assistant transcripts for QQ/Telegram/WeCom/Lark/DingTalk/Slack/etc. into
  `data_v4.db`.
- Decision: AstrBot stays `native-preview`. The existing importer is useful for
  local LLM context plus available WebChat/OpenAPI/live-chat platform rows, but
  ctx should not include it in native-auto/import-all/search-refresh until
  upstream guarantees complete per-platform local transcript retention or a
  sanitized multi-platform runtime fixture proves the durable schema.

## Tabnine CLI

- npx skills evidence: `skills@1.5.14` registers the `tabnine-cli` target with
  `skillsDir = ".tabnine/agent/skills"`,
  `globalSkillsDir = "~/.tabnine/agent/skills"`, and install detection based
  only on `~/.tabnine`. This proves the Tabnine agent home used by skills and
  configuration.
- Official install evidence:
  `https://docs.tabnine.com/main/getting-started/tabnine-cli/getting-started/installation`
  installs Tabnine CLI from a Tabnine host, requires Tabnine Agents to be
  enabled for the team, uses `~/.local/bin/tabnine` as the Linux/macOS default
  binary path, and uninstalls by removing `~/.tabnine/agent`.
- Official settings and command evidence:
  `https://docs.tabnine.com/main/getting-started/tabnine-cli/features/settings`
  documents `~/.tabnine/agent/settings.json`, and
  `https://docs.tabnine.com/main/getting-started/tabnine-cli/features/commands`
  documents `/chat save`, `/chat resume`, `/chat list`, `/chat share`, and
  `/resume`.
- Official checkpoint evidence:
  `https://docs.tabnine.com/main/getting-started/tabnine-cli/features/checkpointing`
  says checkpoint metadata is stored under
  `~/.tabnine/agent/tmp/<project_hash>/checkpoints/` and compares `/chat save`
  conversation history to a location under `~/.tabnine/agent/tmp/...`.
- Package evidence: the public `tabnine@0.0.0-dev-202603280606` npm package is
  a small `stardrop` wrapper, but the official installer at
  `https://console.tabnine.com/update/cli/installer.mjs` installed Tabnine CLI
  `0.25.1` into a scratch `HOME` with bundles under
  `~/.tabnine/agent/.bundles/<version>`.
- Bundle evidence: Tabnine CLI `0.25.1` sets `GEMINI_DIR` to
  `.tabnine/agent`, records project slugs in `~/.tabnine/agent/projects.json`,
  and uses `~/.tabnine/agent/tmp/<project-id>` as the project temp root.
- Transcript evidence: `ChatRecordingService` writes JSONL under
  `tmp/<project-id>/chats/session-<timestamp>-<sessionid8>.jsonl`, with first
  record metadata fields such as `sessionId`, `projectHash`, `startTime`,
  `lastUpdated`, `kind`, and `directories`. Message records include `id`,
  `timestamp`, `type`, and `content`; Tabnine assistant messages use
  `type: "tabnine"`, while legacy Gemini-family records may use
  `type: "gemini"`.
- Subagent evidence: Tabnine writes subagent JSONL under
  `tmp/<project-id>/chats/<parent-session-id>/<agent>.jsonl` and marks those
  records with `kind: "subagent"`.
- Checkpoint evidence from the bundle: `/chat save <tag>` writes
  `checkpoint-<encodedTag>.json` in the project temp directory. ctx does not
  import checkpoint JSON as the primary transcript source; it imports the chat
  recording JSONL.
- Runtime probe: a safe no-auth scratch run of `tabnine -p ...` initialized
  `~/.tabnine/agent/tmp/<project-id>`, `history/<project-id>`, and
  `projects.json`, but timed out before authentication/model access produced a
  chat JSONL file. The fixture is therefore source-shaped from the official
  bundle rather than a live authenticated transcript.
- `ctx` imports this native shape as `tabnine_cli_chat_recording_jsonl` from
  bounded `~/.tabnine/agent/tmp/**/chats/**/*.jsonl` discovery or explicit
  Tabnine agent/history paths. It does not import skills, settings, credentials,
  checkpoint JSON, shared-chat exports, or arbitrary temp files.

## TinyCloud

- npx skills evidence: `skills@1.5.14` registers the `tinycloud` target with
  `skillsDir = ".tinycloud/skills"`,
  `globalSkillsDir = "~/.tinycloud/skills"`, and install detection based only
  on `~/.tinycloud`.
- Package evidence: `@cloudglue/tinycloud@0.3.8` gitHead
  `fb0b313286bc83d4c48f66831e8acb7a6b51847a` documents an isolated
  `--home`/`TINYCLOUD_HOME` state home whose default is `~/.tinycloud`, with
  config, sessions, cache, jobs, artifacts, and skills.
- Session-path evidence: TinyCloud writes project-scoped JSONL sessions under
  `$TINYCLOUD_HOME/projects/*/sessions/*.jsonl` or
  `~/.tinycloud/projects/*/sessions/*.jsonl`, with legacy support for
  `<home>/sessions/*.jsonl`.
- `ctx` imports this native shape as `tinycloud_session_jsonl_tree`, using
  `TINYCLOUD_HOME`, `~/.tinycloud`, or explicit home/session paths.
- Caveat: TinyCloud session support is package-backed by
  `@cloudglue/tinycloud@0.3.8` evidence and sanitized fixtures; future schema
  drift may need adapter updates. `~/.tinycloud/skills` remains skills evidence
  only and is not treated as transcript history.

## AdaL CLI

- Package evidence: `npm view @sylphai/adal-cli@1.4.1` reports repository
  `https://github.com/SylphAI-Inc/adal-cli.git`; the public repository is
  documentation-only and says the main AdaL source lives outside that public
  repo. The npm package is a proprietary wrapper plus platform package
  (`@sylphai/adal-cli-linux-x64@1.4.1` on this host).
- Public docs evidence: AdaL's manage-conversations docs say session history
  saves full conversation messages, tool calls, responses, working context, and
  metadata. The headless docs expose JSON/NDJSON response examples with
  `session_id` and document `--resume <id>` as resuming a saved conversation by
  session ID.
- Safe-run evidence: running `@sylphai/adal-cli@1.4.1` through its installed
  npm wrapper with disposable `HOME`, a public scratch working directory, and
  fixed `--session-id ctx-adal-fixed-session` failed before model execution
  because no credentials were available. The run created
  `~/.adal/sessions/conversation_ctx-adal-fixed-session.jsonl` as an empty
  file and wrote `~/.adal/sessions/ctx-adal-fixed-session_metadata.json`.
- Binary schema evidence: extracting the Linux x64 platform package's
  PyInstaller backend showed `adalflow.components.memory.session_persistence`.
  Its module docstring describes JSONL session persistence for real-time append
  and replay. Bytecode constants show the session file name pattern
  `conversation_<id>.jsonl` and event types `metadata`, `turn_created`,
  `message`, `compact`, `clear`, and `usage`.
- Binary field evidence: the same bytecode constants show record keys:
  `metadata` events include `type`, `conversation_id`, `user_id`,
  `created_at`, `platform`, `version`, and `project_path`; `turn_created`
  events include `type`, `turn_id`, `turn_index`, and `timestamp`; `message`
  events include `type`, `message_id`, `turn_id`, `role`, `content`,
  `metadata`, and `timestamp`; `compact` events include `compact_id`,
  `start_index`, `end_index`, `content`, `metadata`, and `timestamp`; `clear`
  events include `reason` and `timestamp`; `usage` events include `turn_id`,
  `step`, `model`, `input_tokens`, `output_tokens`,
  `cached_read_input_tokens`, `cache_creation_tokens`, `reasoning_tokens`,
  `source`, optional `subagent_type`, and `timestamp`.
- Binary discovery and sidecar evidence: `deep_research.config.config_initializer`
  resolves the sessions directory to `~/.adal/sessions/`, builds
  `conversation_<id>.jsonl`, and globs `conversation_*.jsonl`. Workspace
  endpoint bytecode constructs `<id>_metadata.json` sidecars, and
  `adalflow.components.memory.flexible_memory` restores `SessionMetadata`
  fields including `conversation_id`, `created_at`, `last_accessed`,
  `last_user_message`, `total_messages`, `total_turns`, `project_path`,
  `user_id`, `platform`, and `version`.
- Decision: ctx imports this shape as `adal_session_jsonl`. Default discovery
  checks `~/.adal/sessions` and reports AdaL available only when a non-empty
  `conversation_*.jsonl` exists, so the known empty no-auth bootstrap file is
  not imported. Explicit `--path` imports accept a sessions directory, a
  `conversation_<id>.jsonl` file, or an adjacent `<id>_metadata.json` sidecar.
  The checked-in fixture is synthetic from the package-backed writer schema and
  contains no private user data.
- Unclaimed gaps: no authenticated non-empty live AdaL fixture is checked in;
  project settings under `~/.adal/settings.json`, HTML exports, browser/cloud
  state, and any separate tool-call transcript format remain unparsed until
  independently proven.

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

## Trae

- Official storage evidence: Trae troubleshooting docs document local app data
  roots under `~/Library/Application Support/Trae/ModularData` on macOS and
  `%USERPROFILE%\AppData\Roaming\Trae\ModularData` on Windows, including an
  `ai-agent/snapshot` subdirectory for agent data artifacts.
- Schema evidence: `yuanjing001/trae-chats-exporter` reads Trae chat data from
  `~/Library/Application Support/Trae/User/workspaceStorage/<workspace>/state.vscdb`.
- The exporter opens SQLite table `ItemTable` and reads VS Code-style storage
  keys including `memento/icube-ai-agent-storage`,
  `chat.ChatSessionStore.index`, `ChatStore`,
  `memento/icube-ai-chat-storage-7467774676505887760`, and
  `memento/icube-ai-ng-chat-storage-7467774676505887760`.
- Trae CN evidence: official community reports point to
  `Trae CN/User/workspaceStorage/<workspace>/state.vscdb` plus
  `workspace.json`. `llg23456/ai-dialog-compressor` commit
  `e9dbf1f4b5cd0a033053e62ea9643f675d3a2ca7` reads the same VS Code
  workspaceStorage shape and the Trae-CN-specific
  `icube-ai-agent-storage-input-history` key, whose entries usually contain
  user-only `inputText` or `text` fields.
- `ctx` imports this shape as `trae_state_vscdb`. Default discovery is limited
  to Trae and Trae CN app-data `User/workspaceStorage` roots at
  `~/Library/Application Support/Trae/User/workspaceStorage`,
  `~/Library/Application Support/Trae CN/User/workspaceStorage`,
  `%APPDATA%/Trae/User/workspaceStorage`, and
  `%APPDATA%/Trae CN/User/workspaceStorage`; the bounded probe marks a root
  available only when a direct workspace `state.vscdb` has `ItemTable` and one
  of the known Trae chat/input-history keys. Users can still provide an
  explicit `state.vscdb`, workspace directory, or `User/workspaceStorage` root.
  `ctx import --provider trae-cn` is accepted as a CLI alias and stores
  imported rows under the canonical `trae` provider.
- Fixture provenance: `tests/fixtures/provider-history/trae` is a sanitized,
  source-backed synthetic `state.vscdb` fixture modeled on the exporter-visible
  `ItemTable` key contract. No local Trae binary or data root was available in
  this environment, so no real Trae or Trae CN run fixture was generated.
- Unclaimed paths: `globalStorage`, `ModularData`, arbitrary Trae application
  caches, unknown `ItemTable` keys, and full Trae CN assistant transcript
  recovery from input-history-only databases.

## Warp

- Official local path evidence:
  `https://docs.warp.dev/terminal/sessions/session-restoration/` documents that
  Warp saves session restoration data to a local SQLite database and lists
  `warp.sqlite` paths for macOS, Windows, and Linux. Linux is
  `${XDG_STATE_HOME:-$HOME/.local/state}/warp-terminal/warp.sqlite`; macOS is
  `~/Library/Group Containers/2BBY89MBSN.dev.warp/Library/Application Support/dev.warp.Warp-Stable/warp.sqlite`;
  Windows is `%LOCALAPPDATA%\warp\Warp\data\warp.sqlite`.
- Official local/cloud boundary evidence:
  `https://docs.warp.dev/agent-platform/local-agents/interacting-with-agents/`
  says the conversation panel shows active and past agent conversations and
  notes that when cloud sync is disabled, conversations are stored locally only
  and cannot be shared; cloud agent conversations are always stored in the
  cloud.
- Open-source client evidence: `https://github.com/warpdotdev/warp` is Warp's
  public client repository. At the inspected public checkout, `crates/persistence/src/schema.rs`
  defines `agent_conversations(id, conversation_id, conversation_data, last_modified_at)`,
  `agent_tasks(id, conversation_id, task_id, task, last_modified_at)`, and
  `ai_queries(...)`.
- Write/read evidence: `app/src/persistence/agent.rs` serializes
  `AgentConversationData` JSON into `agent_conversations.conversation_data`,
  writes each `warp_multi_agent_api::Task` with `task.encode_to_vec()` into
  `agent_tasks.task`, and reads those rows back with `api::Task::decode`.
- Message-shape evidence:
  `https://github.com/warpdotdev/warp-proto-apis` publishes
  `apis/multi_agent/v1/task.proto`; `Task` has repeated `Message messages`, and
  `Message` includes `UserQuery.query`, `AgentOutput.text`, tool-call/result,
  reasoning, summary, and inter-agent message variants.
- `ctx` imports this shape as native `warp_sqlite` history from the documented
  Linux, macOS, and Windows local restoration paths. `ctx import --all` and
  default search refresh include discovered `warp.sqlite` sources; explicit
  `ctx import --provider warp --path <warp.sqlite>` remains supported for any
  user-supplied local Warp SQLite database.
- Live SQLite handling: ctx opens the provider database with SQLite read-only
  flags, sets `query_only`, and uses normal SQLite locking/WAL visibility rather
  than immutable file reads or ad hoc copying. The capture tests cover committed
  WAL content while a writer connection remains open.
- Privacy boundary: ctx decodes the public protobuf subset and does not copy
  `server_conversation_token` or `forked_from_server_conversation_token` values
  into ctx metadata.
- Fixture evidence: `tests/fixtures/provider-history/warp/v1/warp.sqlite` is a
  sanitized schema-backed fixture generated for ctx tests, not copied from a
  user Warp profile.
- Caveats and unclaimed formats: ctx does not parse cloud-synced conversations,
  Oz/cloud agent conversations, browser IndexedDB, Markdown exports, Warp Drive
  or team data, shell command history outside decoded `agent_tasks.task`, or
  any private remote data.

## IBM Bob

- Official IBM docs prove Bob skill/config paths such as project `.bob/skills`
  and global `~/.bob/skills`, but those docs do not prove transcript storage.
- IBM community history-export evidence names the Bob IDE task history path as
  `C:\Users\<USER>\AppData\Roaming\Bob-IDE\User\globalStorage\ibm.bob-code\tasks`.
- CodeBurn's IBM Bob provider independently reads IBM Bob IDE task history from
  `User/globalStorage/ibm.bob-code/tasks` below app-data folders named
  `IBM Bob` and `Bob-IDE`, and reuses its Cline-family task JSON parser.
- `ctx` imports this shape as `bob_task_directory_json`, using the existing
  task JSON adapter for `ui_messages.json`, optional
  `api_conversation_history.json`, and `task_metadata.json`.
- Unclaimed paths: Bob Shell `~/.bob` skill/state data, arbitrary VS
  Code/Electron state databases, and cloud/account-backed Bob data are not
  parsed by this adapter.

## Void

- Source evidence: Void stores chat threads through VS Code/Electron application
  storage.
- Current key: `void.chatThreadStorageII`; older keys:
  `void.chatThreadStorage` and `void.chatThreadStorageI`.
- Values are JSON-serialized chat thread/message structures in application
  storage.
- Gap: native import needs a small, proven reader for the VS Code/Electron
  storage location and key formats, plus fixtures covering all three keys.
