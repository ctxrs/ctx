use super::*;
use std::{ffi::OsString, sync::MutexGuard};

const DEFAULT_CONTROL_ENV_KEYS: &[&str] = &[
    "CTX_ANALYTICS_ENABLED",
    "CTX_LOCAL_USAGE_ENABLED",
    "CTX_ANALYTICS_OFF",
    "CTX_DISABLE_ANALYTICS",
    "CTX_INSTALL_DIAGNOSTICS_OFF",
    "CTX_UPGRADE_AUTO",
    "CTX_UPGRADE_OFF",
    "CTX_DISABLE_AUTO_UPGRADE",
    "CTX_DAEMON_ENABLED",
    DAEMON_MODE_ENV,
    "CTX_DAEMON_OFF",
    "CTX_DISABLE_DAEMON",
    "CTX_SEARCH_SEMANTIC",
];

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let lock = TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = keys
            .iter()
            .map(|&key| {
                let value = env::var_os(key);
                env::remove_var(key);
                (key, value)
            })
            .collect();
        Self { _lock: lock, saved }
    }

    fn set(&self, key: &'static str, value: &str) {
        env::set_var(key, value);
    }

    fn set_os(&self, key: &'static str, value: &OsString) {
        env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => env::set_var(*key, value),
                None => env::remove_var(*key),
            }
        }
    }
}

#[test]
fn parses_day_one_config_values() {
    let values = parse_toml_subset(
        r#"
[analytics]
enabled = false

[local_usage]
enabled = false

[upgrade]
auto = "off"
channel = "beta"
interval_hours = 1

[daemon]
enabled = false
mode = "source-refresh-only"
"#,
    )
    .unwrap();
    let mut config = AppConfig::default();
    assert_eq!(
        config.analytics.endpoint,
        "https://cli.ctx.rs/functions/v1/analytics"
    );
    assert!(config.analytics.enabled);
    assert_eq!(config.upgrade.auto, AUTO_UPGRADE_DEFAULT_MODE);
    assert_eq!(config.auto_upgrade_mode(), AutoUpgradeMode::Apply);
    assert!(config.auto_upgrade_enabled());
    assert_eq!(config.search.semantic, None);
    config.apply_values(&values).unwrap();
    assert!(!config.analytics.enabled);
    assert!(!config.local_usage.enabled);
    assert_eq!(config.upgrade.auto, "off");
    assert_eq!(config.upgrade.channel, "beta");
    assert_eq!(config.upgrade.interval, Duration::from_secs(60 * 60));
    assert!(!config.daemon.enabled);
    assert_eq!(config.daemon.mode, DaemonMode::SourceRefreshOnly);
    assert_eq!(config.search.semantic, None);
}

#[test]
fn search_semantic_is_unset_when_absent() {
    let values = parse_toml_subset("[upgrade]\nauto = \"off\"\n").unwrap();
    let mut config = AppConfig::default();

    config.apply_values(&values).unwrap();

    assert_eq!(config.search.semantic, None);
}

#[test]
fn parses_search_semantic_true() {
    let values = parse_toml_subset("[search]\nsemantic = true\n").unwrap();
    let mut config = AppConfig::default();

    config.apply_values(&values).unwrap();

    assert_eq!(config.search.semantic, Some(true));
}

#[test]
fn parses_search_semantic_false() {
    let values = parse_toml_subset("[search]\nsemantic = false\n").unwrap();
    let mut config = AppConfig::default();

    config.apply_values(&values).unwrap();

    assert_eq!(config.search.semantic, Some(false));
}

