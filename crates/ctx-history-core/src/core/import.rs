#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum CaptureSourceKind {
        ProviderImport => "provider_import",
        ProviderHook => "provider_hook",
        DirectCli => "direct_cli",
        Manual => "manual",
    }
    default Manual
}

text_enum! {
    pub enum RunType {
        AgentTurn => "agent_turn",
        Command => "command",
        ToolCall => "tool_call",
        Review => "review",
        Import => "import",
        Summary => "summary",
    }
    default Command
}
