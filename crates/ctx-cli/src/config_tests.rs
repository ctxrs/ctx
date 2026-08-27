use super::*;
use std::{ffi::OsString, sync::MutexGuard};

#[path = "config_tests/semantic.rs"]
mod semantic;

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

fn load_config_error(contents: impl AsRef<[u8]>) -> String {
    let data_root = tempfile::tempdir().unwrap();
    fs::write(data_root.path().join(CONFIG_FILE), contents).unwrap();
    format!("{:#}", AppConfig::load(data_root.path()).unwrap_err())
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
    assert_eq!(config.semantic.enabled, None);
    config.apply_values(&values).unwrap();
    assert!(!config.analytics.enabled);
    assert!(!config.local_usage.enabled);
    assert_eq!(config.upgrade.auto, "off");
    assert_eq!(config.upgrade.channel, "beta");
    assert_eq!(config.upgrade.interval, Duration::from_secs(60 * 60));
    assert_eq!(config.indexing.mode, IndexingMode::Manual);
    assert_eq!(config.daemon.mode, DaemonMode::SourceRefreshOnly);
    assert_eq!(config.semantic.enabled, None);
    assert_eq!(
        config.semantic_indexing_intensity(),
        SemanticIndexingIntensity::Quiet
    );
    assert_eq!(config.semantic_indexing_intensity_source(), "default");
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
    assert_eq!(config.indexing.mode, IndexingMode::Automatic);
    assert_eq!(config.daemon.mode, DaemonMode::Full);
    assert_eq!(config.semantic.enabled, None);
    assert!(!config.semantic_search_enabled());
    assert_eq!(config.semantic_search_source(), "default");
    assert_eq!(
        config.semantic_indexing_intensity(),
        SemanticIndexingIntensity::Quiet
    );
    assert_eq!(config.semantic_indexing_intensity_source(), "default");
    assert!(config.automatic_source_discovery_enabled());
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
        released("indexing.mode"),
        serde_json::json!(config.indexing.mode.as_str())
    );
    assert_eq!(
        released("semantic.enabled"),
        serde_json::json!(config.semantic_search_enabled())
    );
    assert_eq!(
        released("semantic.indexing_intensity"),
        serde_json::json!(config.semantic_indexing_intensity().as_str())
    );
    assert_eq!(
        contract["compatibility_config_keys"]["search.semantic"],
        serde_json::json!("semantic.enabled")
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
    assert_eq!(config.indexing.mode, IndexingMode::Automatic);
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
    assert_eq!(persisted.indexing.mode, IndexingMode::Manual);

    env_guard.set("CTX_DAEMON_ENABLED", "true");
    let still_persisted = AppConfig::load(temp.path()).unwrap();
    assert_eq!(still_persisted.indexing.mode, IndexingMode::Manual);

    fs::remove_file(temp.path().join(CONFIG_FILE)).unwrap();
    env_guard.set("CTX_DAEMON_ENABLED", "false");
    let environment_opt_out = AppConfig::load(temp.path()).unwrap();
    assert_eq!(environment_opt_out.indexing.mode, IndexingMode::Manual);
}

#[test]
fn canonical_indexing_mode_wins_over_legacy_daemon_enabled() {
    let _env_guard = EnvGuard::new(&["CTX_DAEMON_ENABLED"]);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[indexing]\nmode = \"manual\"\n\n[daemon]\nenabled = true\n",
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();

    assert_eq!(config.indexing.mode, IndexingMode::Manual);
}

#[test]
fn indexing_mode_accepts_canonical_and_automatic_alias() {
    for (spelling, expected, canonical) in [
        ("auto", IndexingMode::Automatic, "auto"),
        ("automatic", IndexingMode::Automatic, "auto"),
        ("manual", IndexingMode::Manual, "manual"),
    ] {
        let values = parse_toml_subset(&format!("[indexing]\nmode = \"{spelling}\"\n")).unwrap();
        let mut config = AppConfig::default();

        config.apply_values(&values).unwrap();

        assert_eq!(config.indexing.mode, expected);
        assert_eq!(config.indexing.mode.as_str(), canonical);
    }
}

