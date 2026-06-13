#[cfg(unix)]
#[path = "kill/unix.rs"]
mod platform;
#[cfg(unix)]
pub(in crate::daemon::workspace_runtime) use platform::kill_ctx_managed_sandbox_helper_processes;
#[cfg(not(unix))]
#[path = "kill/non_unix.rs"]
mod platform;
#[cfg(not(unix))]
pub(in crate::daemon::workspace_runtime) use platform::kill_ctx_managed_sandbox_helper_processes;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(in crate::daemon::workspace_runtime) struct SandboxHelperCleanupOutcome {
    pub(in crate::daemon::workspace_runtime) killed: Vec<u32>,
    pub(in crate::daemon::workspace_runtime) failed: Vec<u32>,
    pub(in crate::daemon::workspace_runtime) skipped: Vec<u32>,
}

pub(in crate::daemon::workspace_runtime) fn literal_pkill_pattern(command: &str) -> String {
    let mut pattern = String::with_capacity(command.len());
    for ch in command.chars() {
        if matches!(
            ch,
            '\\' | '.' | '[' | ']' | '(' | ')' | '{' | '}' | '^' | '$' | '*' | '+' | '?' | '|'
        ) {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern
}
