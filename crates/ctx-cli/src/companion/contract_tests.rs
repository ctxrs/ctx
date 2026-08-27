// Contract tests for the Core-to-companion routing boundary.
use super::*;

const ANALYTICS_ENVIRONMENT_NAMES: &[&str] = &[
    "CTX_ANALYTICS_ENABLED",
    "CTX_ANALYTICS_ENDPOINT",
    "CTX_ANALYTICS_OFF",
    "CTX_DISABLE_ANALYTICS",
    "CTX_INSTALL_DIAGNOSTICS_OFF",
];

struct AnalyticsEnvironment {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl AnalyticsEnvironment {
    fn new() -> Self {
        let lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = ANALYTICS_ENVIRONMENT_NAMES
            .iter()
            .map(|&name| {
                let value = std::env::var_os(name);
                std::env::remove_var(name);
                (name, value)
            })
            .collect();
        Self { _lock: lock, saved }
    }

    fn set(&self, name: &'static str, value: &str) {
        std::env::set_var(name, value);
    }

    fn set_os(&self, name: &'static str, value: &OsStr) {
        std::env::set_var(name, value);
    }

    fn remove(&self, name: &'static str) {
        std::env::remove_var(name);
    }
}

impl Drop for AnalyticsEnvironment {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}

fn paid_cli_environment(forwards_core_setup: bool) -> CompanionEnvironment {
    let mut environment = CompanionEnvironment::new();
    forward_environment(&mut environment);
    if forwards_core_setup {
        forward_supervisor_environment(&mut environment);
    }
    forward_paid_cli_analytics_override(&mut environment);
    forward_terminal_environment(&mut environment);
    environment
}

#[test]
fn paid_gate_forwards_the_original_arguments_without_paid_parsing() {
    let arguments = [
        OsString::from("ctx"),
        OsString::from("--data-root"),
        OsString::from("opaque-root"),
        OsString::from("blame"),
        OsString::from("--private-option"),
        OsString::from("opaque-value"),
    ];
    assert_eq!(
        paid_family_arguments(&arguments),
        Some(arguments[1..].to_vec())
    );
}

#[test]
fn core_routes_never_enter_the_companion_gate() {
    for family in [
        "setup",
        "status",
        "doctor",
        "upgrade",
        "uninstall",
        "search",
        "show",
        "mcp",
    ] {
        let arguments = [OsString::from("ctx"), OsString::from(family)];
        assert!(paid_family_arguments(&arguments).is_none(), "{family}");
    }
}

#[test]
fn explicit_pro_selector_routes_setup_and_other_core_families() {
    for arguments in [
        vec!["ctx", "--pro", "setup"],
        vec!["ctx", "setup", "--pro"],
        vec!["ctx", "--pro", "status"],
        vec!["ctx", "--pro", "--help"],
        vec!["ctx", "help", "setup", "--pro"],
    ] {
        let arguments = arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec())
        );
    }
    assert!(paid_family_arguments(&[
        OsString::from("ctx"),
        OsString::from("setup"),
        OsString::from("--"),
        OsString::from("--pro"),
    ])
    .is_none());
}

#[test]
fn forwarded_environment_is_the_complete_fixed_allowlist() {
    assert!(
        FORWARDED_ENVIRONMENT.len() + FORWARDED_TERMINAL_ENVIRONMENT.len()
            < MAX_ENVIRONMENT_ENTRIES
    );
    assert!(FORWARDED_ENVIRONMENT
        .contains(&(EnvironmentKey::LocalUsageEnabled, "CTX_LOCAL_USAGE_ENABLED")));
    assert!(!FORWARDED_ENVIRONMENT
        .iter()
        .any(|(key, _)| *key == EnvironmentKey::AnalyticsEnabled));
    assert!(FORWARDED_ENVIRONMENT.contains(&(
        EnvironmentKey::HostedInstallerSetup,
        "CTX_HOSTED_INSTALLER_SETUP"
    )));
    assert!(FORWARDED_ENVIRONMENT.contains(&(EnvironmentKey::Home, "HOME")));
    assert!(FORWARDED_ENVIRONMENT.contains(&(EnvironmentKey::Path, "PATH")));
    assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::Term, "TERM")));
    assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::ColorTerm, "COLORTERM")));
    assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::NoColor, "NO_COLOR")));
    assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::CliColor, "CLICOLOR")));
    assert!(
        FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::CliColorForce, "CLICOLOR_FORCE"))
    );
    assert!(FORWARDED_TERMINAL_ENVIRONMENT.contains(&(EnvironmentKey::Ci, "CI")));
    assert!(environment_value_is_forwardable(
        EnvironmentKey::Home,
        OsStr::new("/home/tester")
    ));
    assert!(!environment_value_is_forwardable(
        EnvironmentKey::Home,
        OsStr::new("")
    ));
    assert!(environment_value_is_forwardable(
        EnvironmentKey::HostedInstallerSetup,
        OsStr::new("1")
    ));
    assert!(!environment_value_is_forwardable(
        EnvironmentKey::HostedInstallerSetup,
        OsStr::new("0")
    ));
}

