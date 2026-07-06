use ctx_history_core::CaptureProvider;

pub(super) fn empty_source_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => Some("path exists but no Codex JSONL sessions were found"),
        CaptureProvider::Pi => Some("path exists but no Pi session JSONL files were found"),
        CaptureProvider::Claude => {
            Some("path exists but no Claude project JSONL transcripts were found")
        }
        CaptureProvider::OpenCode => Some("path exists but no OpenCode SQLite database was found"),
        CaptureProvider::Kilo => Some("path exists but no Kilo SQLite database was found"),
        CaptureProvider::Crush => Some("path exists but no Crush SQLite database was found"),
        CaptureProvider::Goose => {
            Some("path exists but no Goose sessions SQLite database was found")
        }
        CaptureProvider::Antigravity => {
            Some("path exists but no Antigravity transcript JSONL files were found")
        }
        CaptureProvider::Gemini => Some(
            "path exists but no Gemini CLI chat JSONL transcripts were found under tmp/*/chats",
        ),
        CaptureProvider::Tabnine => Some(
            "path exists but no Tabnine CLI chat JSONL transcripts were found under tmp/*/chats",
        ),
        CaptureProvider::Cursor => {
            Some("path exists but no Cursor agent JSONL transcripts were found")
        }
        CaptureProvider::Zed => Some("path exists but no Zed threads SQLite database was found"),
        CaptureProvider::CopilotCli => {
            Some("path exists but no Copilot CLI session event JSONL files were found")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but no Factory AI Droid session JSONL files were found")
        }
        CaptureProvider::QwenCode => {
            Some("path exists but no Qwen Code chat JSONL files were found under projects/*/chats")
        }
        CaptureProvider::KimiCodeCli => {
            Some("path exists but no Kimi Code CLI agents/*/wire.jsonl files were found")
        }
        CaptureProvider::Auggie => {
            Some("path exists but no Auggie session JSON files with chatHistory were found")
        }
        CaptureProvider::Firebender => {
            Some("path exists but no Firebender chat_sessions table was found")
        }
        CaptureProvider::ForgeCode => {
            Some("path exists but no ForgeCode conversations table was found")
        }
        CaptureProvider::DeepAgents => {
            Some("path exists but no Deep Agents checkpoints/writes tables were found")
        }
        CaptureProvider::MistralVibe => {
            Some("path exists but no Mistral Vibe meta.json/messages.jsonl session directories were found")
        }
        CaptureProvider::Junie => {
            Some("path exists but no Junie index.jsonl entries with session events.jsonl files were found")
        }
        CaptureProvider::Mux => {
            Some("path exists but no Mux chat.jsonl or partial.json session files were found")
        }
        CaptureProvider::RovoDev => {
            Some("path exists but no Rovo Dev session_context.json files were found")
        }
        CaptureProvider::OpenClaw => {
            Some("path exists but no OpenClaw agent session JSONL files were found")
        }
        CaptureProvider::Hermes => Some("path exists but no Hermes state.db file was found"),
        CaptureProvider::NanoClaw => {
            Some("path exists but no NanoClaw data/v2.db and data/v2-sessions store was found")
        }
        CaptureProvider::AstrBot => Some("path exists but no AstrBot data/data_v4.db was found"),
        CaptureProvider::Shelley => Some("path exists but no Shelley SQLite database was found"),
        CaptureProvider::Continue => {
            Some("path exists but no Continue CLI session JSON files were found")
        }
        CaptureProvider::OpenHands => {
            Some("path exists but no OpenHands v1_conversations event JSON files were found")
        }
        CaptureProvider::Cline => Some("path exists but no Cline task JSON files were found"),
        CaptureProvider::RooCode => Some("path exists but no Roo Code task JSON files were found"),
        CaptureProvider::Lingma => {
            Some("path exists but no Lingma chat_record table with the expected columns was found")
        }
        CaptureProvider::Trae => {
            Some("path exists but no Trae workspace state.vscdb with known chat ItemTable keys was found")
        }
        CaptureProvider::Qoder => {
            Some("path exists but no Qoder transcript JSONL files were found")
        }
        CaptureProvider::Warp => Some("path exists but no Warp SQLite database was found"),
        CaptureProvider::CodeBuddy => {
            Some("path exists but no CodeBuddy history sessions were found")
        }
        _ => None,
    }
}

pub(super) fn unknown_source_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => {
            Some("path exists but the Codex session transcript probe hit its scan budget")
        }
        CaptureProvider::Pi => {
            Some("path exists but the Pi session transcript probe hit its scan budget")
        }
        CaptureProvider::Claude => {
            Some("path exists but the Claude transcript probe hit its scan budget")
        }
        CaptureProvider::Antigravity => {
            Some("path exists but the Antigravity transcript probe hit its scan budget")
        }
        CaptureProvider::Gemini => {
            Some("path exists but the Gemini transcript probe hit its scan budget")
        }
        CaptureProvider::Tabnine => {
            Some("path exists but the Tabnine transcript probe hit its scan budget")
        }
        CaptureProvider::Cursor => {
            Some("path exists but the Cursor transcript probe hit its scan budget")
        }
        CaptureProvider::Zed => None,
        CaptureProvider::CopilotCli => {
            Some("path exists but the Copilot CLI transcript probe hit its scan budget")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but the Factory AI Droid transcript probe hit its scan budget")
        }
        CaptureProvider::Continue => {
            Some("path exists but the Continue CLI session probe hit its scan budget")
        }
        CaptureProvider::OpenHands => {
            Some("path exists but the OpenHands event JSON probe hit its scan budget")
        }
        CaptureProvider::QwenCode => {
            Some("path exists but the Qwen Code chat transcript probe hit its scan budget")
        }
        CaptureProvider::KimiCodeCli => {
            Some("path exists but the Kimi Code CLI wire transcript probe hit its scan budget")
        }
        CaptureProvider::Auggie => {
            Some("path exists but the Auggie session JSON probe hit its scan budget")
        }
        CaptureProvider::Firebender => {
            Some("path exists but the Firebender database could not be fully probed")
        }
        CaptureProvider::MistralVibe => {
            Some("path exists but the Mistral Vibe session probe hit its scan budget")
        }
        CaptureProvider::Junie => {
            Some("path exists but the Junie session index probe hit its scan budget")
        }
        CaptureProvider::Mux => Some("path exists but the Mux session probe hit its scan budget"),
        CaptureProvider::RovoDev => {
            Some("path exists but the Rovo Dev session probe hit its scan budget")
        }
        CaptureProvider::OpenClaw => {
            Some("path exists but the OpenClaw transcript probe hit its scan budget")
        }
        CaptureProvider::Cline => {
            Some("path exists but the Cline task JSON probe hit its scan budget")
        }
        CaptureProvider::RooCode => {
            Some("path exists but the Roo Code task JSON probe hit its scan budget")
        }
        CaptureProvider::CodeBuddy => {
            Some("path exists but the CodeBuddy history probe hit its scan budget")
        }
        CaptureProvider::Trae => {
            Some("path exists but the Trae workspaceStorage probe hit its scan budget")
        }
        CaptureProvider::DeepAgents => {
            Some("path exists but the Deep Agents database could not be fully probed")
        }
        _ => None,
    }
}

pub(super) fn probe_io_error_reason(provider: CaptureProvider) -> Option<&'static str> {
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
