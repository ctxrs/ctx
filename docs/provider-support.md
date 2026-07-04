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
| Dexto | `local_import_when_supported` | Explicit Dexto SQLite DB path. | Static local-history fixture smoke; default discovery remains intentionally unclaimed. |
| Lingma | `local_import_when_supported` | `~/.lingma/vscode/sharedClientCache/cache/db/local.db`, `~/.lingma/vscode-insiders/sharedClientCache/cache/db/local.db`, or an explicit Lingma `local.db`. | Static local-history fixture smoke; schema proof comes from WayLog. Assistant content is summary/error_result only and may be partial. Qoder CN is documented as the renamed Lingma product line, but no `qoder-cn` alias is shipped without source-backed DB contract proof. |
| CodeBuddy | `local_import_when_supported` | `~/.codebuddy`, `~/Library/Application Support/CodeBuddyExtension/Data`, `%LOCALAPPDATA%/CodeBuddyExtension`, or an explicit CodeBuddy history root. | Static local-history fixture smoke; schema proof comes from WayLog and sanitized fixtures. |
| Aider Desk | `local_import_when_supported` | Project-local `.aider-desk/tasks/<taskId>/context.json`, `AIDER_DESK_DIR/tasks/<taskId>/context.json`, or an explicit task, tasks, context file, or project root. | Static local-history fixture smoke; cwd/ancestor discovery only reports projects that already have task context files. |
| OpenClaw | `local_import_when_supported` | `OPENCLAW_STATE_DIR`, `~/.openclaw`, legacy `~/.clawdbot`/`~/.moltbot`, or an explicit OpenClaw state tree. | Static local-history fixture smoke; beta storage-contract notes in the matrix. |
| Hermes Agent | `local_import_when_supported` | `HERMES_HOME/state.db`, `~/.hermes/state.db`, or an explicit Hermes SQLite DB. | Static local-history fixture smoke. |
| NanoClaw | `local_import_when_supported` | Preview/manual import from a NanoClaw project root or `data/v2.db`; cwd/ancestor discovery only. | Static local-history fixture smoke; excluded from `ctx import --all` and pre-search refresh until promoted. |
| AstrBot | `local_import_when_supported` | Preview/manual import from `ASTRBOT_ROOT/data/data_v4.db`, `~/.astrbot/data/data_v4.db`, cwd/ancestor project DBs, or an explicit DB path. | Static local-history fixture smoke; imports LLM context plus available platform history, not guaranteed complete IM transcripts. |
| Shelley | `local_import_when_supported` | `SHELLEY_DB`, `~/.config/shelley/shelley.db`, or an explicit Shelley SQLite DB. | Static local-history fixture smoke; imports conversations/messages read-only with tool text, usage/model metadata, and parent conversation links. |
| Continue | `local_import_when_supported` | `CONTINUE_GLOBAL_DIR/sessions`, `~/.continue/sessions`, or an explicit Continue sessions path. | Static local-history fixture smoke; imports Continue CLI `sessions/*.json` history items and optional `sessions.json` metadata. |
| OpenHands | `local_import_when_supported` | `OH_PERSISTENCE_DIR`, legacy `FILE_STORE_PATH`, `~/.openhands`, or an explicit persistence root containing `<user_id>/v1_conversations/<conversation-id-hex>/*.json`. | Static local-history fixture smoke. |
| Antigravity | `local_import_when_supported` | Antigravity `transcript_full.jsonl` or `transcript.jsonl` files under `~/.gemini/antigravity-cli/brain`, or an explicit Antigravity transcript JSONL tree. | Static local-history fixture smoke. |
| Gemini | `local_import_when_supported` | Gemini chat JSONL files under `~/.gemini/tmp/**/chats`, or an explicit Gemini CLI history tree. | Static local-history fixture smoke. |
| Cursor | `local_import_when_supported` | Cursor agent transcript JSONL files under `~/.cursor/projects/**/agent-transcripts`, or an explicit Cursor agent transcript path. | Static local-history fixture smoke. |
| Zed | `local_import_when_supported` | Zed agent threads SQLite DB at `$XDG_DATA_HOME/zed/threads/threads.db`, `~/.local/share/zed/threads/threads.db`, or an explicit Zed `threads.db` path. | Static local-history fixture smoke; imports zstd-compressed Zed `DbThread` messages from `threads.data`. |
| Copilot CLI | `local_import_when_supported` | Copilot CLI `events.jsonl` files under `~/.copilot/session-state`, or an explicit Copilot CLI session-state tree. | Static local-history fixture smoke. |
| Factory AI Droid | `local_import_when_supported` | `~/.factory/sessions` or an explicit Factory AI Droid sessions tree. | Static local-history fixture smoke. |
| Qwen Code | `local_import_when_supported` | Qwen Code chat JSONL files under `QWEN_RUNTIME_DIR/projects`, `QWEN_HOME/projects`, `~/.qwen/projects`, or an explicit Qwen Code projects/chats tree. | Static local-history fixture smoke. |
| Kimi Code CLI | `local_import_when_supported` | Kimi Code CLI `session_index.jsonl` and `sessions/*/*/agents/*/wire.jsonl` files under `KIMI_CODE_HOME`, `~/.kimi-code`, or an explicit Kimi Code home/session tree. | Static local-history fixture smoke. |
| Autohand Code | `local_import_when_supported` | Autohand Code session directories containing `metadata.json` and `conversation.jsonl` under `AUTOHAND_HOME/sessions`, `~/.autohand/sessions`, or an explicit sessions tree. | Static local-history fixture smoke. |
| iFlow CLI | `local_import_when_supported` | iFlow CLI `session-*.jsonl` transcripts under `IFLOW_HOME/projects`, `~/.iflow/projects`, or an explicit projects tree. | Static local-history fixture smoke; supplemental tmp checkpoints are not imported. |
| ForgeCode | `local_import_when_supported` | `FORGE_CONFIG/.forge.db`, legacy `~/forge/.forge.db`, `~/.forge/.forge.db`, or an explicit ForgeCode SQLite DB. | Static local-history fixture smoke; imports conversation context JSON and metrics file touches. |
| Deep Agents | `local_import_when_supported` | `~/.deepagents/.state/sessions.db` or an explicit Deep Agents LangGraph checkpoint SQLite DB. | Static local-history fixture smoke; imports decoded root `writes.messages` chat messages only, not checkpoint state blobs or `history.jsonl`. |
| Mistral Vibe | `local_import_when_supported` | Mistral Vibe session directories containing `meta.json` and `messages.jsonl` under `VIBE_HOME/logs/session`, `~/.vibe/logs/session`, or an explicit session/log root. | Static local-history fixture smoke; schema proof comes from the public Mistral Vibe repo and sanitized fixtures. |
| Mux | `local_import_when_supported` | Mux `chat.jsonl` transcripts under `MUX_ROOT/sessions`, `~/.mux/sessions`, or an explicit sessions/session path, with optional `partial.json` and archived subagent transcripts. | Static local-history fixture smoke; schema proof comes from `coder/mux@v0.27.0` and sanitized fixtures. |
| Reasonix | `local_import_when_supported` | Reasonix session JSONL files under `~/.reasonix/sessions`, including adjacent `.events.jsonl`, `.meta.json`, `.pending.json`, and `.plan.json` sidecars, or an explicit sessions tree/transcript file. | Static local-history fixture smoke; schema proof comes from `esengine/DeepSeek-Reasonix` tag `v0.53.2` and sanitized fixtures. |
| Kode | `local_import_when_supported` | Kode project JSONL transcripts under `KODE_CONFIG_DIR/projects`, `CLAUDE_CONFIG_DIR/projects`, `~/.kode/projects`, or an explicit projects tree. | Static local-history fixture smoke; schema proof comes from `@shareai-lab/kode@2.2.1` and sanitized fixtures. |
| Neovate | `local_import_when_supported` | Neovate project session JSONL transcripts under `~/.neovate/projects` or an explicit projects tree, excluding request/file-history sidecars. | Static local-history fixture smoke; schema proof comes from `@neovate/code@0.28.5` and sanitized fixtures. |
| Terramind | `local_import_when_supported` | Terramind/Nucleus SQLite history at `$XDG_CONFIG_HOME/Nucleus/data/agents.db`, `~/.config/Nucleus/data/agents.db`, macOS `~/Library/Application Support/Nucleus/data/agents.db`, Windows `%APPDATA%/Nucleus/data/agents.db`, or an explicit `agents.db`. | Source-backed SQLite fixture smoke; imports `projects`, `chats`, `sub_chats.messages` JSON, and split `tool_outputs` rows read-only. |
| Cline | `local_import_when_supported` | `CLINE_DATA_DIR`, `CLINE_DIR/data`, `~/.cline/data`, common VS Code globalStorage task folders, or an explicit Cline data/task path. | Static local-history fixture smoke; VS Code state databases are not parsed. |
| Roo Code | `local_import_when_supported` | `roo-cline.customStoragePath`, common VS Code globalStorage task folders for `RooVeterinaryInc.roo-cline`, or an explicit Roo task storage path. | Static local-history fixture smoke; VS Code state databases are not parsed. |

`ctx sources --json` uses `import_support: "preview"` and `native_import:
false` for preview sources such as NanoClaw and AstrBot. Those paths can be
imported explicitly with `ctx import --provider ...` when discovery finds them,
or with `ctx import --provider ... --path ...` for a specific path. They are not
swept up by `ctx import --all` or the default pre-search refresh.

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
