# npx skills Agent Storage Coverage

This ledger compares every `AgentType` in `skills@1.5.14` commit `2adcfe5a4cce0ce5f4d5547a997b2a161ec5d127` against ctx native local-history support on this branch. Upstream evidence comes from `src/types.ts` and `src/agents.ts`; ctx evidence comes from `docs/provider-support-matrix.json`, `crates/ctx-history-capture/src/provider_sources.rs`, and the native provider arguments in `crates/ctx-cli/src/main.rs`.

Support meanings:

- `supported`: ctx can import local history for this npx id from a proven provider-owned source format.
- `not-supported`: ctx does not claim local history ingestion for this npx id on this branch.

Result on this branch: 42 `supported` rows and 30 `not-supported` rows.

## Shared Families

Shared scanner/importer families include `JSONL CLI event logs`, `opencode sqlite family`, `generic sqlite messages`, `CLI session JSON`, `Cline/Roo task JSON`, `VS Code/Electron storage`, `filesystem event JSON`, `Forge conversation SQLite`, `LangGraph checkpoint SQLite`, `Warp restoration SQLite`, and `Junie event-sourced UI stream`.

## Coverage Ledger

| npx skills agent id | ctx support | schema family | evidence source | blocked reason / gap |
| --- | --- | --- | --- | --- |
| `aider-desk` | `not-supported` | `deliberately unsupported` | npx `~/.aider-desk`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `amp` | `not-supported` | `hosted/export boundary` | npx `~/.config/amp`; no ctx provider on this branch | Requires an explicit hosted/export import design; no default local transcript DB is claimed here. |
| `antigravity` | `supported` | `JSONL CLI event logs` | ctx `antigravity_cli_transcript_jsonl_tree`; proven transcript homes `~/.gemini/antigravity-cli` and `~/.gemini/antigravity-ide`; npx skills advertises `~/.gemini/antigravity` | ctx does not crawl the npx `~/.gemini/antigravity` skill home without separate transcript-storage proof. |
| `antigravity-cli` | `supported` | `JSONL CLI event logs` | ctx `antigravity_cli_transcript_jsonl_tree`; npx `~/.gemini/antigravity-cli` | - |
| `astrbot` | `supported` | `generic sqlite messages` | ctx `astrbot_data_v4_sqlite`; npx `~/.astrbot` | - |
| `autohand-code` | `not-supported` | `deliberately unsupported` | npx `AUTOHAND_HOME or ~/.autohand`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `augment` | `supported` | `CLI session JSON` | ctx `auggie_session_json`; npx `~/.augment` | - |
| `bob` | `not-supported` | `deliberately unsupported` | npx `~/.bob`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `claude-code` | `supported` | `JSONL CLI event logs` | ctx `claude_projects_jsonl_tree`; npx `~/.claude` | - |
| `openclaw` | `supported` | `JSONL CLI event logs` | ctx `openclaw_session_jsonl_tree`; npx `~/.openclaw` | - |
| `cline` | `supported` | `Cline/Roo task JSON` | ctx `cline_task_directory_json`; npx `~/.cline` | - |
| `codearts-agent` | `supported` | `opencode sqlite family` | ctx `codearts_agent_kernel_sqlite`; npx `~/.codeartsdoer` | - |
| `codebuddy` | `supported` | `VS Code/Electron storage` | ctx `codebuddy_history_json`; npx `.codebuddy or ~/.codebuddy` | - |
| `codemaker` | `not-supported` | `unknown native history` | npx `~/.codemaker`; no ctx provider on this branch | No stable local transcript store is proven for this public native-history branch. |
| `codestudio` | `not-supported` | `deliberately unsupported` | npx `~/.codestudio`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `codex` | `supported` | `JSONL CLI event logs` | ctx `codex_session_jsonl_tree`; npx `CODEX_HOME` | - |
| `command-code` | `not-supported` | `deliberately unsupported` | npx `~/.commandcode`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `continue` | `supported` | `CLI session JSON` | ctx `continue_cli_sessions_json`; npx `.continue or ~/.continue` | - |
| `cortex` | `not-supported` | `deliberately unsupported` | npx `~/.snowflake/cortex`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `crush` | `supported` | `generic sqlite messages` | ctx `crush_sqlite`; npx `~/.config/crush` | - |
| `cursor` | `supported` | `VS Code/Electron storage` | ctx `cursor_agent_transcript_jsonl_tree`; npx `~/.cursor` | - |
| `deepagents` | `supported` | `LangGraph checkpoint SQLite` | ctx `deepagents_sessions_sqlite`; npx `~/.deepagents` | - |
| `devin` | `not-supported` | `hosted/export boundary` | npx `~/.config/devin`; no ctx provider on this branch | Requires an explicit hosted/export import design; no default local transcript DB is claimed here. |
| `dexto` | `not-supported` | `deliberately unsupported` | npx `~/.dexto`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `droid` | `supported` | `JSONL CLI event logs` | ctx `factory_ai_droid_sessions_jsonl`; npx `~/.factory` | - |
| `eve` | `not-supported` | `deliberately unsupported` | npx `agent project marker`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `firebender` | `supported` | `generic sqlite messages` | ctx `firebender_chat_history_sqlite`; proven transcript DB `<project>/.idea/firebender/chat_history.db`; npx skills advertises `~/.firebender` | ctx does not claim a global `~/.firebender` transcript store; support is for the project-local JetBrains DB. |
| `forgecode` | `supported` | `Forge conversation SQLite` | ctx `forgecode_sqlite`; npx `FORGE_CONFIG or ~/.forge` | - |
| `gemini-cli` | `supported` | `JSONL CLI event logs` | ctx `gemini_cli_chat_recording_jsonl`; npx `~/.gemini` | - |
| `github-copilot` | `supported` | `JSONL CLI event logs` | ctx `copilot_cli_session_events_jsonl`; npx `~/.copilot` | - |
| `goose` | `supported` | `generic sqlite messages` | ctx `goose_sessions_sqlite`; npx `~/.config/goose` | - |
| `hermes-agent` | `supported` | `generic sqlite messages` | ctx `hermes_state_sqlite`; npx `HERMES_HOME` | - |
| `inference-sh` | `not-supported` | `unknown native history` | npx `~/.inferencesh`; no ctx provider on this branch | No stable local transcript store is proven for this public native-history branch. |
| `iflow-cli` | `not-supported` | `deliberately unsupported` | npx `IFLOW_HOME or ~/.iflow`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `jazz` | `not-supported` | `deliberately unsupported` | npx `JAZZ_HOME or ~/.jazz/history`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `junie` | `supported` | `Junie event-sourced UI stream` | ctx `junie_session_events_jsonl_tree`; npx `~/.junie` | - |
| `kilo` | `supported` | `opencode sqlite family` | ctx `kilo_sqlite`; npx `~/.kilocode` | - |
| `kimi-code-cli` | `supported` | `JSONL CLI event logs` | ctx `kimi_code_cli_wire_jsonl_tree`; npx `~/.kimi-code or ~/.kimi` | - |
| `kiro-cli` | `supported` | `generic sqlite messages` | ctx `kiro_cli_sqlite`; npx `~/.kiro` | - |
| `kode` | `not-supported` | `deliberately unsupported` | npx `~/.kode`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `lingma` | `supported` | `VS Code/Electron storage` | ctx `lingma_sqlite`; npx `~/.lingma` | - |
| `loaf` | `not-supported` | `deliberately unsupported` | npx `~/.loaf`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `mcpjam` | `not-supported` | `hosted/export boundary` | npx `~/.mcpjam`; no ctx provider on this branch | No stable local transcript store is proven for this public native-history branch. |
| `mistral-vibe` | `supported` | `JSONL CLI event logs` | ctx `mistral_vibe_session_jsonl_tree`; npx `VIBE_HOME or ~/.vibe` | - |
| `moxby` | `not-supported` | `deliberately unsupported` | npx `~/.moxby`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `mux` | `supported` | `JSONL CLI event logs` | ctx `mux_session_jsonl_tree`; npx `MUX_ROOT or ~/.mux` | - |
| `neovate` | `not-supported` | `deliberately unsupported` | npx `~/.neovate`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `opencode` | `supported` | `opencode sqlite family` | ctx `opencode_sqlite`; npx `~/.config/opencode` | - |
| `openhands` | `supported` | `filesystem event JSON` | ctx `openhands_file_events`; npx `~/.openhands` | - |
| `ona` | `not-supported` | `hosted/export boundary` | npx `~/.ona`; no ctx provider on this branch | No stable local transcript store is proven for this public native-history branch. |
| `pi` | `supported` | `JSONL CLI event logs` | ctx `pi_session_jsonl`; npx `~/.pi/agent` | - |
| `qoder` | `supported` | `JSONL CLI event logs` | ctx `qoder_transcript_jsonl_tree`; npx `~/.qoder` | - |
| `qoder-cn` | `not-supported` | `unknown native history` | npx `~/.qoder-cn`; no ctx provider on this branch | `qoder-cn` is accepted only as a CLI alias for Lingma's `~/.lingma` database; ctx does not claim a separate Qoder CN storage home. |
| `qwen-code` | `supported` | `JSONL CLI event logs` | ctx `qwen_code_chat_jsonl_tree`; npx `~/.qwen` | - |
| `replit` | `not-supported` | `hosted/export boundary` | npx `.replit`; no ctx provider on this branch | No stable local transcript store is proven for this public native-history branch. |
| `reasonix` | `not-supported` | `deliberately unsupported` | npx `~/.reasonix/sessions`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `roo` | `supported` | `Cline/Roo task JSON` | ctx `roo_task_directory_json`; npx `~/.roo` | - |
| `rovodev` | `supported` | `CLI session JSON` | ctx `rovodev_session_json_tree`; npx `~/.rovodev` | - |
| `tabnine-cli` | `supported` | `JSONL CLI event logs` | ctx `tabnine_cli_chat_recording_jsonl`; npx `~/.tabnine` | - |
| `terramind` | `not-supported` | `deliberately unsupported` | npx `terramind package config`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `tinycloud` | `not-supported` | `deliberately unsupported` | npx `~/.tinycloud`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `trae` | `supported` | `VS Code/Electron storage` | ctx `trae_state_vscdb`; npx `~/.trae` | - |
| `trae-cn` | `supported` | `VS Code/Electron storage` | ctx `trae_state_vscdb`; npx `~/.trae-cn` | - |
| `warp` | `supported` | `Warp restoration SQLite` | ctx `warp_sqlite`; npx `~/.warp` | - |
| `windsurf` | `supported` | `JSONL CLI event logs` | ctx `windsurf_cascade_hook_transcript_jsonl_tree`; npx `~/.codeium/windsurf` | - |
| `zed` | `supported` | `VS Code/Electron storage` | ctx `zed_threads_sqlite`; npx `$XDG_DATA_HOME/zed` | - |
| `zencoder` | `supported` | `VS Code/Electron storage` | ctx `zencoder_chat_sessions_json_tree`; npx `~/.zencoder` | - |
| `zenflow` | `not-supported` | `deliberately unsupported` | npx `~/.zencoder`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `pochi` | `not-supported` | `deliberately unsupported` | npx `~/.pochi`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `promptscript` | `not-supported` | `install-only target` | npx `.promptscript or promptscript.yaml`; no ctx provider on this branch | Deliberately unsupported here: this is an npx skills install target, not a history-producing coding-agent harness. |
| `adal` | `not-supported` | `deliberately unsupported` | npx `~/.adal`; no ctx provider on this branch | Deliberately unsupported on this public branch until demand and storage provenance justify native support. |
| `universal` | `not-supported` | `install-only target` | npx `.agents/skills`; no ctx provider on this branch | Deliberately unsupported here: this is an npx skills install target, not a history-producing coding-agent harness. |

## ctx Native Providers Outside This npx Target Set

`nanoclaw` and `shelley` are ctx providers on this branch, but they do not have matching `skills@1.5.14` `AgentType` ids. `nanoclaw` is explicit-import support; `shelley` is supported with `shelley_sqlite`.
