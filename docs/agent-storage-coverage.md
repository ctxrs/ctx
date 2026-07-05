# npx skills Agent Storage Coverage

This ledger compares every `AgentType` in `skills@1.5.14` commit
`2adcfe5a4cce0ce5f4d5547a997b2a161ec5d127` against ctx native history
providers on this integration branch. Upstream evidence comes from `src/types.ts` and
`src/agents.ts`; ctx evidence on this integration branch comes from `docs/provider-support-matrix.json`,
`crates/ctx-history-capture/src/provider_sources.rs`, and the native provider
arguments in `crates/ctx-cli/src/main.rs`.

Status meanings:

- `native-auto`: ctx has an auto-importable native provider path for this npx id.
- `native-explicit`: ctx has a supported native importer for an official
  explicit export or user-supplied path, but no safe default discovery is
  claimed.
- `native-preview`: ctx has an importable native path, but it is preview-only
  and excluded from automatic refresh.
- `candidate-family`: no ctx native importer exists, but the id falls into a
  reusable storage family worth validating before implementation.
- `webapp-boundary`: npx can install skills, but native history appears to live
  behind a desktop app, hosted service, account store, or object-store boundary.
- `unknown`: npx only proves a skill install or detection path, or source
  research found product/session hints without a stable local transcript
  path/schema.
- `install-target`: npx target is an aggregate or project skill target, not a
  proven history-producing agent.

Result on this integration branch: 60 `native-auto`, 2 `native-explicit`, 1
`native-preview`, 0 `candidate-family`, 4 `webapp-boundary`, 3 `unknown`, and
2 `install-target` rows.

## Shared Families

- `opencode sqlite family`: native `opencode`, `kilo`, and CodeArts
  Agent coverage share the reusable SQLite baseline for OpenCode-style
  message/session tables.
- `Cline/Roo/Bob task JSON`: native `cline`, `roo`, and IBM Bob IDE coverage
  share one task JSON importer for file-backed task directories.
- `JSONL CLI event logs`: already covers Codex, Claude Code, OpenClaw,
  Antigravity CLI, Gemini CLI, Tabnine CLI, Pi, Factory Droid, Copilot CLI-shaped logs, and
  Autohand Code, iFlow CLI, Mistral Vibe, Mux, Reasonix, and Command Code
  sessions, plus Qoder transcript JSONL, Windsurf Cascade hook transcript JSONL,
  TinyCloud session JSONL, and OpenLoaf `messages.jsonl` chat-history trees.
- `CLI session JSON`: covers Continue CLI `sessions/*.json` files with
  `sessions.json` metadata, Auggie `~/.augment/sessions/*.json`, Amp explicit
  `amp threads export` JSON, plus Rovo Dev session directories and Cortex Code
  conversation snapshots/history sidecars.
- `project task JSON`: covers Aider Desk project-local task directories such as
  `.aider-desk/tasks/<taskId>/context.json`; related task-directory tools can
  reuse this scanner once storage proof and fixtures exist.
- `filesystem event JSON`: covers OpenHands event JSON under
  `<persistence>/<user_id>/v1_conversations`.
- `generic sqlite messages`: already covers Crush, Goose, Hermes, Kiro CLI,
  Dexto CLI database roots, Terramind/Nucleus `agents.db`, Firebender project
  chat history DBs, the AstrBot preview importer, and ctx-native Shelley.
- `Forge conversation SQLite`: covers ForgeCode's `.forge.db` conversation
  snapshots with JSON context/metrics DTOs.
- `LangGraph checkpoint SQLite`: candidate family for LangGraph-style
  checkpoint databases plus JSONL history sidecars.
- `LiveStore SQLite state DB`: covers native-auto Pochi LiveStore `state*.db`
  imports from bounded discovered `~/.pochi/storage` roots or explicit paths
  when filesystem sync has produced a state database.