#[test]
fn load_without_config_file_uses_defaults() {
    let _env_guard = EnvGuard::new(DEFAULT_CONTROL_ENV_KEYS);
    let temp = tempfile::tempdir().unwrap();

    let config = AppConfig::load(temp.path()).unwrap();

    assert!(config.analytics.enabled);
    assert!(config.local_usage.enabled);
    assert_eq!(config.upgrade.auto, AUTO_UPGRADE_DEFAULT_MODE);
    assert_eq!(config.auto_upgrade_mode(), AutoUpgradeMode::Apply);
    assert!(config.auto_upgrade_enabled());
    assert_eq!(config.upgrade.channel, "stable");
    assert_eq!(config.upgrade.interval, Duration::from_secs(24 * 60 * 60));
    assert!(config.daemon.enabled);
    assert_eq!(config.daemon.mode, DaemonMode::Full);
    assert_eq!(config.search.semantic, None);
    assert!(!config.semantic_search_enabled());
}

#[test]
fn empty_config_runtime_defaults_match_public_control_inventory() {
    let _env_guard = EnvGuard::new(DEFAULT_CONTROL_ENV_KEYS);
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::load(temp.path()).unwrap();
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../../../contracts/public-control-surface-v1.json"
    ))
    .unwrap();
    let released = |config_key: &str| {
        contract["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|control| control["config_key"] == config_key)
            .unwrap()["released_default"]["value"]
            .clone()
    };

    assert_eq!(
        released("analytics.enabled"),
        serde_json::json!(config.analytics.enabled)
    );
    assert_eq!(
        released("local_usage.enabled"),
        serde_json::json!(config.local_usage.enabled)
    );
    assert_eq!(
        released("upgrade.auto"),
        serde_json::json!(config.auto_upgrade_mode().as_str())
    );
    assert_eq!(
        released("daemon.enabled"),
        serde_json::json!(config.daemon.enabled)
    );
    assert_eq!(
        released("search.semantic"),
        serde_json::json!(config.semantic_search_enabled())
    );
}

#[test]
fn legacy_config_without_runtime_control_keys_adopts_public_defaults() {
    let _env_guard = EnvGuard::new(DEFAULT_CONTROL_ENV_KEYS);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[upgrade]\nchannel = \"stable\"\n",
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();

    assert!(config.analytics.enabled);
    assert!(config.local_usage.enabled);
    assert_eq!(config.auto_upgrade_mode(), AutoUpgradeMode::Apply);
    assert!(config.auto_upgrade_enabled());
    assert!(config.daemon.enabled);
    assert!(!config.semantic_search_enabled());
}

#[test]
fn explicit_auto_upgrade_opt_out_wins_over_default_and_env_enable() {
    let env_guard = EnvGuard::new(&[
        "CTX_UPGRADE_AUTO",
        "CTX_UPGRADE_OFF",
        "CTX_DISABLE_AUTO_UPGRADE",
    ]);
    let temp = tempfile::tempdir().unwrap();

    env_guard.set("CTX_UPGRADE_AUTO", "off");
    let environment_opt_out = AppConfig::load(temp.path()).unwrap();
    assert_eq!(
        environment_opt_out.auto_upgrade_mode(),
        AutoUpgradeMode::Off
    );
    assert!(!environment_opt_out.auto_upgrade_enabled());

    fs::write(temp.path().join(CONFIG_FILE), "[upgrade]\nauto = \"off\"\n").unwrap();
    env_guard.set("CTX_UPGRADE_AUTO", "apply");
    let persisted_opt_out = AppConfig::load(temp.path()).unwrap();
    assert_eq!(persisted_opt_out.auto_upgrade_mode(), AutoUpgradeMode::Off);
    assert!(!persisted_opt_out.auto_upgrade_enabled());
}

