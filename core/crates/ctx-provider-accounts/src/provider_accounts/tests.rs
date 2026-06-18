use super::codex_auth::codex_oauth_runtime_home;
use super::*;

fn assert_unknown_account_error(err: anyhow::Error) {
    assert!(
        err.to_string().contains("unknown account"),
        "expected unknown account error, got: {err:#}"
    );
}

fn hold_codex_runtime_lock(home: &Path) -> std::fs::File {
    std::fs::create_dir_all(home).unwrap();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(".ctx-continuity-runtime.lock"))
        .unwrap();
    fs2::FileExt::try_lock_shared(&lock).unwrap();
    lock
}

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const CLAUDE_TEST_SETUP_TOKEN: &str = concat!("sk-ant-", "oat-test-token");

#[test]
fn codex_storage_manifest_keeps_auth_authority_out_of_continuity_state() {
    let continuity_names: Vec<&str> = CODEX_CONTINUITY_STATE_CHILDREN
        .iter()
        .map(|child| child.name)
        .collect();
    assert_eq!(
        continuity_names,
        vec![
            "sessions",
            "shell_snapshots",
            "history.jsonl",
            "config.toml",
            "prompts"
        ]
    );
    assert_eq!(CODEX_AUTHORITY_CHILDREN.len(), 1);
    assert_eq!(CODEX_AUTHORITY_CHILDREN[0].name, "auth.json");
    assert!(
        !continuity_names.contains(&"auth.json"),
        "OAuth auth authority must not be handled by continuity migration"
    );
}

#[tokio::test]
async fn oauth_broker_home_defers_shared_state_repair_when_other_broker_active() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target_broker_home = codex_broker_home(root, "acct-target");
    let other_broker_home = codex_broker_home(root, "acct-other");
    let _other_lock = hold_codex_runtime_lock(&other_broker_home);

    let legacy_history = legacy_codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(legacy_history.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&legacy_history, "legacy-history\n")
        .await
        .unwrap();

    let shared_history = codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(shared_history.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&shared_history, "active-shared-history\n")
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &target_broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("repair is blocked by an active runtime"),
        "missing broker links must fail closed while another runtime is active, got: {error:#}"
    );

    assert_eq!(
        tokio::fs::read_to_string(&shared_history).await.unwrap(),
        "active-shared-history\n"
    );
    assert!(
        tokio::fs::symlink_metadata(target_broker_home.join("history.jsonl"))
            .await
            .is_err(),
        "migration must defer instead of linking while any broker using shared continuity is active"
    );
}

#[tokio::test]
async fn oauth_broker_home_defers_shared_state_repair_when_shared_home_active() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target_broker_home = codex_broker_home(root, "acct-target");
    let shared_home = codex_runtime_home(root);
    let _shared_lock = hold_codex_runtime_lock(&shared_home);

    let legacy_history = legacy_codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(legacy_history.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&legacy_history, "legacy-history\n")
        .await
        .unwrap();
    tokio::fs::write(shared_home.join("history.jsonl"), "active-shared-history\n")
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &target_broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("repair is blocked by an active runtime"),
        "missing broker links must fail closed while the shared runtime home is active, got: {error:#}"
    );

    assert_eq!(
        tokio::fs::read_to_string(shared_home.join("history.jsonl"))
            .await
            .unwrap(),
        "active-shared-history\n"
    );
    assert!(
        tokio::fs::symlink_metadata(target_broker_home.join("history.jsonl"))
            .await
            .is_err(),
        "migration must defer instead of linking while the shared runtime home is active"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_provider_root_before_repair_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.join("providers"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex")).unwrap();

    let broker_home = codex_broker_home(root, "acct-target");
    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("must not contain a symlink component before lock acquisition"),
        "provider-root symlink must be rejected before repair lock creation, got: {error:#}"
    );
    assert!(
        !outside
            .path()
            .join(".ctx-continuity-migration.lock")
            .exists(),
        "failed provider-root validation must not create the repair lock through the symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_provider_root_before_runtime_oauth_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let account_id = "acct-oauth";
    tokio::fs::create_dir_all(root.join("providers"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex")).unwrap();

    let error =
        crate::provider_accounts::codex_auth::migrate_owned_runtime_oauth_projection_to_broker_if_needed(
            root, account_id,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("must not contain a symlink component before broker storage access"),
        "provider-root symlink must be rejected before runtime OAuth projection migration, got: {error:#}"
    );
    assert!(
        !outside.path().join("brokers").exists(),
        "failed provider-root validation must not create broker auth state through the symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_provider_root_before_hydration_registry_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.join("providers"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex")).unwrap();

    let error = hydrate_codex_account_home_from_secret(root, "missing-account")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("must not contain a symlink component before broker storage access"),
        "provider-root symlink must be rejected before hydration reads registry state, got: {error:#}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_provider_root_before_import_broker_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.join("providers"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex")).unwrap();
    let auth = serde_json::json!({
        "tokens": {
            "access_token": "access",
            "refresh_token": "refresh",
            "account_id": "upstream-acct"
        }
    });

    let error = import_codex_auth_value_to_secret_store(root, None, &auth)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("must not contain a symlink component before broker storage access"),
        "provider-root symlink must be rejected before import creates broker state, got: {error:#}"
    );
    assert!(
        !outside.path().join("accounts").exists() && !outside.path().join("brokers").exists(),
        "failed provider-root validation must not create registry or broker state through the symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_runtime_home_before_owner_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let account_id = "acct-oauth";
    tokio::fs::create_dir_all(root.join("providers/codex"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex/home")).unwrap();
    tokio::fs::write(outside.path().join(CODEX_RUNTIME_OWNER_FILE), account_id)
        .await
        .unwrap();
    tokio::fs::write(
        outside.path().join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();

    let error =
        crate::provider_accounts::codex_auth::migrate_owned_runtime_oauth_projection_to_broker_if_needed(
            root, account_id,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Codex runtime home path")
            && format!("{error:#}").contains("before broker storage access"),
        "symlinked runtime home must be rejected before owner/auth reads, got: {error:#}"
    );
    assert!(
        !codex_brokers_root(root).exists(),
        "failed runtime-home validation must not create broker state"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_brokers_root_before_import_lock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.join("providers/codex"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex/brokers")).unwrap();
    let auth = serde_json::json!({
        "tokens": {
            "access_token": "access",
            "refresh_token": "refresh",
            "account_id": "upstream-acct"
        }
    });

    let error = import_codex_auth_value_to_secret_store(root, None, &auth)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Codex broker home path")
            && format!("{error:#}").contains("before broker storage access"),
        "symlinked brokers root must be rejected before broker lock creation, got: {error:#}"
    );
    assert!(
        std::fs::read_dir(outside.path()).unwrap().next().is_none(),
        "failed brokers-root validation must not create lock or auth state through the symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_broker_entry_before_broker_auth_probe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let account_id = "acct-oauth";
    let runtime_home = codex_runtime_home(root);
    tokio::fs::create_dir_all(&runtime_home).await.unwrap();
    tokio::fs::write(codex_runtime_owner_path(root), account_id)
        .await
        .unwrap();
    tokio::fs::write(
        runtime_home.join("auth.json"),
        br#"{"tokens":{"access_token":"runtime-access","refresh_token":"runtime-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    let brokers_root = codex_brokers_root(root);
    tokio::fs::create_dir_all(&brokers_root).await.unwrap();
    tokio::fs::create_dir_all(outside.path().join("home"))
        .await
        .unwrap();
    tokio::fs::write(
        outside.path().join("home/auth.json"),
        br#"{"tokens":{"access_token":"outside-access","refresh_token":"outside-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), brokers_root.join(account_id)).unwrap();

    let error =
        crate::provider_accounts::codex_auth::migrate_owned_runtime_oauth_projection_to_broker_if_needed(
            root, account_id,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Codex broker home path")
            && format!("{error:#}").contains("before broker storage access"),
        "symlinked broker entry must be rejected before broker auth probes, got: {error:#}"
    );
    assert!(
        runtime_home.join("auth.json").exists(),
        "failed broker-home validation must not read outside broker auth and clear runtime auth"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_broker_entry_before_lock_scan() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    let brokers_root = codex_brokers_root(root);
    tokio::fs::create_dir_all(&brokers_root).await.unwrap();
    tokio::fs::create_dir_all(outside.path().join("home"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), brokers_root.join("acct-symlink")).unwrap();

    let target_broker_home = codex_broker_home(root, "acct-target");
    let error = expose_legacy_codex_state_to_broker_home(root, &target_broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("must not be a symlink before lock acquisition"),
        "symlinked broker entry must be rejected before lock scan dereferences it, got: {error:#}"
    );
    assert!(
        !outside
            .path()
            .join("home/.ctx-continuity-runtime.lock")
            .exists(),
        "failed broker scan validation must not create a runtime lock through the symlink"
    );
}

#[tokio::test]
async fn oauth_broker_home_skips_regular_broker_root_entries_before_lock_scan() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let brokers_root = codex_brokers_root(root);
    tokio::fs::create_dir_all(&brokers_root).await.unwrap();
    tokio::fs::write(brokers_root.join(".DS_Store"), b"metadata")
        .await
        .unwrap();

    let target_broker_home = codex_broker_home(root, "acct-target");
    expose_legacy_codex_state_to_broker_home(root, &target_broker_home)
        .await
        .unwrap();

    assert!(
        target_broker_home.join("history.jsonl").exists(),
        "regular broker-root files must not block continuity preparation"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_unsafe_broker_child_before_deferred_repair() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target_broker_home = codex_broker_home(root, "acct-target");
    let other_broker_home = codex_broker_home(root, "acct-other");
    let _other_lock = hold_codex_runtime_lock(&other_broker_home);

    tokio::fs::create_dir_all(&target_broker_home)
        .await
        .unwrap();
    let shared_home = codex_runtime_home(root);
    tokio::fs::create_dir_all(shared_home.join("sessions"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(shared_home.join("shell_snapshots"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(
        shared_home.join("sessions"),
        target_broker_home.join("sessions"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        shared_home.join("shell_snapshots"),
        target_broker_home.join("shell_snapshots"),
    )
    .unwrap();
    tokio::fs::write(
        target_broker_home.join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink("auth.json", target_broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &target_broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("target is not a declared Codex continuity child path"),
        "unsafe target broker child must fail closed before deferred repair, got: {error:#}"
    );
}

#[tokio::test]
async fn oauth_broker_home_waits_for_in_progress_repair_before_first_launch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target_broker_home = codex_broker_home(root, "acct-target");
    let shared_home = codex_runtime_home(root);
    tokio::fs::create_dir_all(&shared_home).await.unwrap();
    tokio::fs::write(shared_home.join("history.jsonl"), "shared-history\n")
        .await
        .unwrap();

    let repair_lock_root = shared_home.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&repair_lock_root).unwrap();
    let repair_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(repair_lock_root.join(".ctx-continuity-migration.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&repair_lock).unwrap();

    let root_for_task = root.to_path_buf();
    let broker_for_task = target_broker_home.clone();
    let handle = tokio::spawn(async move {
        expose_legacy_codex_state_to_broker_home(&root_for_task, &broker_for_task).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !handle.is_finished(),
        "first launch must wait for the in-progress continuity repair instead of launching unlinked"
    );

    drop(repair_lock);
    handle.await.unwrap().unwrap();
    assert_eq!(
        tokio::fs::read_to_string(target_broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "shared-history\n"
    );
}

#[tokio::test]
async fn oauth_broker_home_defers_legacy_import_when_legacy_home_active() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let target_broker_home = codex_broker_home(root, "acct-target");
    let legacy_home = legacy_codex_runtime_home(root);
    let _legacy_lock = hold_codex_runtime_lock(&legacy_home);

    let legacy_history = legacy_home.join("history.jsonl");
    tokio::fs::write(&legacy_history, "legacy-history\n")
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &target_broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("repair is blocked by an active runtime"),
        "missing broker links must fail closed while the legacy runtime home is active, got: {error:#}"
    );

    assert!(
        tokio::fs::symlink_metadata(codex_runtime_home(root).join("history.jsonl"))
            .await
            .is_err(),
        "migration must defer instead of importing from an active legacy runtime home"
    );
    assert!(
        tokio::fs::symlink_metadata(target_broker_home.join("history.jsonl"))
            .await
            .is_err(),
        "migration must not link broker continuity after deferring active legacy import"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_stale_link_to_auth_authority() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink("auth.json", broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("refusing to merge stale Codex continuity link target"),
        "expected auth-target stale link rejection, got {error:#}"
    );
    assert!(
        tokio::fs::symlink_metadata(codex_runtime_home(root).join("history.jsonl"))
            .await
            .is_err(),
        "auth authority must not be copied into shared continuity state"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_stale_link_to_same_named_undeclared_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    let external_dir = tempfile::tempdir().unwrap();
    let external_history = external_dir.path().join("history.jsonl");
    tokio::fs::write(
        &external_history,
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&external_history, broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("target is not a declared Codex continuity child path"),
        "expected undeclared stale link rejection, got {error:#}"
    );
    assert!(
        tokio::fs::symlink_metadata(codex_runtime_home(root).join("history.jsonl"))
            .await
            .is_err(),
        "undeclared same-name target must not be copied into shared continuity state"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_skips_symlinked_legacy_file_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    let legacy_home = legacy_codex_runtime_home(root);
    tokio::fs::create_dir_all(&legacy_home).await.unwrap();
    tokio::fs::write(
        legacy_home.join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink("auth.json", legacy_home.join("history.jsonl")).unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join("history.jsonl"))
            .await
            .unwrap(),
        ""
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        ""
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_stale_link_to_symlinked_legacy_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    let legacy_home = legacy_codex_runtime_home(root);
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::create_dir_all(&legacy_home).await.unwrap();
    tokio::fs::write(
        legacy_home.join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink("auth.json", legacy_home.join("history.jsonl")).unwrap();
    std::os::unix::fs::symlink(
        legacy_home.join("history.jsonl"),
        broker_home.join("history.jsonl"),
    )
    .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("target is a symlink"),
        "expected symlinked legacy child rejection, got {error:#}"
    );
    assert!(
        tokio::fs::symlink_metadata(codex_runtime_home(root).join("history.jsonl"))
            .await
            .is_err(),
        "symlinked legacy child must not be copied into shared continuity state"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_broker_link_through_symlink_alias() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    let alias_dir = tempfile::tempdir().unwrap();
    let shared_history = codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(shared_history.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&shared_history, "shared-history\n")
        .await
        .unwrap();
    let alias = alias_dir.path().join("history-alias");
    std::os::unix::fs::symlink(&shared_history, &alias).unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&alias, broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("target is a symlink"),
        "expected symlink alias rejection, got {error:#}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_broker_link_through_symlink_parent_alias() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    let alias_dir = tempfile::tempdir().unwrap();
    let shared_home = codex_runtime_home(root);
    let shared_history = shared_home.join("history.jsonl");
    tokio::fs::create_dir_all(&shared_home).await.unwrap();
    tokio::fs::write(&shared_history, "shared-history\n")
        .await
        .unwrap();
    let alias = alias_dir.path().join("shared-home-alias");
    std::os::unix::fs::symlink(&shared_home, &alias).unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(
        alias.join("history.jsonl"),
        broker_home.join("history.jsonl"),
    )
    .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("target path contains a symlink"),
        "expected symlink parent alias rejection, got {error:#}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_shared_file_before_repair() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink(
        broker_home.join("auth.json"),
        codex_runtime_home(root).join("history.jsonl"),
    )
    .unwrap();
    tokio::fs::write(broker_home.join("history.jsonl"), "broker-history\n")
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("must not be a symlink"),
        "expected symlinked shared child rejection, got {error:#}"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "broker-history\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_invalid_shared_file_before_linking() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("auth.json"),
        br#"{"tokens":{"access_token":"access","refresh_token":"refresh"}}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink(
        broker_home.join("auth.json"),
        codex_runtime_home(root).join("history.jsonl"),
    )
    .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("must not be a symlink"),
        "expected invalid shared child rejection, got {error:#}"
    );
    assert!(
        tokio::fs::symlink_metadata(broker_home.join("history.jsonl"))
            .await
            .is_err(),
        "broker must not be linked to invalid shared continuity child"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_wrong_kind_shared_child_for_existing_broker_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    let shared_history_dir = codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(&shared_history_dir)
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&shared_history_dir, broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("kind does not match manifest"),
        "expected wrong-kind shared child rejection, got {error:#}"
    );
}

#[tokio::test]
async fn oauth_broker_home_rejects_wrong_kind_broker_backup_before_merge() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let broker_home = codex_broker_home(root, "acct-target");
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(codex_runtime_home(root).join("history.jsonl"), "shared\n")
        .await
        .unwrap();
    tokio::fs::create_dir_all(broker_home.join("history.jsonl"))
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("kind does not match manifest"),
        "expected wrong-kind broker backup rejection, got {error:#}"
    );
    assert!(
        tokio::fs::symlink_metadata(broker_home.join("history.jsonl"))
            .await
            .unwrap()
            .file_type()
            .is_dir(),
        "failed repair should roll the wrong-kind broker child back"
    );
}

#[tokio::test]
async fn oauth_broker_home_rejects_wrong_kind_legacy_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let legacy_history_dir = legacy_codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(&legacy_history_dir)
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &codex_broker_home(root, "acct"))
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("kind does not match manifest"),
        "expected wrong-kind legacy child rejection, got {error:#}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_legacy_home_before_import() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside_home = root.join("outside-legacy-home");
    tokio::fs::create_dir_all(&outside_home).await.unwrap();
    tokio::fs::write(outside_home.join("history.jsonl"), "outside-history\n")
        .await
        .unwrap();
    let legacy_home = legacy_codex_runtime_home(root);
    tokio::fs::create_dir_all(legacy_home.parent().unwrap())
        .await
        .unwrap();
    std::os::unix::fs::symlink(&outside_home, &legacy_home).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &codex_broker_home(root, "acct"))
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("symlink component"),
        "expected symlinked legacy home rejection, got {error:#}"
    );
    assert!(
        !codex_runtime_home(root).join("history.jsonl").exists(),
        "symlinked legacy home must not be imported into shared continuity"
    );
    assert!(
        !outside_home.join(".ctx-continuity-runtime.lock").exists(),
        "symlinked legacy home must be rejected before acquiring an external runtime lock"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_shared_directory_before_merge() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let external_dir = tempfile::tempdir().unwrap();
    let legacy_rollout = legacy_codex_runtime_home(root).join("sessions/rollout.jsonl");
    tokio::fs::create_dir_all(legacy_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&legacy_rollout, "legacy-session\n")
        .await
        .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    std::os::unix::fs::symlink(
        external_dir.path(),
        codex_runtime_home(root).join("sessions"),
    )
    .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &codex_broker_home(root, "acct"))
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains("must not be a symlink"),
        "expected symlinked shared directory rejection, got {error:#}"
    );
    assert!(
        tokio::fs::symlink_metadata(external_dir.path().join("rollout.jsonl"))
            .await
            .is_err(),
        "merge must not follow shared directory symlink"
    );
}

async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn without(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.as_deref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[tokio::test]
async fn codex_env_mirrors_active_account_auth_into_runtime_home() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, "acct-123").await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_runtime_home(root).to_string_lossy());
    assert!(codex_runtime_home(root).exists());
    let mirrored = tokio::fs::read_to_string(codex_runtime_home(root).join("auth.json"))
        .await
        .unwrap();
    assert!(mirrored.contains("OPENAI_API_KEY"));
}

#[cfg(unix)]
#[tokio::test]
async fn codex_env_rejects_symlinked_runtime_home_before_api_key_projection() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, "acct-123").await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), codex_runtime_home(root)).unwrap();

    let error = codex_env_for_active_account(root).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("Codex runtime home path")
            && format!("{error:#}").contains("before broker storage access"),
        "symlinked runtime home must be rejected before API-key auth projection, got: {error:#}"
    );
    assert!(
        !outside.path().join("auth.json").exists(),
        "failed runtime-home validation must not write auth through the symlink"
    );
}

#[tokio::test]
async fn codex_account_deletion_marker_blocks_active_runtime_auth() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::without(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV);
    let _path_guard = EnvGuard::without(CTX_CODEX_HOST_AUTH_PATH_ENV);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, "acct-123").await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();

    begin_codex_account_deletion(root, "acct-123")
        .await
        .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = PathBuf::from(env.get("CODEX_HOME").unwrap());
    assert_eq!(home, codex_runtime_home(root));
    assert!(tokio::fs::metadata(home.join("auth.json")).await.is_err());
    assert!(codex_account_deletion_in_progress(root, "acct-123")
        .await
        .unwrap());
    let registry = load_codex_registry(root).await.unwrap();
    assert!(registry.active_account_id.is_none());
}

