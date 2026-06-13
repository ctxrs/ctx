pub const FULL_YOLO_SANDBOX_MODE: &str = "danger-full-access";
pub const FULL_YOLO_APPROVAL_POLICY: &str = "never";
pub const CTX_CRP_LAUNCH_POLICY_ENV: &str = "CTX_CRP_LAUNCH_POLICY";
pub const CTX_CRP_LAUNCH_POLICY_FULL: &str = "full";
pub const CODEX_APP_SERVER_ARGS: [&str; 5] = [
    "-s",
    FULL_YOLO_SANDBOX_MODE,
    "-a",
    FULL_YOLO_APPROVAL_POLICY,
    "app-server",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_app_server_args_match_full_yolo_policy() {
        assert_eq!(CODEX_APP_SERVER_ARGS[1], FULL_YOLO_SANDBOX_MODE);
        assert_eq!(CODEX_APP_SERVER_ARGS[3], FULL_YOLO_APPROVAL_POLICY);
    }
}