#[test]
fn explicit_daemon_opt_out_wins_over_default_and_env_enable() {
    let env_guard = EnvGuard::new(&["CTX_DAEMON_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(CONFIG_FILE), "[daemon]\nenabled = false\n").unwrap();

    let persisted = AppConfig::load(temp.path()).unwrap();
    assert!(!persisted.daemon.enabled);

    env_guard.set("CTX_DAEMON_ENABLED", "true");
    let still_persisted = AppConfig::load(temp.path()).unwrap();
    assert!(!still_persisted.daemon.enabled);

    fs::remove_file(temp.path().join(CONFIG_FILE)).unwrap();
    env_guard.set("CTX_DAEMON_ENABLED", "false");
    let environment_opt_out = AppConfig::load(temp.path()).unwrap();
    assert!(!environment_opt_out.daemon.enabled);
}

#[test]
fn daemon_mode_has_documented_config_and_environment_contract() {
    let env_guard = EnvGuard::new(&[DAEMON_MODE_ENV]);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nmode = \"source-refresh-only\"\n",
    )
    .unwrap();

    assert_eq!(
        AppConfig::load(temp.path()).unwrap().daemon.mode,
        DaemonMode::SourceRefreshOnly
    );

    env_guard.set(DAEMON_MODE_ENV, "full");
    assert_eq!(
        AppConfig::load(temp.path()).unwrap().daemon.mode,
        DaemonMode::Full
    );
}

#[test]
fn daemon_mode_rejects_unknown_config_and_environment_values() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nmode = \"source-only-ish\"\n",
    )
    .unwrap();
    let config_error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());
    assert!(config_error.contains("daemon.mode"), "{config_error}");
    assert!(
        config_error.contains("source-refresh-only"),
        "{config_error}"
    );

    let env_error = format!(
        "{:#}",
        parse_daemon_mode_text(DAEMON_MODE_ENV, "source-only-ish").unwrap_err()
    );
    assert!(env_error.contains(DAEMON_MODE_ENV), "{env_error}");
    assert!(env_error.contains("source-refresh-only"), "{env_error}");
}

#[test]
fn explicit_local_usage_opt_out_wins_over_default_and_env_enable() {
    let env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[local_usage]\nenabled = false\n",
    )
    .unwrap();

    let persisted = AppConfig::load(temp.path()).unwrap();
    assert!(!persisted.local_usage.enabled);

    env_guard.set("CTX_LOCAL_USAGE_ENABLED", "true");
    let still_persisted = AppConfig::load(temp.path()).unwrap();
    assert!(!still_persisted.local_usage.enabled);

    fs::remove_file(temp.path().join(CONFIG_FILE)).unwrap();
    env_guard.set("CTX_LOCAL_USAGE_ENABLED", "false");
    let environment_opt_out = AppConfig::load(temp.path()).unwrap();
    assert!(!environment_opt_out.local_usage.enabled);
}

#[test]
fn local_usage_env_accepts_only_exact_documented_booleans() {
    let env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();

    for value in ["true", "false"] {
        env_guard.set("CTX_LOCAL_USAGE_ENABLED", value);
        let control = read_local_usage_control(temp.path()).unwrap();
        assert_eq!(control.effective_enabled, value == "true");
        assert_eq!(
            control.environment_override,
            if value == "true" {
                LocalUsageEnvOverride::Enabled
            } else {
                LocalUsageEnvOverride::Disabled
            }
        );
    }
    for value in ["TRUE", "1", "yes", " true ", "\"true\"", "invalid"] {
        env_guard.set("CTX_LOCAL_USAGE_ENABLED", value);
        let control = read_local_usage_control(temp.path()).unwrap();
        assert!(!control.effective_enabled, "{value}");
        assert_eq!(
            control.environment_override,
            LocalUsageEnvOverride::Invalid,
            "{value}"
        );
        assert!(!AppConfig::load(temp.path()).unwrap().local_usage.enabled);
    }
}