#[test]
fn indexing_mode_rejects_unknown_values() {
    let error = load_config_error("[indexing]\nmode = \"on-demand\"\n");

    assert!(error.contains("indexing.mode"), "{error}");
    assert!(error.contains("\"auto\""), "{error}");
    assert!(error.contains("manual"), "{error}");
    assert!(!error.contains("automatic"), "{error}");
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
    let config_error = load_config_error("[daemon]\nmode = \"source-only-ish\"\n");
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
    assert_eq!(inactive.indexing.mode, IndexingMode::Automatic);
    assert_eq!(inactive.upgrade.auto, "apply");

    env_guard.set("CTX_INSTALL_DIAGNOSTICS_OFF", "yes");
    env_guard.set("CTX_DISABLE_DAEMON", "anything");
    env_guard.set("CTX_UPGRADE_OFF", "ON");
    let active = AppConfig::load(temp.path()).unwrap();
    assert!(!active.analytics.enabled);
    assert_eq!(active.indexing.mode, IndexingMode::Manual);
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
    assert_eq!(config.indexing.mode, IndexingMode::Manual);
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
fn daemon_enablement_updates_write_canonical_indexing_mode_and_remove_legacy_key() {
    let _env_guard = EnvGuard::new(DEFAULT_CONTROL_ENV_KEYS);
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        "# retained\n[daemon]\nenabled = true\nmode = \"full\"\n",
    )
    .unwrap();

    set_daemon_enabled(temp.path(), false).unwrap();
    let disabled = AppConfig::load(temp.path()).unwrap();
    assert_eq!(disabled.indexing.mode, IndexingMode::Manual);
    let text = fs::read_to_string(temp.path().join(CONFIG_FILE)).unwrap();
    assert!(text.contains("[indexing]"));
    assert!(text.contains("mode = \"manual\""));
    assert!(!text.contains("enabled = false"));
    assert!(!text.contains("enabled = true"));
    assert!(text.contains("# retained"));
    assert!(text.contains("mode = \"full\""));

    set_daemon_enabled(temp.path(), true).unwrap();
    let enabled = AppConfig::load(temp.path()).unwrap();
    assert_eq!(enabled.indexing.mode, IndexingMode::Automatic);
    let text = fs::read_to_string(temp.path().join(CONFIG_FILE)).unwrap();
    assert!(text.contains("mode = \"auto\""));
    assert!(!text.contains("automatic"));
    assert!(!text.contains("enabled = true"));
}

#[test]
fn default_config_is_not_written_for_implicit_defaults() {
    let temp = tempfile::tempdir().unwrap();
    write_default_config(temp.path()).unwrap();

    assert!(!temp.path().join(CONFIG_FILE).exists());
}