#[tokio::test]
async fn codex_env_projects_active_account_auth_into_runtime_root() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let runtime_root = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, "acct-123").await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();

    let env = codex_env_for_active_account_with_runtime_root(root, runtime_root.path())
        .await
        .unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(
        home,
        &codex_runtime_home(runtime_root.path()).to_string_lossy()
    );
    assert!(codex_runtime_home(runtime_root.path()).exists());
    let mirrored =
        tokio::fs::read_to_string(codex_runtime_home(runtime_root.path()).join("auth.json"))
            .await
            .unwrap();
    assert!(mirrored.contains("OPENAI_API_KEY"));
}

#[cfg(unix)]
#[tokio::test]
async fn codex_env_rejects_symlinked_sandbox_runtime_home_before_api_key_projection() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let runtime_root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, "acct-123").await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(runtime_root.path().join("providers/codex"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), codex_runtime_home(runtime_root.path())).unwrap();

    let error = codex_env_for_active_account_with_runtime_root(root, runtime_root.path())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Codex runtime home path")
            && format!("{error:#}").contains("before broker storage access"),
        "symlinked sandbox runtime home must be rejected before API-key auth projection, got: {error:#}"
    );
    assert!(
        !outside.path().join("auth.json").exists(),
        "failed sandbox runtime-home validation must not write auth through the symlink"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_env_rejects_symlinked_source_provider_root_before_sandbox_projection() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let runtime_root = tempfile::tempdir().unwrap();
    let root = dir.path();
    tokio::fs::create_dir_all(root.join("providers"))
        .await
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("providers/codex")).unwrap();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-123".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-123".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    tokio::fs::create_dir_all(outside.path().join("accounts/acct-123"))
        .await
        .unwrap();
    tokio::fs::write(
        outside.path().join("accounts/index.json"),
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        outside.path().join("accounts/acct-123/auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();

    let error = codex_env_for_active_account_with_runtime_root(root, runtime_root.path())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("Codex provider root path")
            && format!("{error:#}").contains("before broker storage access"),
        "symlinked source provider root must be rejected before sandbox registry/auth reads, got: {error:#}"
    );
    assert!(
        !codex_runtime_home(runtime_root.path())
            .join("auth.json")
            .exists(),
        "failed source provider-root validation must not project auth into the sandbox runtime"
    );
}

#[tokio::test]
async fn codex_env_defaults_to_runtime_home() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::without(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV);
    let _path_guard = EnvGuard::without(CTX_CODEX_HOST_AUTH_PATH_ENV);
    let dir = tempfile::tempdir().unwrap();

    let env = codex_env_for_active_account(dir.path()).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_runtime_home(dir.path()).to_string_lossy());
    assert!(codex_runtime_home(dir.path()).exists());
}