#[cfg(unix)]
#[test]
fn non_unicode_local_usage_env_fails_closed_without_exposing_the_value() {
    use std::os::unix::ffi::OsStringExt as _;

    let env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    env_guard.set_os(
        "CTX_LOCAL_USAGE_ENABLED",
        &OsString::from_vec(vec![b's', b'e', b'c', b'r', b'e', b't', 0xff]),
    );

    let control = read_local_usage_control(temp.path()).unwrap();
    assert!(!control.effective_enabled);
    assert_eq!(control.environment_override, LocalUsageEnvOverride::Invalid);
    assert!(!AppConfig::load(temp.path()).unwrap().local_usage.enabled);
    assert_eq!(control.environment_override.as_str(), "invalid");
}

#[test]
fn focused_local_usage_reader_distinguishes_valid_unrelated_and_local_damage() {
    let env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "unrelated malformed line\n[local_usage]\nenabled = false\n",
    )
    .unwrap();
    assert!(
        !read_local_usage_control(temp.path())
            .unwrap()
            .effective_enabled
    );
    assert_eq!(
        resolve_local_usage_control(temp.path()).config_state,
        LocalUsageConfigState::Resolved(false)
    );

    fs::write(temp.path().join(CONFIG_FILE), "unrelated malformed line\n").unwrap();
    let unresolved = resolve_local_usage_control(temp.path());
    assert_eq!(unresolved.config_state, LocalUsageConfigState::Unresolved);
    assert!(!unresolved.effective_on_startup());
    assert!(read_local_usage_control(temp.path()).is_err());

    fs::write(
        temp.path().join(CONFIG_FILE),
        "[local_usage]\nenabled = maybe\n",
    )
    .unwrap();
    assert_eq!(
        resolve_local_usage_control(temp.path()).config_state,
        LocalUsageConfigState::Malformed
    );
    assert!(read_local_usage_control(temp.path()).is_err());

    fs::write(temp.path().join(CONFIG_FILE), "unrelated malformed line\n").unwrap();
    env_guard.set("CTX_LOCAL_USAGE_ENABLED", "invalid");
    let invalid_environment = resolve_local_usage_control(temp.path());
    assert_eq!(
        invalid_environment.environment_override,
        LocalUsageEnvOverride::Invalid
    );
    assert!(!invalid_environment.effective_after(Some(true)));
}

const MALFORMED_LOCAL_USAGE_FORMS: &[(&str, &str)] = &[
    ("bare", "local_usage = true\n"),
    ("bare_without_value", "local_usage\n"),
    ("inline_table", "local_usage = { enabled = true }\n"),
    ("quoted_key", "\"local_usage\".enabled = true\n"),
    ("single_quoted_key", "'local_usage'.enabled = true\n"),
    ("quoted_dotted_key", "\"local_usage.enabled\" = true\n"),
    (
        "unicode_u_escaped_key",
        "\"local\\u005Fusage\".enabled = false\n",
    ),
    (
        "unicode_upper_u_escaped_key",
        "\"\\U0000006Cocal_usage\".enabled = false\n",
    ),
    (
        "unicode_escaped_dotted_key",
        "\"local\\u005Fusage.enabled\" = false\n",
    ),
    ("quoted_leaf", "local_usage.\"enabled\" = true\n"),
    ("quoted_table", "[\"local_usage\"]\nenabled = true\n"),
    (
        "unicode_escaped_table",
        "[\"local\\u005Fusage\"]\nenabled = false\n",
    ),
    (
        "unicode_escaped_table_path",
        "[\"\\U0000006Cocal_usage\".nested]\nvalue = false\n",
    ),
    ("single_quoted_table", "['local_usage']\nenabled = true\n"),
    (
        "owned_prefix_before_malformed_escape",
        "\"local\\u005Fusage.\\uZZZZ\" = false\n",
    ),
    (
        "owned_key_before_malformed_escape",
        "\"local\\u005Fusage\\q\" = false\n",
    ),
    ("spaced_dotted_key", "local_usage . enabled = true\n"),
    (
        "nested_local_usage_table",
        "[local_usage.enabled]\nvalue = true\n",
    ),
    ("array_table", "[[local_usage]]\nenabled = true\n"),
    ("duplicate_empty_tables", "[local_usage]\n[local_usage]\n"),
    (
        "duplicate_table",
        "[local_usage]\nenabled = true\n[local_usage]\n",
    ),
    (
        "separated_duplicate_table",
        "[local_usage]\n[analytics]\nenabled = true\n[local_usage]\nenabled = true\n",
    ),
];