#[test]
fn paid_cli_analytics_override_is_normalized_closed_and_optional() {
    let controls = AnalyticsEnvironment::new();
    let analytics_name = EnvironmentKey::AnalyticsEnabled.as_str();

    controls.set(
        "CTX_ANALYTICS_ENDPOINT",
        "https://ambient.example.test/private",
    );
    let absent = paid_cli_environment(false);
    assert_eq!(absent.get(analytics_name), None);
    assert_eq!(absent.get("CTX_ANALYTICS_ENDPOINT"), None);
    controls.remove("CTX_ANALYTICS_ENDPOINT");

    for value in ["false", " 0 ", "NO", "off"] {
        controls.set("CTX_ANALYTICS_ENABLED", value);
        let environment = paid_cli_environment(false);
        assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
    }
    for value in ["true", " 1 ", "YES", "on"] {
        controls.set("CTX_ANALYTICS_ENABLED", value);
        let environment = paid_cli_environment(false);
        assert_eq!(environment.get(analytics_name), Some(OsStr::new("true")));
    }
    for value in ["", "malformed", "2"] {
        controls.set("CTX_ANALYTICS_ENABLED", value);
        let environment = paid_cli_environment(false);
        assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let non_unicode = OsString::from_vec(vec![0xff]);
        controls.set_os("CTX_ANALYTICS_ENABLED", &non_unicode);
        let environment = paid_cli_environment(false);
        assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
    }

    controls.remove("CTX_ANALYTICS_ENABLED");
    for alias in [
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
    ] {
        controls.set(alias, "yes");
        let environment = paid_cli_environment(false);
        assert_eq!(environment.get(analytics_name), Some(OsStr::new("false")));
        controls.remove(alias);
    }

    controls.set("CTX_ANALYTICS_ENABLED", "YES");
    controls.set("CTX_ANALYTICS_OFF", "1");
    controls.set(
        "CTX_ANALYTICS_ENDPOINT",
        "https://ambient.example.test/private",
    );
    let setup = paid_cli_environment(true);
    assert_eq!(setup.get(analytics_name), Some(OsStr::new("false")));
    assert_eq!(setup.get("CTX_ANALYTICS_ENDPOINT"), None);
}