#[tokio::test]
async fn ensure_codex_endpoint_runtime_home_from_env_sets_container_accessible_codex_home() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = HashMap::new();
    env.insert("OPENAI_API_KEY".to_string(), "endpoint-key".to_string());
    env.insert(
        "OPENAI_BASE_URL".to_string(),
        "https://openrouter.ai/api/v1".to_string(),
    );
    env.insert(
        "CODEX_HOME".to_string(),
        "/tmp/host/endpoint-homes/abc123".to_string(),
    );

    ensure_codex_endpoint_runtime_home_from_env(dir.path(), &mut env)
        .await
        .unwrap();

    let codex_home = env.get("CODEX_HOME").cloned().unwrap_or_default();
    assert_eq!(codex_home, codex_runtime_home(dir.path()).to_string_lossy());
    let auth_path = Path::new(&codex_home).join("auth.json");
    let auth = tokio::fs::read_to_string(auth_path).await.unwrap();
    assert!(auth.contains("OPENAI_API_KEY"));
    assert!(auth.contains("endpoint-key"));
    assert!(auth.contains("OPENAI_BASE_URL"));
    assert!(auth.contains("https://openrouter.ai/api/v1"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let perms = tokio::fs::metadata(Path::new(&codex_home).join("auth.json"))
            .await
            .unwrap()
            .permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn ensure_codex_endpoint_runtime_home_from_env_uses_endpoint_home_auth_when_env_key_missing()
{
    let dir = tempfile::tempdir().unwrap();
    let endpoint_home = dir
        .path()
        .join("providers")
        .join("codex")
        .join("endpoint-homes")
        .join("ep-1");
    tokio::fs::create_dir_all(&endpoint_home).await.unwrap();
    tokio::fs::write(
        endpoint_home.join("auth.json"),
        br#"{"OPENAI_API_KEY":"endpoint-home-key","OPENAI_BASE_URL":"https://openrouter.ai/api/v1"}"#,
    )
    .await
    .unwrap();
    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        endpoint_home.to_string_lossy().to_string(),
    );

    ensure_codex_endpoint_runtime_home_from_env(dir.path(), &mut env)
        .await
        .unwrap();

    let codex_home = env.get("CODEX_HOME").cloned().unwrap_or_default();
    assert_eq!(codex_home, codex_runtime_home(dir.path()).to_string_lossy());
    assert_eq!(
        env.get("OPENAI_API_KEY").map(String::as_str),
        Some("endpoint-home-key")
    );
    assert_eq!(
        env.get("OPENAI_BASE_URL").map(String::as_str),
        Some("https://openrouter.ai/api/v1")
    );
    let auth = tokio::fs::read_to_string(Path::new(&codex_home).join("auth.json"))
        .await
        .unwrap();
    assert!(auth.contains("endpoint-home-key"));
    assert!(auth.contains("https://openrouter.ai/api/v1"));
}

#[tokio::test]
async fn ensure_codex_endpoint_runtime_home_from_env_errors_without_env_key_or_endpoint_auth() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = HashMap::new();
    let err = ensure_codex_endpoint_runtime_home_from_env(dir.path(), &mut env)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(
        "missing OPENAI_API_KEY and CODEX_HOME while preparing codex endpoint runtime home"
    ));
}

#[tokio::test]
async fn ensure_provider_runtime_home_env_sets_home_and_xdg_dirs_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = HashMap::new();

    ensure_provider_runtime_home_env(dir.path(), "opencode", &mut env)
        .await
        .unwrap();

    let home = env.get("HOME").cloned().unwrap_or_default();
    let expected_config = Path::new(&home)
        .join(".config")
        .to_string_lossy()
        .to_string();
    let expected_cache = Path::new(&home)
        .join(".cache")
        .to_string_lossy()
        .to_string();
    let expected_data = Path::new(&home)
        .join(".local/share")
        .to_string_lossy()
        .to_string();
    let expected_state = Path::new(&home)
        .join(".local/state")
        .to_string_lossy()
        .to_string();
    assert_eq!(
        home,
        dir.path()
            .join("providers")
            .join("opencode")
            .join("home")
            .to_string_lossy()
    );
    assert_eq!(
        env.get("XDG_CONFIG_HOME").map(String::as_str),
        Some(expected_config.as_str())
    );
    assert_eq!(
        env.get("XDG_CACHE_HOME").map(String::as_str),
        Some(expected_cache.as_str())
    );
    assert_eq!(
        env.get("XDG_DATA_HOME").map(String::as_str),
        Some(expected_data.as_str())
    );
    assert_eq!(
        env.get("XDG_STATE_HOME").map(String::as_str),
        Some(expected_state.as_str())
    );
}

#[tokio::test]
async fn codex_env_seeds_runtime_home_from_host_when_enabled_without_active_account() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::set(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV, "1");
    let host = tempfile::tempdir().unwrap();
    let host_auth = host.path().join("auth.json");
    tokio::fs::write(&host_auth, br#"{"OPENAI_API_KEY":"seeded-key"}"#)
        .await
        .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        host_auth.to_string_lossy().as_ref(),
    );
    let dir = tempfile::tempdir().unwrap();

    let env = codex_env_for_active_account(dir.path()).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_runtime_home(dir.path()).to_string_lossy());
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
}

#[tokio::test]
async fn codex_env_does_not_implicitly_copy_host_auth_without_active_account() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::without(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV);
    let host = tempfile::tempdir().unwrap();
    let host_auth = host.path().join("auth.json");
    tokio::fs::write(
        &host_auth,
        br#"{"tokens":{"access_token":"seeded-access","refresh_token":"seeded-refresh"}}"#,
    )
    .await
    .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        host_auth.to_string_lossy().as_ref(),
    );
    let dir = tempfile::tempdir().unwrap();

    let env = codex_env_for_active_account(dir.path()).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_runtime_home(dir.path()).to_string_lossy());
    let err = ensure_codex_auth_ready(Path::new(home)).await.unwrap_err();
    assert!(err.to_string().contains("missing codex auth file"));
}

#[tokio::test]
async fn codex_env_seed_enabled_fails_when_host_auth_missing() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::set(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV, "1");
    let missing = tempfile::tempdir()
        .unwrap()
        .path()
        .join("missing-auth.json");
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        missing.to_string_lossy().as_ref(),
    );
    let dir = tempfile::tempdir().unwrap();

    let err = codex_env_for_active_account(dir.path()).await.unwrap_err();
    assert!(err.to_string().contains("host auth file is missing"));
}

#[tokio::test]
async fn codex_env_clears_stale_runtime_auth_when_no_active_account() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let _seed_guard = EnvGuard::without(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV);
    let missing_host_auth = tempfile::tempdir()
        .unwrap()
        .path()
        .join("missing-auth.json");
    let _host_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        missing_host_auth.to_string_lossy().as_ref(),
    );
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"OPENAI_API_KEY":"stale-key"}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, "acct-stale")
        .await
        .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_runtime_home(root).to_string_lossy());
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(!codex_runtime_owner_path(root).exists());
}

#[tokio::test]
async fn codex_auth_preflight_accepts_api_key_shape() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(
        dir.path().join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();
    ensure_codex_auth_ready(dir.path()).await.unwrap();
}

#[tokio::test]
async fn codex_auth_preflight_accepts_access_token_only_oauth_shape() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(
        dir.path().join("auth.json"),
        br#"{"tokens":{"access_token":"a"}}"#,
    )
    .await
    .unwrap();
    ensure_codex_auth_ready(dir.path()).await.unwrap();
}

#[tokio::test]
async fn codex_auth_preflight_rejects_missing_supported_fields() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("auth.json"), br#"{"tokens":{}}"#)
        .await
        .unwrap();
    let err = ensure_codex_auth_ready(dir.path()).await.unwrap_err();
    assert!(err.to_string().contains("tokens.access_token"));
}

fn codex_test_jwt(exp: i64) -> String {
    use base64::Engine as _;

    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::json!({ "exp": exp }).to_string());
    format!("{header}.{payload}.sig")
}

fn codex_test_fresh_jwt() -> String {
    codex_test_jwt(Utc::now().timestamp() + 3600)
}

async fn spawn_codex_oauth_refresh_server(
    status: u16,
    access_token: String,
    refresh_token: String,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    spawn_codex_oauth_refresh_server_with_error_body(
        status,
        access_token,
        refresh_token,
        r#"{"error":"refresh failed"}"#.to_string(),
    )
    .await
}

async fn spawn_codex_oauth_refresh_server_with_error_body(
    status: u16,
    access_token: String,
    refresh_token: String,
    error_body: String,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let calls_for_task = std::sync::Arc::clone(&calls);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            calls_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            let mut expected_request_len = None;
            loop {
                let read = match socket.read(&mut buffer).await {
                    Ok(read) => read,
                    Err(_) => 0,
                };
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if expected_request_len.is_none() {
                    if let Some(headers_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..headers_end]);
                        let content_length = headers
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find_map(|(name, value)| {
                                if name.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        expected_request_len = Some(headers_end + 4 + content_length);
                    }
                }
                if expected_request_len.is_some_and(|expected| request.len() >= expected) {
                    break;
                }
            }
            let body_start = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
                .unwrap_or(request.len());
            let headers = String::from_utf8_lossy(&request[..body_start]).to_ascii_lowercase();
            let request_body = String::from_utf8_lossy(&request[body_start..]);
            let scope_is_encoded = request_body.contains("scope=openid+profile+email")
                || request_body.contains("scope=openid%20profile%20email");
            let request_is_form_encoded = headers
                .contains("content-type: application/x-www-form-urlencoded")
                && request_body.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann")
                && request_body.contains("grant_type=refresh_token")
                && request_body.contains("refresh_token=")
                && scope_is_encoded;
            let (effective_status, reason, body) = if !request_is_form_encoded {
                (
                    415_u16,
                    "Unsupported Media Type",
                    format!("bad refresh request: {request_body}"),
                )
            } else if status == 200 {
                (
                    status,
                    "OK",
                    serde_json::json!({
                        "access_token": access_token,
                        "refresh_token": refresh_token,
                        "id_token": "fresh-id"
                    })
                    .to_string(),
                )
            } else {
                (status, "Internal Server Error", error_body.clone())
            };
            let response = format!(
                "HTTP/1.1 {effective_status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.as_bytes().len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{addr}/oauth/token"), calls)
}

#[tokio::test]
async fn codex_broker_refresh_concurrent_launches_singleflight_and_uses_broker_home() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let fresh_access = codex_test_jwt(Utc::now().timestamp() + 3600);
    let (token_url, calls) =
        spawn_codex_oauth_refresh_server(200, fresh_access.clone(), "fresh-refresh".to_string())
            .await;
    let _token_guard = EnvGuard::set("CTX_CODEX_OAUTH_TOKEN_URL", &token_url);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let expired_access = codex_test_jwt(Utc::now().timestamp() - 60);
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "OPENAI_API_KEY": "must-not-project",
                "tokens": {
                    "access_token": expired_access,
                    "refresh_token": "stale-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let (first, second) = tokio::join!(
        codex_env_for_active_account(root),
        codex_env_for_active_account(root)
    );
    let first = first.unwrap();
    let second = second.unwrap();

    assert_eq!(first.get("CODEX_HOME"), second.get("CODEX_HOME"));
    let runtime_home = codex_broker_home(root, account_id)
        .to_string_lossy()
        .to_string();
    assert_eq!(
        first.get("CODEX_HOME").map(String::as_str),
        Some(runtime_home.as_str())
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let runtime_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(runtime_payload.contains(&fresh_access));
    assert!(runtime_payload.contains("fresh-refresh"));
    assert!(!runtime_payload.contains("OPENAI_API_KEY"));
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("fresh-refresh"));
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, &secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("fresh-refresh"));
}

#[tokio::test]
async fn codex_broker_refreshes_access_token_without_readable_exp() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let fresh_access = codex_test_fresh_jwt();
    let (token_url, calls) =
        spawn_codex_oauth_refresh_server(200, fresh_access.clone(), "fresh-refresh".to_string())
            .await;
    let _token_guard = EnvGuard::set("CTX_CODEX_OAUTH_TOKEN_URL", &token_url);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": "access-without-readable-exp",
                    "refresh_token": "stale-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let runtime_home = codex_broker_home(root, account_id);
    let runtime_home_text = runtime_home.to_string_lossy().to_string();
    assert_eq!(
        env.get("CODEX_HOME").map(String::as_str),
        Some(runtime_home_text.as_str())
    );
    let runtime_payload = tokio::fs::read_to_string(runtime_home.join("auth.json"))
        .await
        .unwrap();
    assert!(runtime_payload.contains(&fresh_access));
    assert!(runtime_payload.contains("fresh-refresh"));
    assert!(!runtime_payload.contains("stale-refresh"));
    assert!(!runtime_payload.contains("access-without-readable-exp"));
}

#[tokio::test]
async fn codex_broker_refresh_failure_does_not_project_partial_runtime_auth() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let (token_url, calls) = spawn_codex_oauth_refresh_server(
        500,
        "unused-access".to_string(),
        "unused-refresh".to_string(),
    )
    .await;
    let _token_guard = EnvGuard::set("CTX_CODEX_OAUTH_TOKEN_URL", &token_url);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let expired_access = codex_test_jwt(Utc::now().timestamp() - 60);
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": expired_access,
                    "refresh_token": "stale-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let error = codex_env_for_active_account(root).await.unwrap_err();

    assert!(
        format!("{error:#}").contains("Codex OAuth refresh failed"),
        "unexpected error: {error:#}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(!codex_oauth_runtime_home(root, account_id)
        .unwrap()
        .join("auth.json")
        .exists());
}

