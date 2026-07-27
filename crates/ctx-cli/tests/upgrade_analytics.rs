mod support;

use support::*;

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_manual_dry_run_outcome() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--dry-run", "--json"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off");
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "dry_run");
    assert_eq!(properties["dry_run"], true);
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["update_was_available"], true);
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_eq!(properties["self_upgrade_allowed"], true);
    assert_eq!(properties["auto_upgrade_allowed"], true);
    assert!(properties.get("upgrade_warning_count_bucket").is_some());
    assert_analytics_properties_are_allowlisted(properties);
}
