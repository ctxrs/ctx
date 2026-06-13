use super::*;

mod helpers;
mod http;
mod intent;
mod local;
mod ssh;

pub(super) use helpers::*;

#[test]
fn demo_commands_enabled_respects_env_flag() {
    let _guard = EnvVarGuard::set("CTX_DESKTOP_ALLOW_DEMO_COMMANDS", "1");
    assert!(demo_commands_enabled());
}