#[tokio::test]
async fn codex_usage_env_projects_current_access_only_without_refreshing() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let fresh_access = codex_test_fresh_jwt();
    let (token_url, calls) =
        spawn_codex_oauth_refresh_server(200, fresh_access, "fresh-refresh".to_string()).await;
    let _token_guard = EnvGuard::set("CTX_CODEX_OAUTH_TOKEN_URL", &token_url);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let expired_access = codex_test_jwt(Utc::now().timestamp() - 60);
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": expired_access,
                    "refresh_token": "stale-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let env = codex_usage_env_for_active_account(root).await.unwrap();
    let account_env = codex_usage_env_for_account(root, account_id).await.unwrap();

    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let runtime_home = codex_oauth_runtime_home(root, account_id).unwrap();
    let runtime_home_text = runtime_home.to_string_lossy().to_string();
    assert_eq!(
        env.get("CODEX_HOME").map(String::as_str),
        Some(runtime_home_text.as_str())
    );
    assert_eq!(
        account_env.get("CODEX_HOME").map(String::as_str),
        Some(runtime_home_text.as_str())
    );
    let runtime_payload = tokio::fs::read_to_string(runtime_home.join("auth.json"))
        .await
        .unwrap();
    assert!(runtime_payload.contains(&expired_access));
    assert!(!runtime_payload.contains("refresh_token"));
}

#[tokio::test]
async fn invalid_codex_refresh_token_enters_terminal_reauth_state() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let (token_url, calls) = spawn_codex_oauth_refresh_server_with_error_body(
        400,
        "unused-access".to_string(),
        "unused-refresh".to_string(),
        r#"{"error":"refresh_token_reused","error_description":"Refresh token expired"}"#
            .to_string(),
    )
    .await;
    let _token_guard = EnvGuard::set("CTX_CODEX_OAUTH_TOKEN_URL", &token_url);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let expired_access = codex_test_jwt(Utc::now().timestamp() - 60);
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": expired_access,
                    "refresh_token": "stale-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let first_error = codex_env_for_active_account(root).await.unwrap_err();
    assert!(
        format!("{first_error:#}").contains("Sign in to Codex again through ctx"),
        "unexpected first error: {first_error:#}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    let second_error = codex_env_for_active_account(root).await.unwrap_err();
    assert!(
        format!("{second_error:#}").contains("reauthentication is required"),
        "unexpected terminal error: {second_error:#}"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "terminal reauth state must not retry the invalid refresh token"
    );

    let repaired_access = codex_test_jwt(Utc::now().timestamp() + 3600);
    let repaired_auth = serde_json::json!({
        "tokens": {
            "access_token": repaired_access,
            "refresh_token": "fresh-refresh",
            "account_id": "upstream-acct"
        }
    });
    let repaired =
        import_codex_auth_value_to_secret_store(root, Some("acct".to_string()), &repaired_auth)
            .await
            .unwrap();
    assert_eq!(repaired.account_id, account_id);
    assert!(!repaired.created);

    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("fresh-refresh"));
    assert!(!broker_payload.contains("stale-refresh"));

    let registry = load_codex_registry(root).await.unwrap();
    let entry = registry
        .accounts
        .iter()
        .find(|entry| entry.id == account_id)
        .unwrap();
    let secret_payload = tokio::fs::read_to_string(
        codex_secret_path(root, entry.secret_ref.as_deref().unwrap()).unwrap(),
    )
    .await
    .unwrap();
    assert!(secret_payload.contains("fresh-refresh"));
    assert!(!secret_payload.contains("stale-refresh"));

    let env = codex_env_for_active_account(root).await.unwrap();
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "fresh reauth import must not retry the invalid broker refresh token"
    );
    let runtime_auth =
        tokio::fs::read_to_string(Path::new(env.get("CODEX_HOME").unwrap()).join("auth.json"))
            .await
            .unwrap();
    assert!(runtime_auth.contains(&repaired_access));
    assert!(runtime_auth.contains("fresh-refresh"));
}

#[tokio::test]
async fn ingested_secret_projects_even_without_account_dir_auth() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-123";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, account_id).await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();

    ingest_codex_account_auth_to_secret_store(root, account_id)
        .await
        .unwrap();
    tokio::fs::remove_file(account_dir.join("auth.json"))
        .await
        .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_runtime_home(root).to_string_lossy());
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
}

#[tokio::test]
async fn oauth_secret_uses_broker_home_and_retains_broker_refresh() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    let access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": access,
                    "refresh_token": "refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    let expected_home = codex_broker_home(root, account_id)
        .to_string_lossy()
        .to_string();
    assert_eq!(home, &expected_home);
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
    let runtime_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(runtime_payload.contains(&access));
    assert!(runtime_payload.contains("refresh"));
    assert!(!runtime_payload.contains("OPENAI_API_KEY"));
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("refresh"));
}

#[tokio::test]
async fn existing_oauth_broker_home_strips_api_key_before_launch() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let secret_access = codex_test_fresh_jwt();
    let broker_access = codex_test_fresh_jwt();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "OPENAI_API_KEY": "secret-key-must-not-launch",
                "tokens": {
                    "access_token": secret_access,
                    "refresh_token": "secret-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();
    let broker_home = codex_broker_home(root, account_id);
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("auth.json"),
        serde_json::json!({
            "OPENAI_API_KEY": "broker-key-must-not-launch",
            "tokens": {
                "access_token": broker_access,
                "refresh_token": "broker-refresh",
                "account_id": "upstream-acct"
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();

    assert_eq!(
        env.get("CODEX_HOME").map(String::as_str),
        Some(broker_home.to_string_lossy().as_ref())
    );
    let broker_payload = tokio::fs::read_to_string(broker_home.join("auth.json"))
        .await
        .unwrap();
    assert!(broker_payload.contains(&broker_access));
    assert!(broker_payload.contains("broker-refresh"));
    assert!(!broker_payload.contains("OPENAI_API_KEY"));
    assert!(!broker_payload.contains("broker-key-must-not-launch"));
}

#[tokio::test]
async fn oauth_broker_home_is_account_scoped_when_active_account_changes() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let first_id = "acct-oauth-a";
    let second_id = "acct-oauth-b";
    let first_ref = format!("{first_id}.json");
    let second_ref = format!("{second_id}.json");
    let first_access = codex_test_jwt(Utc::now().timestamp() + 3600);
    let second_access = codex_test_jwt(Utc::now().timestamp() + 7200);
    let registry = CodexAccountRegistry {
        active_account_id: Some(first_id.to_string()),
        accounts: vec![
            CodexAccountEntry {
                id: first_id.to_string(),
                label: "acct a".to_string(),
                kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: Some("upstream-a".to_string()),
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: None,
                secret_ref: Some(first_ref.clone()),
                endpoint_profile: CodexEndpointProfile::default(),
            },
            CodexAccountEntry {
                id: second_id.to_string(),
                label: "acct b".to_string(),
                kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: Some("upstream-b".to_string()),
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: None,
                secret_ref: Some(second_ref.clone()),
                endpoint_profile: CodexEndpointProfile::default(),
            },
        ],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &first_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": first_access,
                    "refresh_token": "refresh-a",
                    "account_id": "upstream-a"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &second_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": second_access,
                    "refresh_token": "refresh-b",
                    "account_id": "upstream-b"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let first_env = codex_env_for_active_account(root).await.unwrap();
    set_active_codex_account(root, Some(second_id.to_string()))
        .await
        .unwrap();
    let second_env = codex_env_for_active_account(root).await.unwrap();

    let first_home = codex_broker_home(root, first_id);
    let second_home = codex_broker_home(root, second_id);
    let first_home_text = first_home.to_string_lossy().to_string();
    let second_home_text = second_home.to_string_lossy().to_string();
    assert_ne!(first_home, second_home);
    assert_eq!(
        first_env.get("CODEX_HOME").map(String::as_str),
        Some(first_home_text.as_str())
    );
    assert_eq!(
        second_env.get("CODEX_HOME").map(String::as_str),
        Some(second_home_text.as_str())
    );
    let first_payload = tokio::fs::read_to_string(first_home.join("auth.json"))
        .await
        .unwrap();
    let second_payload = tokio::fs::read_to_string(second_home.join("auth.json"))
        .await
        .unwrap();
    assert!(first_payload.contains(&first_access));
    assert!(!first_payload.contains(&second_access));
    assert!(first_payload.contains("refresh-a"));
    assert!(second_payload.contains(&second_access));
    assert!(!second_payload.contains(&first_access));
    assert!(second_payload.contains("refresh-b"));
}

#[tokio::test]
async fn oauth_broker_home_exposes_legacy_session_state_without_copying_auth() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    let access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": access,
                    "refresh_token": "refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let legacy_home = codex_runtime_home(root);
    let legacy_rollout = legacy_home.join(
        "sessions/2026/04/26/rollout-2026-04-26T18-38-41-019dcc28-d7b1-7233-9bf1-2d34c2752b42.jsonl",
    );
    let legacy_snapshot =
        legacy_home.join("shell_snapshots/019dcc28-d7b1-7233-9bf1-2d34c2752b42.0001.sh");
    tokio::fs::create_dir_all(legacy_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(legacy_snapshot.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&legacy_rollout, "{\"type\":\"session.opened\"}\n")
        .await
        .unwrap();
    tokio::fs::write(&legacy_snapshot, "export PWD=/tmp/project\n")
        .await
        .unwrap();
    tokio::fs::write(
        legacy_home.join("history.jsonl"),
        "{\"session\":\"legacy\"}\n",
    )
    .await
    .unwrap();
    tokio::fs::write(legacy_home.join("config.toml"), "[projects]\n")
        .await
        .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    let broker_home = codex_broker_home(root, account_id);
    let expected_home = codex_broker_home(root, account_id)
        .to_string_lossy()
        .to_string();
    assert_eq!(home, &expected_home);
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join(
            "sessions/2026/04/26/rollout-2026-04-26T18-38-41-019dcc28-d7b1-7233-9bf1-2d34c2752b42.jsonl",
        ))
        .await
        .unwrap(),
        "{\"type\":\"session.opened\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(
            broker_home.join("shell_snapshots/019dcc28-d7b1-7233-9bf1-2d34c2752b42.0001.sh",)
        )
        .await
        .unwrap(),
        "export PWD=/tmp/project\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"legacy\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("config.toml"))
            .await
            .unwrap(),
        "[projects]\n"
    );

    #[cfg(unix)]
    {
        assert!(tokio::fs::symlink_metadata(broker_home.join("sessions"))
            .await
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(
            tokio::fs::symlink_metadata(broker_home.join("shell_snapshots"))
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(
        !codex_oauth_runtime_home(root, account_id)
            .unwrap()
            .join("auth.json")
            .exists(),
        "normal host launches must use the broker OAuth home, not a copied account runtime home"
    );
    assert!(
        tokio::fs::read_to_string(broker_home.join("auth.json"))
            .await
            .unwrap()
            .contains("refresh_token"),
        "the broker home remains the OAuth refresh-token authority"
    );
}

#[tokio::test]
async fn oauth_broker_home_repairs_preinitialized_continuity_dirs() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    let access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": access,
                    "refresh_token": "refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let shared_rollout = codex_runtime_home(root).join(
        "sessions/2026/04/26/rollout-2026-04-26T18-38-41-019dcc28-d7b1-7233-9bf1-2d34c2752b42.jsonl",
    );
    let broker_rollout = codex_broker_home(root, account_id).join(
        "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000001.jsonl",
    );
    let broker_snapshot = codex_broker_home(root, account_id)
        .join("shell_snapshots/019e2700-0000-7000-9000-000000000001.0001.sh");
    let broker_prompt = codex_broker_home(root, account_id).join("prompts/custom.md");
    tokio::fs::create_dir_all(shared_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(broker_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(broker_snapshot.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(broker_prompt.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&shared_rollout, "{\"thread\":\"legacy-shared\"}\n")
        .await
        .unwrap();
    tokio::fs::write(&broker_rollout, "{\"thread\":\"broker-local\"}\n")
        .await
        .unwrap();
    tokio::fs::write(&broker_snapshot, "export PWD=/tmp/broker\n")
        .await
        .unwrap();
    tokio::fs::write(&broker_prompt, "custom prompt\n")
        .await
        .unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let runtime_home = PathBuf::from(env.get("CODEX_HOME").unwrap());
    let broker_home = codex_broker_home(root, account_id);
    assert_eq!(runtime_home, broker_home);
    assert_eq!(
        tokio::fs::read_to_string(&shared_rollout).await.unwrap(),
        "{\"thread\":\"legacy-shared\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(runtime_home.join(
            "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000001.jsonl",
        ))
        .await
        .unwrap(),
        "{\"thread\":\"broker-local\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join(
            "sessions/2026/04/26/rollout-2026-04-26T18-38-41-019dcc28-d7b1-7233-9bf1-2d34c2752b42.jsonl",
        ))
        .await
        .unwrap(),
        "{\"thread\":\"legacy-shared\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(
            broker_home.join("shell_snapshots/019e2700-0000-7000-9000-000000000001.0001.sh",)
        )
        .await
        .unwrap(),
        "export PWD=/tmp/broker\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("prompts/custom.md"))
            .await
            .unwrap(),
        "custom prompt\n"
    );

    #[cfg(unix)]
    {
        assert!(tokio::fs::symlink_metadata(broker_home.join("sessions"))
            .await
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(
            tokio::fs::symlink_metadata(broker_home.join("shell_snapshots"))
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(tokio::fs::symlink_metadata(broker_home.join("prompts"))
            .await
            .unwrap()
            .file_type()
            .is_symlink());
    }
    assert!(
        broker_home
            .join(".ctx-continuity-migration-backups/sessions.0")
            .exists(),
        "preinitialized broker sessions should be backed up before link repair"
    );
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(
        tokio::fs::read_to_string(runtime_home.join("auth.json"))
            .await
            .unwrap()
            .contains("refresh_token"),
        "the broker home remains the OAuth refresh-token authority"
    );
    assert!(
        !codex_oauth_runtime_home(root, account_id)
            .unwrap()
            .join("auth.json")
            .exists(),
        "normal host launches must not create a copied account runtime home"
    );
}

#[tokio::test]
async fn oauth_broker_home_links_missing_continuity_files_before_launch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert!(codex_runtime_home(root).join("history.jsonl").exists());
    assert!(codex_runtime_home(root).join("config.toml").exists());
    assert!(broker_home.join("history.jsonl").exists());
    assert!(broker_home.join("config.toml").exists());
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::canonicalize(std::fs::read_link(broker_home.join("history.jsonl")).unwrap())
                .unwrap(),
            std::fs::canonicalize(codex_runtime_home(root).join("history.jsonl")).unwrap()
        );
        assert_eq!(
            std::fs::canonicalize(std::fs::read_link(broker_home.join("config.toml")).unwrap())
                .unwrap(),
            std::fs::canonicalize(codex_runtime_home(root).join("config.toml")).unwrap()
        );
    }
}

