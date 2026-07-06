#[allow(unused_imports)]
use super::*;

pub(crate) fn probe_io_error_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => {
            Some("path exists but Codex session transcripts could not be read; check permissions")
        }
        CaptureProvider::Pi => {
            Some("path exists but Pi session transcripts could not be read; check permissions")
        }
        CaptureProvider::Claude => {
            Some("path exists but Claude project transcripts could not be read; check permissions")
        }
        CaptureProvider::OpenCode => {
            Some("path exists but the OpenCode database could not be read; check permissions")
        }
        CaptureProvider::Kilo => {
            Some("path exists but the Kilo database could not be read; check permissions")
        }
        CaptureProvider::KiroCli => {
            Some("path exists but the Kiro CLI database could not be read; check permissions")
        }
        CaptureProvider::Crush => {
            Some("path exists but the Crush database could not be read; check permissions")
        }
        CaptureProvider::Goose => {
            Some("path exists but the Goose sessions database could not be read; check permissions")
        }
        CaptureProvider::Antigravity => {
            Some("path exists but Antigravity transcripts could not be read; check permissions")
        }
        CaptureProvider::Gemini => {
            Some("path exists but Gemini CLI chat transcripts could not be read; check permissions")
        }
        CaptureProvider::Tabnine => {
            Some("path exists but Tabnine CLI chat transcripts could not be read; check permissions")
        }
        CaptureProvider::Cursor => {
            Some("path exists but Cursor agent transcripts could not be read; check permissions")
        }
        CaptureProvider::Zed => {
            Some("path exists but the Zed threads database could not be read; check permissions")
        }
        CaptureProvider::CopilotCli => {
            Some("path exists but Copilot CLI session events could not be read; check permissions")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but Factory AI Droid sessions could not be read; check permissions")
        }
        CaptureProvider::QwenCode => {
            Some("path exists but Qwen Code chat transcripts could not be read; check permissions")
        }
        CaptureProvider::KimiCodeCli => Some(
            "path exists but Kimi Code CLI wire transcripts could not be read; check permissions",
        ),
        CaptureProvider::Auggie => {
            Some("path exists but Auggie session JSON files could not be read; check permissions")
        }
        CaptureProvider::Junie => {
            Some("path exists but Junie session files could not be read; check permissions")
        }
        CaptureProvider::Firebender => {
            Some("path exists but the Firebender chat history database could not be read; check permissions")
        }
        CaptureProvider::ForgeCode => {
            Some("path exists but the ForgeCode database could not be read; check permissions")
        }
        CaptureProvider::DeepAgents => {
            Some("path exists but the Deep Agents database could not be read; check permissions")
        }
        CaptureProvider::MistralVibe => {
            Some("path exists but Mistral Vibe session files could not be read; check permissions")
        }
        CaptureProvider::Mux => {
            Some("path exists but Mux session files could not be read; check permissions")
        }
        CaptureProvider::RovoDev => {
            Some("path exists but Rovo Dev session files could not be read; check permissions")
        }
        CaptureProvider::OpenClaw => Some(
            "path exists but OpenClaw session transcripts could not be read; check permissions",
        ),
        CaptureProvider::Hermes => {
            Some("path exists but the Hermes state database could not be read; check permissions")
        }
        CaptureProvider::NanoClaw => {
            Some("path exists but the NanoClaw project store could not be read; check permissions")
        }
        CaptureProvider::AstrBot => {
            Some("path exists but the AstrBot data database could not be read; check permissions")
        }
        CaptureProvider::Shelley => {
            Some("path exists but the Shelley database could not be read; check permissions")
        }
        CaptureProvider::Continue => {
            Some("path exists but Continue CLI sessions could not be read; check permissions")
        }
        CaptureProvider::OpenHands => {
            Some("path exists but OpenHands event JSON files could not be read; check permissions")
        }
        CaptureProvider::Cline => {
            Some("path exists but Cline task JSON files could not be read; check permissions")
        }
        CaptureProvider::RooCode => {
            Some("path exists but Roo Code task JSON files could not be read; check permissions")
        }
        CaptureProvider::Lingma => {
            Some("path exists but the Lingma chat_record SQLite database could not be read")
        }
        CaptureProvider::Trae => {
            Some("path exists but Trae workspace state.vscdb files could not be read")
        }
        CaptureProvider::Qoder => {
            Some("path exists but Qoder transcript JSONL files could not be read; check permissions")
        }
        CaptureProvider::CodeBuddy => Some(
            "path exists but CodeBuddy history JSON files could not be read; check permissions",
        ),
        _ => None,
    }
}