#[test]
fn mcp_analytics_consent_is_resolved_from_authoritative_config() {
    let controls = AnalyticsEnvironment::new();

    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join(crate::config::CONFIG_FILE),
        "[analytics]\nenabled = true\n",
    )
    .unwrap();
    let mut enabled = CompanionEnvironment::new();
    forward_mcp_environment(&mut enabled, root.path());
    assert_eq!(
        enabled.get(EnvironmentKey::AnalyticsEnabled.as_str()),
        Some(OsStr::new("true"))
    );
    assert_eq!(enabled.get("CTX_ANALYTICS_ENDPOINT"), None);

    std::env::set_var("CTX_ANALYTICS_ENABLED", "false");
    let mut overridden = CompanionEnvironment::new();
    forward_mcp_environment(&mut overridden, root.path());
    assert_eq!(
        overridden.get(EnvironmentKey::AnalyticsEnabled.as_str()),
        Some(OsStr::new("false"))
    );

    std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
    let mut explicitly_enabled = CompanionEnvironment::new();
    forward_mcp_environment(&mut explicitly_enabled, root.path());
    assert_eq!(
        explicitly_enabled.get(EnvironmentKey::AnalyticsEnabled.as_str()),
        Some(OsStr::new("true"))
    );

    std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
    std::fs::write(
        root.path().join(crate::config::CONFIG_FILE),
        "[analytics]\nenabled = false\n",
    )
    .unwrap();
    let mut persisted_disabled = CompanionEnvironment::new();
    forward_mcp_environment(&mut persisted_disabled, root.path());
    assert_eq!(
        persisted_disabled.get(EnvironmentKey::AnalyticsEnabled.as_str()),
        Some(OsStr::new("false"))
    );

    std::fs::write(
        root.path().join(crate::config::CONFIG_FILE),
        "[analytics]\nenabled = true\n",
    )
    .unwrap();
    for value in ["", "malformed", "2"] {
        controls.set("CTX_ANALYTICS_ENABLED", value);
        let mut malformed_override = CompanionEnvironment::new();
        forward_mcp_environment(&mut malformed_override, root.path());
        assert_eq!(
            malformed_override.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("false")),
            "override {value:?} must fail closed"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        controls.set_os("CTX_ANALYTICS_ENABLED", &OsString::from_vec(vec![0xff]));
        let mut non_unicode_override = CompanionEnvironment::new();
        forward_mcp_environment(&mut non_unicode_override, root.path());
        assert_eq!(
            non_unicode_override.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("false"))
        );
    }

    controls.remove("CTX_ANALYTICS_ENABLED");
    for alias in [
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
    ] {
        controls.set(alias, "yes");
        let mut deprecated_opt_out = CompanionEnvironment::new();
        forward_mcp_environment(&mut deprecated_opt_out, root.path());
        assert_eq!(
            deprecated_opt_out.get(EnvironmentKey::AnalyticsEnabled.as_str()),
            Some(OsStr::new("false")),
            "deprecated alias {alias} must fail closed"
        );
        controls.remove(alias);
    }

    std::env::set_var("CTX_ANALYTICS_ENABLED", "true");
    std::fs::write(
        root.path().join(crate::config::CONFIG_FILE),
        "[analytics]\nenabled = malformed\n",
    )
    .unwrap();
    let mut malformed = CompanionEnvironment::new();
    forward_mcp_environment(&mut malformed, root.path());
    assert_eq!(
        malformed.get(EnvironmentKey::AnalyticsEnabled.as_str()),
        Some(OsStr::new("false"))
    );
}