- `VS Code/Electron storage`: Cursor is covered through a known transcript tree,
  CodeBuddy is covered through its file-backed history JSON, Zed is covered
  through its agent `threads.db`, Trae has preview explicit `state.vscdb`
  imports from `User/workspaceStorage`, Zencoder imports
  `ZencoderAI.zencoder/zencoder-chat` session trees, and Code Studio imports
  app-data session-store SQLite DBs.
- `webapp/object-store boundary`: prefer explicit exporters or
  `ctx-history-jsonl-v1` history-source plugins over speculative native readers.

## Coverage Ledger

| npx skills agent id | ctx storage ingestion status | schema family | evidence source | blocked reason / gap |
| --- | --- | --- | --- | --- |
| `aider-desk` | `native-auto` | `project task JSON` | ctx `aider_desk_task_context_json`; npx `~/.aider-desk`; source proof shows project `.aider-desk/tasks/<taskId>/context.json` task context files | - |
| `amp` | `native-explicit` | `CLI session JSON` | ctx `amp_threads_export_json`; npx `~/.config/amp`; `@ampcode/cli@0.0.1783181941-g187572` exposes `amp threads export`, `amp threads markdown`, and `amp threads raw`; export handler serializes `threadRemote.getThread(...)` | Official explicit import from user-supplied `amp threads export` JSON; no default `~/.config/amp` scan and no `$XDG_CACHE_HOME/amp/logs/cli.log` crawl. |
| `antigravity` | `native-auto` | `JSONL CLI event logs` | ctx `antigravity_cli_transcript_jsonl_tree`; npx `~/.gemini/antigravity`; official IDE transcripts live under `~/.gemini/antigravity-ide/brain` | ctx imports official IDE brain transcripts, not the npx skill/config path `~/.gemini/antigravity`. |
| `antigravity-cli` | `native-auto` | `JSONL CLI event logs` | ctx `antigravity_cli_transcript_jsonl_tree`; npx `~/.gemini/antigravity-cli` | - |
| `astrbot` | `native-preview` | `generic sqlite messages` | ctx `astrbot_data_v4_sqlite`; npx `~/.astrbot`; upstream AstrBot v4.26.4 source confirms `data/data_v4.db` under `ASTRBOT_ROOT`/packaged `~/.astrbot`, `conversations`, and `platform_message_history` | Preview explicit import only. `conversations.content` is durable LLM context and `platform_message_history` is proven for WebChat/OpenAPI/live chat, but upstream model docs still say non-WebChat platform conversations do not persist non-LLM replies because those are stored on their platforms. Full per-platform raw IM transcript coverage remains unproven. |
| `autohand-code` | `native-auto` | `JSONL CLI event logs` | ctx `autohand_code_sessions_jsonl`; npx `AUTOHAND_HOME` or `~/.autohand` | - |
| `augment` | `native-auto` | `CLI session JSON` | ctx `auggie_session_json`; npx `~/.augment`; `@augmentcode/auggie@0.32.0` stores sessions under `~/.augment/sessions/<session_id>.json` | Imports package-backed Auggie CLI `chatHistory` request/response text only; richer IDE/app storage remains unclaimed. |
| `bob` | `native-auto` | `Cline/Roo/Bob task JSON` | ctx `bob_task_directory_json`; IBM community and CodeBurn prove IBM Bob IDE app-data task storage under `User/globalStorage/ibm.bob-code/tasks`; npx `~/.bob` remains Bob Shell skill/state evidence only | Imports IBM Bob IDE task JSON from app-data `IBM Bob`/`Bob-IDE` folders only; Bob Shell `~/.bob` is not crawled. |
| `claude-code` | `native-auto` | `JSONL CLI event logs` | ctx `claude_projects_jsonl_tree`; npx `~/.claude` | - |
| `openclaw` | `native-auto` | `JSONL CLI event logs` | ctx `openclaw_session_jsonl_tree`; npx `~/.openclaw` or legacy homes | Provider matrix still notes GA schema-stability validation. |
| `cline` | `native-auto` | `Cline/Roo/Bob task JSON` | ctx `cline_task_directory_json`; npx `~/.cline` | - |
| `codearts-agent` | `native-auto` | `opencode sqlite family` | ctx `codearts_agent_kernel_sqlite`; npx `~/.codeartsdoer`; VSIX `HuaweiCloud.vscode-codebot` v26.6.0 SHA256 `394f54ba999bdab8095f9bdd3ccd28ce5e0df3ab804911703efdbe1a329a2d34` proves an OpenCode-derived kernel SQLite family under `~/.codeartsdoer/vscode-data/opencode.db`, `~/.codeartsdoer/codearts-data/opencode.db`, and XDG data `codeartsdoer/opencode.db` paths | Imports only the kernel-managed OpenCode-derived SQLite DB from proven `opencode.db` paths or an explicit DB path; older/private VS Code cache JSON remains unclaimed. |
| `codebuddy` | `native-auto` | `VS Code/Electron storage` | ctx `codebuddy_history_json`; npx project or home `.codebuddy` | Schema proof from WayLog `shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09`; continue validating schema drift. |
| `codemaker` | `unknown` | `unknown native history` | npx `~/.codemaker`; no ctx provider; public CodeMaker CLI source writes `~/.codemaker/config` for API credentials | No local transcript/session store is proven; do not import config or skills directories as history. |
| `codestudio` | `native-auto` | `VS Code/Electron storage` | ctx `codestudio_session_store_sqlite`; npx `~/.codestudio`; Syncfusion Code Studio v2.0.4 artifact SHA256 `603991729cbb154e1bbc1a12d292b389895e1c2fb6f07b25bc1cda4fe14fa13b` and docs repo `syncfusion-content/syncfusion-code-studio-docs@de44c7b35fc30ece937ff85d113b5b360a3e3955` prove the local session-store family | Imports only proven `session-store.db` SQLite files under Code Studio app-data `User/globalStorage` or an explicit DB path; `.codestudio` skills/agents/settings and debug logs remain unclaimed. |
| `codex` | `native-auto` | `JSONL CLI event logs` | ctx `codex_session_jsonl_tree` and `codex_history_jsonl`; npx `CODEX_HOME` | - |
| `command-code` | `native-auto` | `JSONL CLI event logs` | ctx `command_code_session_jsonl_tree`; npx `~/.commandcode`; default discovery reads `~/.commandcode/projects` | - |
| `continue` | `native-auto` | `CLI session JSON` | ctx `continue_cli_sessions_json`; npx project or home `.continue` | - |
| `cortex` | `native-auto` | `CLI session JSON` | ctx `cortex_code_conversations_json`; npx `~/.snowflake/cortex`; default discovery reads `~/.snowflake/cortex/conversations` | - |
| `crush` | `native-auto` | `generic sqlite messages` | ctx `crush_sqlite`; npx `~/.config/crush` | - |
| `cursor` | `native-auto` | `VS Code/Electron storage` | ctx `cursor_agent_transcript_jsonl_tree`; npx `~/.cursor` | - |
| `deepagents` | `native-auto` | `LangGraph checkpoint SQLite` | ctx `deepagents_sessions_sqlite`; npx `~/.deepagents`; official local state evidence points to `~/.deepagents/.state/sessions.db` and `history.jsonl` | Imports decoded root `writes.messages` chat messages only; `history.jsonl` and arbitrary checkpoint state blobs are not indexed. |
| `devin` | `native-explicit` | `explicit ATIF export JSON` | ctx `devin_atif_json` via explicit `--path`; npx `~/.config/devin` remains unclaimed | Official explicit import from user-supplied Devin CLI `devin --export [PATH]` ATIF files/directories. No Devin cloud scraping, login, account paths, default discovery, or `~/.config/devin` local conversation DB is claimed; the fixture remains synthetic from the public ATIF/export contract. |
| `dexto` | `native-auto` | `generic sqlite messages` | ctx `dexto_sqlite`; npx `~/.dexto`; `dexto@1.9.1`, `@dexto/storage@1.9.1`, and `@dexto/agent-management@1.9.1` prove CLI-enriched SQLite paths under Dexto `database` roots | Auto import scans bounded `.dexto/database/*.db` roots with Dexto `kv_store`/`list_store` schema; custom SQLite paths outside default roots still require explicit `--path`. |
| `droid` | `native-auto` | `JSONL CLI event logs` | ctx `factory_ai_droid_sessions_jsonl`; npx `~/.factory` | - |
| `eve` | `native-auto` | `Workflow local-world streams` | ctx `eve_workflow_data_streams`; npx project `agent`; `eve@0.19.0` local development uses Workflow local-world `.workflow-data` durable stream storage | Imports default Eve message stream chunks from `WORKFLOW_LOCAL_DATA_DIR`, current project `.workflow-data`, or explicit paths; `.eve` build/runtime artifacts are not treated as history. |
| `firebender` | `native-auto` | `generic sqlite messages` | ctx `firebender_chat_history_sqlite`; npx `~/.firebender`; public Firebender 1.0.10 JetBrains plugin stores project chat history in `.idea/firebender/chat_history.db` | Proven transcript storage is project-local `.idea/firebender/chat_history.db`; no global `~/.firebender` chat history file is claimed. |
| `forgecode` | `native-auto` | `Forge conversation SQLite` | ctx `forgecode_sqlite`; npx `FORGE_CONFIG`, legacy `~/forge`, or `~/.forge` | - |
| `gemini-cli` | `native-auto` | `JSONL CLI event logs` | ctx `gemini_cli_chat_recording_jsonl`; npx `~/.gemini` | - |
| `github-copilot` | `native-auto` | `JSONL CLI event logs` | ctx `copilot_cli_session_events_jsonl`; npx `~/.copilot` | Coverage is for Copilot CLI session-state logs, not editor or web history. |
| `goose` | `native-auto` | `generic sqlite messages` | ctx `goose_sessions_sqlite`; npx `~/.config/goose` | - |
| `hermes-agent` | `native-auto` | `generic sqlite messages` | ctx `hermes_state_sqlite`; npx `HERMES_HOME` | - |
| `inference-sh` | `unknown` | `unknown native history` | npx `~/.inferencesh`; no ctx provider; current `infsh` npm package is a stub for `@inferencesh/belt`, and `belt` docs/package evidence covers skills, cloud/runtime commands, and binary install | Need an official local transcript export/API/plugin contract before claiming import support. |
| `iflow-cli` | `native-auto` | `JSONL CLI event logs` | ctx `iflow_cli_session_jsonl_tree`; npx `IFLOW_HOME` or `~/.iflow` | - |
| `jazz` | `native-auto` | `per-agent history JSON` | ctx `jazz_history_json`; npx `JAZZ_HOME` or `~/.jazz/history`; package `jazz-ai@0.12.5` writes `history/<agentId>.json` | Imports the retained conversations present in each per-agent history file; Jazz currently caps the stored conversation list in the package writer. |
| `junie` | `native-auto` | `Junie event-sourced UI stream` | ctx `junie_session_events_jsonl_tree`; npx `~/.junie`; source-backed proof from Claudescope's Junie connector and JetBrains release event classes | Imports `~/.junie/sessions/index.jsonl` plus `session-*/events.jsonl`, `JUNIE_SESSIONS_DIR`, `JUNIE_HOME/sessions`, or explicit session paths. Schema is not first-party documented, so future Junie event drift may need adapter updates. |
| `kilo` | `native-auto` | `opencode sqlite family` | ctx `kilo_sqlite`; npx `~/.kilocode` | - |
| `kimi-code-cli` | `native-auto` | `JSONL CLI event logs` | ctx `kimi_code_cli_wire_jsonl_tree`; npx `~/.kimi-code` or `~/.kimi` | - |
| `kiro-cli` | `native-auto` | `generic sqlite messages` | ctx `kiro_cli_sqlite`; npx `~/.kiro` | SQLite import covers the proven `conversations_v2`/`conversations` DB at the Kiro CLI data dir; newer `~/.kiro/sessions/cli` event logs are not imported yet. |
| `kode` | `native-auto` | `JSONL CLI event logs` | ctx `kode_session_jsonl_tree`; npx `~/.kode`; `@shareai-lab/kode` stores project JSONL sessions under `KODE_CONFIG_DIR`, `CLAUDE_CONFIG_DIR`, or `~/.kode` | - |
| `lingma` | `native-auto` | `VS Code/Electron storage` | ctx `lingma_sqlite`; npx `~/.lingma` | Schema proof from WayLog plus official Qoder CN VSIX/package path evidence; imports `chat_prompt` plus assistant `summary`/`error_result`, which may be partial. `qoder-cn` aliases to this importer; separate Qoder IDE homes remain unclaimed. |
| `loaf` | `native-auto` | `OpenLoaf chat JSONL` | ctx `openloaf_chat_jsonl_tree`; npx `~/.loaf` detects `loaf`, but the source-backed importer targets OpenLoaf paths `~/.openloaf/chat-history`, `~/OpenLoafData/projects`, and explicit project `.openloaf/chat-history` roots | ctx aliases `loaf`/`openloaf` to canonical provider `openloaf`; `~/.loaf` remains detection-only and is not crawled by the native importer. |
| `mcpjam` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.mcpjam`; no ctx provider; current evidence points to a stateless CLI/backend/object-store activity model rather than a durable local transcript file | UI or account-backed activity should use exporter or plugin until local storage is proven. |
| `mistral-vibe` | `native-auto` | `JSONL CLI event logs` | ctx `mistral_vibe_session_jsonl_tree`; npx `VIBE_HOME` or `~/.vibe` | - |
| `moxby` | `unknown` | `unknown native history` | npx `~/.moxby`; no ctx provider; public product docs imply local desktop/browser data and chat/session features, and local SQLite chat DB proof suggests there may be native history | Need the exact app-data path plus a sanitized schema/fixture before importing Chromium/desktop internals. |
| `mux` | `native-auto` | `JSONL CLI event logs` | ctx `mux_session_jsonl_tree`; npx `MUX_ROOT` or `~/.mux` | - |
| `neovate` | `native-auto` | `JSONL CLI event logs` | ctx `neovate_session_jsonl_tree`; npx `~/.neovate`; `@neovate/code` stores project session JSONL under `~/.neovate/projects` | - |
| `opencode` | `native-auto` | `opencode sqlite family` | ctx `opencode_sqlite`; npx `~/.config/opencode` | - |
| `openhands` | `native-auto` | `filesystem event JSON` | ctx `openhands_file_events`; npx `~/.openhands` | - |
| `ona` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.ona`; no ctx provider; current lead is a support-bundle or managed API import rather than direct local transcript discovery | No proven stable local transcript boundary; prefer exporter, plugin, or managed import contract. |
| `pi` | `native-auto` | `JSONL CLI event logs` | ctx `pi_session_jsonl`; npx `~/.pi/agent` | - |
| `qoder` | `native-auto` | `JSONL CLI event logs` | ctx `qoder_transcript_jsonl_tree`; npx `~/.qoder`; official Qoder Hooks docs define transcript JSONL at `~/.qoder/projects/<project>/transcript/<session-id>.jsonl` | Imports documented transcript records only; encrypted Qoder app logs and VS Code/Electron state databases remain unclaimed. |
| `qoder-cn` | `native-auto` | `VS Code/Electron storage` | ctx `lingma_sqlite` via `qoder-cn` alias; npx `~/.qoder-cn`; official Qoder CN VSIX remains `Alibaba-Cloud.tongyi-lingma` and uses `~/.lingma/vscode/sharedClientCache/cache/db/local.db` | Imports the Lingma/Qoder CN VS Code extension database; `~/.qoder-cn` and `.qodercn` homes remain unclaimed. |
| `qwen-code` | `native-auto` | `JSONL CLI event logs` | ctx `qwen_code_chat_jsonl_tree`; npx `~/.qwen` | - |
| `replit` | `webapp-boundary` | `webapp/object-store boundary` | npx project `.replit`; no ctx provider; evidence remains at the project/cloud boundary | Project marker is not a local agent history contract. |
| `reasonix` | `native-auto` | `JSONL CLI event logs` | ctx `reasonix_session_jsonl_tree`; npx `~/.reasonix/sessions`; package `reasonix@0.53.2` | - |
| `roo` | `native-auto` | `Cline/Roo/Bob task JSON` | ctx `roo_task_directory_json`; npx `~/.roo` | - |
| `rovodev` | `native-auto` | `CLI session JSON` | ctx `rovodev_session_json_tree`; npx `~/.rovodev`; default discovery reads `~/.rovodev/sessions` | - |
| `tabnine-cli` | `native-auto` | `JSONL CLI event logs` | ctx `tabnine_cli_chat_recording_jsonl`; npx `~/.tabnine`; official installer bundle Tabnine CLI 0.25.1 writes `~/.tabnine/agent/tmp/<project-id>/chats/session-*.jsonl` and nested subagent chat JSONL under `chats/<parent-session-id>/*.jsonl` | Imports chat recording JSONL only; skills, settings, credentials, checkpoint JSON, shared-chat exports, and arbitrary temp files remain unclaimed. |
| `terramind` | `native-auto` | `generic sqlite messages` | ctx `terramind_agents_sqlite`; npx package `terramind@0.2.91` resolves Nucleus app data to `$XDG_CONFIG_HOME/Nucleus/data/agents.db`, `~/.config/Nucleus/data/agents.db`, macOS `~/Library/Application Support/Nucleus/data/agents.db`, or Windows `%APPDATA%/Nucleus/data/agents.db` | Fixture is source-backed from the published package schema because a no-auth `npx terramind@0.2.91 list --chats` probe did not complete. |
| `tinycloud` | `native-auto` | `JSONL CLI event logs` | ctx `tinycloud_session_jsonl_tree`; npx `~/.tinycloud`; `@cloudglue/tinycloud@0.3.8` gitHead `fb0b313286bc83d4c48f66831e8acb7a6b51847a` writes `$TINYCLOUD_HOME`/`~/.tinycloud/projects/*/sessions/*.jsonl` and legacy `sessions/*.jsonl` | - |
| `trae` | `native-auto` | `VS Code/Electron storage` | ctx `trae_state_vscdb`; npx `~/.trae`; official Trae docs show `ModularData` roots; `yuanjing001/trae-chats-exporter` reads Trae `User/workspaceStorage/<workspace>/state.vscdb` `ItemTable` keys including `memento/icube-ai-agent-storage`, `chat.ChatSessionStore.index`, and `ChatStore`; default discovery probes `~/Library/Application Support/Trae/User/workspaceStorage` and `%APPDATA%/Trae/User/workspaceStorage` for known `ItemTable` keys | Imports only proven `User/workspaceStorage` `state.vscdb` databases; source-backed synthetic fixture, no bundled real Trae run fixture; `globalStorage`, `ModularData`, arbitrary caches, and unknown keys remain unclaimed. |
| `trae-cn` | `native-auto` | `VS Code/Electron storage` | ctx `trae_state_vscdb` via `trae-cn` alias; npx `~/.trae-cn`; official forum evidence points to `Trae CN/User/workspaceStorage/<workspace>/state.vscdb` plus `workspace.json`; `llg23456/ai-dialog-compressor@e9dbf1f4b5cd0a033053e62ea9643f675d3a2ca7` reads `icube-ai-agent-storage-input-history`; default discovery probes `~/Library/Application Support/Trae CN/User/workspaceStorage` and `%APPDATA%/Trae CN/User/workspaceStorage` for known `ItemTable` keys | Imports CN input-history rows as user prompts when assistant replies are absent; full assistant transcript recovery, `globalStorage`, `ModularData`, and non-workspaceStorage Trae caches remain unclaimed. |
| `warp` | `native-auto` | `Warp restoration SQLite` | ctx `warp_sqlite`; npx `~/.warp`; official docs document platform `warp.sqlite` paths, and public Warp source/proto define `agent_conversations`, `agent_tasks.task`, and `Task.messages` | Imports documented local restoration `warp.sqlite` paths through native discovery/search refresh/import-all, and explicit `ctx import --provider warp --path <warp.sqlite>` remains supported. Cloud sync endpoints, Oz/cloud conversations, browser IndexedDB, Markdown exports, command history outside `agent_tasks`, and Warp Drive/team data are not parsed. |
| `windsurf` | `native-auto` | `JSONL CLI event logs` | ctx `windsurf_cascade_hook_transcript_jsonl_tree`; npx `~/.codeium/windsurf` detection evidence only; official hook writes `~/.windsurf/transcripts/{trajectory_id}.jsonl` | - |
| `zed` | `native-auto` | `VS Code/Electron storage` | ctx `zed_threads_sqlite`; npx `$XDG_DATA_HOME/zed` or `~/.local/share/zed` | Per-message timestamps are unavailable; ctx uses thread `updated_at`. |
| `zencoder` | `native-auto` | `VS Code/Electron storage` | ctx `zencoder_chat_sessions_json_tree`; npx `~/.zencoder`; `jverre/opik-chat-history@5e7380933564d4fe1084d0e6f48f0e49e43e45ea` reads a local `zencoder-chat` folder with `sessions.json` plus `sessions/<id>.json`, and Open VSX `ZencoderAI.zencoder` v3.63.9002 constants confirm the extension storage anchors | Imports only `User/globalStorage/ZencoderAI.zencoder/zencoder-chat` `sessions.json` and `sessions/*.json` from proven VS Code-family app-data roots or explicit paths; `.zencoder` skill/config homes and unrelated extension caches are not imported. |
| `zenflow` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.zencoder`; no ctx provider; Zenflow Desktop 2.3.1 creates local `db.sqlite` tables for tasks, chats, execution processes/logs, assistant sessions, and attachments | Blocked on a sanitized real fixture or source-backed row proof with user/assistant/tool-call/tool-output records; isolated no-auth probes only produced task/chat/process rows, synthetic error-message patches, and empty raw-log sidecars. Prefer exporter or underlying provider imports meanwhile. |
| `pochi` | `native-auto` | `LiveStore SQLite state DB` | ctx `pochi_livestore_state_sqlite`; npx `~/.pochi`; Pochi CLI writes `~/.pochi/storage/<storeId>/state<schemaHash>@6.db` only when `POCHI_LIVEKIT_SYNC_ON` is enabled | Auto import is limited to bounded `~/.pochi/storage/**/state*.db` discovery with a `tasks`/`messages` schema probe plus explicit state DB file/directory paths; no `config.jsonc` parsing or VS Code OPFS import. |
| `promptscript` | `install-target` | `agent skills aggregate` | npx project `.promptscript` or `promptscript.yaml`; no ctx provider | Project skill target only; use custom history JSONL if it emits runs. |
| `adal` | `native-auto` | `JSONL CLI event logs` | ctx `adal_session_jsonl`; npx `~/.adal`; package-backed evidence points to `~/.adal/sessions/conversation_<id>.jsonl` plus `<id>_metadata.json` | Fixture is synthetic from package writer bytecode and sidecar proof; the no-auth safe run still produced only an empty JSONL, so authenticated live fixtures, settings discovery, and exports remain unclaimed. |
| `universal` | `install-target` | `agent skills aggregate` | npx virtual `.agents/skills` target; no ctx provider | Aggregate skill install target, not a history-producing native provider. |

## ctx Native Providers Outside This npx Target Set

`nanoclaw` and `shelley` are native ctx providers on `origin/main`, but they do
not have matching `skills@1.5.14` `AgentType` ids. `nanoclaw` is preview-only;
`shelley` is native auto-importable with `shelley_sqlite`.
