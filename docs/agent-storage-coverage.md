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

Result on this integration branch: 20 `native-auto`, 2 `native-preview`, 22
`candidate-family`, 10 `webapp-boundary`, 16 `unknown`, and 2 `install-target`
rows.

## Shared Families

- `opencode sqlite family`: native `opencode` and `kilo` coverage share the
  reusable SQLite baseline for OpenCode-style message/session tables.
- `Cline/Roo task JSON`: native `cline` and `roo` coverage share one task JSON
  importer for file-backed task directories.
- `JSONL CLI event logs`: already covers Codex, Claude Code, OpenClaw,
  Antigravity CLI, Gemini CLI, Pi, Factory Droid, and Copilot CLI-shaped logs.
- `CLI session JSON`: covers Continue CLI `sessions/*.json` files with
  `sessions.json` metadata.
- `filesystem event JSON`: covers OpenHands event JSON under
  `<persistence>/<user_id>/v1_conversations`.
- `generic sqlite messages`: already covers Crush, Goose, Hermes, Dexto
  explicit imports, the AstrBot preview importer, and ctx-native Shelley.
- `VS Code/Electron storage`: Cursor is covered through a known transcript tree;
  other IDE-like tools need storage discovery before reuse.
- `webapp/object-store boundary`: prefer explicit exporters or
  `ctx-history-jsonl-v1` history-source plugins over speculative native readers.

## Coverage Ledger