#[tokio::test]
async fn oauth_broker_home_allows_system_temp_symlink_ancestors() {
    let dir = tempfile::Builder::new()
        .prefix("ctx-provider-accounts-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert!(broker_home.join("history.jsonl").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_accepts_existing_link_through_system_temp_alias() {
    let dir = tempfile::Builder::new()
        .prefix("ctx-provider-accounts-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let shared_history = codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(shared_history.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&shared_history, "{\"thread\":\"shared\"}\n")
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&shared_history, broker_home.join("history.jsonl")).unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"thread\":\"shared\"}\n"
    );
}

#[tokio::test]
async fn oauth_broker_home_adopts_broker_local_file_before_placeholder_creation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("history.jsonl"),
        "{\"session\":\"broker-only\"}\n",
    )
    .await
    .unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"broker-only\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"broker-only\"}\n"
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read_link(broker_home.join("history.jsonl")).unwrap(),
        codex_runtime_home(root).join("history.jsonl")
    );
}

#[tokio::test]
async fn oauth_broker_home_does_not_overwrite_empty_shared_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(codex_runtime_home(root).join("history.jsonl"), "")
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("history.jsonl"),
        "{\"session\":\"broker-real\"}\n",
    )
    .await
    .unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join("history.jsonl"))
            .await
            .unwrap(),
        ""
    );
    assert_eq!(
        tokio::fs::read_to_string(
            broker_home
                .join(".ctx-continuity-migration-backups")
                .join("history.jsonl.0")
        )
        .await
        .unwrap(),
        "{\"session\":\"broker-real\"}\n"
    );
}

#[tokio::test]
async fn oauth_broker_home_active_broker_home_defers_repair_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let broker_rollout = broker_home.join(
        "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000004.jsonl",
    );
    tokio::fs::create_dir_all(broker_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&broker_rollout, "{\"thread\":\"active-broker\"}\n")
        .await
        .unwrap();

    let _lock = hold_codex_runtime_lock(&broker_home);

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("requires repair while repair is blocked"),
        "broker-local active state should fail closed without mutation, got: {error:#}"
    );
    assert!(tokio::fs::symlink_metadata(broker_home.join("sessions"))
        .await
        .unwrap()
        .file_type()
        .is_dir());
    assert_eq!(
        tokio::fs::read_to_string(&broker_rollout).await.unwrap(),
        "{\"thread\":\"active-broker\"}\n"
    );
    assert!(
        !codex_runtime_home(root)
            .join(
                "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000004.jsonl",
            )
            .exists(),
        "active broker state must not be moved while the broker runtime lease is held"
    );
}

#[tokio::test]
async fn oauth_broker_home_resumes_pending_backup_when_destination_missing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let backup = broker_home.join(".ctx-continuity-migration-backups/history.jsonl.0");
    tokio::fs::create_dir_all(backup.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&backup, "{\"thread\":\"pending-backup\"}\n")
        .await
        .unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join("history.jsonl"))
            .await
            .unwrap(),
        "{\"thread\":\"pending-backup\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"thread\":\"pending-backup\"}\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_symlinked_backup_root_before_moving_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let external_backup_root = root.join("external-backups");
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::create_dir_all(&external_backup_root)
        .await
        .unwrap();
    std::os::unix::fs::symlink(
        &external_backup_root,
        broker_home.join(".ctx-continuity-migration-backups"),
    )
    .unwrap();
    tokio::fs::write(
        broker_home.join("history.jsonl"),
        "{\"thread\":\"broker-local\"}\n",
    )
    .await
    .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("backup root"),
        "expected symlinked backup root rejection, got {error:#}"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"thread\":\"broker-local\"}\n",
        "failed repair must leave the broker child in place"
    );
    assert!(
        std::fs::read_dir(&external_backup_root)
            .unwrap()
            .next()
            .is_none(),
        "symlinked backup root must not receive moved continuity state"
    );
}

#[tokio::test]
async fn oauth_broker_home_repair_rolls_back_failed_backup_move() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(
        broker_home.join("history.jsonl"),
        "{\"thread\":\"rollback\"}\n",
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root).join("history.jsonl"))
        .await
        .unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("repairing preexisting broker Codex state"),
        "expected repair failure, got {error:#}"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"thread\":\"rollback\"}\n",
        "failed repair should roll the moved broker file back into the active path"
    );
    assert!(
        tokio::fs::symlink_metadata(broker_home.join("history.jsonl"))
            .await
            .unwrap()
            .file_type()
            .is_file()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_read_only_probe_allows_active_correct_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let shared_home = codex_runtime_home(root);
    tokio::fs::create_dir_all(shared_home.join("sessions"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(shared_home.join("shell_snapshots"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(shared_home.join("prompts"))
        .await
        .unwrap();
    tokio::fs::write(
        shared_home.join("history.jsonl"),
        "{\"session\":\"shared\"}\n",
    )
    .await
    .unwrap();
    tokio::fs::write(shared_home.join("config.toml"), "[projects]\n")
        .await
        .unwrap();
    let legacy_home = legacy_codex_runtime_home(root);
    tokio::fs::create_dir_all(&legacy_home).await.unwrap();
    tokio::fs::write(
        legacy_home.join("history.jsonl"),
        "{\"session\":\"legacy\"}\n",
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(shared_home.join("sessions"), broker_home.join("sessions")).unwrap();
    std::os::unix::fs::symlink(
        shared_home.join("shell_snapshots"),
        broker_home.join("shell_snapshots"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        shared_home.join("history.jsonl"),
        broker_home.join("history.jsonl"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        shared_home.join("config.toml"),
        broker_home.join("config.toml"),
    )
    .unwrap();
    std::os::unix::fs::symlink(shared_home.join("prompts"), broker_home.join("prompts")).unwrap();

    let _lock = hold_codex_runtime_lock(&broker_home);

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"shared\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(shared_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"shared\"}\n",
        "active correct-link probes must not merge legacy state while the app-server lock is held"
    );
}

#[tokio::test]
async fn oauth_broker_home_keeps_existing_shared_history_idempotent() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    let access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": access,
                    "refresh_token": "refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::create_dir_all(legacy_codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("history.jsonl"),
        "{\"session\":\"shared\"}\n",
    )
    .await
    .unwrap();
    tokio::fs::write(
        legacy_codex_runtime_home(root).join("history.jsonl"),
        "{\"session\":\"legacy\"}\n",
    )
    .await
    .unwrap();

    let first_env = codex_env_for_active_account(root).await.unwrap();
    let second_env = codex_env_for_active_account(root).await.unwrap();
    let broker_home = codex_broker_home(root, account_id);
    let runtime_home = codex_broker_home(root, account_id)
        .to_string_lossy()
        .to_string();
    assert_eq!(first_env.get("CODEX_HOME"), second_env.get("CODEX_HOME"));
    assert_eq!(
        second_env.get("CODEX_HOME").map(String::as_str),
        Some(runtime_home.as_str())
    );
    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"shared\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"shared\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(legacy_codex_runtime_home(root).join("history.jsonl"))
            .await
            .unwrap(),
        "{\"session\":\"legacy\"}\n"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn oauth_broker_home_concurrent_repair_does_not_deadlock_current_thread() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let broker_rollout = broker_home.join(
        "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000002.jsonl",
    );
    tokio::fs::create_dir_all(broker_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&broker_rollout, "{\"thread\":\"broker-local\"}\n")
        .await
        .unwrap();

    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            expose_legacy_codex_state_to_broker_home(root, &broker_home),
            expose_legacy_codex_state_to_broker_home(root, &broker_home)
        )
    })
    .await
    .expect("concurrent broker repair should not deadlock the current-thread runtime");
    first.unwrap();
    second.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join(
            "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000002.jsonl",
        ))
        .await
        .unwrap(),
        "{\"thread\":\"broker-local\"}\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_retargets_stale_continuity_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let legacy_sessions = legacy_codex_runtime_home(root).join("sessions");
    let legacy_rollout = legacy_sessions
        .join("2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000003.jsonl");
    tokio::fs::create_dir_all(legacy_rollout.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    tokio::fs::write(&legacy_rollout, "{\"thread\":\"legacy-link\"}\n")
        .await
        .unwrap();
    std::os::unix::fs::symlink(&legacy_sessions, broker_home.join("sessions")).unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_link(broker_home.join("sessions")).unwrap(),
        codex_runtime_home(root).join("sessions")
    );
    assert_eq!(
        tokio::fs::read_to_string(codex_runtime_home(root).join(
            "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000003.jsonl",
        ))
        .await
        .unwrap(),
        "{\"thread\":\"legacy-link\"}\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(broker_home.join(
            "sessions/2026/05/14/rollout-2026-05-14T19-20-30-019e2700-0000-7000-9000-000000000003.jsonl",
        ))
        .await
        .unwrap(),
        "{\"thread\":\"legacy-link\"}\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_retargets_broken_stale_directory_link_to_created_canonical_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(
        legacy_codex_runtime_home(root).join("missing-sessions"),
        broker_home.join("sessions"),
    )
    .unwrap();

    expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap();

    assert!(codex_runtime_home(root).join("sessions").is_dir());
    assert_eq!(
        std::fs::read_link(broker_home.join("sessions")).unwrap(),
        codex_runtime_home(root).join("sessions")
    );
    assert!(broker_home.join("sessions").is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_stale_file_link_to_special_file_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let empty_legacy_home = root.join("empty-legacy-home");
    let stale_target = legacy_codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(&empty_legacy_home).await.unwrap();
    tokio::fs::create_dir_all(stale_target.parent().unwrap())
        .await
        .unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&stale_target).unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&stale_target, broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_from_home(root, &empty_legacy_home, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("not a regular file"),
        "expected special-file stale target rejection, got {error:#}"
    );
    assert_eq!(
        std::fs::read_link(broker_home.join("history.jsonl")).unwrap(),
        stale_target,
        "failed repair must leave the old broker link intact"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_broken_stale_link_to_symlinked_legacy_child() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let stale_target = legacy_codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(stale_target.parent().unwrap())
        .await
        .unwrap();
    std::os::unix::fs::symlink("missing-auth.json", &stale_target).unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&stale_target, broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("target is a symlink"),
        "expected broken symlink stale target rejection, got {error:#}"
    );
    assert_eq!(
        std::fs::read_link(broker_home.join("history.jsonl")).unwrap(),
        stale_target,
        "failed repair must leave the old broker link intact"
    );
    assert!(
        !codex_runtime_home(root).join("history.jsonl").exists(),
        "broken symlink stale target must not be treated as a missing canonical file"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oauth_broker_home_rejects_invalid_shared_file_before_removing_stale_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let broker_home = codex_broker_home(root, account_id);
    let stale_target = legacy_codex_runtime_home(root).join("history.jsonl");
    tokio::fs::create_dir_all(stale_target.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stale_target, "{\"thread\":\"legacy-stale\"}\n")
        .await
        .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root).join("history.jsonl"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(&broker_home).await.unwrap();
    std::os::unix::fs::symlink(&stale_target, broker_home.join("history.jsonl")).unwrap();

    let error = expose_legacy_codex_state_to_broker_home(root, &broker_home)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("kind does not match manifest"),
        "expected invalid shared child rejection, got {error:#}"
    );
    assert_eq!(
        std::fs::read_link(broker_home.join("history.jsonl")).unwrap(),
        stale_target,
        "failed repair must not remove the old broker link"
    );
}