#[test]
fn invalid_scalar_values_and_unknown_keys_report_the_owned_field() {
    for (name, contents, expected) in [
        (
            "analytics boolean",
            "[analytics]\nenabled = flase\n",
            ["analytics.enabled", "boolean"],
        ),
        (
            "legacy semantic boolean",
            "[search]\nsemantic = maybe\n",
            ["search.semantic", "boolean"],
        ),
        (
            "canonical semantic boolean",
            "[semantic]\nenabled = maybe\n",
            ["semantic.enabled", "boolean"],
        ),
        (
            "unquoted semantic intensity",
            "[semantic]\nindexing_intensity = full\n",
            ["semantic.indexing_intensity", "quoted string"],
        ),
        (
            "upgrade mode",
            "[upgrade]\nauto = \"offf\"\n",
            ["upgrade.auto", "\"apply\" or \"off\""],
        ),
        (
            "unquoted upgrade mode",
            "[upgrade]\nauto = offf\n",
            ["upgrade.auto", "quoted string"],
        ),
        (
            "upgrade interval",
            "[upgrade]\ninterval_hours = nope\n",
            ["upgrade.interval_hours", "unsigned integer"],
        ),
        (
            "analytics key",
            "[analytics]\nenabld = false\n",
            ["unknown config key", "analytics.enabld"],
        ),
        (
            "search key",
            "[search]\nsemantics = true\n",
            ["unknown config key", "search.semantics"],
        ),
    ] {
        let error = load_config_error(contents);
        for fragment in expected {
            assert!(error.contains(fragment), "{name}: {error}");
        }
    }
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
fn rejects_malformed_config_lines() {
    let error = parse_toml_subset("[upgrade]\nthis is not valid\n").unwrap_err();
    let error = error.to_string();

    assert!(error.contains("invalid config line 2"), "{error}");
}

#[test]
fn parses_multiple_named_provider_roots_and_global_automatic_disable() {
    let temp = tempfile::tempdir().unwrap();
    let claude_home = tempfile::tempdir().unwrap();
    let codex_home = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        format!(
            r#"
[sources]
automatic = false

[sources.roots.claude-personal]
provider = "claude"
path = {:?}
group = "personal"

[sources.roots.codex-work]
provider = "codex"
path = {:?}
group = "work"
"#,
            claude_home.path().display().to_string(),
            codex_home.path().display().to_string(),
        ),
    )
    .unwrap();

    let config = AppConfig::load(temp.path()).unwrap();
    assert!(!config.automatic_source_discovery_enabled());
    assert_eq!(
        config
            .provider_root_definitions()
            .into_iter()
            .map(|root| (root.id, root.provider, root.path, root.group))
            .collect::<Vec<_>>(),
        vec![
            (
                "claude-personal".to_owned(),
                CaptureProvider::Claude,
                fs::canonicalize(claude_home.path()).unwrap(),
                Some("personal".to_owned()),
            ),
            (
                "codex-work".to_owned(),
                CaptureProvider::Codex,
                fs::canonicalize(codex_home.path()).unwrap(),
                Some("work".to_owned()),
            ),
        ]
    );
}

#[test]
fn hand_edited_provider_roots_accept_the_cli_provider_vocabulary() {
    use ctx_history_capture::{
        configured_root_capabilities, ConfiguredRootPathKind, ProviderRootKind,
    };

    for capability in configured_root_capabilities()
        .iter()
        .filter(|capability| capability.state.is_enabled())
    {
        let provider_parent = tempfile::tempdir().unwrap();
        let provider_root = provider_parent.path().join("history");
        match capability
            .state
            .expected_path_kind()
            .expect("enabled configured-root capability must declare its path kind")
        {
            ConfiguredRootPathKind::Directory => fs::create_dir(&provider_root).unwrap(),
            ConfiguredRootPathKind::File => fs::write(&provider_root, b"history").unwrap(),
        }
        let spec = ctx_history_cli::provider_cli_spec(capability.provider)
            .expect("configured-root provider must have a public CLI vocabulary entry");
        let names = std::iter::once(spec.cli_name)
            .chain(std::iter::once(spec.provider.as_str()))
            .chain(spec.aliases.iter().copied())
            .collect::<std::collections::BTreeSet<_>>();

        for name in names {
            let data_root = tempfile::tempdir().unwrap();
            let kind = (capability.provider == CaptureProvider::OpenHands)
                .then_some(ProviderRootKind::OpenHandsCurrentConversations)
                .map(|kind| format!("kind = {:?}\n", kind.as_str()))
                .unwrap_or_default();
            fs::write(
                data_root.path().join(CONFIG_FILE),
                format!(
                    "[sources.roots.work]\nprovider = {name:?}\npath = {:?}\n{kind}",
                    provider_root.display().to_string(),
                ),
            )
            .unwrap();

            let config = AppConfig::load(data_root.path()).unwrap_or_else(|error| {
                panic!(
                    "configured-root provider name {name:?} did not match the CLI vocabulary: {error:#}"
                )
            });
            assert_eq!(
                config.provider_roots["work"].provider, capability.provider,
                "configured-root provider name {name:?} resolved differently from the CLI"
            );
        }
    }
}

