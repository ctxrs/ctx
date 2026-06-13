use anyhow::{anyhow, Result};
use ctx_core::provider_ids::CODEX_PROVIDER_ID;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CrpSlashCommand {
    Compact,
    Undo,
    Review { instructions: Option<String> },
}

pub(super) fn parse_crp_slash_command(content: &str) -> Option<CrpSlashCommand> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let matches_command = |name: &str| {
        if !trimmed.starts_with(name) {
            return false;
        }
        trimmed
            .chars()
            .nth(name.len())
            .map(|ch| ch.is_whitespace())
            .unwrap_or(true)
    };
    if matches_command("/compact") {
        return Some(CrpSlashCommand::Compact);
    }
    if matches_command("/undo") {
        return Some(CrpSlashCommand::Undo);
    }
    if matches_command("/review") {
        let rest = trimmed["/review".len()..].trim();
        let instructions = if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        };
        return Some(CrpSlashCommand::Review { instructions });
    }
    None
}

pub(super) fn parse_native_crp_slash_command_for_provider(
    provider_id: &str,
    content: &str,
) -> Option<CrpSlashCommand> {
    if provider_id != CODEX_PROVIDER_ID {
        return None;
    }
    parse_crp_slash_command(content)
}

#[derive(Debug, PartialEq, Eq)]
enum CodexSlashCommandPolicy {
    Supported,
    Redundant(&'static str),
    Unsupported(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
enum ClaudeSlashCommandPolicy {
    Supported,
    Redundant(&'static str),
    Unsupported(&'static str),
}

fn extract_slash_command_name(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let token = trimmed.split_whitespace().next()?;
    let normalized = token.trim_start_matches('/').trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(normalized)
}

fn classify_codex_slash_command(name: &str) -> CodexSlashCommandPolicy {
    if name.starts_with("prompts:") {
        return CodexSlashCommandPolicy::Unsupported(
            "Codex custom prompt slash commands are not wired through ctx today.",
        );
    }

    match name {
        "compact" | "review" => CodexSlashCommandPolicy::Supported,
        "approvals"
        | "clear"
        | "copy"
        | "diff"
        | "exit"
        | "mention"
        | "model"
        | "new"
        | "permissions"
        | "quit"
        | "resume"
        | "status" => CodexSlashCommandPolicy::Redundant(
            "ctx handles this workflow outside Codex slash commands.",
        ),
        "agent"
        | "apps"
        | "clean"
        | "collab"
        | "debug-config"
        | "debug-m-drop"
        | "debug-m-update"
        | "experimental"
        | "feedback"
        | "fork"
        | "init"
        | "logout"
        | "mcp"
        | "personality"
        | "plan"
        | "ps"
        | "realtime"
        | "rename"
        | "rollout"
        | "sandbox-add-read-dir"
        | "setup-default-sandbox"
        | "skills"
        | "statusline"
        | "test-approval"
        | "theme" => CodexSlashCommandPolicy::Unsupported(
            "Codex exposes this command in its TUI, but ctx cannot wire it through the CRP integration today.",
        ),
        _ => CodexSlashCommandPolicy::Supported,
    }
}

fn classify_claude_slash_command(name: &str) -> ClaudeSlashCommandPolicy {
    if name.starts_with("mcp__") {
        return ClaudeSlashCommandPolicy::Unsupported(
            "MCP prompt commands are intentionally out of scope in ctx right now.",
        );
    }

    match name {
        "allowed-tools"
        | "clear"
        | "config"
        | "continue"
        | "diff"
        | "exit"
        | "login"
        | "logout"
        | "model"
        | "new"
        | "permissions"
        | "quit"
        | "rename"
        | "reset"
        | "resume"
        | "sandbox"
        | "settings"
        | "status"
        | "tasks" => ClaudeSlashCommandPolicy::Redundant(
            "ctx handles this workflow outside Claude slash commands.",
        ),
        "add-dir"
        | "agents"
        | "android"
        | "app"
        | "checkpoint"
        | "chrome"
        | "copy"
        | "desktop"
        | "export"
        | "extra-usage"
        | "fork"
        | "hooks"
        | "ide"
        | "install-github-app"
        | "install-slack-app"
        | "ios"
        | "keybindings"
        | "mcp"
        | "mobile"
        | "passes"
        | "plugin"
        | "privacy-settings"
        | "rc"
        | "reload-plugins"
        | "remote-control"
        | "remote-env"
        | "rewind"
        | "statusline"
        | "stickers"
        | "terminal-setup"
        | "theme"
        | "upgrade"
        | "vim" => ClaudeSlashCommandPolicy::Unsupported(
            "Claude Code exposes this command in TUI/native integrations, but ctx cannot wire it through the Claude Agent SDK path today.",
        ),
        _ => ClaudeSlashCommandPolicy::Supported,
    }
}

pub(super) fn validate_provider_slash_command_support(
    provider_id: &str,
    content: &str,
) -> Result<()> {
    let Some(name) = extract_slash_command_name(content) else {
        return Ok(());
    };
    match provider_id {
        CODEX_PROVIDER_ID => match classify_codex_slash_command(&name) {
            CodexSlashCommandPolicy::Supported => Ok(()),
            CodexSlashCommandPolicy::Redundant(reason) => Err(anyhow!(
                "Codex command `/{name}` is intentionally not supported in ctx: {reason}"
            )),
            CodexSlashCommandPolicy::Unsupported(reason) => Err(anyhow!(
                "Codex command `/{name}` is not supported in ctx today: {reason}"
            )),
        },
        "claude-crp" => match classify_claude_slash_command(&name) {
            ClaudeSlashCommandPolicy::Supported => Ok(()),
            ClaudeSlashCommandPolicy::Redundant(reason) => Err(anyhow!(
                "Claude command `/{name}` is intentionally not supported in ctx: {reason}"
            )),
            ClaudeSlashCommandPolicy::Unsupported(reason) => Err(anyhow!(
                "Claude command `/{name}` is not supported in ctx today: {reason}"
            )),
        },
        _ => Ok(()),
    }
}

pub(super) fn extract_auth_url_from_stderr_line(line: &str) -> Option<String> {
    let lowered_line = line.to_ascii_lowercase();
    let mut search_from = 0usize;
    while search_from < line.len() {
        let haystack = &line[search_from..];
        let start_rel = haystack
            .find("https://")
            .or_else(|| haystack.find("http://"))?;
        let start = search_from + start_rel;
        let end = line[start..]
            .char_indices()
            .find_map(|(idx, ch)| {
                if ch.is_whitespace()
                    || matches!(ch, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']')
                {
                    Some(start + idx)
                } else {
                    None
                }
            })
            .unwrap_or(line.len());
        let candidate = line[start..end].trim_end_matches(['.', ',', ';', ':']);
        let lowered_candidate = candidate.to_ascii_lowercase();
        let looks_like_auth_prompt = [
            "auth required",
            "authentication required",
            "authenticate",
            "authentication",
            "oauth",
            "consent",
            "sign in",
            "sign-in",
            "signin",
            "log in",
            "login",
        ]
        .iter()
        .any(|needle| lowered_line.contains(needle))
            || [
                "auth.openai.com",
                "accounts.google.com/o/oauth2",
                "/oauth",
                "/authorize",
                "/login",
                "/signin",
            ]
            .iter()
            .any(|needle| lowered_candidate.contains(needle));
        if looks_like_auth_prompt
            && (candidate.starts_with("http://") || candidate.starts_with("https://"))
        {
            return Some(candidate.to_string());
        }
        search_from = end.saturating_add(1);
    }
    None
}

pub(super) fn extract_auth_error_from_stderr_line(line: &str) -> Option<String> {
    let lowered = line.to_ascii_lowercase();
    if (lowered.contains("refresh token") && lowered.contains("already used"))
        || lowered.contains("refresh_token_reused")
        || (lowered.contains("access token could not be refreshed")
            && lowered.contains("unauthorized"))
    {
        return Some(
            "Provider sign-in needs renewal. Please sign in again in ctx; the previous refresh token was rejected by the upstream OAuth server."
                .to_string(),
        );
    }
    if lowered.contains("another codex session is already using this signed-in account")
        || lowered.contains("ctx serializes codex oauth sessions")
    {
        return Some(
            "Codex signed-in account is already in use by another active session. Wait for that session to finish or switch to a separate Codex account."
                .to_string(),
        );
    }
    if lowered.contains("auggie does not currently support authenticating over acp")
        || lowered.contains("please run `auggie login` from your terminal then try again")
    {
        return Some(
            "Auggie does not currently support ACP authentication in this environment. Run `auggie login` via the fallback flow."
                .to_string(),
        );
    }
    if lowered.contains("interactive consent could not be obtained")
        || lowered.contains("please run gemini cli in an interactive terminal to authenticate")
    {
        return Some(
            "Gemini CLI could not obtain interactive OAuth consent in this environment."
                .to_string(),
        );
    }
    None
}

pub(super) fn extract_runtime_fatal_error_from_stderr_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.contains("level=fatal") {
        return Some(trimmed.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_auth_url_from_stderr_line_parses_google_url() {
        let line = "ERROR ... Raw: https://accounts.google.com/o/oauth2/v2/auth?client_id=abc";
        assert_eq!(
            extract_auth_url_from_stderr_line(line).as_deref(),
            Some("https://accounts.google.com/o/oauth2/v2/auth?client_id=abc")
        );
    }

    #[test]
    fn extract_auth_url_from_stderr_line_ignores_generic_startup_urls() {
        let line = "INFO docs: https://example.com/help/getting-started";
        assert_eq!(extract_auth_url_from_stderr_line(line), None);
    }

    #[test]
    fn extract_auth_error_from_stderr_line_detects_interactive_consent_failure() {
        let line = "message: 'Interactive consent could not be obtained.'";
        assert_eq!(
            extract_auth_error_from_stderr_line(line).as_deref(),
            Some("Gemini CLI could not obtain interactive OAuth consent in this environment.")
        );
    }

    #[test]
    fn extract_auth_error_from_stderr_line_detects_auggie_acp_auth_unsupported() {
        let line = "message: 'Authentication required: Auggie does not currently support authenticating over ACP. Please run `auggie login` from your terminal then try again.'";
        assert_eq!(
            extract_auth_error_from_stderr_line(line).as_deref(),
            Some(
                "Auggie does not currently support ACP authentication in this environment. Run `auggie login` via the fallback flow."
            )
        );
    }

    #[test]
    fn extract_auth_error_from_stderr_line_detects_codex_refresh_token_reuse() {
        let line = r#"Error: Your access token could not be refreshed because your refresh token was already used. Details: "unauthorized""#;
        assert_eq!(
            extract_auth_error_from_stderr_line(line).as_deref(),
            Some(
                "Provider sign-in needs renewal. Please sign in again in ctx; the previous refresh token was rejected by the upstream OAuth server."
            )
        );
    }

    #[test]
    fn extract_auth_error_from_stderr_line_detects_codex_oauth_lock_contention() {
        let line = "Error: Another Codex session is already using this signed-in account. ctx serializes Codex OAuth sessions to protect rotating refresh tokens.";
        assert_eq!(
            extract_auth_error_from_stderr_line(line).as_deref(),
            Some(
                "Codex signed-in account is already in use by another active session. Wait for that session to finish or switch to a separate Codex account."
            )
        );
    }

    #[test]
    fn extract_runtime_fatal_error_from_stderr_line_detects_structured_fatal_logs() {
        let line = "time=\"2026-04-02T22:10:57Z\" level=fatal msg=\"failed to create temp dir\"";
        assert_eq!(
            extract_runtime_fatal_error_from_stderr_line(line).as_deref(),
            Some(line)
        );
        assert_eq!(
            extract_runtime_fatal_error_from_stderr_line("warning: retrying"),
            None
        );
    }

    #[test]
    fn native_crp_slash_commands_are_codex_only() {
        assert_eq!(
            parse_native_crp_slash_command_for_provider("codex", "/compact"),
            Some(CrpSlashCommand::Compact)
        );
        assert_eq!(
            parse_native_crp_slash_command_for_provider("codex", "/review focus on security"),
            Some(CrpSlashCommand::Review {
                instructions: Some("focus on security".to_string())
            })
        );
        assert_eq!(
            parse_native_crp_slash_command_for_provider("claude-crp", "/compact"),
            None
        );
        assert_eq!(
            parse_native_crp_slash_command_for_provider("claude-crp", "/review focus on security"),
            None
        );
    }

    #[test]
    fn codex_command_policy_blocks_redundant_and_unsupported_commands() {
        assert_eq!(
            classify_codex_slash_command("compact"),
            CodexSlashCommandPolicy::Supported
        );
        assert_eq!(
            classify_codex_slash_command("status"),
            CodexSlashCommandPolicy::Redundant(
                "ctx handles this workflow outside Codex slash commands."
            )
        );
        assert_eq!(
            classify_codex_slash_command("prompts:shipit"),
            CodexSlashCommandPolicy::Unsupported(
                "Codex custom prompt slash commands are not wired through ctx today."
            )
        );
        assert!(validate_provider_slash_command_support("codex", "/compact").is_ok());
        assert!(validate_provider_slash_command_support("codex", "/undo").is_ok());
        assert!(validate_provider_slash_command_support("codex", "/status").is_err());
        assert!(validate_provider_slash_command_support("codex", "/prompts:shipit").is_err());
    }

    #[test]
    fn claude_command_policy_blocks_redundant_and_unsupported_commands() {
        assert_eq!(
            classify_claude_slash_command("compact"),
            ClaudeSlashCommandPolicy::Supported
        );
        assert_eq!(
            classify_claude_slash_command("clear"),
            ClaudeSlashCommandPolicy::Redundant(
                "ctx handles this workflow outside Claude slash commands."
            )
        );
        assert_eq!(
            classify_claude_slash_command("mcp__docs__search"),
            ClaudeSlashCommandPolicy::Unsupported(
                "MCP prompt commands are intentionally out of scope in ctx right now."
            )
        );
        assert!(validate_provider_slash_command_support("claude-crp", "/compact").is_ok());
        assert!(validate_provider_slash_command_support("claude-crp", "/clear").is_err());
        assert!(
            validate_provider_slash_command_support("claude-crp", "/mcp__docs__search").is_err()
        );
    }
}