#[test]
fn malformed_local_usage_owned_forms_disable_on_startup() {
    let _env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();

    for (name, text) in MALFORMED_LOCAL_USAGE_FORMS {
        fs::write(temp.path().join(CONFIG_FILE), text).unwrap();
        let resolution = resolve_local_usage_control(temp.path());
        assert_eq!(
            resolution.config_state,
            LocalUsageConfigState::Malformed,
            "{name}"
        );
        assert!(!resolution.effective_on_startup(), "{name}");
    }
}

#[test]
fn malformed_local_usage_owned_refresh_disables_a_valid_enabled_state() {
    let _env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    let mut resolver = LocalUsageConfigResolver::default();

    for (name, text) in MALFORMED_LOCAL_USAGE_FORMS {
        fs::write(&path, "[local_usage]\nenabled = true\n").unwrap();
        let enabled = resolver.resolve(temp.path());
        assert_eq!(
            enabled.config_state,
            LocalUsageConfigState::Resolved(true),
            "{name}"
        );
        assert!(enabled.effective_after(Some(false)), "{name}");

        fs::write(&path, text).unwrap();
        let malformed = resolver.resolve(temp.path());
        assert_eq!(
            malformed.config_state,
            LocalUsageConfigState::Malformed,
            "{name}"
        );
        assert!(!malformed.effective_after(Some(true)), "{name}");
    }
}

#[test]
fn unrelated_config_failure_still_retains_the_previous_refresh_state() {
    let _env_guard = EnvGuard::new(&["CTX_LOCAL_USAGE_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    let mut resolver = LocalUsageConfigResolver::default();

    fs::write(&path, "[local_usage]\nenabled = true\n").unwrap();
    assert!(resolver.resolve(temp.path()).effective_on_startup());

    for (name, text) in [
        ("ordinary", "unrelated malformed line\n"),
        (
            "escaped_unrelated_key",
            "\"analytics\\u005Fendpoint\" = \"broken\"\n",
        ),
        (
            "escaped_unrelated_table",
            "[\"ana\\u006Cytics\"]\nunknown = true\n",
        ),
        (
            "malformed_escape_before_ownership",
            "\"local\\uZZZZ_usage\" = true\n",
        ),
    ] {
        fs::write(&path, text).unwrap();
        let unresolved = resolver.resolve(temp.path());
        assert_eq!(
            unresolved.config_state,
            LocalUsageConfigState::Unresolved,
            "{name}"
        );
        assert!(unresolved.effective_after(Some(true)), "{name}");
    }
}

#[test]
fn deprecated_opt_outs_keep_historical_truthiness_and_win_over_enabling() {
    const KEYS: &[&str] = &[
        "CTX_ANALYTICS_ENABLED",
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
        "CTX_DAEMON_ENABLED",
        "CTX_DAEMON_OFF",
        "CTX_DISABLE_DAEMON",
        "CTX_UPGRADE_AUTO",
        "CTX_UPGRADE_OFF",
        "CTX_DISABLE_AUTO_UPGRADE",
    ];
    let env_guard = EnvGuard::new(KEYS);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[analytics]\nenabled = true\n[daemon]\nenabled = true\n[upgrade]\nauto = \"apply\"\n",
    )
    .unwrap();
    env_guard.set("CTX_ANALYTICS_ENABLED", "true");
    env_guard.set("CTX_DAEMON_ENABLED", "true");
    env_guard.set("CTX_UPGRADE_AUTO", "apply");

    for key in &KEYS[1..] {
        if !matches!(
            *key,
            "CTX_DAEMON_ENABLED" | "CTX_UPGRADE_AUTO" | "CTX_DAEMON_OFF"
        ) {
            env_guard.set(key, " false ");
        }
    }
    env_guard.set("CTX_DAEMON_OFF", "0");
    let inactive = AppConfig::load(temp.path()).unwrap();
    assert!(inactive.analytics.enabled);
    assert!(inactive.daemon.enabled);
    assert_eq!(inactive.upgrade.auto, "apply");

    env_guard.set("CTX_INSTALL_DIAGNOSTICS_OFF", "yes");
    env_guard.set("CTX_DISABLE_DAEMON", "anything");
    env_guard.set("CTX_UPGRADE_OFF", "ON");
    let active = AppConfig::load(temp.path()).unwrap();
    assert!(!active.analytics.enabled);
    assert!(!active.daemon.enabled);
    assert_eq!(active.upgrade.auto, "off");
}

#[test]
fn load_valid_config_file_applies_values() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        r#"
[analytics]
enabled = false
endpoint = "file:///tmp/ctx-analytics.jsonl"

[upgrade]
auto = "off"
channel = "beta"
interval_hours = 2

[daemon]
enabled = false
"#,
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();

    assert!(!config.analytics.enabled);
    assert_eq!(config.analytics.endpoint, "file:///tmp/ctx-analytics.jsonl");
    assert_eq!(config.upgrade.auto, "off");
    assert_eq!(config.upgrade.channel, "beta");
    assert_eq!(config.upgrade.interval, Duration::from_secs(2 * 60 * 60));
    assert!(!config.daemon.enabled);
}