#[test]
fn setup_pro_forwards_the_complete_named_supervisor_environment() {
    static ENVIRONMENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct Restore(Vec<(&'static str, Option<OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    let _lock = ENVIRONMENT_LOCK.lock().unwrap();
    let values = [
        ("CODEX_HOME", "/tmp/codex-home"),
        ("CTX_UPGRADE_AUTO", "false"),
        ("HTTP_PROXY", "http://proxy.example.test:8080"),
        ("SSL_CERT_FILE", "/tmp/private-ca.pem"),
    ];
    let _restore = Restore(
        values
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .chain(std::iter::once(("HOME", std::env::var_os("HOME"))))
            .collect(),
    );
    for (name, value) in values {
        std::env::set_var(name, value);
    }
    std::env::set_var("HOME", "");

    let mut environment = CompanionEnvironment::new();
    forward_supervisor_environment(&mut environment);
    let names = environment
        .get(SUPERVISOR_ENV_NAMES)
        .unwrap()
        .to_str()
        .unwrap()
        .split('\n')
        .collect::<Vec<_>>();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    for (name, value) in values {
        assert!(names.contains(&name));
        assert_eq!(environment.get(name), Some(OsStr::new(value)));
    }
    assert!(names.contains(&"CTX_SEARCH_SEMANTIC"));
    assert!(names.contains(&"CTX_UPGRADE_CHANNEL"));
    assert!(names.contains(&"HOME"));
    assert_eq!(environment.get("HOME"), None);
}

#[test]
fn only_forwarded_setup_arguments_request_the_supervisor_environment() {
    for arguments in [
        vec!["--pro", "setup"],
        vec!["--data-root", "/tmp/setup", "setup", "--pro"],
        vec!["--", "setup", "--pro"],
    ] {
        let arguments = arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert!(forwarded_arguments_select_setup(&arguments));
    }
    for arguments in [
        vec!["--pro", "status"],
        vec!["--data-root", "setup", "status", "--pro"],
        vec!["help", "setup", "--pro"],
    ] {
        let arguments = arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert!(!forwarded_arguments_select_setup(&arguments));
    }
}

#[test]
fn explicit_override_selects_only_the_protocol_compatible_pro_executable() {
    let temp = tempfile::tempdir().unwrap();
    let source_core = temp.path().join("source/target/debug/ctx");
    let pro = temp.path().join("installed/libexec/ctx-pro");
    let companion =
        installed_companion_from_parts(&source_core, Some(pro.clone())).expect("source override");

    assert_eq!(companion.executable(), pro);
}

#[test]
fn installed_core_defaults_to_its_sibling_pro_executable() {
    let temp = tempfile::tempdir().unwrap();
    let core =
        temp.path()
            .join("installed/bin")
            .join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    let expected = temp
        .path()
        .join("installed/libexec")
        .join(if cfg!(windows) {
            "ctx-pro.exe"
        } else {
            "ctx-pro"
        });
    let companion = installed_companion_from_parts(&core, None).unwrap();

    assert_eq!(companion.executable(), expected);
}

#[test]
fn missing_pro_is_a_distinct_typed_error() {
    let temp = tempfile::tempdir().unwrap();
    let core =
        temp.path()
            .join("installed/bin")
            .join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    let companion = installed_companion_from_parts(&core, None).unwrap();
    let missing_path = companion.executable().to_path_buf();
    let error = CompanionBridge::default()
        .launch_mcp(
            &companion,
            McpRequest::new(Vec::new()),
            &CancellationToken::new(),
        )
        .unwrap_err();
    let error = classify_bridge_error(error);
    assert!(matches!(
        error,
        CompanionLaunchError::MissingExecutable { ref path } if path == &missing_path
    ));
    assert_eq!(error.code(), "companion_missing_executable");
}

#[cfg(unix)]
#[test]
fn protocol_v3_alone_launches_pro_without_any_context_environment() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let core = temp.path().join("installed/bin/ctx");
    let pro = temp.path().join("installed/libexec/ctx-pro");
    std::fs::create_dir_all(core.parent().unwrap()).unwrap();
    std::fs::create_dir_all(pro.parent().unwrap()).unwrap();
    std::fs::write(
        &pro,
        br##"#!/bin/sh
if [ "$1" = "--ctx-pro-protocol-v3" ] && [ "$2" = "handshake" ]; then
  printf '{"protocol_version":3}\n'
  exit 0
fi
if [ "$1" != "--ctx-pro-protocol-v3" ] || [ "$2" != "mcp-serve" ]; then
  exit 91
fi
for name in CTX_PRO_PATH CTX_PRO_INSTALL_CONTEXT CTX_DATA_ROOT CTX_PRO_DATA_ROOT CTX_MANAGED_PAIR_CHANNEL CTX_PRO_INSTALLATION_ID CTX_MANAGED_PAIR_INVOCATION_FINGERPRINT CTX_MANAGED_PAIR_CORE_CAPABILITY_FINGERPRINT CTX_RELEASE_BUILD_SOURCE_COMMIT; do
  eval "value=\${$name-}"
  [ -z "$value" ] || exit 92
done
printf '{"jsonrpc":"2.0"}\n'
"##,
    )
    .unwrap();
    std::fs::set_permissions(&pro, std::fs::Permissions::from_mode(0o700)).unwrap();
    let companion = installed_companion_from_parts(&core, None).unwrap();
    let response = CompanionBridge::default()
        .launch_mcp(
            &companion,
            McpRequest::new(Vec::new()),
            &CancellationToken::new(),
        )
        .unwrap();

    assert_eq!(response.exit_class(), ExitClass::Success);
    assert_eq!(response.stdout(), b"{\"jsonrpc\":\"2.0\"}\n");
    assert!(response.stderr().is_empty());
}

#[cfg(unix)]
#[test]
fn protocol_mismatch_is_a_distinct_typed_error() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let pro = temp.path().join("ctx-pro");
    std::fs::write(&pro, b"#!/bin/sh\nprintf '{\"protocol_version\":2}\\n'\n").unwrap();
    std::fs::set_permissions(&pro, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = CompanionBridge::default()
        .launch_mcp(
            &InstalledCompanion::new(&pro),
            McpRequest::new(Vec::new()),
            &CancellationToken::new(),
        )
        .unwrap_err();
    let error = classify_bridge_error(error);
    assert!(matches!(
        error,
        CompanionLaunchError::ProtocolMismatch {
            expected,
            observed,
        } if expected.get() == 3 && observed.get() == 2
    ));
    assert_eq!(error.code(), "companion_protocol_mismatch");
}

#[test]
fn pre_handshake_exit_is_retryable_unavailable_not_protocol_mismatch() {
    let error = classify_bridge_error(BridgeError::HandshakeFailed {
        exit: ExitClass::Code(70),
        stderr: b"loader diagnostic".to_vec(),
        stderr_truncated: false,
    });
    assert!(matches!(
        error,
        CompanionLaunchError::LaunchFailed {
            ref stderr,
            stderr_truncated: false,
        } if stderr == b"loader diagnostic"
    ));
    assert_eq!(error.code(), "companion_unavailable");
    assert!(error.retryable());
    let document = cli_launch_error_document(&error);
    assert_eq!(document["details"]["stderr"], "loader diagnostic");
    assert_eq!(document["details"]["stderr_truncated"], false);
    assert!(document["details"]
        .get("observed_protocol_version")
        .is_none());
    assert_eq!(
        CompanionRouteError::from(error),
        CompanionRouteError::Unavailable
    );
}

#[test]
fn global_help_and_version_never_enter_the_companion_gate() {
    for option in ["-h", "--help", "-V", "--version"] {
        for family in ["pro", "blame", "referral"] {
            let arguments = [
                OsString::from("ctx"),
                OsString::from(option),
                OsString::from(family),
            ];
            assert!(
                paid_family_arguments(&arguments).is_none(),
                "{option} {family}"
            );
        }
    }
}

#[test]
fn subcommand_help_and_help_alias_route_to_the_companion() {
    for arguments in [
        vec!["ctx", "pro", "--help"],
        vec!["ctx", "blame", "--help"],
        vec!["ctx", "referral", "--help"],
        vec!["ctx", "help", "pro"],
    ] {
        let arguments = arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec())
        );
    }
}