pub(crate) fn default_location_import_probe(
    provider: CaptureProvider,
    location: &ProviderDefaultLocation,
    path: &Path,
) -> BoundedProbe {
    match provider {
        CaptureProvider::Codex if location.source_format == "codex_history_jsonl" => {
            path_is_file_probe(path)
        }
        CaptureProvider::Codex => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::Pi => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenCode => path_is_file_probe(path),
        CaptureProvider::Kilo => path_is_file_probe(path),
        CaptureProvider::KiroCli => path_is_file_probe(path),
        CaptureProvider::Crush => path_is_file_probe(path),
        CaptureProvider::Goose => path_is_file_probe(path),
        CaptureProvider::Claude => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenClaw => has_openclaw_session_jsonl(path, 10_000),
        CaptureProvider::Hermes => path_is_file_probe(path),
        CaptureProvider::NanoClaw => has_nanoclaw_project(path),
        CaptureProvider::AstrBot => path_is_file_probe(path),
        CaptureProvider::Shelley => path_is_file_probe(path),
        CaptureProvider::Continue => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
        }),
        CaptureProvider::OpenHands => has_openhands_event_json(path, 10_000),
        CaptureProvider::Antigravity => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            matches!(
                candidate.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => has_gemini_chat_jsonl(path, 10_000),
        CaptureProvider::Cursor => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "agent-transcripts")
        }),
        CaptureProvider::Windsurf => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::Qoder => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "transcript")
        }),
        CaptureProvider::Zed => path_is_file_probe(path),
        CaptureProvider::CopilotCli => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }),
        CaptureProvider::FactoryAiDroid => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::QwenCode => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "chats")
        }),
        CaptureProvider::KimiCodeCli => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path_has_component(candidate, "agents")
        }),
        CaptureProvider::Auggie => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.extension().and_then(|ext| ext.to_str()) == Some("json")
        }),
        CaptureProvider::Junie => has_junie_session_events(path, 10_000),
        CaptureProvider::Firebender => has_firebender_chat_sessions_table(path),
        CaptureProvider::ForgeCode => has_forgecode_conversations_table(path),
        CaptureProvider::DeepAgents => has_deepagents_checkpoint_tables(path),
        CaptureProvider::MistralVibe => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                && candidate
                    .parent()
                    .is_some_and(|parent| parent.join("meta.json").is_file())
        }),
        CaptureProvider::Mux => has_mux_session_files(path, 10_000),
        CaptureProvider::RovoDev => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("session_context.json")
        }),
        CaptureProvider::Cline => has_task_json_file_under_matching(path, 10_000, |name| {
            matches!(
                name,
                "api_conversation_history.json"
                    | "ui_messages.json"
                    | "context_history.json"
                    | "task_metadata.json"
            )
        }),
        CaptureProvider::RooCode => has_task_json_file_under_matching(path, 10_000, |name| {
            matches!(
                name,
                "api_conversation_history.json"
                    | "ui_messages.json"
                    | "history_item.json"
                    | "_index.json"
                    | "claude_messages.json"
            )
        }),
        CaptureProvider::Lingma => has_lingma_chat_record_table(path),
        CaptureProvider::Trae => has_trae_state_vscdb_chat_history(path, 10_000),
        CaptureProvider::Warp => path_is_file_probe(path),
        CaptureProvider::CodeBuddy => has_codebuddy_history_json(path, 10_000),
        CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => BoundedProbe::NotFound,
    }
}