#[test]
fn config_rejects_upgrade_metadata_authority_substitution() {
    let values = parse_toml_subset(
        "[upgrade]\nfunctions_base = \"file:///attacker/ctx-release-metadata.env\"\n",
    )
    .unwrap();
    let error = AppConfig::default().apply_values(&values).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unknown config key `upgrade.functions_base`"),
        "{error:#}"
    );
}

#[test]
fn set_daemon_enabled_rewrites_or_adds_config_key() {
    let temp = tempfile::tempdir().unwrap();

    set_daemon_enabled(temp.path(), false).unwrap();
    let disabled = AppConfig::load(temp.path()).unwrap();
    assert!(!disabled.daemon.enabled);
    let text = fs::read_to_string(temp.path().join(CONFIG_FILE)).unwrap();
    assert!(text.contains("[daemon]"));
    assert!(text.contains("enabled = false"));

    set_daemon_enabled(temp.path(), true).unwrap();
    let enabled = AppConfig::load(temp.path()).unwrap();
    assert!(enabled.daemon.enabled);
    let text = fs::read_to_string(temp.path().join(CONFIG_FILE)).unwrap();
    assert!(text.contains("enabled = true"));
}

#[test]
fn set_semantic_search_enabled_is_durable_preserving_and_idempotent() {
    let _env_guard = EnvGuard::new(&["CTX_SEARCH_SEMANTIC"]);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(CONFIG_FILE);
    let original =
        "# retained comment\n[analytics]\nenabled = false\n\n[search]\nsemantic = false\n";
    fs::write(&path, original).unwrap();

    set_semantic_search_enabled(temp.path(), true).unwrap();
    let enabled = AppConfig::load(temp.path()).unwrap();
    assert!(enabled.semantic_search_enabled());
    let once = fs::read_to_string(&path).unwrap();
    assert!(once.starts_with("# retained comment\n[analytics]\nenabled = false\n"));
    assert!(once.contains("[search]\nsemantic = true\n"));

    set_semantic_search_enabled(temp.path(), true).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), once);
}

#[test]
fn default_config_is_not_written_for_implicit_defaults() {
    let temp = tempfile::tempdir().unwrap();
    write_default_config(temp.path()).unwrap();

    assert!(!temp.path().join(CONFIG_FILE).exists());
}