#[test]
fn paid_data_root_argument_is_forwarded_without_core_derivation() {
    let arguments = [
        OsString::from("ctx"),
        OsString::from("--data-root=relative-root"),
        OsString::from("pro"),
    ];
    assert_eq!(
        paid_family_arguments(&arguments),
        Some(arguments[1..].to_vec())
    );
}

#[test]
fn blame_usage_root_tracks_only_completed_command_invocations() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().to_path_buf();
    let missing = root.join("missing");
    for arguments in [
        vec![
            OsString::from("--data-root"),
            root.clone().into_os_string(),
            OsString::from("blame"),
            OsString::from("opaque-target"),
        ],
        vec![
            OsString::from("--pro"),
            OsString::from("blame"),
            OsString::from("--data-root"),
            root.clone().into_os_string(),
            OsString::from("opaque-target"),
        ],
        vec![
            OsString::from("--data-root"),
            root.clone().into_os_string(),
            OsString::from("--"),
            OsString::from("blame"),
            OsString::from("opaque-target"),
        ],
    ] {
        assert_eq!(paid_blame_data_root(&arguments), Some(root.clone()));
    }
    assert_eq!(
        paid_blame_data_root(&[
            OsString::from("--data-root"),
            missing.clone().into_os_string(),
            OsString::from("blame"),
        ]),
        Some(missing)
    );

    for arguments in [
        vec![
            OsString::from("--data-root"),
            root.clone().into_os_string(),
            OsString::from("pro"),
        ],
        vec![
            OsString::from("blame"),
            OsString::from("--help"),
            OsString::from("--data-root"),
            root.clone().into_os_string(),
        ],
        vec![
            OsString::from("--data-root"),
            OsString::from("relative"),
            OsString::from("blame"),
        ],
    ] {
        assert_eq!(paid_blame_data_root(&arguments), None);
    }
}

