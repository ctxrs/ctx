# npx skills Agent Storage Coverage

This ledger compares every `AgentType` in `skills@1.5.14` commit
`2adcfe5a4cce0ce5f4d5547a997b2a161ec5d127` against ctx native history
providers on `origin/main`. Upstream evidence comes from `src/types.ts` and
`src/agents.ts`; ctx evidence comes from `docs/provider-support-matrix.json`,
`crates/ctx-history-capture/src/provider_sources.rs`, and the native provider
arguments in `crates/ctx-cli/src/main.rs`.

Status meanings:

- `native-auto`: ctx has an auto-importable native provider path for this npx id.
- `native-preview`: ctx has an explicit native importer, but it is preview-only
  and excluded from automatic refresh.
- `candidate-family`: no ctx native importer exists, but the id falls into a
  reusable storage family worth validating before implementation.
- `webapp-boundary`: npx can install skills, but native history appears to live
  behind a desktop app, hosted service, account store, or object-store boundary.
- `unknown`: npx only proves a skill install or detection path; native history
  storage still needs source research.
- `install-target`: npx target is an aggregate or project skill target, not a
  proven history-producing agent.

Result on this integration branch: 35 `native-auto`, 2 `native-preview`, 14
`candidate-family`, 9 `webapp-boundary`, 10 `unknown`, and 2 `install-target`
rows.

## Shared Families

- `opencode sqlite family`: native `opencode` and `kilo` coverage share the
  reusable SQLite baseline for OpenCode-style message/session tables.
- `Cline/Roo task JSON`: native `cline` and `roo` coverage share one task JSON
  importer for file-backed task directories.
- `JSONL CLI event logs`: already covers Codex, Claude Code, OpenClaw,
  Antigravity CLI, Gemini CLI, Pi, Factory Droid, Copilot CLI-shaped logs, and
  Autohand Code, iFlow CLI, Mistral Vibe, Mux, and Reasonix sessions.
- `CLI session JSON`: covers Continue CLI `sessions/*.json` files with
  `sessions.json` metadata.
- `project task JSON`: covers Aider Desk project-local task directories such as
  `.aider-desk/tasks/<taskId>/context.json`; related task-directory tools can
  reuse this scanner once storage proof and fixtures exist.
- `filesystem event JSON`: covers OpenHands event JSON under
  `<persistence>/<user_id>/v1_conversations`.
- `generic sqlite messages`: already covers Crush, Goose, Hermes, Kiro CLI,
  Dexto explicit imports, Terramind/Nucleus `agents.db`, the AstrBot preview
  importer, and ctx-native Shelley.
- `Forge conversation SQLite`: covers ForgeCode's `.forge.db` conversation
  snapshots with JSON context/metrics DTOs.
- `LangGraph checkpoint SQLite`: candidate family for LangGraph-style
  checkpoint databases plus JSONL history sidecars.
- `VS Code/Electron storage`: Cursor is covered through a known transcript tree,
  CodeBuddy is covered through its file-backed history JSON, and Zed is covered
  through its agent `threads.db`; other IDE-like tools need storage discovery
  before reuse.
- `webapp/object-store boundary`: prefer explicit exporters or
  `ctx-history-jsonl-v1` history-source plugins over speculative native readers.

## Coverage Ledger