#[test]
fn rejects_invalid_config_booleans() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[analytics]\nenabled = flase\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("analytics.enabled"), "{error}");
    assert!(error.contains("boolean"), "{error}");
}

#[test]
fn rejects_invalid_search_semantic_values() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[search]\nsemantic = maybe\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("search.semantic"), "{error}");
    assert!(error.contains("boolean"), "{error}");
}

#[test]
fn env_overrides_search_semantic_config() {
    let env_guard = EnvGuard::new(&["CTX_SEARCH_SEMANTIC"]);
    let temp = tempfile::tempdir().unwrap();

    fs::write(
        temp.path().join(CONFIG_FILE),
        "[search]\nsemantic = false\n",
    )
    .unwrap();
    env_guard.set("CTX_SEARCH_SEMANTIC", "true");
    let config = AppConfig::load(temp.path()).unwrap();
    assert_eq!(config.search.semantic, Some(true));

    fs::write(temp.path().join(CONFIG_FILE), "[search]\nsemantic = true\n").unwrap();
    env_guard.set("CTX_SEARCH_SEMANTIC", "false");
    let config = AppConfig::load(temp.path()).unwrap();
    assert_eq!(config.search.semantic, Some(false));
}

#[test]
fn analytics_config_opt_out_wins_over_env_enable_and_endpoint() {
    let env_guard = EnvGuard::new(&["CTX_ANALYTICS_ENABLED", "CTX_ANALYTICS_ENDPOINT"]);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[analytics]\nenabled = false\n",
    )
    .unwrap();
    env_guard.set("CTX_ANALYTICS_ENABLED", "true");
    env_guard.set("CTX_ANALYTICS_ENDPOINT", "https://example.test/analytics");

    let config = AppConfig::load(temp.path()).unwrap();

    assert!(!config.analytics.enabled);
    assert_eq!(config.analytics.endpoint, "https://example.test/analytics");
}

#[test]
fn analytics_enabled_false_is_an_env_opt_out() {
    let env_guard = EnvGuard::new(&["CTX_ANALYTICS_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    env_guard.set("CTX_ANALYTICS_ENABLED", "false");

    let config = AppConfig::load(temp.path()).unwrap();

    assert!(!config.analytics.enabled);
}

#[test]
fn rejects_invalid_upgrade_auto_values() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[upgrade]\nauto = \"offf\"\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("upgrade.auto"), "{error}");
    assert!(error.contains("\"apply\" or \"off\""), "{error}");
}

#[test]
fn rejects_unquoted_upgrade_auto_values() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(CONFIG_FILE), "[upgrade]\nauto = offf\n").unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("upgrade.auto"), "{error}");
    assert!(error.contains("quoted string"), "{error}");
}

#[test]
fn rejects_invalid_config_numbers() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[upgrade]\ninterval_hours = nope\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("upgrade.interval_hours"), "{error}");
    assert!(error.contains("unsigned integer"), "{error}");
}

#[test]
fn rejects_malformed_config_lines() {
    let error = parse_toml_subset("[upgrade]\nthis is not valid\n").unwrap_err();
    let error = error.to_string();

    assert!(error.contains("invalid config line 2"), "{error}");
}

#[test]
fn rejects_unknown_config_keys() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[analytics]\nenabld = false\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("unknown config key"), "{error}");
    assert!(error.contains("analytics.enabld"), "{error}");
}

#[test]
fn rejects_unknown_search_config_keys() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[search]\nsemantics = true\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());

    assert!(error.contains("unknown config key"), "{error}");
    assert!(error.contains("search.semantics"), "{error}");
}

#[test]
fn rejects_removed_cloud_config_keys() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[cloud]\nmode = \"local_and_cloud\"\n",
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());
    assert!(error.contains("unknown config key"), "{error}");
    assert!(error.contains("cloud.mode"), "{error}");
}
