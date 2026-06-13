use crate::protocol::CrpCommandInfo;

struct SlashCommandDef {
    name: &'static str,
    description: &'static str,
    argument_hint: Option<&'static str>,
    visible: fn() -> bool,
}

fn visible_always() -> bool {
    true
}

fn visible_copy() -> bool {
    !cfg!(target_os = "android")
}

fn visible_windows_only() -> bool {
    cfg!(target_os = "windows")
}

fn visible_debug_only() -> bool {
    cfg!(debug_assertions)
}

const BUILTIN_COMMANDS: &[SlashCommandDef] = &[
    SlashCommandDef {
        name: "model",
        description: "choose what model and reasoning effort to use",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "fast",
        description: "toggle Fast mode to enable fastest inference at 2X plan usage",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "approvals",
        description: "choose what Codex is allowed to do",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "permissions",
        description: "choose what Codex is allowed to do",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "setup-default-sandbox",
        description: "set up elevated agent sandbox",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "sandbox-add-read-dir",
        description: "let sandbox read a directory: /sandbox-add-read-dir <absolute_path>",
        argument_hint: Some("<absolute_path>"),
        visible: visible_windows_only,
    },
    SlashCommandDef {
        name: "experimental",
        description: "toggle experimental features",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "skills",
        description: "use skills to improve how Codex performs specific tasks",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "review",
        description: "review my current changes and find issues",
        argument_hint: Some("<instructions>"),
        visible: visible_always,
    },
    SlashCommandDef {
        name: "rename",
        description: "rename the current thread",
        argument_hint: Some("<title>"),
        visible: visible_always,
    },
    SlashCommandDef {
        name: "new",
        description: "start a new chat during a conversation",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "resume",
        description: "resume a saved chat",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "fork",
        description: "fork the current chat",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "init",
        description: "create an AGENTS.md file with instructions for Codex",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "compact",
        description: "summarize conversation to prevent hitting the context limit",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "plan",
        description: "switch to Plan mode",
        argument_hint: Some("<prompt>"),
        visible: visible_always,
    },
    SlashCommandDef {
        name: "collab",
        description: "change collaboration mode (experimental)",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "agent",
        description: "switch the active agent thread",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "diff",
        description: "show git diff (including untracked files)",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "copy",
        description: "copy the latest Codex output to your clipboard",
        argument_hint: None,
        visible: visible_copy,
    },
    SlashCommandDef {
        name: "mention",
        description: "mention a file",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "status",
        description: "show current session configuration and token usage",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "debug-config",
        description: "show config layers and requirement sources for debugging",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "statusline",
        description: "configure which items appear in the status line",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "theme",
        description: "choose a syntax highlighting theme",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "mcp",
        description: "list configured MCP tools",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "apps",
        description: "manage apps",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "logout",
        description: "log out of Codex",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "quit",
        description: "exit Codex",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "exit",
        description: "exit Codex",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "feedback",
        description: "send logs to maintainers",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "rollout",
        description: "print the rollout file path",
        argument_hint: None,
        visible: visible_debug_only,
    },
    SlashCommandDef {
        name: "ps",
        description: "list background terminals",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "clean",
        description: "stop all background terminals",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "clear",
        description: "clear the terminal and start a new chat",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "personality",
        description: "choose a communication style for Codex",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "realtime",
        description: "toggle realtime voice mode (experimental)",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "settings",
        description: "configure realtime microphone/speaker",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "test-approval",
        description: "test approval request",
        argument_hint: None,
        visible: visible_debug_only,
    },
    SlashCommandDef {
        name: "multi-agents",
        description: "switch the active agent thread",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "debug-m-drop",
        description: "DO NOT USE",
        argument_hint: None,
        visible: visible_always,
    },
    SlashCommandDef {
        name: "debug-m-update",
        description: "DO NOT USE",
        argument_hint: None,
        visible: visible_always,
    },
];

pub(super) fn build_builtin_command_infos() -> Vec<CrpCommandInfo> {
    BUILTIN_COMMANDS
        .iter()
        .filter(|command| (command.visible)())
        .map(|command| CrpCommandInfo {
            name: command.name.to_string(),
            description: Some(command.description.to_string()),
            argument_hint: command.argument_hint.map(str::to_string),
        })
        .collect()
}