#[tokio::test]
async fn usage_hydration_migrates_raw_oauth_account_to_broker_home() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, account_id).await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"tokens":{"access_token":"legacy-access","refresh_token":"legacy-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let env = codex_env_for_account(root, account_id);
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_broker_home(root, account_id).to_string_lossy());
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
    assert!(
        !account_dir.join("auth.json").exists(),
        "legacy raw OAuth auth should be removed after broker adoption"
    );
    assert!(
        !codex_runtime_home(root).join("auth.json").exists(),
        "usage hydration must not copy OAuth refresh tokens into the runtime home"
    );
    let registry = load_codex_registry(root).await.unwrap();
    let entry = registry
        .accounts
        .iter()
        .find(|entry| entry.id == account_id)
        .unwrap();
    assert!(entry.secret_ref.is_some());
    assert_eq!(entry.provider_account_id.as_deref(), Some("upstream-acct"));
}

#[tokio::test]
async fn usage_hydration_adopts_legacy_oauth_home_before_stale_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"tokens":{"access_token":"stale-access","refresh_token":"stale-refresh","account_id":"upstream-acct"}}}"#,
    )
    .await
    .unwrap();
    let account_dir = ensure_codex_account_dir(root, account_id).await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"tokens":{"access_token":"fresh-access","refresh_token":"fresh-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("fresh-access"));
    assert!(broker_payload.contains("fresh-refresh"));
    assert!(!broker_payload.contains("stale-access"));
    assert!(!account_dir.join("auth.json").exists());
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, &secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("fresh-access"));
    assert!(!secret_payload.contains("stale-access"));
}

#[tokio::test]
async fn usage_hydration_adopts_owned_runtime_oauth_before_stale_secret() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"tokens":{"access_token":"stale-access","refresh_token":"stale-refresh","account_id":"upstream-acct"}}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"tokens":{"access_token":"fresh-access","refresh_token":"fresh-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("fresh-access"));
    assert!(!broker_payload.contains("stale-access"));
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, &secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("fresh-access"));
    assert!(!secret_payload.contains("stale-access"));
}

#[tokio::test]
async fn usage_hydration_adopts_owned_runtime_oauth_without_secret_ref() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"tokens":{"access_token":"fresh-access","refresh_token":"fresh-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("fresh-access"));
    let registry = load_codex_registry(root).await.unwrap();
    let secret_ref = registry.accounts[0].secret_ref.as_deref().unwrap();
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("fresh-access"));
}

#[tokio::test]
async fn usage_hydration_clears_corrupt_owned_runtime_auth_when_broker_exists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    let secret_access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": secret_access,
                    "refresh_token": "secret-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_broker_home(root, account_id))
        .await
        .unwrap();
    tokio::fs::write(
        codex_broker_home(root, account_id).join("auth.json"),
        br#"{"tokens":{"access_token":"broker-access","refresh_token":"broker-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(codex_runtime_home(root).join("auth.json"), "{ invalid json")
        .await
        .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(!hydrated);
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(!codex_runtime_owner_path(root).exists());
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("broker-access"));
    assert!(!broker_payload.contains("secret-access"));
}

#[tokio::test]
async fn usage_hydration_preserves_runtime_oauth_owned_by_other_account() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let other_account_id = "acct-other";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(other_account_id.to_string()),
        accounts: vec![
            CodexAccountEntry {
                id: account_id.to_string(),
                label: "acct".to_string(),
                kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: Some("upstream-acct".to_string()),
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: None,
                secret_ref: Some(secret_ref.clone()),
                endpoint_profile: CodexEndpointProfile::default(),
            },
            CodexAccountEntry {
                id: other_account_id.to_string(),
                label: "other".to_string(),
                kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: Some("upstream-other".to_string()),
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: None,
                secret_ref: None,
                endpoint_profile: CodexEndpointProfile::default(),
            },
        ],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"tokens":{"access_token":"acct-access","refresh_token":"acct-refresh","account_id":"upstream-acct"}}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"tokens":{"access_token":"other-access","refresh_token":"other-refresh","account_id":"upstream-other"}}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, other_account_id)
        .await
        .unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let runtime_payload = tokio::fs::read_to_string(codex_runtime_home(root).join("auth.json"))
        .await
        .unwrap();
    assert!(runtime_payload.contains("other-access"));
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("acct-access"));
    assert!(!broker_payload.contains("other-access"));
}

#[tokio::test]
async fn usage_hydration_preserves_api_key_runtime_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-api-key";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"OPENAI_API_KEY":"secret-key"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"OPENAI_API_KEY":"runtime-key"}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let runtime_payload = tokio::fs::read_to_string(codex_runtime_home(root).join("auth.json"))
        .await
        .unwrap();
    assert!(runtime_payload.contains("runtime-key"));
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("secret-key"));
}

#[tokio::test]
async fn usage_hydration_preserves_legacy_api_key_runtime_projection_when_broker_exists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-api-key";
    let secret_ref = format!("{account_id}.json");
    let registry_path = codex_registry_path(root);
    tokio::fs::create_dir_all(registry_path.parent().unwrap())
        .await
        .unwrap();
    let registry = serde_json::json!({
        "active_account_id": account_id,
        "accounts": [{
            "id": account_id,
            "label": "acct",
            "created_at": Utc::now(),
            "secret_ref": secret_ref
        }]
    });
    tokio::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"OPENAI_API_KEY":"secret-key"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_broker_home(root, account_id))
        .await
        .unwrap();
    tokio::fs::write(
        codex_broker_home(root, account_id).join("auth.json"),
        br#"{"OPENAI_API_KEY":"broker-key"}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"OPENAI_API_KEY":"runtime-key"}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let runtime_payload = tokio::fs::read_to_string(codex_runtime_home(root).join("auth.json"))
        .await
        .unwrap();
    assert!(runtime_payload.contains("runtime-key"));
    assert!(codex_runtime_owner_path(root).exists());
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("secret-key"));
}

#[tokio::test]
async fn usage_hydration_ignores_corrupt_owned_runtime_auth_when_api_key_broker_exists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-api-key";
    let secret_ref = format!("{account_id}.json");
    let registry_path = codex_registry_path(root);
    tokio::fs::create_dir_all(registry_path.parent().unwrap())
        .await
        .unwrap();
    let registry = serde_json::json!({
        "active_account_id": account_id,
        "accounts": [{
            "id": account_id,
            "label": "acct",
            "created_at": Utc::now(),
            "secret_ref": secret_ref
        }]
    });
    tokio::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"OPENAI_API_KEY":"secret-key"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_broker_home(root, account_id))
        .await
        .unwrap();
    tokio::fs::write(
        codex_broker_home(root, account_id).join("auth.json"),
        br#"{"OPENAI_API_KEY":"broker-key"}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(codex_runtime_home(root).join("auth.json"), "{ invalid json")
        .await
        .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    assert!(codex_runtime_home(root).join("auth.json").exists());
    assert!(codex_runtime_owner_path(root).exists());
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("secret-key"));
}

#[tokio::test]
async fn usage_hydration_adopts_owned_runtime_oauth_when_broker_has_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"OPENAI_API_KEY":"secret-key"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_broker_home(root, account_id))
        .await
        .unwrap();
    tokio::fs::write(
        codex_broker_home(root, account_id).join("auth.json"),
        br#"{"OPENAI_API_KEY":"broker-key"}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"tokens":{"access_token":"fresh-access","refresh_token":"fresh-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(!codex_runtime_owner_path(root).exists());
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("fresh-access"));
    assert!(!broker_payload.contains("broker-key"));
    let registry = load_codex_registry(root).await.unwrap();
    assert_eq!(registry.accounts[0].kind, CODEX_CREDENTIAL_KIND_OAUTH);
    let secret_ref = registry.accounts[0].secret_ref.as_deref().unwrap();
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("fresh-access"));
    assert!(!secret_payload.contains("secret-key"));
}

#[tokio::test]
async fn usage_hydration_preserves_existing_broker_authority_for_raw_oauth_account() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, account_id).await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"tokens":{"access_token":"old-access","refresh_token":"old-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_broker_home(root, account_id))
        .await
        .unwrap();
    tokio::fs::write(
        codex_broker_home(root, account_id).join("auth.json"),
        br#"{"tokens":{"access_token":"new-access","refresh_token":"new-refresh","account_id":"upstream-acct"}}"#,
    )
    .await
    .unwrap();

    hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("new-access"));
    assert!(broker_payload.contains("new-refresh"));
    assert!(!broker_payload.contains("old-access"));
    assert!(!account_dir.join("auth.json").exists());
    let registry = load_codex_registry(root).await.unwrap();
    let secret_ref = registry.accounts[0].secret_ref.as_deref().unwrap();
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("new-access"));
    assert!(!secret_payload.contains("old-access"));
}

#[tokio::test]
async fn usage_hydration_projects_raw_api_key_account_to_broker_home() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-api-key";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let account_dir = ensure_codex_account_dir(root, account_id).await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"test-key"}"#,
    )
    .await
    .unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let env = codex_env_for_account(root, account_id);
    let home = env.get("CODEX_HOME").unwrap();
    assert_eq!(home, &codex_broker_home(root, account_id).to_string_lossy());
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
    assert!(!account_dir.join("auth.json").exists());
    let registry = load_codex_registry(root).await.unwrap();
    assert!(registry.accounts[0].secret_ref.is_some());
}

#[tokio::test]
async fn usage_hydration_projects_legacy_api_key_without_kind_to_broker_home() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-api-key";
    let registry_path = codex_registry_path(root);
    tokio::fs::create_dir_all(registry_path.parent().unwrap())
        .await
        .unwrap();
    let registry = serde_json::json!({
        "active_account_id": account_id,
        "accounts": [{
            "id": account_id,
            "label": "acct",
            "created_at": Utc::now()
        }]
    });
    tokio::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&registry).unwrap(),
    )
    .await
    .unwrap();
    let account_dir = ensure_codex_account_dir(root, account_id).await.unwrap();
    tokio::fs::write(
        account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"legacy-key"}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"OPENAI_API_KEY":"runtime-key"}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let hydrated = hydrate_codex_account_home_from_secret(root, account_id)
        .await
        .unwrap();

    assert!(hydrated);
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("legacy-key"));
    assert!(!account_dir.join("auth.json").exists());
    let runtime_payload = tokio::fs::read_to_string(codex_runtime_home(root).join("auth.json"))
        .await
        .unwrap();
    assert!(runtime_payload.contains("runtime-key"));
    let registry = load_codex_registry(root).await.unwrap();
    assert_eq!(registry.accounts[0].kind, CODEX_CREDENTIAL_KIND_API_KEY);
    assert!(registry.accounts[0].secret_ref.is_some());
}