#[test]
fn hand_edited_provider_roots_reject_invalid_provider_names() {
    let provider_root = tempfile::tempdir().unwrap();
    for name in ["grokbuild", "Grok-Build", "grok build", "not-a-provider"] {
        let data_root = tempfile::tempdir().unwrap();
        fs::write(
            data_root.path().join(CONFIG_FILE),
            format!(
                "[sources.roots.work]\nprovider = {name:?}\npath = {:?}\n",
                provider_root.path().display().to_string(),
            ),
        )
        .unwrap();

        let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
        assert!(
            error.contains("sources.roots.work.provider at line 2 is unknown"),
            "{name:?} produced an unexpected error: {error}"
        );
    }
}

#[test]
fn rejects_invalid_provider_root_config_as_one_atomic_config() {
    let provider_home = tempfile::tempdir().unwrap();
    let provider_path = provider_home.path().display().to_string();
    let oversized_path = provider_home
        .path()
        .join("x".repeat(ctx_history_capture::MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES + 1));
    let cases = [
        (
            format!(
                "[sources.roots.work]\nprovider = \"nanoclaw\"\npath = {:?}\n",
                provider_path
            ),
            "configured history roots are not enabled for nanoclaw",
        ),
        (
            "[sources.roots.work]\nprovider = \"claude\"\npath = \"relative\"\n".to_owned(),
            "normalized absolute UTF-8 path",
        ),
        (
            format!(
                "[sources.roots.work]\nprovider = \"claude\"\npath = {:?}\n",
                oversized_path.display().to_string()
            ),
            "encoded path limit",
        ),
        (
            format!(
                "[sources.roots.bad.name]\nprovider = \"claude\"\npath = {:?}\n",
                provider_path
            ),
            "provider root name",
        ),
        (
            "[search]\ndefault_group = \"work\"\n".to_owned(),
            "unknown config key",
        ),
        (
            format!(
                "[sources.roots.work]\nprovider = \"claude\"\npath = {:?}\nscope = \"work\"\n",
                provider_path
            ),
            "unknown config key `sources.roots.work.scope`",
        ),
    ];
    for (text, expected) in cases {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(CONFIG_FILE), text).unwrap();
        let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());
        assert!(error.contains(expected), "{error}");
    }

    let temp = tempfile::tempdir().unwrap();
    let duplicate_home = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join(CONFIG_FILE),
        format!(
            "[sources.roots.first]\nprovider = \"claude\"\npath = {:?}\n\n[sources.roots.second]\nprovider = \"claude\"\npath = {:?}\n",
            duplicate_home.path().display().to_string(),
            duplicate_home.path().display().to_string()
        ),
    )
    .unwrap();
    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());
    assert!(
        error.contains("select the same claude history root"),
        "{error}"
    );
}

