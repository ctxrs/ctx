# Provider Support

Provider support is intentionally conservative. A provider is documented as
supported only when the public CLI can read existing local history for that
provider from a bounded source format.

Machine-readable provider metadata lives in
[`provider-support-matrix.json`](provider-support-matrix.json). The public truth
is:

| Provider | Support | Source format | Public smoke |
| --- | --- | --- | --- |
| Codex | Supported | `codex_session_jsonl_tree`, `codex_history_jsonl` | Public fixture smoke. |
| Pi | Supported | `pi_session_jsonl` | Public fixture smoke. |
| Claude | Supported | `claude_projects_jsonl_tree` | Public CLI coverage. |
| OpenCode | Supported | `opencode_sqlite` | Public CLI coverage. |
| Kilo Code | Supported | `kilo_sqlite` | Public fixture smoke. |
| Kiro CLI | Supported | `kiro_cli_sqlite` | Public fixture smoke. |
| Crush | Supported | `crush_sqlite` | Public fixture smoke. |
| Goose | Supported | `goose_sessions_sqlite` | Public fixture smoke. |
| Lingma | Supported | `lingma_sqlite` | Public fixture smoke. |
| Qoder | Supported | `qoder_transcript_jsonl_tree` | Public fixture smoke. |
| Warp | Supported | `warp_sqlite` | Public fixture smoke. |
| CodeBuddy | Supported | `codebuddy_history_json` | Public fixture smoke. |
| Trae | Supported | `trae_state_vscdb` | Public fixture smoke. |
| OpenClaw | Supported | `openclaw_session_jsonl_tree` | Public CLI coverage. |
| Hermes Agent | Supported | `hermes_state_sqlite` | Public CLI coverage. |
| NanoClaw | Supported | `nanoclaw_project` | Public CLI coverage. |
| AstrBot | Supported | `astrbot_data_v4_sqlite` | Public fixture smoke. |
| Shelley | Supported | `shelley_sqlite` | Public CLI coverage. |
| Continue | Supported | `continue_cli_sessions_json` | Public CLI coverage. |
| OpenHands | Supported | `openhands_file_events` | Public CLI coverage. |
| Antigravity | Supported | `antigravity_cli_transcript_jsonl_tree` | Public fixture smoke. |
| Gemini | Supported | `gemini_cli_chat_recording_jsonl` | Public CLI coverage. |
| Tabnine | Supported | `tabnine_cli_chat_recording_jsonl` | Public fixture smoke. |
| Cursor | Supported | `cursor_agent_transcript_jsonl_tree` | Public fixture smoke. |
| Windsurf | Supported | `windsurf_cascade_hook_transcript_jsonl_tree` | Public fixture smoke. |
| Zed | Supported | `zed_threads_sqlite` | Public fixture smoke. |
| Copilot CLI | Supported | `copilot_cli_session_events_jsonl` | Public CLI coverage. |
| Factory AI Droid | Supported | `factory_ai_droid_sessions_jsonl` | Public CLI coverage. |
| Qwen Code | Supported | `qwen_code_chat_jsonl_tree` | Public fixture smoke. |
| Kimi Code CLI | Supported | `kimi_code_cli_wire_jsonl_tree` | Public fixture smoke. |
| Auggie | Supported | `auggie_session_json` | Public fixture smoke. |
| Junie | Supported | `junie_session_events_jsonl_tree` | Public fixture smoke. |
| Firebender | Supported | `firebender_chat_history_sqlite` | Public fixture smoke. |
| ForgeCode | Supported | `forgecode_sqlite` | Public fixture smoke. |
| Deep Agents | Supported | `deepagents_sessions_sqlite` | Public fixture smoke. |
| Mistral Vibe | Supported | `mistral_vibe_session_jsonl_tree` | Public fixture smoke. |
| Mux | Supported | `mux_session_jsonl_tree` | Public fixture smoke. |
| Rovo Dev | Supported | `rovodev_session_json_tree` | Public fixture smoke. |
| Cline | Supported | `cline_task_directory_json` | Public fixture smoke. |
| Roo Code | Supported | `roo_task_directory_json` | Public fixture smoke. |

`ctx sources --json` reports each known provider source with `import_support`
and `importable` fields. A source is importable only when provider-specific
transcript files exist and match the documented format. NanoClaw remains
explicit-import only; it is not included in `ctx import --all` or pre-search
refresh.

## Provider Smoke

Provider smoke coverage uses public fixture data and generated local-history
trees. It verifies supported imports, provider filtering, citations, and
deterministic search without executing provider CLIs, reading real user history,
requiring API keys, or making network calls.

## Required Evidence For Promotion

Before a provider is documented as supported, the change needs a documented
local source format, bounded discovery paths, static fixture coverage, CLI
coverage, and a public matrix row.