| npx skills agent id | ctx storage ingestion status | schema family | evidence source | blocked reason / gap |
| --- | --- | --- | --- | --- |
| `aider-desk` | `unknown` | `unknown native history` | npx `~/.aider-desk`; no ctx provider | Need native history storage research before claiming import support. |
| `amp` | `candidate-family` | `JSONL CLI event logs` | npx `~/.config/amp`; no ctx provider | Need transcript location and schema proof. |
| `antigravity` | `candidate-family` | `VS Code/Electron storage` | npx `~/.gemini/antigravity`; no ctx provider for IDE id | IDE history is not proven equivalent to CLI brain transcripts. |
| `antigravity-cli` | `native-auto` | `JSONL CLI event logs` | ctx `antigravity_cli_transcript_jsonl_tree`; npx `~/.gemini/antigravity-cli` | - |
| `astrbot` | `native-preview` | `generic sqlite messages` | ctx `astrbot_data_v4_sqlite`; npx `~/.astrbot` | Preview explicit import only; full per-platform transcript coverage remains unproven. |
| `autohand-code` | `candidate-family` | `JSONL CLI event logs` | npx `AUTOHAND_HOME` or `~/.autohand`; no ctx provider | Need native transcript schema proof. |
| `augment` | `candidate-family` | `VS Code/Electron storage` | npx `~/.augment`; no ctx provider | Need local app storage or export contract proof. |
| `bob` | `unknown` | `unknown native history` | npx `~/.bob`; no ctx provider | Need native history storage research before claiming import support. |
| `claude-code` | `native-auto` | `JSONL CLI event logs` | ctx `claude_projects_jsonl_tree`; npx `~/.claude` | - |
| `openclaw` | `native-auto` | `JSONL CLI event logs` | ctx `openclaw_session_jsonl_tree`; npx `~/.openclaw` or legacy homes | Provider matrix still notes GA schema-stability validation. |
| `cline` | `native-auto` | `Cline/Roo task JSON` | ctx `cline_task_directory_json`; npx `~/.cline` | - |
| `codearts-agent` | `candidate-family` | `VS Code/Electron storage` | npx `~/.codeartsdoer`; no ctx provider | Need app storage proof before adapting IDE-family importers. |
| `codebuddy` | `candidate-family` | `VS Code/Electron storage` | npx project or home `.codebuddy`; no ctx provider | Need app storage proof before adapting IDE-family importers. |
| `codemaker` | `unknown` | `unknown native history` | npx `~/.codemaker`; no ctx provider | Need native history storage research before claiming import support. |
| `codestudio` | `candidate-family` | `VS Code/Electron storage` | npx `~/.codestudio`; no ctx provider | Need app storage proof before adapting IDE-family importers. |
| `codex` | `native-auto` | `JSONL CLI event logs` | ctx `codex_session_jsonl_tree` and `codex_history_jsonl`; npx `CODEX_HOME` | - |
| `command-code` | `unknown` | `unknown native history` | npx `~/.commandcode`; no ctx provider | Need native history storage research before claiming import support. |
| `continue` | `native-auto` | `CLI session JSON` | ctx `continue_cli_sessions_json`; npx project or home `.continue` | - |
| `cortex` | `unknown` | `unknown native history` | npx `~/.snowflake/cortex`; no ctx provider | Need native history storage research before claiming import support. |
| `crush` | `native-auto` | `generic sqlite messages` | ctx `crush_sqlite`; npx `~/.config/crush` | - |
| `cursor` | `native-auto` | `VS Code/Electron storage` | ctx `cursor_agent_transcript_jsonl_tree`; npx `~/.cursor` | - |
| `deepagents` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.deepagents`; no ctx provider | No proven stable local transcript boundary; prefer exporter or plugin. |
| `devin` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.config/devin`; no ctx provider | Terminal skill target is not enough to prove local hosted-agent history. |
| `dexto` | `native-preview` | `generic sqlite messages` | ctx `dexto_sqlite`; npx `~/.dexto` | Preview explicit import only; no proven default discovery path yet. |
| `droid` | `native-auto` | `JSONL CLI event logs` | ctx `factory_ai_droid_sessions_jsonl`; npx `~/.factory` | - |
| `eve` | `unknown` | `unknown native history` | npx project `agent`; no ctx provider | Project skill layout does not prove a local history schema. |
| `firebender` | `candidate-family` | `VS Code/Electron storage` | npx `~/.firebender`; no ctx provider | Need local app storage or export contract proof. |
| `forgecode` | `candidate-family` | `JSONL CLI event logs` | npx `~/.forge`; no ctx provider | Need transcript location and schema proof before implementation. |
| `gemini-cli` | `native-auto` | `JSONL CLI event logs` | ctx `gemini_cli_chat_recording_jsonl`; npx `~/.gemini` | - |
| `github-copilot` | `native-auto` | `JSONL CLI event logs` | ctx `copilot_cli_session_events_jsonl`; npx `~/.copilot` | Coverage is for Copilot CLI session-state logs, not editor or web history. |
| `goose` | `native-auto` | `generic sqlite messages` | ctx `goose_sessions_sqlite`; npx `~/.config/goose` | - |
| `hermes-agent` | `native-auto` | `generic sqlite messages` | ctx `hermes_state_sqlite`; npx `HERMES_HOME` | - |
| `inference-sh` | `unknown` | `unknown native history` | npx `~/.inferencesh`; no ctx provider | Need native history storage research before claiming import support. |
| `iflow-cli` | `candidate-family` | `JSONL CLI event logs` | npx `~/.iflow`; no ctx provider | Need transcript location and schema proof. |
| `jazz` | `unknown` | `unknown native history` | npx project or home `.jazz`; no ctx provider | Need native history storage research before claiming import support. |
| `junie` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.junie`; no ctx provider | IDE-managed history boundary needs a verified local export or plugin. |
| `kilo` | `native-auto` | `opencode sqlite family` | ctx `kilo_sqlite`; npx `~/.kilocode` | - |
| `kimi-code-cli` | `native-auto` | `JSONL CLI event logs` | ctx `kimi_code_cli_wire_jsonl_tree`; npx `~/.kimi-code` or `~/.kimi` | - |
| `kiro-cli` | `candidate-family` | `JSONL CLI event logs` | npx `~/.kiro`; no ctx provider | Need transcript location and schema proof. |
| `kode` | `unknown` | `unknown native history` | npx `~/.kode`; no ctx provider | Need native history storage research before claiming import support. |
| `lingma` | `candidate-family` | `VS Code/Electron storage` | npx `~/.lingma`; no ctx provider | Need local app storage or export contract proof. |
| `loaf` | `unknown` | `unknown native history` | npx `~/.loaf`; no ctx provider | Need native history storage research before claiming import support. |
| `mcpjam` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.mcpjam`; no ctx provider | UI or account-backed activity should use exporter or plugin until local storage is proven. |
| `mistral-vibe` | `candidate-family` | `JSONL CLI event logs` | npx `VIBE_HOME`; no ctx provider | Need transcript location and schema proof. |
| `moxby` | `unknown` | `unknown native history` | npx `~/.moxby`; no ctx provider | Need native history storage research before claiming import support. |
| `mux` | `unknown` | `unknown native history` | npx `~/.mux`; no ctx provider | Need native history storage research before claiming import support. |
| `neovate` | `unknown` | `unknown native history` | npx `~/.neovate`; no ctx provider | Need native history storage research before claiming import support. |
| `opencode` | `native-auto` | `opencode sqlite family` | ctx `opencode_sqlite`; npx `~/.config/opencode` | - |
| `openhands` | `native-auto` | `filesystem event JSON` | ctx `openhands_file_events`; npx `~/.openhands` | - |
| `ona` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.ona`; no ctx provider | No proven stable local transcript boundary; prefer exporter or plugin. |
| `pi` | `native-auto` | `JSONL CLI event logs` | ctx `pi_session_jsonl`; npx `~/.pi/agent` | - |
| `qoder` | `candidate-family` | `VS Code/Electron storage` | npx `~/.qoder`; no ctx provider | Need local app storage or export contract proof. |
| `qoder-cn` | `candidate-family` | `VS Code/Electron storage` | npx `~/.qoder-cn`; no ctx provider | Need local app storage or export contract proof. |
| `qwen-code` | `native-auto` | `JSONL CLI event logs` | ctx `qwen_code_chat_jsonl_tree`; npx `~/.qwen` | - |
| `replit` | `webapp-boundary` | `webapp/object-store boundary` | npx project `.replit`; no ctx provider | Project marker is not a local agent history contract. |
| `reasonix` | `unknown` | `unknown native history` | npx `~/.reasonix`; no ctx provider | Need native history storage research before claiming import support. |
| `roo` | `native-auto` | `Cline/Roo task JSON` | ctx `roo_task_directory_json`; npx `~/.roo` | - |
| `rovodev` | `candidate-family` | `JSONL CLI event logs` | npx `~/.rovodev`; no ctx provider | Need transcript location and schema proof. |
| `tabnine-cli` | `candidate-family` | `JSONL CLI event logs` | npx `~/.tabnine`; no ctx provider | Need transcript location and schema proof. |
| `terramind` | `unknown` | `unknown native history` | npx `~/.terramind`; no ctx provider | Need native history storage research before claiming import support. |
| `tinycloud` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.tinycloud`; no ctx provider | No proven stable local transcript boundary; prefer exporter or plugin. |
| `trae` | `candidate-family` | `VS Code/Electron storage` | npx `~/.trae`; no ctx provider | Need local app storage or export contract proof. |
| `trae-cn` | `candidate-family` | `VS Code/Electron storage` | npx `~/.trae-cn`; no ctx provider | Need local app storage or export contract proof. |
| `warp` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.warp`; no ctx provider | Terminal app history may be account or app-store backed; needs explicit export proof. |
| `windsurf` | `candidate-family` | `VS Code/Electron storage` | npx `~/.codeium/windsurf`; no ctx provider | Need local app storage or export contract proof. |
| `zed` | `candidate-family` | `VS Code/Electron storage` | npx Zed config dirs; no ctx provider | Desktop IDE storage needs proof before reusing Cursor-style importers. |
| `zencoder` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.zencoder`; no ctx provider | No proven stable local transcript boundary; prefer exporter or plugin. |
| `zenflow` | `webapp-boundary` | `webapp/object-store boundary` | npx `~/.zencoder`; no ctx provider | Shares Zencoder skill home but no proven local history contract. |
| `pochi` | `candidate-family` | `VS Code/Electron storage` | npx `~/.pochi`; no ctx provider | Need local app storage or export contract proof. |
| `promptscript` | `install-target` | `agent skills aggregate` | npx project `.promptscript` or `promptscript.yaml`; no ctx provider | Project skill target only; use custom history JSONL if it emits runs. |
| `adal` | `unknown` | `unknown native history` | npx `~/.adal`; no ctx provider | Need native history storage research before claiming import support. |
| `universal` | `install-target` | `agent skills aggregate` | npx virtual `.agents/skills` target; no ctx provider | Aggregate skill install target, not a history-producing native provider. |

## ctx Native Providers Outside This npx Target Set

`nanoclaw` and `shelley` are native ctx providers on `origin/main`, but they do
not have matching `skills@1.5.14` `AgentType` ids. `nanoclaw` is preview-only;
`shelley` is native auto-importable with `shelley_sqlite`.