#[test]
fn hand_edited_provider_roots_reject_data_root_overlap() {
    for relationship in ["equal", "ancestor", "descendant"] {
        let fixture = tempfile::tempdir().unwrap();
        let data_root = fixture.path().join("ctx-data");
        fs::create_dir(&data_root).unwrap();
        let provider_root = match relationship {
            "equal" => data_root.clone(),
            "ancestor" => fixture.path().to_path_buf(),
            "descendant" => {
                let nested = data_root.join("provider-home");
                fs::create_dir(&nested).unwrap();
                nested
            }
            _ => unreachable!(),
        };
        fs::write(
            data_root.join(CONFIG_FILE),
            format!(
                "[sources.roots.work]\nprovider = \"claude\"\npath = {:?}\n",
                provider_root.display().to_string()
            ),
        )
        .unwrap();

        let error = format!("{:#}", AppConfig::load(&data_root).unwrap_err());
        assert!(
            error.contains("must not overlap the ctx data root"),
            "{error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn hand_edited_provider_root_symlink_is_canonicalized() {
    use std::os::unix::fs::symlink;

    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let home = provider_parent.path().join("claude-home");
    let alias = provider_parent.path().join("alias");
    fs::create_dir(&home).unwrap();
    symlink(&home, &alias).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        format!(
            "[sources.roots.personal]\nprovider = \"claude\"\npath = {:?}\ngroup = \"personal\"\n",
            alias.display().to_string(),
        ),
    )
    .unwrap();

    let config = AppConfig::load(data_root.path()).unwrap();
    assert_eq!(
        config.provider_roots["personal"].path,
        fs::canonicalize(home).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn hand_edited_provider_roots_reject_distinct_symlinks_to_one_physical_home() {
    use std::os::unix::fs::symlink;

    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let home = provider_parent.path().join("claude-home");
    let first = provider_parent.path().join("first-alias");
    let second = provider_parent.path().join("second-alias");
    fs::create_dir(&home).unwrap();
    symlink(&home, &first).unwrap();
    symlink(&home, &second).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        format!(
            "[sources.roots.first]\nprovider = \"claude\"\npath = {:?}\n\n[sources.roots.second]\nprovider = \"claude\"\npath = {:?}\n",
            first.display().to_string(),
            second.display().to_string(),
        ),
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(
        error.contains("select the same claude history root"),
        "{error}"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn hand_edited_file_roots_reject_hard_links_to_one_physical_database() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let database = provider_parent.path().join("opencode.db");
    let alias = provider_parent.path().join("opencode-alias.db");
    fs::write(&database, b"provider database").unwrap();
    fs::hard_link(&database, &alias).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        format!(
            "[sources.roots.first]\nprovider = \"opencode\"\npath = {:?}\n\n[sources.roots.second]\nprovider = \"opencode\"\npath = {:?}\n",
            database.display().to_string(),
            alias.display().to_string(),
        ),
    )
    .unwrap();

    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(
        error.contains("select the same opencode history root"),
        "{error}"
    );
    assert!(error.contains("`first` and `second`"), "{error}");
}

#[test]
fn hand_edited_distinct_directory_roots_remain_independent() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let first = provider_parent.path().join("claude-first");
    let second = provider_parent.path().join("claude-second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        format!(
            "[sources.roots.first]\nprovider = \"claude\"\npath = {:?}\n\n[sources.roots.second]\nprovider = \"claude\"\npath = {:?}\n",
            first.display().to_string(),
            second.display().to_string(),
        ),
    )
    .unwrap();

    let config = AppConfig::load(data_root.path()).unwrap();
    assert_eq!(config.provider_roots.len(), 2);
}

#[test]
fn provider_root_count_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let text = (0..=MAX_CONFIGURED_PROVIDER_ROOTS)
        .map(|index| {
            format!(
                "[sources.roots.root{index}]\nprovider = \"claude\"\npath = {:?}\n",
                provider_parent
                    .path()
                    .join(format!("claude-{index}"))
                    .display()
                    .to_string()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(temp.path().join(CONFIG_FILE), text).unwrap();

    let error = format!("{:#}", AppConfig::load(temp.path()).unwrap_err());
    assert!(error.contains("exceed the maximum"), "{error}");
}

#[test]
fn provider_root_cli_mutations_are_durable_and_preserve_other_config() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let provider_home = provider_parent.path().join("claude-personal");
    fs::create_dir(&provider_home).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        "[analytics]\nenabled = false\n\n[sources]\nautomatic = false\n",
    )
    .unwrap();

    let added = add_claude_root(
        data_root.path(),
        "personal",
        &provider_home,
        Some("personal"),
        false,
    )
    .unwrap();
    assert!(added.changed);
    let unchanged = add_claude_root(
        data_root.path(),
        "personal",
        &provider_home,
        Some("personal"),
        false,
    )
    .unwrap();
    assert!(!unchanged.changed);
    let loaded = AppConfig::load(data_root.path()).unwrap();
    assert!(!loaded.analytics.enabled);
    assert!(!loaded.automatic_source_discovery_enabled());
    assert_eq!(loaded.provider_roots["personal"], added.root);

    let removed = remove_provider_root(data_root.path(), "personal").unwrap();
    assert!(removed.changed);
    let loaded = AppConfig::load(data_root.path()).unwrap();
    assert!(!loaded.analytics.enabled);
    assert!(!loaded.automatic_source_discovery_enabled());
    assert!(loaded.provider_roots.is_empty());
}

#[test]
fn persisted_provider_root_kind_is_openhands_only_and_exact() {
    let data_root = tempfile::tempdir().unwrap();
    let path = data_root.path().join("openhands-root");
    let missing_kind = format!(
        "[sources.roots.work]\nprovider = \"openhands\"\npath = {:?}\n",
        path.display().to_string()
    );
    fs::write(data_root.path().join(CONFIG_FILE), missing_kind).unwrap();
    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(error.contains("require --kind"), "{error}");

    let old_provider_kind = format!(
        "[sources.roots.work]\nprovider = \"claude\"\npath = {:?}\nkind = \"legacy-persistence\"\n",
        path.display().to_string()
    );
    fs::write(data_root.path().join(CONFIG_FILE), old_provider_kind).unwrap();
    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(error.contains("only supported for openhands"), "{error}");

    let invalid_spelling = format!(
        "[sources.roots.work]\nprovider = \"openhands\"\npath = {:?}\nkind = \"Current-Conversations\"\n",
        path.display().to_string()
    );
    fs::write(data_root.path().join(CONFIG_FILE), invalid_spelling).unwrap();
    let error = format!("{:#}", AppConfig::load(data_root.path()).unwrap_err());
    assert!(error.contains("must be current-conversations"), "{error}");
}

#[test]
fn provider_root_cli_mutation_validates_the_capability_path_kind() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let provider_file = provider_parent.path().join("claude-history-file");
    fs::write(&provider_file, b"not a provider directory").unwrap();

    let error = format!(
        "{:#}",
        add_claude_root(data_root.path(), "personal", &provider_file, None, false).unwrap_err()
    );

    assert!(error.contains("existing non-symlink directory"), "{error}");
    assert!(!data_root.path().join(CONFIG_FILE).exists());
}

