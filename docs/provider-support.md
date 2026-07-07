# Provider Support

Provider support is intentionally conservative. A provider is documented as
supported only when the public CLI can read existing local history for that
provider from a bounded source format.

The provider import policy in
[`provider-import-policy.md`](provider-import-policy.md) defines the native
storage families and the rules for real conversation text, tool output, raw
diffs, oversized rows, and fixtures.

Machine-readable provider metadata lives in
[`provider-support-matrix.json`](provider-support-matrix.json). The public
matrix's `tool_output` and `command_output` fidelity flags describe structured
metadata and failed-output diagnostic support; successful stdout/stderr and raw
tool result bodies are still excluded by the provider import policy. The public
source formats below identify discovery/import source shapes; stored event
metadata may use the corresponding per-file adapter format, such as
`codex_session_jsonl` for files discovered under `codex_session_jsonl_tree`.
The public
support matrix is:

| Provider | Support | Source format |
| --- | --- | --- |
| Codex | Supported | `codex_session_jsonl_tree`, `codex_history_jsonl` |
| Pi | Supported | `pi_session_jsonl` |
| Claude | Supported | `claude_projects_jsonl_tree` |
| OpenCode | Supported | `opencode_sqlite` |
| Kilo Code | Supported | `kilo_sqlite` |
| Kiro CLI | Supported | `kiro_cli_sqlite` |
| Crush | Supported | `crush_sqlite` |
| Goose | Supported | `goose_sessions_sqlite` |
| Lingma | Supported | `lingma_sqlite` |
| Qoder | Supported | `qoder_transcript_jsonl_tree` |
| Warp | Supported | `warp_sqlite` |
| CodeBuddy | Supported | `codebuddy_history_json` |
| Trae | Supported | `trae_state_vscdb` |
| OpenClaw | Supported | `openclaw_session_jsonl_tree` |
| Hermes Agent | Supported | `hermes_state_sqlite` |
| NanoClaw | Supported | `nanoclaw_project` |
| AstrBot | Supported | `astrbot_data_v4_sqlite` |
| Shelley | Supported | `shelley_sqlite` |
| Continue | Supported | `continue_cli_sessions_json` |
| OpenHands | Supported | `openhands_file_events` |
| Antigravity | Supported | `antigravity_cli_transcript_jsonl_tree` |
| Gemini | Supported | `gemini_cli_chat_recording_jsonl` |
| Tabnine | Supported | `tabnine_cli_chat_recording_jsonl` |
| Cursor | Supported | `cursor_agent_transcript_jsonl_tree` |
| Windsurf | Supported | `windsurf_cascade_hook_transcript_jsonl_tree` |
| Zed | Supported | `zed_threads_sqlite` |
| Copilot CLI | Supported | `copilot_cli_session_events_jsonl` |
| Factory AI Droid | Supported | `factory_ai_droid_sessions_jsonl` |
| Qwen Code | Supported | `qwen_code_chat_jsonl_tree` |
| Kimi Code CLI | Supported | `kimi_code_cli_wire_jsonl_tree` |
| Auggie | Supported | `auggie_session_json` |
| Junie | Supported | `junie_session_events_jsonl_tree` |
| Firebender | Supported | `firebender_chat_history_sqlite` |
| ForgeCode | Supported | `forgecode_sqlite` |
| Deep Agents | Supported | `deepagents_sessions_sqlite` |
| Mistral Vibe | Supported | `mistral_vibe_session_jsonl_tree` |
| Mux | Supported | `mux_session_jsonl_tree` |
| Rovo Dev | Supported | `rovodev_session_json_tree` |
| Cline | Supported | `cline_task_directory_json` |
| Roo Code | Supported | `roo_task_directory_json` |

`ctx sources --json` reports each known provider source with `import_support`
and `importable` fields. A source is importable only when provider-specific
transcript files exist and match the documented format. NanoClaw remains
explicit-import only; it is not included in `ctx import --all` or pre-search
refresh.

## Local Checks

Local checks exercise supported imports, provider filtering, citations, and
deterministic search without executing provider CLIs, reading real user history,
requiring API keys, or making network calls.
