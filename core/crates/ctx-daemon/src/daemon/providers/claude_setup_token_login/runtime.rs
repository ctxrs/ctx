mod process;
mod shim;

pub(in crate::daemon::providers::claude_setup_token_login) use process::{
    spawn_claude_setup_token_command, ClaudeLoginSpawn,
};

#[cfg(test)]
pub(super) use shim::{
    claude_browser_open_shim_script, claude_login_should_skip_browser_open,
    CLAUDE_BROWSER_AUTH_TIER,
};