| npx skills agent id | ctx storage ingestion status | schema family | evidence source | blocked reason / gap |
| --- | --- | --- | --- | --- |
| `aider-desk` | `native-auto` | `project task JSON` | ctx `aider_desk_task_context_json`; npx `~/.aider-desk`; source proof shows project `.aider-desk/tasks/<taskId>/context.json` task context files | - |
| `amp` | `candidate-family` | `JSONL CLI event logs` | npx `~/.config/amp`; no ctx provider | Need transcript location and schema proof. |
| `antigravity` | `candidate-family` | `VS Code/Electron storage` | npx `~/.gemini/antigravity`; no ctx provider for IDE id | IDE history is not proven equivalent to CLI brain transcripts. |
| `antigravity-cli` | `native-auto` | `JSONL CLI event logs` | ctx `antigravity_cli_transcript_jsonl_tree`; npx `~/.gemini/antigravity-cli` | - |
| `astrbot` | `native-preview` | `generic sqlite messages` | ctx `astrbot_data_v4_sqlite`; npx `~/.astrbot` | Preview explicit import only; full per-platform transcript coverage remains unproven. |
| `autohand-code` | `native-auto` | `JSONL CLI event logs` | ctx `autohand_code_sessions_jsonl`; npx `AUTOHAND_HOME` or `~/.autohand` | - |
| `augment` | `candidate-family` | `VS Code/Electron storage` | npx `~/.augment`; no ctx provider | Need local app storage or export contract proof. |
| `bob` | `unknown` | `unknown native history` | npx `~/.bob`; no ctx provider | Need native history storage research before claiming import support. |
| `claude-code` | `native-auto` | `JSONL CLI event logs` | ctx `claude_projects_jsonl_tree`; npx `~/.claude` | - |
| `openclaw` | `native-auto` | `JSONL CLI event logs` | ctx `openclaw_session_jsonl_tree`; npx `~/.openclaw` or legacy homes | Provider matrix still notes GA schema-stability validation. |
| `cline` | `native-auto` | `Cline/Roo task JSON` | ctx `cline_task_directory_json`; npx `~/.cline` | - |
| `codearts-agent` | `candidate-family` | `VS Code/Electron storage` | npx `~/.codeartsdoer`; no ctx provider | Need app storage proof before adapting IDE-family importers. |
| `codebuddy` | `native-auto` | `VS Code/Electron storage` | ctx `codebuddy_history_json`; npx project or home `.codebuddy` | Schema proof from WayLog `shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09`; continue validating schema drift. |
| `codemaker` | `unknown` | `unknown native history` | npx `~/.codemaker`; no ctx provider | Need native history storage research before claiming import support. |
| `codestudio` | `candidate-family` | `VS Code/Electron storage` | npx `~/.codestudio`; no ctx provider | Need app storage proof before adapting IDE-family importers. |
| `codex` | `native-auto` | `JSONL CLI event logs` | ctx `codex_session_jsonl_tree` and `codex_history_jsonl`; npx `CODEX_HOME` | - |
| `command-code` | `unknown` | `unknown native history` | npx `~/.commandcode`; no ctx provider | Need native history storage research before claiming import support. |
| `continue` | `native-auto` | `CLI session JSON` | ctx `continue_cli_sessions_json`; npx project or home `.continue` | - |
| `cortex` | `unknown` | `unknown native history` | npx `~/.snowflake/cortex`; no ctx provider | Need native history storage research before claiming import support. |
| `crush` | `native-auto` | `generic sqlite messages` | ctx `crush_sqlite`; npx `~/.config/crush` | - |
| `cursor` | `native-auto` | `VS Code/Electron storage` | ctx `cursor_agent_transcript_jsonl_tree`; npx `~/.cursor` | - |
| `deepagents` | `native-auto` | `LangGraph checkpoint SQLite` | ctx `deepagents_sessions_sqlite`; npx `~/.deepagents`; official local state evidence points to `~/.deepagents/.state/sessions.db` and `history.jsonl` | Imports decoded root `writes.messages` chat messages only; `history.jsonl` and arbitrary checkpoint state blobs are not indexed. |
| `devin` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.config/devin`; no ctx provider | Hosted-agent history should use an explicit export path such as ATIF when available; no local conversation DB is proven. |
| `dexto` | `native-preview` | `generic sqlite messages` | ctx `dexto_sqlite`; npx `~/.dexto` | Preview explicit import only; no proven default discovery path yet. |
| `droid` | `native-auto` | `JSONL CLI event logs` | ctx `factory_ai_droid_sessions_jsonl`; npx `~/.factory` | - |
| `eve` | `unknown` | `unknown native history` | npx project `agent`; no ctx provider | Project skill layout does not prove a local history schema. |
| `firebender` | `candidate-family` | `VS Code/Electron storage` | npx `~/.firebender`; no ctx provider | Need local app storage or export contract proof. |
| `forgecode` | `native-auto` | `Forge conversation SQLite` | ctx `forgecode_sqlite`; npx `FORGE_CONFIG`, legacy `~/forge`, or `~/.forge` | - |
| `gemini-cli` | `native-auto` | `JSONL CLI event logs` | ctx `gemini_cli_chat_recording_jsonl`; npx `~/.gemini` | - |
| `github-copilot` | `native-auto` | `JSONL CLI event logs` | ctx `copilot_cli_session_events_jsonl`; npx `~/.copilot` | Coverage is for Copilot CLI session-state logs, not editor or web history. |
| `goose` | `native-auto` | `generic sqlite messages` | ctx `goose_sessions_sqlite`; npx `~/.config/goose` | - |
| `hermes-agent` | `native-auto` | `generic sqlite messages` | ctx `hermes_state_sqlite`; npx `HERMES_HOME` | - |
| `inference-sh` | `unknown` | `unknown native history` | npx `~/.inferencesh`; no ctx provider | Need native history storage research before claiming import support. |
| `iflow-cli` | `native-auto` | `JSONL CLI event logs` | ctx `iflow_cli_session_jsonl_tree`; npx `IFLOW_HOME` or `~/.iflow` | - |
| `jazz` | `unknown` | `unknown native history` | npx project or home `.jazz`; no ctx provider | Need native history storage research before claiming import support. |
| `junie` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.junie`; no ctx provider | IDE-managed history boundary needs a verified local export or plugin. |
| `kilo` | `native-auto` | `opencode sqlite family` | ctx `kilo_sqlite`; npx `~/.kilocode` | - |
| `kimi-code-cli` | `native-auto` | `JSONL CLI event logs` | ctx `kimi_code_cli_wire_jsonl_tree`; npx `~/.kimi-code` or `~/.kimi` | - |
| `kiro-cli` | `native-auto` | `generic sqlite messages` | ctx `kiro_cli_sqlite`; npx `~/.kiro` | SQLite import covers the proven `conversations_v2`/`conversations` DB at the Kiro CLI data dir; newer `~/.kiro/sessions/cli` event logs are not imported yet. |
| `kode` | `native-auto` | `JSONL CLI event logs` | ctx `kode_session_jsonl_tree`; npx `~/.kode`; `@shareai-lab/kode` stores project JSONL sessions under `KODE_CONFIG_DIR`, `CLAUDE_CONFIG_DIR`, or `~/.kode` | - |
| `lingma` | `native-auto` | `VS Code/Electron storage` | ctx `lingma_sqlite`; npx `~/.lingma` | Schema proof from WayLog `shayne-snap/WayLog@6939033b7a39326fbdc249e28e6aa12461db1f09`; imports `chat_prompt` plus assistant `summary`/`error_result`, which may be partial. Qoder CN is documented by Alibaba as the renamed Lingma product line, but no `qoder-cn` alias is shipped without source-backed DB contract proof. |
| `loaf` | `unknown` | `unknown native history` | npx `~/.loaf`; no ctx provider | Need native history storage research before claiming import support. |
| `mcpjam` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.mcpjam`; no ctx provider | UI or account-backed activity should use exporter or plugin until local storage is proven. |
| `mistral-vibe` | `native-auto` | `JSONL CLI event logs` | ctx `mistral_vibe_session_jsonl_tree`; npx `VIBE_HOME` or `~/.vibe` | - |
| `moxby` | `unknown` | `unknown native history` | npx `~/.moxby`; no ctx provider | Need native history storage research before claiming import support. |
| `mux` | `native-auto` | `JSONL CLI event logs` | ctx `mux_session_jsonl_tree`; npx `MUX_ROOT` or `~/.mux` | - |
| `neovate` | `native-auto` | `JSONL CLI event logs` | ctx `neovate_session_jsonl_tree`; npx `~/.neovate`; `@neovate/code` stores project session JSONL under `~/.neovate/projects` | - |
| `opencode` | `native-auto` | `opencode sqlite family` | ctx `opencode_sqlite`; npx `~/.config/opencode` | - |
| `openhands` | `native-auto` | `filesystem event JSON` | ctx `openhands_file_events`; npx `~/.openhands` | - |
| `ona` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.ona`; no ctx provider | No proven stable local transcript boundary; prefer exporter or plugin. |
| `pi` | `native-auto` | `JSONL CLI event logs` | ctx `pi_session_jsonl`; npx `~/.pi/agent` | - |
| `qoder` | `candidate-family` | `VS Code/Electron storage` | npx `~/.qoder`; no ctx provider | Need local app storage or export contract proof; Lingma/Qoder CN rename evidence does not prove this separate home path. |
| `qoder-cn` | `candidate-family` | `VS Code/Electron storage` | npx `~/.qoder-cn`; no ctx provider | Alibaba docs identify Qoder CN as the renamed Lingma product line, but ctx needs source-backed proof that Qoder CN still uses the same local DB path/schema before adding an alias. |
| `qwen-code` | `native-auto` | `JSONL CLI event logs` | ctx `qwen_code_chat_jsonl_tree`; npx `~/.qwen` | - |
| `replit` | `webapp-boundary` | `webapp/object-store boundary` | npx project `.replit`; no ctx provider | Project marker is not a local agent history contract. |
| `reasonix` | `native-auto` | `JSONL CLI event logs` | ctx `reasonix_session_jsonl_tree`; npx `~/.reasonix/sessions`; package `reasonix@0.53.2` | - |
| `roo` | `native-auto` | `Cline/Roo task JSON` | ctx `roo_task_directory_json`; npx `~/.roo` | - |
| `rovodev` | `candidate-family` | `JSONL CLI event logs` | npx `~/.rovodev`; no ctx provider | Need transcript location and schema proof. |
| `tabnine-cli` | `candidate-family` | `JSONL CLI event logs` | npx `~/.tabnine`; no ctx provider | Need transcript location and schema proof. |
| `terramind` | `native-auto` | `generic sqlite messages` | ctx `terramind_agents_sqlite`; npx package `terramind@0.2.91` resolves Nucleus app data to `$XDG_CONFIG_HOME/Nucleus/data/agents.db`, `~/.config/Nucleus/data/agents.db`, macOS `~/Library/Application Support/Nucleus/data/agents.db`, or Windows `%APPDATA%/Nucleus/data/agents.db` | Fixture is source-backed from the published package schema because a no-auth `npx terramind@0.2.91 list --chats` probe did not complete. |
| `tinycloud` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.tinycloud`; no ctx provider | No proven stable local transcript boundary; prefer exporter or plugin. |
| `trae` | `candidate-family` | `VS Code/Electron storage` | npx `~/.trae`; no ctx provider | Need local app storage or export contract proof. |
| `trae-cn` | `candidate-family` | `VS Code/Electron storage` | npx `~/.trae-cn`; no ctx provider | Need local app storage or export contract proof. |
| `warp` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.warp`; no ctx provider | Skill/config target is not a local transcript contract; native support needs explicit export or local DB proof. |
| `windsurf` | `candidate-family` | `VS Code/Electron storage` | npx `~/.codeium/windsurf`; no ctx provider | Need local app storage or export contract proof. |
| `zed` | `native-auto` | `VS Code/Electron storage` | ctx `zed_threads_sqlite`; npx `$XDG_DATA_HOME/zed` or `~/.local/share/zed` | Per-message timestamps are unavailable; ctx uses thread `updated_at`. |
| `zencoder` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.zencoder`; no ctx provider | Skill home evidence is not a transcript schema; prefer exporter, plugin, or underlying provider imports. |
| `zenflow` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.zencoder`; no ctx provider | Shares Zencoder skill home but no proven local history contract; prefer exporter or underlying provider imports. |
| `pochi` | `candidate-family` | `VS Code/Electron storage` | npx `~/.pochi`; no ctx provider | Need local app storage or export contract proof. |
| `promptscript` | `install-target` | `agent skills aggregate` | npx project `.promptscript` or `promptscript.yaml`; no ctx provider | Project skill target only; use custom history JSONL if it emits runs. |
| `adal` | `unknown` | `unknown native history` | npx `~/.adal`; no ctx provider | Need native history storage research before claiming import support. |
| `universal` | `install-target` | `agent skills aggregate` | npx virtual `.agents/skills` target; no ctx provider | Aggregate skill install target, not a history-producing native provider. |

## ctx Native Providers Outside This npx Target Set

`nanoclaw` and `shelley` are native ctx providers on `origin/main`, but they do
not have matching `skills@1.5.14` `AgentType` ids. `nanoclaw` is preview-only;
`shelley` is native auto-importable with `shelley_sqlite`.
