# Provider Support

Provider support is intentionally conservative. A provider is documented as
locally importable only when the public CLI can read existing local history for
that provider.

## Status Meanings

| Status | Meaning |
| --- | --- |
| `local_import` | The CLI can import an existing local history source for this provider. |
| `local_import_when_supported` | The CLI has an importer for a specific local format, but support depends on that file existing and matching the documented format. |
| `fixture_only` | The repository has sanitized fixture coverage, but the public CLI does not discover or import native local history for that provider. |
| `detected_unsupported` | The CLI can detect something about the provider but intentionally does not import it. |
| `blocked` | No shipped discovery or import path exists. |

## Current Matrix

Machine-readable provider metadata lives in
[provider-support-matrix.json](provider-support-matrix.json). The public truth
is:

| Provider | Status | Public import path | Public smoke |
| --- | --- | --- | --- |
| Codex | `local_import` | `~/.codex/sessions`, `~/.codex/history.jsonl`, or an explicit Codex path. | Static local-history fixture smoke. |
| Pi | `local_import_when_supported` | `~/.pi/agent/sessions`, `~/.omp/agent/sessions` (Oh My Pi fork), or an explicit Pi session JSONL path. | Static local-history fixture smoke. |
| Claude | `local_import_when_supported` | `~/.claude/projects` or an explicit Claude projects JSONL tree. | Static local-history fixture smoke. |
| OpenCode | `local_import_when_supported` | `~/.local/share/opencode/opencode.db` or an explicit OpenCode SQLite DB. | Static local-history fixture smoke. |
| Kilo Code | `local_import_when_supported` | `KILO_DB`, `$XDG_DATA_HOME/kilo/kilo.db`, `~/.local/share/kilo/kilo.db`, channel `kilo-*.db`, or an explicit Kilo SQLite DB. | Static local-history fixture smoke. |
| Kiro CLI | `local_import_when_supported` | `$XDG_DATA_HOME/kiro-cli/data.sqlite3`, `~/.local/share/kiro-cli/data.sqlite3`, macOS `~/Library/Application Support/kiro-cli/data.sqlite3`, or an explicit Kiro CLI SQLite DB. | Static local-history fixture smoke; imports the proven `conversations_v2`/`conversations` SQLite DB, not the newer `~/.kiro/sessions/cli` event-log path. |
| Crush | `local_import_when_supported` | `CRUSH_GLOBAL_DATA/crush.db`, `$XDG_DATA_HOME/crush/crush.db`, `~/.local/share/crush/crush.db`, configured `data_directory`, project `.crush/crush.db`, or an explicit Crush SQLite DB. | Static local-history fixture smoke. |
| Goose | `local_import_when_supported` | `GOOSE_PATH_ROOT/data/sessions/sessions.db`, `$XDG_DATA_HOME/goose/sessions/sessions.db`, `$XDG_DATA_HOME/Block/goose/sessions/sessions.db`, defaults under `~/.local/share`, or an explicit Goose sessions SQLite DB. | Static local-history fixture smoke. |
| Dexto | `local_import_when_supported` | Dexto CLI SQLite DBs under `~/.dexto/database/*.db`, current project `.dexto/database/*.db`, or an explicit Dexto SQLite DB path. | Static local-history fixture smoke; package proof from `dexto@1.9.1`, `@dexto/storage@1.9.1`, and `@dexto/agent-management@1.9.1`; discovery is schema-gated to `kv_store`/`list_store`. |
| Lingma | `local_import_when_supported` | `~/.lingma/vscode/sharedClientCache/cache/db/local.db`, `~/.lingma/vscode-insiders/sharedClientCache/cache/db/local.db`, or an explicit Lingma `local.db`. | Static local-history fixture smoke; schema proof comes from WayLog and official Qoder CN VSIX/package path evidence. Assistant content is summary/error_result only and may be partial. `qoder-cn` is accepted as an alias for the same Lingma `local.db`; separate `.qodercn`/`.qoder-cn` paths remain unclaimed. |
| Qoder | `local_import_when_supported` | Official transcript JSONL files under `~/.qoder/projects/<project>/transcript/<session-id>.jsonl`, `~/.qoder/projects`, or an explicit transcript file/directory. | Static local-history fixture smoke; schema proof comes from Qoder Hooks docs. Imports documented `session_meta`, `user`, `assistant`, `progress`, `tool_use`, and `tool_result` records. Encrypted Qoder app logs and VS Code/Electron state databases are not parsed. |
| Pochi | `local_import_when_supported` | Synced LiveStore state DBs under `~/.pochi/storage`, an explicit `state*.db` SQLite file, or a directory containing those files. | Static local-history fixture smoke; default discovery is limited to `~/.pochi/storage/**/state*.db` with the expected `tasks`/`messages` schema when filesystem sync has produced state DBs. Discovered storage roots participate in `ctx import --all` and default search refresh. No `config.jsonc` parsing or VS Code OPFS import is claimed. |
| Warp | `local_import_when_supported` | Documented Warp Terminal restoration `warp.sqlite` paths on Linux, macOS, Windows, or an explicit `warp.sqlite` path. | Static schema-backed SQLite fixture smoke plus live WAL read coverage. Discovered local restoration DBs participate in `ctx import --all` and default search refresh. Cloud sync endpoints, Oz/cloud conversations, browser IndexedDB, Markdown exports, command history outside `agent_tasks`, and Warp Drive/team data are not parsed. |
| CodeBuddy | `local_import_when_supported` | `~/.codebuddy`, `~/Library/Application Support/CodeBuddyExtension/Data`, `%LOCALAPPDATA%/CodeBuddyExtension`, or an explicit CodeBuddy history root. | Static local-history fixture smoke; schema proof comes from WayLog and sanitized fixtures. |
| CodeArts Agent | `local_import_when_supported` | CodeArts Agent kernel `opencode.db` under `~/.codeartsdoer/vscode-data/opencode.db`, `~/.codeartsdoer/codearts-data/opencode.db`, `$XDG_DATA_HOME/codeartsdoer/opencode.db`, `~/.local/share/codeartsdoer/opencode.db`, or an explicit DB path. | Static OpenCode-family SQLite fixture smoke; schema proof comes from `HuaweiCloud.vscode-codebot` v26.6.0 VSIX evidence. Imports only the kernel-managed SQLite DB, not older/private VS Code cache JSON. |
| Zencoder | `local_import_when_supported` | Zencoder `zencoder-chat` session trees under common VS Code-family `User/globalStorage/ZencoderAI.zencoder/zencoder-chat` roots, or an explicit chat tree/file path containing `sessions.json` plus `sessions/<id>.json`. | Static local-history fixture smoke; default discovery, `ctx import --all`, and pre-search refresh import the session tree when present. Schema proof comes from `jverre/opik-chat-history` and Open VSX `ZencoderAI.zencoder` v3.63.9002 constants. `.zencoder` skill/config homes and unrelated extension caches are not parsed. |
| Zenflow | `local_import_when_supported` | Zenflow Desktop `db.sqlite` at `ZENFLOW_DATA_DIR/db.sqlite`, `$XDG_DATA_HOME/zenflow/db.sqlite`, `~/.local/share/zenflow/db.sqlite`, macOS `~/Library/Application Support/ai.forgoodai.zenflow/db.sqlite`, Windows `%APPDATA%/forgoodai/zenflow/data/db.sqlite`, or an explicit DB path. | Static real-schema SQLite fixture smoke; schema proof comes from Zenflow Desktop 2.3.1 Linux artifact SHA256 `e623e073a212fccbfa295e2a7b7645a2c34525ab55f9cf247edce15babc731f2`, extracted app path code, and isolated app-created `db.sqlite`. Imports `tasks`, `chats`, executor sessions, raw/normalized execution logs, assistant session metadata, summaries, and tool-shaped log rows read-only. Cloud/auth state and raw sidecar logs are not parsed. |
| Syncfusion Code Studio | `local_import_when_supported` | Code Studio app-data `User/globalStorage/session-store.db` on Linux, macOS, or Windows, or an explicit Code Studio session-store SQLite DB. | Static fixture smoke plus source-backed artifact/docs evidence; limited to the proven session-store SQLite DB. `.codestudio` skills/agents/settings and debug logs are not imported as history. |
| Aider Desk | `local_import_when_supported` | Project-local `.aider-desk/tasks/<taskId>/context.json`, `AIDER_DESK_DIR/tasks/<taskId>/context.json`, or an explicit task, tasks, context file, or project root. | Static local-history fixture smoke; cwd/ancestor discovery only reports projects that already have task context files. |
| Amp | `local_import_when_supported` | Explicit JSON files emitted by `amp threads export <threadIDOrURL>`. | Explicit export fixture smoke; no default Amp discovery is claimed, and operational logs under `$XDG_CACHE_HOME/amp/logs` are not treated as durable transcript history. |
| Trae | `local_import_when_supported` | Native-auto discovery for Trae and Trae CN `User/workspaceStorage` roots at `~/Library/Application Support/Trae/User/workspaceStorage`, `~/Library/Application Support/Trae CN/User/workspaceStorage`, `%APPDATA%/Trae/User/workspaceStorage`, and `%APPDATA%/Trae CN/User/workspaceStorage`, plus explicit import from a workspace root, workspace directory, or `state.vscdb` file. | Source-backed synthetic state.vscdb fixture smoke; `trae-cn` is a CLI alias for the canonical `trae` provider. Default discovery is limited to workspace `state.vscdb` files with known `ItemTable` chat/input-history keys. Input-history rows are usually user prompts only and may not include assistant replies; `globalStorage`, `ModularData`, arbitrary caches, and unknown keys remain unclaimed. |
| OpenClaw | `local_import_when_supported` | `OPENCLAW_STATE_DIR`, `~/.openclaw`, legacy `~/.clawdbot`/`~/.moltbot`, or an explicit OpenClaw state tree. | Static local-history fixture smoke; beta storage-contract notes in the matrix. |
| Hermes Agent | `local_import_when_supported` | `HERMES_HOME/state.db`, `~/.hermes/state.db`, or an explicit Hermes SQLite DB. | Static local-history fixture smoke. |
| NanoClaw | `local_import_when_supported` | Preview/manual import from a NanoClaw project root or `data/v2.db`; cwd/ancestor discovery only. | Static local-history fixture smoke; excluded from `ctx import --all` and pre-search refresh until promoted. |
| AstrBot | `local_import_when_supported` | Native-auto import from `ASTRBOT_ROOT/data/data_v4.db`, packaged desktop `~/.astrbot/data/data_v4.db`, cwd/ancestor project DBs, or an explicit DB path. | Static local-history fixture smoke plus upstream AstrBot v4.26.4 source inspection; default discovery, `ctx import --all`, and pre-search refresh import bounded `data_v4.db` paths. Imports durable LLM context and available platform history rows, but is not a complete raw IM transcript importer for every AstrBot platform. |
| Shelley | `local_import_when_supported` | `SHELLEY_DB`, `~/.config/shelley/shelley.db`, or an explicit Shelley SQLite DB. | Static local-history fixture smoke; imports conversations/messages read-only with tool text, usage/model metadata, and parent conversation links. |
| Continue | `local_import_when_supported` | `CONTINUE_GLOBAL_DIR/sessions`, `~/.continue/sessions`, or an explicit Continue sessions path. | Static local-history fixture smoke; imports Continue CLI `sessions/*.json` history items and optional `sessions.json` metadata. |
| OpenHands | `local_import_when_supported` | `OH_PERSISTENCE_DIR`, legacy `FILE_STORE_PATH`, `~/.openhands`, or an explicit persistence root containing `<user_id>/v1_conversations/<conversation-id-hex>/*.json`. | Static local-history fixture smoke. |
| Antigravity | `local_import_when_supported` | Antigravity `transcript_full.jsonl` or `transcript.jsonl` files under `~/.gemini/antigravity-cli/brain`, official IDE transcripts under `~/.gemini/antigravity-ide/brain`, or an explicit Antigravity transcript JSONL tree. | Static local-history fixture smoke. |
| Gemini | `local_import_when_supported` | Gemini chat JSONL files under `~/.gemini/tmp/**/chats`, or an explicit Gemini CLI history tree. | Static local-history fixture smoke. |
| Tabnine | `local_import_when_supported` | Tabnine CLI chat JSONL files under `~/.tabnine/agent/tmp/**/chats`, or an explicit Tabnine CLI history tree. | Source-shaped fixture from the official Tabnine CLI 0.25.1 bundle. |
| Cursor | `local_import_when_supported` | Cursor agent transcript JSONL files under `~/.cursor/projects/**/agent-transcripts`, or an explicit Cursor agent transcript path. | Static local-history fixture smoke. |
| Windsurf | `local_import_when_supported` | Official Cascade hook transcript JSONL files under `~/.windsurf/transcripts`, or an explicit transcript file/directory. | Static local-history fixture smoke; default discovery, `ctx import --all`, and pre-search refresh import hook transcripts when JSONL files are present. Supported boundary is official hook transcript JSONL only; the hook is usually opt-in and captures sessions after hook setup. Does not import `~/.codeium/windsurf/cascade` or VS Code state DBs. |
| Zed | `local_import_when_supported` | Zed agent threads SQLite DB at `$XDG_DATA_HOME/zed/threads/threads.db`, `~/.local/share/zed/threads/threads.db`, or an explicit Zed `threads.db` path. | Static local-history fixture smoke; imports zstd-compressed Zed `DbThread` messages from `threads.data`. |
| Copilot CLI | `local_import_when_supported` | Copilot CLI `events.jsonl` files under `~/.copilot/session-state`, or an explicit Copilot CLI session-state tree. | Static local-history fixture smoke. |
| Factory AI Droid | `local_import_when_supported` | `~/.factory/sessions` or an explicit Factory AI Droid sessions tree. | Static local-history fixture smoke. |
| Qwen Code | `local_import_when_supported` | Qwen Code chat JSONL files under `QWEN_RUNTIME_DIR/projects`, `QWEN_HOME/projects`, `~/.qwen/projects`, or an explicit Qwen Code projects/chats tree. | Static local-history fixture smoke. |
| Kimi Code CLI | `local_import_when_supported` | Kimi Code CLI `session_index.jsonl` and `sessions/*/*/agents/*/wire.jsonl` files under `KIMI_CODE_HOME`, `~/.kimi-code`, or an explicit Kimi Code home/session tree. | Static local-history fixture smoke. |
| Autohand Code | `local_import_when_supported` | Autohand Code session directories containing `metadata.json` and `conversation.jsonl` under `AUTOHAND_HOME/sessions`, `~/.autohand/sessions`, or an explicit sessions tree. | Static local-history fixture smoke. |
| iFlow CLI | `local_import_when_supported` | iFlow CLI `session-*.jsonl` transcripts under `IFLOW_HOME/projects`, `~/.iflow/projects`, or an explicit projects tree. | Static local-history fixture smoke; supplemental tmp checkpoints are not imported. |
| Jazz | `local_import_when_supported` | Jazz per-agent history JSON files under `JAZZ_HOME/history`, `~/.jazz/history`, or an explicit history directory/file. | Static local-history fixture smoke; imports only conversations retained in each Jazz history file. |
| OpenLoaf | `local_import_when_supported` | OpenLoaf `chat-history/<session>/messages.jsonl` with optional `session.json` under `~/.openloaf/chat-history`, bounded `~/OpenLoafData/projects/*/.openloaf/chat-history`, or an explicit project/session path. | Static local-history fixture smoke; `loaf` is accepted as a provider alias, while `~/.loaf` remains npx detection-only. |
| Auggie | `local_import_when_supported` | Auggie session JSON files under `~/.augment/sessions`, or an explicit session JSON/session directory. | Static package-derived fixture smoke; imports `chatHistory` request/response text and recognized text nodes. |
| Devin CLI | `local_import_when_supported` | Explicit Devin CLI `devin --export [PATH]` ATIF JSON file or directory supplied with `--path`. | Static synthetic ATIF fixture smoke; proof comes from official Devin CLI docs/changelog plus the public ATIF RFC. No Devin cloud, login, account, default local, or `~/.config/devin` history paths are claimed. |
| Eve | `local_import_when_supported` | Eve local Workflow `.workflow-data` streams from `WORKFLOW_LOCAL_DATA_DIR`, a current project `.workflow-data`, or an explicit `.workflow-data`/project path. | Static package-derived Workflow stream fixture smoke; imports durable Eve message stream events, not `.eve` build/runtime artifacts. |
| Junie | `local_import_when_supported` | Junie `index.jsonl` plus `session-*/events.jsonl` under `JUNIE_SESSIONS_DIR`, `JUNIE_HOME/sessions`, `~/.junie/sessions`, or an explicit sessions/session path. | Static local-history fixture smoke; imports Junie's event-sourced UI stream and skips attachment files/project memory. |
| TinyCloud | `local_import_when_supported` | `$TINYCLOUD_HOME/projects/*/sessions/*.jsonl`, `~/.tinycloud/projects/*/sessions/*.jsonl`, legacy `<home>/sessions/*.jsonl`, or an explicit TinyCloud home/session path. | Static local-history fixture smoke; schema proof comes from `@cloudglue/tinycloud@0.3.8` gitHead `fb0b313286bc83d4c48f66831e8acb7a6b51847a`. TinyCloud config, cache, job, artifact, and skill directories are not imported as history. |
| Firebender | `local_import_when_supported` | Firebender JetBrains project chat history SQLite at `.idea/firebender/chat_history.db`, or an explicit project root/DB path. | Static local-history fixture smoke; schema proof comes from public Firebender Marketplace plugin bytecode. |
| ForgeCode | `local_import_when_supported` | `FORGE_CONFIG/.forge.db`, legacy `~/forge/.forge.db`, `~/.forge/.forge.db`, or an explicit ForgeCode SQLite DB. | Static local-history fixture smoke; imports conversation context JSON and metrics file touches. |
| Deep Agents | `local_import_when_supported` | `~/.deepagents/.state/sessions.db` or an explicit Deep Agents LangGraph checkpoint SQLite DB. | Static local-history fixture smoke; imports decoded root `writes.messages` chat messages only, not checkpoint state blobs or `history.jsonl`. |
| Mistral Vibe | `local_import_when_supported` | Mistral Vibe session directories containing `meta.json` and `messages.jsonl` under `VIBE_HOME/logs/session`, `~/.vibe/logs/session`, or an explicit session/log root. | Static local-history fixture smoke; schema proof comes from the public Mistral Vibe repo and sanitized fixtures. |
| Mux | `local_import_when_supported` | Mux `chat.jsonl` transcripts under `MUX_ROOT/sessions`, `~/.mux/sessions`, or an explicit sessions/session path, with optional `partial.json` and archived subagent transcripts. | Static local-history fixture smoke; schema proof comes from `coder/mux@v0.27.0` and sanitized fixtures. |
| Moxby | `local_import_when_supported` | Moxby `moxby_chats.db` SQLite chat history under `MOXBY_STATE_DIR`, XDG data, macOS Application Support, Windows APPDATA, or an explicit Moxby state directory/DB path. | Static sanitized SQLite fixture smoke; schema proof comes from the Moxby v2.3.0 `ChainAI-Org/moxby-agent-releases` macOS bundle strings and tool/schema descriptions. |
| Reasonix | `local_import_when_supported` | Reasonix session JSONL files under `~/.reasonix/sessions`, including adjacent `.events.jsonl`, `.meta.json`, `.pending.json`, and `.plan.json` sidecars, or an explicit sessions tree/transcript file. | Static local-history fixture smoke; schema proof comes from `esengine/DeepSeek-Reasonix` tag `v0.53.2` and sanitized fixtures. |
| AdaL | `local_import_when_supported` | AdaL event-sourced JSONL sessions under `~/.adal/sessions/conversation_<id>.jsonl`, optional `<id>_metadata.json` sidecars, or an explicit sessions tree/session JSONL path. | Static synthetic fixture smoke; schema proof comes from `@sylphai/adal-cli@1.4.1` platform package bytecode and no private user history. |
| Kode | `local_import_when_supported` | Kode project JSONL transcripts under `KODE_CONFIG_DIR/projects`, `CLAUDE_CONFIG_DIR/projects`, `~/.kode/projects`, or an explicit projects tree. | Static local-history fixture smoke; schema proof comes from `@shareai-lab/kode@2.2.1` and sanitized fixtures. |
| Neovate | `local_import_when_supported` | Neovate project session JSONL transcripts under `~/.neovate/projects` or an explicit projects tree, excluding request/file-history sidecars. | Static local-history fixture smoke; schema proof comes from `@neovate/code@0.28.5` and sanitized fixtures. |
| Command Code | `local_import_when_supported` | Command Code JSONL transcripts under `~/.commandcode/projects`, or an explicit projects/session JSONL path. | Reads session JSONL files read-only; skips checkpoint and prompt sidecars. |
| Rovo Dev | `local_import_when_supported` | Rovo Dev session directories containing `session_context.json` and optional `metadata.json` under `~/.rovodev/sessions`, or an explicit sessions/session path. | Reads session context files read-only; imports `message_history`/`messages` entries and parent-session metadata when present. |
| Cortex Code | `local_import_when_supported` | Cortex Code conversation JSON files and optional `<session>.history.jsonl` files under `~/.snowflake/cortex/conversations`, or an explicit conversations/session path. | Reads session snapshots/history files read-only; schema anchor is Snowflake Cortex Code ACP docs. |
| Terramind | `local_import_when_supported` | Terramind/Nucleus SQLite history at `$XDG_CONFIG_HOME/Nucleus/data/agents.db`, `~/.config/Nucleus/data/agents.db`, macOS `~/Library/Application Support/Nucleus/data/agents.db`, Windows `%APPDATA%/Nucleus/data/agents.db`, or an explicit `agents.db`. | Source-backed SQLite fixture smoke; imports `projects`, `chats`, `sub_chats.messages` JSON, and split `tool_outputs` rows read-only. |
| IBM Bob | `local_import_when_supported` | IBM Bob IDE task JSON under `IBM Bob` or `Bob-IDE` app-data `User/globalStorage/ibm.bob-code/tasks`, or an explicit task/globalStorage path. | Static local-history fixture smoke; Bob Shell `~/.bob` skill/state paths are not parsed. |
| Cline | `local_import_when_supported` | `CLINE_DATA_DIR`, `CLINE_DIR/data`, `~/.cline/data`, common VS Code globalStorage task folders, or an explicit Cline data/task path. | Static local-history fixture smoke; VS Code state databases are not parsed. |
| Roo Code | `local_import_when_supported` | `roo-cline.customStoragePath`, common VS Code globalStorage task folders for `RooVeterinaryInc.roo-cline`, or an explicit Roo task storage path. | Static local-history fixture smoke; VS Code state databases are not parsed. |