#[test]
fn attached_blame_usage_root_is_resolved_without_parsing_private_arguments() {
    let root = tempfile::tempdir().unwrap();
    let mut attached = OsString::from("--data-root=");
    attached.push(root.path());
    let arguments = [
        OsString::from("blame"),
        OsString::from("opaque-target"),
        attached,
        OsString::from("--private-option"),
    ];

    assert_eq!(
        paid_blame_data_root(&arguments),
        Some(root.path().to_path_buf())
    );
}

#[cfg(unix)]
#[test]
fn blame_usage_root_preserves_non_utf8_native_paths() {
    use std::os::unix::ffi::OsStringExt as _;

    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join(OsString::from_vec(vec![b'n', b'o', b'n', 0xff]));
    let mut attached = OsString::from("--data-root=");
    attached.push(&root);

    assert_eq!(
        paid_blame_data_root(&[attached, OsString::from("blame")]),
        Some(root)
    );
}

#[cfg(unix)]
#[test]
fn paid_blame_wrapper_records_after_companion_exit_and_remains_controlled() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let pro = temp.path().join("ctx-pro");
    std::fs::write(
        &pro,
        b"#!/bin/sh\nif [ \"$1\" = \"--ctx-pro-protocol-v3\" ] && [ \"$2\" = \"handshake\" ]; then\n  printf '{\"protocol_version\":3}\\n'\n  exit 0\nfi\n[ \"$1\" = \"--ctx-pro-protocol-v3\" ] && [ \"$2\" = \"cli\" ] && exit 0\nexit 91\n",
    )
    .unwrap();
    std::fs::set_permissions(&pro, std::fs::Permissions::from_mode(0o700)).unwrap();

    let enabled = temp.path().join("enabled");
    std::fs::create_dir(&enabled).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&enabled).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .arg("--data-root")
        .arg(&enabled)
        .arg("blame")
        .arg("opaque-target")
        .env("CTX_PRO_PATH", &pro)
        .env_remove("CTX_LOCAL_USAGE_ENABLED")
        .status()
        .unwrap();
    assert!(status.success());

    let authority = ctx_client_observability::local_usage::LocalUsageStorageAuthority::new(
        enabled.join("usage.sqlite"),
        "1.0.0",
    );
    let report = ctx_client_observability::local_usage::read_report_authorized(
        &authority,
        &ctx_client_observability::local_usage::UsageControlSnapshot::unversioned(true),
        true,
    );
    let definition = &report.definitions.unwrap()[0];
    assert_eq!(definition.definition_version, 3);
    assert_eq!(definition.summary.calls, 1);
    assert_eq!(definition.summary.successful_calls, 1);
    assert_eq!(definition.summary.not_applicable_calls, 1);
    assert_eq!(definition.summary.result_count, 0);
    assert_eq!(definition.summary.delivered_output_bytes, 0);

    let disabled = temp.path().join("disabled");
    std::fs::create_dir(&disabled).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&disabled).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_ctx"))
        .arg("--data-root")
        .arg(&disabled)
        .arg("blame")
        .arg("opaque-target")
        .env("CTX_PRO_PATH", &pro)
        .env("CTX_LOCAL_USAGE_ENABLED", "false")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!disabled.join("usage.sqlite").exists());
}

#[test]
fn data_root_options_after_delimiter_remain_opaque_pro_arguments() {
    for trailing in [
        vec!["--data-root", "/private/positional"],
        vec!["--data-root=/private/positional"],
    ] {
        let mut arguments = vec![
            OsString::from("ctx"),
            OsString::from("pro"),
            OsString::from("--"),
        ];
        arguments.extend(trailing.into_iter().map(OsString::from));
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec()),
            "{arguments:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn opaque_paid_arguments_are_preserved_byte_for_byte() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let opaque = OsString::from_vec(vec![b'v', 0xff, b'x']);
    let arguments = [
        OsString::from("ctx"),
        OsString::from("referral"),
        opaque.clone(),
    ];
    let forwarded = paid_family_arguments(&arguments).unwrap();
    assert_eq!(
        forwarded[1].as_os_str().as_bytes(),
        opaque.as_os_str().as_bytes()
    );
}

#[test]
fn mcp_response_must_be_one_opaque_framed_line() {
    assert!(is_one_framed_line(b"{\"jsonrpc\":\"2.0\"}\n"));
    assert!(!is_one_framed_line(b"{}"));
    assert!(!is_one_framed_line(b"{}\n{}\n"));
}