#[test]
fn provider_root_cli_mutation_rejects_data_root_overlap_before_writing() {
    for relationship in ["equal", "ancestor", "descendant"] {
        let fixture = tempfile::tempdir().unwrap();
        let data_root = fixture.path().join("ctx-data");
        fs::create_dir(&data_root).unwrap();
        let provider_root = match relationship {
            "equal" => data_root.clone(),
            "ancestor" => fixture.path().to_path_buf(),
            "descendant" => {
                let nested = data_root.join("provider-home");
                fs::create_dir(&nested).unwrap();
                nested
            }
            _ => unreachable!(),
        };

        let error = format!(
            "{:#}",
            add_claude_root(&data_root, "work", &provider_root, Some("work"), false).unwrap_err()
        );
        assert!(
            error.contains("must not overlap the ctx data root"),
            "{error}"
        );
        assert!(!data_root.join(CONFIG_FILE).exists());
    }
}

#[test]
fn provider_root_mutation_waits_for_the_shared_config_transaction_lock() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let provider_home = provider_parent.path().join("claude-personal");
    fs::create_dir(&provider_home).unwrap();
    let config_path = AppConfig::config_path(data_root.path());
    let lock = durable_write::ConfigMutationLock::acquire(&config_path).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let data_root_path = data_root.path().to_path_buf();
    let provider_home_path = provider_home.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = add_claude_root(
            &data_root_path,
            "personal",
            &provider_home_path,
            Some("personal"),
            false,
        );
        finished_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(
        finished_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "a concurrent read-modify-write must not pass the config lock"
    );
    drop(lock);
    assert!(finished_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("provider-root mutation did not resume after unlock")
        .is_ok());
    worker.join().unwrap();
}

#[test]
fn removing_the_last_member_of_a_group_is_allowed() {
    let data_root = tempfile::tempdir().unwrap();
    let provider_parent = tempfile::tempdir().unwrap();
    let provider_home = provider_parent.path().join("claude-personal");
    fs::create_dir(&provider_home).unwrap();
    fs::write(
        data_root.path().join(CONFIG_FILE),
        format!(
            "[sources.roots.personal]\nprovider = \"claude\"\npath = {:?}\ngroup = \"personal\"\n",
            provider_home.display().to_string()
        ),
    )
    .unwrap();

    remove_provider_root(data_root.path(), "personal").unwrap();
    assert!(AppConfig::load(data_root.path())
        .unwrap()
        .provider_roots
        .is_empty());
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