#[tokio::test]
async fn oauth_broker_launch_clears_legacy_runtime_projection() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: Some("upstream-acct".to_string()),
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    let secret_access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        serde_json::json!({
            "version": 1,
            "auth": {
                "tokens": {
                    "access_token": secret_access,
                    "refresh_token": "secret-refresh",
                    "account_id": "upstream-acct"
                }
            }
        })
        .to_string(),
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    let legacy_access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        serde_json::json!({
            "tokens": {
                "access_token": legacy_access,
                "refresh_token": "legacy-refresh",
                "account_id": "upstream-acct"
            }
        })
        .to_string(),
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, account_id).await.unwrap();

    let env = codex_env_for_active_account(root).await.unwrap();
    let expected_home = codex_broker_home(root, account_id)
        .to_string_lossy()
        .to_string();
    assert_eq!(
        env.get("CODEX_HOME").map(String::as_str),
        Some(expected_home.as_str())
    );
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    let runtime_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(runtime_payload.contains(&legacy_access));
    assert!(runtime_payload.contains("legacy-refresh"));
    assert!(!runtime_payload.contains("secret-refresh"));
    ensure_codex_auth_ready(&codex_broker_home(root, account_id))
        .await
        .unwrap();
}

#[tokio::test]
async fn oauth_secret_rejects_runtime_root_projection() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let runtime_root = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-oauth";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"tokens":{"access_token":"access","refresh_token":"refresh"}}}"#,
    )
    .await
    .unwrap();

    let err = codex_env_for_active_account_with_runtime_root(root, runtime_root.path())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("cannot be copied into this sandbox runtime"),
        "unexpected error: {err:#}"
    );
    assert!(!codex_runtime_home(runtime_root.path())
        .join("auth.json")
        .exists());
}

#[tokio::test]
async fn existing_broker_home_is_not_rewritten_by_stale_secret() {
    let _env_lock = lock_env().await;
    let _guard = EnvGuard::without("CTX_CODEX_HOME");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-123";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"tokens":{"access_token":"old-access","refresh_token":"old-refresh"}}}"#,
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_broker_home(root, account_id))
        .await
        .unwrap();
    let new_access = codex_test_fresh_jwt();
    tokio::fs::write(
        codex_broker_home(root, account_id).join("auth.json"),
        serde_json::json!({
            "tokens": {
                "access_token": new_access,
                "refresh_token": "new-refresh"
            }
        })
        .to_string(),
    )
    .await
    .unwrap();

    let _ = codex_env_for_active_account(root).await.unwrap();

    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, &secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("old-access"));
    assert!(secret_payload.contains("old-refresh"));
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, account_id).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains(&new_access));
    assert!(broker_payload.contains("new-refresh"));
}

#[tokio::test]
async fn removing_account_cleans_secret_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-123";
    let secret_ref = format!("{account_id}.json");
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.clone()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_secrets_root(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_secret_path(root, &secret_ref).unwrap(),
        br#"{"version":1,"auth":{"OPENAI_API_KEY":"test-key"}}"#,
    )
    .await
    .unwrap();

    remove_codex_account(root, account_id).await.unwrap();
    assert!(!codex_secret_path(root, &secret_ref).unwrap().exists());
}

#[tokio::test]
async fn removing_account_leaves_broker_home_for_post_restart_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-locked";
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    let broker_root = codex_broker_home(root, account_id)
        .parent()
        .unwrap()
        .to_path_buf();
    tokio::fs::create_dir_all(broker_root.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&broker_root, b"not-a-directory")
        .await
        .unwrap();

    let registry = remove_codex_account(root, account_id).await.unwrap();

    assert!(registry.accounts.is_empty());
    assert!(registry.active_account_id.is_none());
    let persisted = load_codex_registry(root).await.unwrap();
    assert!(persisted.accounts.is_empty());
    assert!(persisted.active_account_id.is_none());
    assert!(
        broker_root.exists(),
        "failed broker cleanup should be deferred after registry removal"
    );
}

#[tokio::test]
async fn removing_account_with_unsafe_secret_ref_preserves_outside_file_and_clears_registry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let account_id = "acct-unsafe";
    let secret_ref = "../../outside-secret.json";
    let outside_secret = root.join("outside-secret.json");
    tokio::fs::write(&outside_secret, b"do-not-touch")
        .await
        .unwrap();
    let registry = CodexAccountRegistry {
        active_account_id: Some(account_id.to_string()),
        accounts: vec![CodexAccountEntry {
            id: account_id.to_string(),
            label: "acct".to_string(),
            kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: Some(secret_ref.to_string()),
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();

    remove_codex_account(root, account_id).await.unwrap();

    let persisted = load_codex_registry(root).await.unwrap();
    assert!(persisted.accounts.is_empty());
    assert!(persisted.active_account_id.is_none());
    assert_eq!(
        tokio::fs::read_to_string(&outside_secret).await.unwrap(),
        "do-not-touch"
    );
}

#[tokio::test]
async fn load_codex_registry_fails_closed_on_malformed_registry_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = codex_registry_path(root);
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&path, "{ invalid json").await.unwrap();

    let err = load_codex_registry(root)
        .await
        .expect_err("malformed registry should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("Codex account registry"),
        "expected registry label in error: {message}"
    );
    assert!(
        message.contains("parsing"),
        "expected parse context in error: {message}"
    );
}

#[tokio::test]
async fn probe_host_auth_candidate_reports_api_key_shape() {
    let _env_lock = lock_env().await;
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    tokio::fs::write(&auth_path, br#"{"OPENAI_API_KEY":"test-key"}"#)
        .await
        .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    let probe = probe_host_codex_auth_candidate().await;
    assert!(probe.available);
    assert_eq!(
        probe.auth_kind.as_deref(),
        Some(CODEX_CREDENTIAL_KIND_API_KEY)
    );
    assert_eq!(
        probe.path.as_deref(),
        Some(auth_path.to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn import_host_auth_persists_secret_and_sets_active() {
    let _env_lock = lock_env().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let host_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    let access = codex_test_fresh_jwt();
    tokio::fs::write(
        &auth_path,
        serde_json::json!({
            "tokens": {
                "access_token": access,
                "refresh_token": "b",
                "account_id": "upstream-1"
            }
        })
        .to_string(),
    )
    .await
    .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    let registry = import_host_codex_auth_to_secret_store(root, Some("Imported".to_string()))
        .await
        .unwrap();
    let active = registry.active_account_id.clone().expect("active account");
    let entry = registry
        .accounts
        .iter()
        .find(|account| account.id == active)
        .expect("imported account");
    assert_eq!(entry.label, "Imported");
    assert_eq!(entry.kind, CODEX_CREDENTIAL_KIND_OAUTH);
    assert_eq!(entry.provider_account_id.as_deref(), Some("upstream-1"));
    assert!(entry.secret_ref.is_some());
    assert_eq!(
        entry.endpoint_profile.api_shape,
        CODEX_API_SHAPE_OPENAI_RESPONSES
    );
    assert_eq!(entry.endpoint_profile.auth_type, CODEX_AUTH_TYPE_BEARER);

    let env = codex_env_for_active_account(root).await.unwrap();
    let home = env.get("CODEX_HOME").unwrap();
    ensure_codex_auth_ready(Path::new(home)).await.unwrap();
}

#[tokio::test]
async fn import_host_auth_dedupes_existing_account() {
    let _env_lock = lock_env().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let host_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"tokens":{"access_token":"access-1","refresh_token":"refresh-1","account_id":"upstream-1"}}"#,
    )
    .await
    .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    let first = import_host_codex_auth_to_secret_store(root, Some("First".to_string()))
        .await
        .unwrap();
    let first_active = first.active_account_id.clone().expect("active account");
    assert_eq!(first.accounts.len(), 1);
    let mut legacy_registry = first.clone();
    legacy_registry.accounts[0].provider_account_id = None;
    save_codex_registry(root, &legacy_registry).await.unwrap();

    tokio::fs::write(
        &auth_path,
        br#"{"tokens":{"access_token":"access-2","refresh_token":"refresh-2","account_id":"upstream-1"}}"#,
    )
    .await
    .unwrap();
    let second = import_host_codex_auth_to_secret_store(root, Some("Second".to_string()))
        .await
        .unwrap();
    let second_active = second.active_account_id.clone().expect("active account");
    assert_eq!(second.accounts.len(), 1);
    assert_eq!(second_active, first_active);
    assert_eq!(
        second.accounts[0].provider_account_id.as_deref(),
        Some("upstream-1")
    );
    let secret_ref = second.accounts[0]
        .secret_ref
        .as_deref()
        .expect("secret ref");
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("access-1"));
    assert!(secret_payload.contains("refresh-1"));
    assert!(!secret_payload.contains("access-2"));
    let broker_payload =
        tokio::fs::read_to_string(codex_broker_home(root, &second_active).join("auth.json"))
            .await
            .unwrap();
    assert!(broker_payload.contains("access-1"));
    assert!(broker_payload.contains("refresh-1"));
    assert!(!broker_payload.contains("access-2"));
}

#[tokio::test]
async fn import_host_auth_sanitizes_existing_oauth_broker_home() {
    let _env_lock = lock_env().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let host_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"tokens":{"access_token":"access-1","refresh_token":"refresh-1","account_id":"upstream-1"}}"#,
    )
    .await
    .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );
    let first = import_host_codex_auth_to_secret_store(root, Some("First".to_string()))
        .await
        .unwrap();
    let account_id = first.active_account_id.clone().expect("active account");
    let broker_home = codex_broker_home(root, &account_id);
    tokio::fs::write(
        broker_home.join("auth.json"),
        br#"{"OPENAI_API_KEY":"must-not-survive","tokens":{"access_token":"broker-access","refresh_token":"broker-refresh","account_id":"upstream-1"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        &auth_path,
        br#"{"tokens":{"access_token":"access-2","refresh_token":"refresh-2","account_id":"upstream-1"}}"#,
    )
    .await
    .unwrap();

    let second = import_host_codex_auth_to_secret_store(root, Some("Second".to_string()))
        .await
        .unwrap();

    assert_eq!(
        second.active_account_id.as_deref(),
        Some(account_id.as_str())
    );
    let secret_ref = second.accounts[0]
        .secret_ref
        .as_deref()
        .expect("secret ref");
    let secret_payload = tokio::fs::read_to_string(codex_secret_path(root, secret_ref).unwrap())
        .await
        .unwrap();
    assert!(secret_payload.contains("broker-access"));
    assert!(secret_payload.contains("broker-refresh"));
    assert!(!secret_payload.contains("OPENAI_API_KEY"));
    assert!(!secret_payload.contains("must-not-survive"));
    assert!(!secret_payload.contains("access-2"));
    let broker_payload = tokio::fs::read_to_string(broker_home.join("auth.json"))
        .await
        .unwrap();
    assert!(broker_payload.contains("broker-access"));
    assert!(broker_payload.contains("broker-refresh"));
    assert!(!broker_payload.contains("OPENAI_API_KEY"));
    assert!(!broker_payload.contains("must-not-survive"));
}

#[tokio::test]
async fn seed_host_auth_projects_valid_auth_into_private_runtime_home() {
    let _env_lock = lock_env().await;
    let host_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(&auth_path, br#"{"OPENAI_API_KEY":"seeded-key"}"#)
        .await
        .unwrap();
    let _seed_guard = EnvGuard::set(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV, "1");
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    let wrote = seed_codex_auth_from_host(runtime_dir.path()).await.unwrap();
    assert!(wrote);
    ensure_codex_auth_ready(runtime_dir.path()).await.unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let perms = tokio::fs::metadata(runtime_dir.path().join("auth.json"))
            .await
            .unwrap()
            .permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn seed_host_auth_rejects_oauth_refresh_token() {
    let _env_lock = lock_env().await;
    let host_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"tokens":{"access_token":"a","refresh_token":"b"}}"#,
    )
    .await
    .unwrap();
    let _seed_guard = EnvGuard::set(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV, "1");
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    let err = seed_codex_auth_from_host(runtime_dir.path())
        .await
        .expect_err("OAuth host auth should not be seeded");
    assert!(err.to_string().contains("OAuth host auth cannot be seeded"));
    assert!(tokio::fs::metadata(runtime_dir.path().join("auth.json"))
        .await
        .is_err());
}

#[tokio::test]
async fn seed_host_auth_fails_closed_on_invalid_host_json() {
    let _env_lock = lock_env().await;
    let host_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(&auth_path, "{ invalid json")
        .await
        .unwrap();
    let _seed_guard = EnvGuard::set(CTX_SEED_CODEX_AUTH_FROM_HOST_ENV, "1");
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    let err = seed_codex_auth_from_host(runtime_dir.path())
        .await
        .expect_err("invalid host auth should fail closed");
    assert!(err.to_string().contains("invalid codex auth JSON"));
    assert!(tokio::fs::metadata(runtime_dir.path().join("auth.json"))
        .await
        .is_err());
}