`ctx sources --json` uses `import_support: "preview"` and `native_import:
false` for preview sources/importers such as NanoClaw and state
databases outside the proven Trae/Trae CN workspaceStorage defaults.
Those paths can be imported explicitly with
`ctx import --provider ...` when discovery finds them, or with
`ctx import --provider ... --path ...` for a specific path. They are not swept up
by `ctx import --all` or the default pre-search refresh.

Fidelity fields in the machine-readable matrix describe the default public CLI
import behavior and normalized ctx storage fields. Supported adapters record
normalized `files_touched` metadata when provider transcripts expose file paths
in tool calls, command output, patches, or native provider fields. Command
output, tool output, and token details remain skipped unless lower-level adapter
modes import them explicitly.

## Provider Smoke

Provider smoke coverage uses public fixture data and generated local-history
trees. The public smoke target exercises supported imports, blocked unsupported
providers, provider filtering, citations, and deterministic search without
executing provider CLIs, reading real user history, requiring API keys, or
making network calls:

```bash
bazel test //:provider_fixture_e2e --config=ci
```

## Required Evidence For Promotion

Before a provider moves beyond `fixture_only`, `detected_unsupported`, or
`blocked` into native local-history support, the change needs:

- a documented local source format;
- read-only source discovery or an explicit `--path` contract;
- malformed-input tests;
- idempotent re-import tests;
- source citation fields in search output;
- storage/privacy notes for provider-specific sensitive fields;
- docs updates in this file and `provider-support-matrix.json`.
