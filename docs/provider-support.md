# Provider Support

Provider support is intentionally conservative. A provider is documented as locally importable only when the public CLI can read existing local history for that provider.

## Status Meanings

| Status | Meaning |
| --- | --- |
| `local_import` | The CLI can import an existing local history source for this provider. |
| `local_import_when_supported` | The CLI has an importer for a specific local format, but support depends on that file existing and matching the documented format. |
| `fixture_only` | The repository has sanitized fixture coverage, but the public CLI does not discover or import native local history for that provider. |
| `detected_unsupported` | The CLI can detect something about the provider but intentionally does not import it. |
| `blocked` | No shipped discovery or import path exists. |

## Current Matrix

Machine-readable provider metadata lives in [provider-support-matrix.json](provider-support-matrix.json). The public truth is:

| Provider | Status | Source format | Public smoke |
| --- | --- | --- | --- |
| Codex | `local_import` | `codex_session_jsonl_tree`, `codex_history_jsonl` | Public fixture smoke. |
| Pi | `local_import_when_supported` | `pi_session_jsonl` | Public fixture smoke. |
| Claude | `local_import_when_supported` | `claude_projects_jsonl_tree` | Public CLI coverage. |
| OpenCode | `local_import_when_supported` | `opencode_sqlite` | Public CLI coverage. |
| Kilo Code | `local_import_when_supported` | `kilo_sqlite` | Public fixture smoke. |
| Kiro CLI | `local_import_when_supported` | `kiro_cli_sqlite` | Public fixture smoke. |
| Crush | `local_import_when_supported` | `crush_sqlite` | Public fixture smoke. |
| Goose | `local_import_when_supported` | `goose_sessions_sqlite` | Public fixture smoke. |
| Lingma | `local_import_when_supported` | `lingma_sqlite` | Public fixture smoke. |
| Qoder | `local_import_when_supported` | `qoder_transcript_jsonl_tree` | Public fixture smoke. |
| Warp | `local_import_when_supported` | `warp_sqlite` | Public fixture smoke. |
| CodeBuddy | `local_import_when_supported` | `codebuddy_history_json` | Public fixture smoke. |
| CodeArts Agent | `local_import_when_supported` | `codearts_agent_kernel_sqlite` | Public fixture smoke. |
| Zencoder | `local_import_when_supported` | `zencoder_chat_sessions_json_tree` | Public fixture smoke. |
| Trae | `local_import_when_supported` | `trae_state_vscdb` | Public fixture smoke. |
| OpenClaw | `local_import_when_supported` | `openclaw_session_jsonl_tree` | Public CLI coverage. |
| Hermes Agent | `local_import_when_supported` | `hermes_state_sqlite` | Public CLI coverage. |
| NanoClaw | `local_import_when_supported` | `nanoclaw_project` | Public CLI coverage. |
| AstrBot | `local_import_when_supported` | `astrbot_data_v4_sqlite` | Public fixture smoke. |
| Shelley | `local_import_when_supported` | `shelley_sqlite` | Public CLI coverage. |
| Continue | `local_import_when_supported` | `continue_cli_sessions_json` | Public CLI coverage. |
| OpenHands | `local_import_when_supported` | `openhands_file_events` | Public CLI coverage. |
| Antigravity | `local_import_when_supported` | `antigravity_cli_transcript_jsonl_tree` | Public fixture smoke. |
| Gemini | `local_import_when_supported` | `gemini_cli_chat_recording_jsonl` | Public CLI coverage. |
| Tabnine | `local_import_when_supported` | `tabnine_cli_chat_recording_jsonl` | Public fixture smoke. |
| Cursor | `local_import_when_supported` | `cursor_agent_transcript_jsonl_tree` | Public fixture smoke. |
| Windsurf | `local_import_when_supported` | `windsurf_cascade_hook_transcript_jsonl_tree` | Public fixture smoke. |
| Zed | `local_import_when_supported` | `zed_threads_sqlite` | Public fixture smoke. |
| Copilot CLI | `local_import_when_supported` | `copilot_cli_session_events_jsonl` | Public CLI coverage. |
| Factory AI Droid | `local_import_when_supported` | `factory_ai_droid_sessions_jsonl` | Public CLI coverage. |
| Qwen Code | `local_import_when_supported` | `qwen_code_chat_jsonl_tree` | Public fixture smoke. |
| Kimi Code CLI | `local_import_when_supported` | `kimi_code_cli_wire_jsonl_tree` | Public fixture smoke. |
| Auggie | `local_import_when_supported` | `auggie_session_json` | Public fixture smoke. |
| Junie | `local_import_when_supported` | `junie_session_events_jsonl_tree` | Public fixture smoke. |
| Firebender | `local_import_when_supported` | `firebender_chat_history_sqlite` | Public fixture smoke. |
| ForgeCode | `local_import_when_supported` | `forgecode_sqlite` | Public fixture smoke. |
| Deep Agents | `local_import_when_supported` | `deepagents_sessions_sqlite` | Public fixture smoke. |
| Mistral Vibe | `local_import_when_supported` | `mistral_vibe_session_jsonl_tree` | Public fixture smoke. |
| Mux | `local_import_when_supported` | `mux_session_jsonl_tree` | Public fixture smoke. |
| Rovo Dev | `local_import_when_supported` | `rovodev_session_json_tree` | Public fixture smoke. |
| Cline | `local_import_when_supported` | `cline_task_directory_json` | Public fixture smoke. |
| Roo Code | `local_import_when_supported` | `roo_task_directory_json` | Public fixture smoke. |

`ctx sources --json` reports each known provider source with `import_support` and `importable` fields. A source is importable only when provider-specific transcript files exist and match the documented format. Preview/manual paths such as NanoClaw remain explicit-import only until promoted.

## Provider Smoke

Provider smoke coverage uses public fixture data and generated local-history trees. It verifies supported imports, provider filtering, citations, and deterministic search without executing provider CLIs, reading real user history, requiring API keys, or making network calls.

## Required Evidence For Promotion

Before a provider moves into native local-history support, the change needs a documented local source format, bounded discovery paths, static fixture coverage, CLI coverage, and a public matrix row.