#[tokio::test]
async fn import_host_auth_fails_closed_on_malformed_existing_account_auth() {
    let _env_lock = lock_env().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let host_dir = tempfile::tempdir().unwrap();
    let auth_path = host_dir.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        br#"{"tokens":{"access_token":"a","refresh_token":"b"}}"#,
    )
    .await
    .unwrap();
    let _path_guard = EnvGuard::set(
        CTX_CODEX_HOST_AUTH_PATH_ENV,
        auth_path.to_string_lossy().as_ref(),
    );

    save_codex_registry(
        root,
        &CodexAccountRegistry {
            active_account_id: Some("acct-1".to_string()),
            accounts: vec![CodexAccountEntry {
                id: "acct-1".to_string(),
                label: "Existing".to_string(),
                kind: CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: None,
                plan_type: None,
                created_at: Utc::now(),
                last_used_at: Some(Utc::now()),
                secret_ref: None,
                endpoint_profile: CodexEndpointProfile::default(),
            }],
        },
    )
    .await
    .unwrap();
    tokio::fs::create_dir_all(codex_account_dir(root, "acct-1"))
        .await
        .unwrap();
    tokio::fs::write(
        codex_account_dir(root, "acct-1").join("auth.json"),
        "{ invalid json",
    )
    .await
    .unwrap();

    let err = import_host_codex_auth_to_secret_store(root, Some("Imported".to_string()))
        .await
        .expect_err("malformed existing account auth should fail closed");
    let message = format!("{err:#}");
    assert!(
        message.contains("invalid codex auth JSON"),
        "expected parse context in error: {message}"
    );
    assert!(
        message.contains("acct-1/auth.json"),
        "expected existing auth path in error: {message}"
    );
}

#[tokio::test]
async fn upsert_rejects_incompatible_endpoint_profile() {
    let dir = tempfile::tempdir().unwrap();
    let entry = CodexAccountEntry {
        id: "acct-incompatible".to_string(),
        label: "Bad Profile".to_string(),
        kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
        email: None,
        provider_account_id: None,
        plan_type: None,
        created_at: Utc::now(),
        last_used_at: None,
        secret_ref: None,
        endpoint_profile: CodexEndpointProfile {
            api_shape: "anthropic_messages".to_string(),
            auth_type: CODEX_AUTH_TYPE_BEARER.to_string(),
            base_url: Some("https://example.com/v1".to_string()),
        },
    };

    let err = upsert_codex_account(dir.path(), entry).await.unwrap_err();
    assert!(err.to_string().contains("api_shape=openai_responses"));
}

#[tokio::test]
async fn set_active_rejects_incompatible_auth_type() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: None,
        accounts: vec![CodexAccountEntry {
            id: "acct-bad".to_string(),
            label: "Bad Profile".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile {
                api_shape: CODEX_API_SHAPE_OPENAI_RESPONSES.to_string(),
                auth_type: "basic".to_string(),
                base_url: Some("https://example.com/v1".to_string()),
            },
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();

    let err = set_active_codex_account(root, Some("acct-bad".to_string()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("auth_type=bearer"));
}

#[tokio::test]
async fn clearing_active_account_clears_runtime_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-1".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-1".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"OPENAI_API_KEY":"stale"}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, "acct-1").await.unwrap();

    let _ = set_active_codex_account(root, None).await.unwrap();
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(!codex_runtime_owner_path(root).exists());
}

#[tokio::test]
async fn removing_active_account_clears_runtime_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let registry = CodexAccountRegistry {
        active_account_id: Some("acct-remove".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-remove".to_string(),
            label: "Account".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &registry).await.unwrap();
    tokio::fs::create_dir_all(codex_runtime_home(root))
        .await
        .unwrap();
    tokio::fs::write(
        codex_runtime_home(root).join("auth.json"),
        br#"{"OPENAI_API_KEY":"stale"}"#,
    )
    .await
    .unwrap();
    write_runtime_owner_marker(root, "acct-remove")
        .await
        .unwrap();

    let _ = remove_codex_account(root, "acct-remove").await.unwrap();
    assert!(!codex_runtime_home(root).join("auth.json").exists());
    assert!(!codex_runtime_owner_path(root).exists());
}

#[tokio::test]
async fn subscription_env_dispatches_to_supported_providers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _ = add_claude_account(
        root,
        Some("Claude".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();
    let codex_registry = CodexAccountRegistry {
        active_account_id: Some("acct-codex".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-codex".to_string(),
            label: "Codex".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &codex_registry).await.unwrap();
    let codex_account_dir = ensure_codex_account_dir(root, "acct-codex").await.unwrap();
    tokio::fs::write(
        codex_account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"codex-test-key"}"#,
    )
    .await
    .unwrap();
    let _ = add_gemini_account(
        root,
        Some("Gemini".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let _ = add_qwen_account(
        root,
        Some("Qwen".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r","token_type":"Bearer","expiry_date":4102444800000}"#.to_string(),
        None,
    )
    .await
    .unwrap();
    let _ = add_kimi_account(
        root,
        Some("Kimi".to_string()),
        None,
        r#"{"access_token":"token-a"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let _ = add_copilot_account(
        root,
        Some("Copilot".to_string()),
        "github-token-invalid".to_string(),
        None,
    )
    .await
    .unwrap();
    let _ = add_cursor_account(
        root,
        Some("Cursor".to_string()),
        "cursor-key".to_string(),
        None,
    )
    .await
    .unwrap();
    let _ = upsert_amp_account(
        root,
        Some("Amp".to_string()),
        Some("amp@example.com".to_string()),
    )
    .await
    .unwrap();
    let _ = upsert_mistral_account(
        root,
        Some("Mistral".to_string()),
        Some("mistral@example.com".to_string()),
    )
    .await
    .unwrap();

    let claude_env = subscription_env_for_active_account(root, "claude-crp")
        .await
        .unwrap();
    assert!(claude_env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
    let gemini_env = subscription_env_for_active_account(root, "gemini")
        .await
        .unwrap();
    assert!(gemini_env.contains_key("GEMINI_CLI_HOME"));
    let qwen_env = subscription_env_for_active_account(root, "qwen")
        .await
        .unwrap();
    assert!(qwen_env.contains_key("HOME"));
    let kimi_env = subscription_env_for_active_account(root, "kimi")
        .await
        .unwrap();
    assert!(kimi_env.contains_key(KIMI_SHARE_DIR_ENV));
    let mistral_env = subscription_env_for_active_account(root, "mistral")
        .await
        .unwrap();
    assert!(mistral_env.contains_key("HOME"));
    let copilot_env = subscription_env_for_active_account(root, "copilot")
        .await
        .unwrap();
    assert!(copilot_env.contains_key("GH_TOKEN"));
    let cursor_env = subscription_env_for_active_account(root, "cursor")
        .await
        .unwrap();
    assert!(cursor_env.contains_key("CURSOR_CONFIG_DIR"));
    let amp_env = subscription_env_for_active_account(root, "amp")
        .await
        .unwrap();
    assert!(amp_env.contains_key("HOME"));
    let unknown_env = subscription_env_for_active_account(root, "unknown")
        .await
        .unwrap();
    assert!(unknown_env.is_empty());
}

#[tokio::test]
async fn subscription_env_runtime_root_projects_path_based_providers() {
    let dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let runtime_root = runtime_dir.path();

    let _ = add_claude_account(
        root,
        Some("Claude".to_string()),
        CLAUDE_TEST_SETUP_TOKEN.to_string(),
    )
    .await
    .unwrap();
    let codex_registry = CodexAccountRegistry {
        active_account_id: Some("acct-codex".to_string()),
        accounts: vec![CodexAccountEntry {
            id: "acct-codex".to_string(),
            label: "Codex".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile::default(),
        }],
    };
    save_codex_registry(root, &codex_registry).await.unwrap();
    let codex_account_dir = ensure_codex_account_dir(root, "acct-codex").await.unwrap();
    tokio::fs::write(
        codex_account_dir.join("auth.json"),
        br#"{"OPENAI_API_KEY":"codex-test-key"}"#,
    )
    .await
    .unwrap();
    let _ = add_gemini_account(
        root,
        Some("Gemini".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let _ = add_qwen_account(
        root,
        Some("Qwen".to_string()),
        r#"{"access_token":"token-a","refresh_token":"token-r","token_type":"Bearer","expiry_date":4102444800000}"#.to_string(),
        None,
    )
    .await
    .unwrap();
    let _ = add_kimi_account(
        root,
        Some("Kimi".to_string()),
        None,
        r#"{"access_token":"token-a"}"#.to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    let _ = add_copilot_account(
        root,
        Some("Copilot".to_string()),
        "github-runtime-token-invalid".to_string(),
        Some("copilot@example.com".to_string()),
    )
    .await
    .unwrap();
    let _ = add_cursor_account(
        root,
        Some("Cursor".to_string()),
        "cursor-key".to_string(),
        None,
    )
    .await
    .unwrap();
    let _ = upsert_amp_account(
        root,
        Some("Amp".to_string()),
        Some("amp@example.com".to_string()),
    )
    .await
    .unwrap();
    let _ = upsert_mistral_account(
        root,
        Some("Mistral".to_string()),
        Some("mistral@example.com".to_string()),
    )
    .await
    .unwrap();

    let claude_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "claude-crp")
            .await
            .unwrap();
    let claude_dir = PathBuf::from(claude_env.get("CLAUDE_CONFIG_DIR").unwrap());
    assert!(claude_dir.starts_with(runtime_root));

    let codex_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "codex")
            .await
            .unwrap();
    let codex_home = PathBuf::from(codex_env.get("CODEX_HOME").unwrap());
    assert!(codex_home.starts_with(runtime_root));
    assert!(codex_home.join("auth.json").exists());

    let gemini_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "gemini")
            .await
            .unwrap();
    let gemini_home = PathBuf::from(gemini_env.get("GEMINI_CLI_HOME").unwrap());
    assert!(gemini_home.starts_with(runtime_root));

    let qwen_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "qwen")
            .await
            .unwrap();
    let qwen_home = PathBuf::from(qwen_env.get("HOME").unwrap());
    assert!(qwen_home.starts_with(runtime_root));

    let kimi_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "kimi")
            .await
            .unwrap();
    let kimi_share = PathBuf::from(kimi_env.get(KIMI_SHARE_DIR_ENV).unwrap());
    assert!(kimi_share.starts_with(runtime_root));
    assert!(kimi_share
        .join("credentials")
        .join("kimi-code.json")
        .exists());
    let kimi_config = tokio::fs::read_to_string(kimi_share.join("config.toml"))
        .await
        .unwrap();
    assert!(kimi_config.contains("default_model = \"kimi-code/kimi-for-coding\""));

    let copilot_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "copilot")
            .await
            .unwrap();
    let copilot_home = PathBuf::from(copilot_env.get("HOME").unwrap());
    assert!(copilot_home.starts_with(runtime_root));
    let copilot_config = PathBuf::from(copilot_env.get("XDG_CONFIG_HOME").unwrap());
    assert!(copilot_config.starts_with(runtime_root));
    assert_eq!(
        copilot_env.get("COPILOT_MODEL").map(String::as_str),
        Some("gpt-5-mini")
    );

    let cursor_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "cursor")
            .await
            .unwrap();
    let cursor_config = PathBuf::from(cursor_env.get("CURSOR_CONFIG_DIR").unwrap());
    assert!(cursor_config.starts_with(runtime_root));

    let amp_env = subscription_env_for_active_account_with_runtime_root(root, runtime_root, "amp")
        .await
        .unwrap();
    let amp_home = PathBuf::from(amp_env.get("HOME").unwrap());
    assert!(amp_home.starts_with(runtime_root));
    let amp_config = PathBuf::from(amp_env.get("XDG_CONFIG_HOME").unwrap());
    assert!(amp_config.starts_with(runtime_root));
    let amp_cache = PathBuf::from(amp_env.get("XDG_CACHE_HOME").unwrap());
    assert!(amp_cache.starts_with(runtime_root));

    let mistral_env =
        subscription_env_for_active_account_with_runtime_root(root, runtime_root, "mistral")
            .await
            .unwrap();
    let mistral_home = PathBuf::from(mistral_env.get("HOME").unwrap());
    assert!(mistral_home.starts_with(runtime_root));
}

#[tokio::test]
async fn removing_missing_accounts_returns_unknown_account() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    assert_unknown_account_error(remove_codex_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_claude_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_gemini_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_qwen_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_kimi_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_amp_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_mistral_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_copilot_account(root, "missing").await.unwrap_err());
    assert_unknown_account_error(remove_cursor_account(root, "missing").await.unwrap_err());
}
