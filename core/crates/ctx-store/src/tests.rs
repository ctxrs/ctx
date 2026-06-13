use super::Store;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;

use ctx_core::ids::{
    ConnectionProfileId, MessageId, RunId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId,
};
use ctx_core::models::{
    ExecutionEnvironment, Message, MessageDelivery, MessageRole, SessionEventType, SessionTurn,
    SessionTurnStatus, SessionTurnTool, VcsKind,
};
use sqlx::{Row, SqlitePool};
use tokio::sync::Barrier;

mod team_enterprise_policy_runs_archive;

struct SessionFixture {
    _dir: tempfile::TempDir,
    db_path: std::path::PathBuf,
    store: Store,
    task_id: TaskId,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    session_id: SessionId,
}

fn sqlite_url(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.to_string_lossy())
}

async fn open_store_with_retry(path: &std::path::Path) -> Store {
    loop {
        match Store::open(path).await {
            Ok(store) => break store,
            Err(err) if err.to_string().contains("database is locked") => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => panic!("failed to reopen store: {err:#}"),
        }
    }
}

async fn setup_session_fixture() -> SessionFixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "event projections".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/test".into(), "deadbeef".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    SessionFixture {
        _dir: dir,
        db_path,
        store,
        task_id: task.id,
        workspace_id: ws.id,
        worktree_id: worktree.id,
        session_id: session.id,
    }
}

#[tokio::test]
async fn invalid_persisted_session_execution_environment_fails_closed() {
    let fixture = setup_session_fixture().await;
    sqlx::query("UPDATE sessions SET execution_environment = ? WHERE id = ?")
        .bind("sand_box")
        .bind(fixture.session_id.0.to_string())
        .execute(fixture.store.pool())
        .await
        .unwrap();

    let err = fixture
        .store
        .get_session(fixture.session_id)
        .await
        .expect_err("invalid execution_environment must not decode as host");
    assert!(format!("{err:#}").contains("invalid persisted execution_environment"));
}

#[tokio::test]
async fn whitespace_persisted_session_execution_environment_fails_closed() {
    let fixture = setup_session_fixture().await;
    sqlx::query("UPDATE sessions SET execution_environment = ? WHERE id = ?")
        .bind(" sandbox ")
        .bind(fixture.session_id.0.to_string())
        .execute(fixture.store.pool())
        .await
        .unwrap();

    let err = fixture
        .store
        .get_session(fixture.session_id)
        .await
        .expect_err("whitespace-mutated execution_environment must not decode as sandbox");
    assert!(format!("{err:#}").contains("invalid persisted execution_environment"));
}

#[tokio::test]
async fn unknown_container_session_execution_environment_fails_closed() {
    let fixture = setup_session_fixture().await;
    sqlx::query("UPDATE sessions SET execution_environment = ? WHERE id = ?")
        .bind("container_future_mode")
        .bind(fixture.session_id.0.to_string())
        .execute(fixture.store.pool())
        .await
        .unwrap();

    let err = fixture
        .store
        .get_session(fixture.session_id)
        .await
        .expect_err("unknown container execution_environment must not decode as sandbox");
    assert!(format!("{err:#}").contains("invalid persisted execution_environment"));
}

#[tokio::test]
async fn legacy_container_session_execution_environment_decodes_as_sandbox() {
    for legacy_value in ["container_host_mounted", "container_disk_isolated"] {
        let fixture = setup_session_fixture().await;
        sqlx::query("UPDATE sessions SET execution_environment = ? WHERE id = ?")
            .bind(legacy_value)
            .bind(fixture.session_id.0.to_string())
            .execute(fixture.store.pool())
            .await
            .unwrap();

        let session = fixture
            .store
            .get_session(fixture.session_id)
            .await
            .unwrap()
            .expect("fixture session should decode");
        assert_eq!(
            session.execution_environment,
            ExecutionEnvironment::Sandbox,
            "{legacy_value} should remain a sandbox compatibility alias"
        );
    }
}

async fn load_mobile_access_secret_ref(store: &Store) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT secret_ref FROM mobile_access_config WHERE id = 'default'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap()
}

async fn load_runtime_settings_secret_ref(store: &Store) -> String {
    sqlx::query_scalar::<_, String>("SELECT secret_ref FROM runtime_settings WHERE id = 'default'")
        .fetch_one(store.pool())
        .await
        .unwrap()
}

fn mobile_access_secret_sidecar_path(
    root: &std::path::Path,
    db_file_name: &str,
    secret_ref: &str,
) -> std::path::PathBuf {
    root.join("mobile_access_secrets")
        .join(db_file_name)
        .join(format!("{secret_ref}.json"))
}

fn runtime_settings_secret_sidecar_path(
    root: &std::path::Path,
    db_file_name: &str,
    secret_ref: &str,
) -> std::path::PathBuf {
    root.join("runtime_settings_secrets")
        .join(db_file_name)
        .join(format!("{secret_ref}.json"))
}

fn sqlite_artifact_paths(db_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    vec![
        db_path.to_path_buf(),
        db_path.with_extension("sqlite-wal"),
        db_path.with_extension("sqlite-shm"),
        db_path.with_extension("sqlite-journal"),
    ]
}

#[cfg(unix)]
fn file_mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_file_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

async fn assert_secret_absent_from_sqlite_artifacts(db_path: &std::path::Path, secret: &str) {
    for artifact_path in sqlite_artifact_paths(db_path) {
        let bytes = match tokio::fs::read(&artifact_path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => panic!(
                "failed to read sqlite artifact {}: {err}",
                artifact_path.display()
            ),
        };
        assert!(
            !bytes_contain(&bytes, secret.as_bytes()),
            "found secret bytes in sqlite artifact {}",
            artifact_path.display()
        );
    }
}

async fn write_legacy_mobile_access_sidecar(
    root: &std::path::Path,
    db_file_name: &str,
    tunnel_secret: &str,
    daemon_private_key: &str,
) -> std::path::PathBuf {
    let legacy_path = mobile_access_secret_sidecar_path(root, db_file_name, "default");
    if let Some(parent) = legacy_path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    let payload = serde_json::json!({
        "version": 1,
        "tunnel_secret": tunnel_secret,
        "daemon_private_key": daemon_private_key,
    });
    tokio::fs::write(&legacy_path, serde_json::to_vec_pretty(&payload).unwrap())
        .await
        .unwrap();
    legacy_path
}

async fn insert_pre_secret_ref_mobile_access_state(
    store: &Store,
    tunnel_secret: &str,
    daemon_private_key: &str,
) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"INSERT INTO mobile_access_config
            (id, profile_id, tunnel_id, public_base_url, relay_base_url, tunnel_secret, daemon_public_key, daemon_private_key, enabled, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind("default")
    .bind(ConnectionProfileId::new().0.to_string())
    .bind("legacy-tunnel")
    .bind("https://legacy.example.com")
    .bind("https://legacy-relay.example.com")
    .bind("")
    .bind("legacy-public-key")
    .bind("")
    .bind(1)
    .bind(&now)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
    assert!(!tunnel_secret.is_empty());
    assert!(!daemon_private_key.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn mobile_access_config_upsert_persists_secrets_outside_sqlite() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let profile_id = ConnectionProfileId::new();
    let config = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id,
        tunnel_id: "tunnel-1".to_string(),
        public_base_url: "https://example.com".to_string(),
        relay_base_url: "https://relay.example.com".to_string(),
        tunnel_secret: "secret-1".to_string(),
        daemon_public_key: "public-key".to_string(),
        daemon_private_key: "private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let persisted = store.upsert_mobile_access_config(config).await.unwrap();
    assert_eq!(persisted.tunnel_secret, "secret-1");
    assert_eq!(persisted.daemon_private_key, "private-key");

    let row = sqlx::query(
        "SELECT secret_ref, tunnel_secret, daemon_private_key FROM mobile_access_config WHERE id = 'default'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    let secret_ref: String = row.try_get("secret_ref").unwrap();
    let tunnel_secret: String = row.try_get("tunnel_secret").unwrap();
    let daemon_private_key: String = row.try_get("daemon_private_key").unwrap();
    assert!(!secret_ref.is_empty());
    assert!(tunnel_secret.is_empty());
    assert!(daemon_private_key.is_empty());

    let secret_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    let perms = tokio::fs::metadata(&secret_path)
        .await
        .unwrap()
        .permissions();
    assert_eq!(perms.mode() & 0o777, 0o600);
    assert_secret_absent_from_sqlite_artifacts(&db_path, "secret-1").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "private-key").await;
}

#[tokio::test]
async fn mobile_access_config_get_migrates_legacy_sqlite_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let profile_id = ctx_core::ids::ConnectionProfileId::new();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"INSERT INTO mobile_access_config
            (id, profile_id, tunnel_id, public_base_url, relay_base_url, tunnel_secret, daemon_public_key, daemon_private_key, enabled, created_at, updated_at)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind("default")
    .bind(profile_id.0.to_string())
    .bind("tunnel-legacy")
    .bind("https://example.com")
    .bind("https://relay.example.com")
    .bind("legacy-secret")
    .bind("public-key")
    .bind("legacy-private")
    .bind(1)
    .bind(&now)
    .bind(&now)
    .execute(store.pool())
    .await
    .unwrap();
    store.close().await;

    let store = Store::open(&db_path).await.unwrap();

    let config = store.get_mobile_access_config().await.unwrap().unwrap();
    assert_eq!(config.tunnel_secret, "legacy-secret");
    assert_eq!(config.daemon_private_key, "legacy-private");

    let row = sqlx::query(
        "SELECT secret_ref, tunnel_secret, daemon_private_key FROM mobile_access_config WHERE id = 'default'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    let secret_ref: String = row.try_get("secret_ref").unwrap();
    let tunnel_secret: String = row.try_get("tunnel_secret").unwrap();
    let daemon_private_key: String = row.try_get("daemon_private_key").unwrap();
    assert!(!secret_ref.is_empty());
    assert!(tunnel_secret.is_empty());
    assert!(daemon_private_key.is_empty());
    assert!(mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref).exists());
    assert_secret_absent_from_sqlite_artifacts(&db_path, "legacy-secret").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "legacy-private").await;
}

#[tokio::test]
async fn mobile_access_config_get_migrates_legacy_id_keyed_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    insert_pre_secret_ref_mobile_access_state(&store, "legacy-secret", "legacy-private").await;
    let legacy_path = write_legacy_mobile_access_sidecar(
        dir.path(),
        "db.sqlite",
        "legacy-secret",
        "legacy-private",
    )
    .await;

    let config = store.get_mobile_access_config().await.unwrap().unwrap();
    let secret_ref = load_mobile_access_secret_ref(&store).await;
    let new_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    assert_eq!(config.tunnel_secret, "legacy-secret");
    assert_eq!(config.daemon_private_key, "legacy-private");
    assert_ne!(secret_ref, "default");
    assert!(!legacy_path.exists());
    assert!(new_path.exists());
}

#[tokio::test]
async fn delete_mobile_access_config_removes_secret_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let config = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ctx_core::ids::ConnectionProfileId::new(),
        tunnel_id: "tunnel-1".to_string(),
        public_base_url: "https://example.com".to_string(),
        relay_base_url: "https://relay.example.com".to_string(),
        tunnel_secret: "secret-1".to_string(),
        daemon_public_key: "public-key".to_string(),
        daemon_private_key: "private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.upsert_mobile_access_config(config).await.unwrap();
    let secret_ref = load_mobile_access_secret_ref(&store).await;
    let secret_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    assert!(secret_path.exists());

    store.delete_mobile_access_config().await.unwrap();
    assert!(!secret_path.exists());
}

#[tokio::test]
async fn delete_mobile_access_config_removes_legacy_id_keyed_sidecar_without_secret_ref() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    insert_pre_secret_ref_mobile_access_state(&store, "legacy-secret", "legacy-private").await;
    let legacy_path = write_legacy_mobile_access_sidecar(
        dir.path(),
        "db.sqlite",
        "legacy-secret",
        "legacy-private",
    )
    .await;
    assert!(legacy_path.exists());

    store.delete_mobile_access_config().await.unwrap();
    assert!(!legacy_path.exists());
}

#[tokio::test]
async fn mobile_access_config_get_fails_closed_on_corrupt_secret_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let config = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-1".to_string(),
        public_base_url: "https://example.com".to_string(),
        relay_base_url: "https://relay.example.com".to_string(),
        tunnel_secret: "secret-1".to_string(),
        daemon_public_key: "public-key".to_string(),
        daemon_private_key: "private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.upsert_mobile_access_config(config).await.unwrap();

    let secret_ref = load_mobile_access_secret_ref(&store).await;
    let secret_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    tokio::fs::write(&secret_path, "{not-json").await.unwrap();

    let err = store.get_mobile_access_config().await.unwrap_err();
    assert!(err.to_string().contains("parsing mobile access secrets"));
}

#[cfg(unix)]
#[tokio::test]
async fn mobile_access_config_get_repairs_existing_secret_sidecar_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let config = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-1".to_string(),
        public_base_url: "https://example.com".to_string(),
        relay_base_url: "https://relay.example.com".to_string(),
        tunnel_secret: "secret-1".to_string(),
        daemon_public_key: "public-key".to_string(),
        daemon_private_key: "private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.upsert_mobile_access_config(config).await.unwrap();
    let secret_ref = load_mobile_access_secret_ref(&store).await;
    let secret_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    set_file_mode(&secret_path, 0o644);

    let loaded = store.get_mobile_access_config().await.unwrap().unwrap();

    assert_eq!(loaded.tunnel_secret, "secret-1");
    assert_eq!(loaded.daemon_private_key, "private-key");
    assert_eq!(file_mode(&secret_path), 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn mobile_access_config_get_rejects_symlinked_secret_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let config = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-1".to_string(),
        public_base_url: "https://example.com".to_string(),
        relay_base_url: "https://relay.example.com".to_string(),
        tunnel_secret: "secret-1".to_string(),
        daemon_public_key: "public-key".to_string(),
        daemon_private_key: "private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.upsert_mobile_access_config(config).await.unwrap();
    let secret_ref = load_mobile_access_secret_ref(&store).await;
    let secret_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    let outside_secret = outside.path().join("outside.json");
    std::fs::write(&outside_secret, "{}").unwrap();
    std::fs::remove_file(&secret_path).unwrap();
    std::os::unix::fs::symlink(&outside_secret, &secret_path).unwrap();

    let err = store.get_mobile_access_config().await.unwrap_err();

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

#[tokio::test]
async fn mobile_access_config_sidecars_are_namespaced_per_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    let store_a = Store::open(dir.path().join("a.sqlite")).await.unwrap();
    let store_b = Store::open(dir.path().join("b.sqlite")).await.unwrap();
    let config_a = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-a".to_string(),
        public_base_url: "https://a.example.com".to_string(),
        relay_base_url: "https://relay-a.example.com".to_string(),
        tunnel_secret: "secret-a".to_string(),
        daemon_public_key: "public-a".to_string(),
        daemon_private_key: "private-a".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let config_b = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-b".to_string(),
        public_base_url: "https://b.example.com".to_string(),
        relay_base_url: "https://relay-b.example.com".to_string(),
        tunnel_secret: "secret-b".to_string(),
        daemon_public_key: "public-b".to_string(),
        daemon_private_key: "private-b".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    store_a.upsert_mobile_access_config(config_a).await.unwrap();
    store_b.upsert_mobile_access_config(config_b).await.unwrap();

    let loaded_a = store_a.get_mobile_access_config().await.unwrap().unwrap();
    let loaded_b = store_b.get_mobile_access_config().await.unwrap().unwrap();
    let secret_ref_a = load_mobile_access_secret_ref(&store_a).await;
    let secret_ref_b = load_mobile_access_secret_ref(&store_b).await;
    assert_eq!(loaded_a.tunnel_secret, "secret-a");
    assert_eq!(loaded_b.tunnel_secret, "secret-b");
    assert!(mobile_access_secret_sidecar_path(dir.path(), "a.sqlite", &secret_ref_a).exists());
    assert!(mobile_access_secret_sidecar_path(dir.path(), "b.sqlite", &secret_ref_b).exists());

    store_a.delete_mobile_access_config().await.unwrap();
    assert!(mobile_access_secret_sidecar_path(dir.path(), "b.sqlite", &secret_ref_b).exists());
}

#[tokio::test]
async fn mobile_access_config_upsert_rotates_secret_ref_and_cleans_old_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let first = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-1".to_string(),
        public_base_url: "https://example.com".to_string(),
        relay_base_url: "https://relay.example.com".to_string(),
        tunnel_secret: "secret-1".to_string(),
        daemon_public_key: "public-key".to_string(),
        daemon_private_key: "private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.upsert_mobile_access_config(first).await.unwrap();
    let first_ref = load_mobile_access_secret_ref(&store).await;
    let first_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &first_ref);
    assert!(first_path.exists());

    let second = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-2".to_string(),
        public_base_url: "https://example-2.com".to_string(),
        relay_base_url: "https://relay-2.example.com".to_string(),
        tunnel_secret: "secret-2".to_string(),
        daemon_public_key: "public-key-2".to_string(),
        daemon_private_key: "private-key-2".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let persisted = store.upsert_mobile_access_config(second).await.unwrap();
    let second_ref = load_mobile_access_secret_ref(&store).await;
    let second_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &second_ref);
    assert_ne!(first_ref, second_ref);
    assert!(!first_path.exists());
    assert!(second_path.exists());
    assert_eq!(persisted.tunnel_secret, "secret-2");
    assert_eq!(persisted.daemon_private_key, "private-key-2");
    assert_secret_absent_from_sqlite_artifacts(&db_path, "secret-2").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "private-key-2").await;
}

#[tokio::test]
async fn mobile_access_config_upsert_cleans_legacy_id_keyed_sidecar_without_secret_ref() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    insert_pre_secret_ref_mobile_access_state(&store, "legacy-secret", "legacy-private").await;
    let legacy_path = write_legacy_mobile_access_sidecar(
        dir.path(),
        "db.sqlite",
        "legacy-secret",
        "legacy-private",
    )
    .await;

    let updated = crate::store::MobileAccessConfig {
        id: "default".to_string(),
        profile_id: ConnectionProfileId::new(),
        tunnel_id: "tunnel-new".to_string(),
        public_base_url: "https://new.example.com".to_string(),
        relay_base_url: "https://relay-new.example.com".to_string(),
        tunnel_secret: "new-secret".to_string(),
        daemon_public_key: "new-public-key".to_string(),
        daemon_private_key: "new-private-key".to_string(),
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let persisted = store.upsert_mobile_access_config(updated).await.unwrap();
    let secret_ref = load_mobile_access_secret_ref(&store).await;
    let new_path = mobile_access_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    assert_eq!(persisted.tunnel_secret, "new-secret");
    assert_eq!(persisted.daemon_private_key, "new-private-key");
    assert!(!legacy_path.exists());
    assert!(new_path.exists());
    assert_secret_absent_from_sqlite_artifacts(&db_path, "new-secret").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "new-private-key").await;
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_settings_upsert_with_secrets_persists_secret_blob_outside_sqlite() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();

    let doc = store
        .upsert_runtime_settings_document_with_secrets(
            1,
            "{\"dictation\":{}}",
            "{\"api_key\":\"secret-1\",\"oracle\":\"secret-2\"}",
        )
        .await
        .unwrap();
    let secret_ref = load_runtime_settings_secret_ref(&store).await;
    assert_eq!(doc.secret_ref.as_deref(), Some(secret_ref.as_str()));
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    let sidecar = tokio::fs::read_to_string(&secret_path).await.unwrap();
    let metadata = tokio::fs::metadata(&secret_path).await.unwrap();

    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(sidecar.contains("secret-1"));
    assert!(sidecar.contains("secret-2"));
    assert_secret_absent_from_sqlite_artifacts(&db_path, "secret-1").await;
    assert_secret_absent_from_sqlite_artifacts(&db_path, "secret-2").await;
}

#[tokio::test]
async fn runtime_settings_upsert_with_secrets_rotates_secret_ref_and_cleans_old_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();

    store
        .upsert_runtime_settings_document_with_secrets(
            1,
            "{\"dictation\":{}}",
            "{\"api_key\":\"secret-1\"}",
        )
        .await
        .unwrap();
    let first_ref = load_runtime_settings_secret_ref(&store).await;
    let first_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &first_ref);
    assert!(first_path.exists());

    store
        .upsert_runtime_settings_document_with_secrets(
            1,
            "{\"dictation\":{}}",
            "{\"api_key\":\"secret-2\"}",
        )
        .await
        .unwrap();
    let second_ref = load_runtime_settings_secret_ref(&store).await;
    let second_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &second_ref);

    assert_ne!(first_ref, second_ref);
    assert!(!first_path.exists());
    assert!(second_path.exists());
    assert_secret_absent_from_sqlite_artifacts(&db_path, "secret-2").await;
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_settings_secret_read_repairs_existing_sidecar_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    store
        .upsert_runtime_settings_document_with_secrets(
            1,
            "{\"dictation\":{}}",
            "{\"api_key\":\"secret-1\"}",
        )
        .await
        .unwrap();
    let secret_ref = load_runtime_settings_secret_ref(&store).await;
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    set_file_mode(&secret_path, 0o644);

    let payload = store
        .read_runtime_settings_secrets_if_present(&secret_ref)
        .await
        .unwrap()
        .unwrap();

    assert!(payload.contains("secret-1"));
    assert_eq!(file_mode(&secret_path), 0o600);
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_settings_secret_read_rejects_symlinked_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    store
        .upsert_runtime_settings_document_with_secrets(
            1,
            "{\"dictation\":{}}",
            "{\"api_key\":\"secret-1\"}",
        )
        .await
        .unwrap();
    let secret_ref = load_runtime_settings_secret_ref(&store).await;
    let secret_path = runtime_settings_secret_sidecar_path(dir.path(), "db.sqlite", &secret_ref);
    let outside_secret = outside.path().join("outside.json");
    std::fs::write(&outside_secret, "{}").unwrap();
    std::fs::remove_file(&secret_path).unwrap();
    std::os::unix::fs::symlink(&outside_secret, &secret_path).unwrap();

    let err = store
        .read_runtime_settings_secrets_if_present(&secret_ref)
        .await
        .unwrap_err();

    assert!(format!("{err:#}").contains("must not be a symlink"));
}

async fn create_peer_session(fixture: &SessionFixture) -> SessionId {
    fixture
        .store
        .create_session(
            fixture.task_id,
            fixture.workspace_id,
            fixture.worktree_id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn provider_session_ref_claim_sets_projection_and_binding() {
    let fixture = setup_session_fixture().await;

    fixture
        .store
        .claim_session_provider_session_ref(fixture.session_id, "provider-thread-1".into(), "test")
        .await
        .unwrap();

    let session = fixture
        .store
        .get_session(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        session.provider_session_ref.as_deref(),
        Some("provider-thread-1")
    );

    let owner: String = sqlx::query_scalar(
        "SELECT session_id FROM provider_session_bindings WHERE provider_id = 'fake' AND provider_session_ref = 'provider-thread-1'",
    )
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    assert_eq!(owner, fixture.session_id.0.to_string());
}

#[tokio::test]
async fn provider_session_ref_claim_is_idempotent_for_same_session() {
    let fixture = setup_session_fixture().await;

    fixture
        .store
        .claim_session_provider_session_ref(fixture.session_id, "provider-thread-1".into(), "test")
        .await
        .unwrap();
    fixture
        .store
        .claim_session_provider_session_ref(fixture.session_id, "provider-thread-1".into(), "test")
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_session_bindings WHERE provider_id = 'fake' AND provider_session_ref = 'provider-thread-1'",
    )
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn provider_session_ref_claim_waits_for_external_writer() {
    let fixture = setup_session_fixture().await;
    let external_pool = SqlitePool::connect(&sqlite_url(&fixture.db_path))
        .await
        .unwrap();
    let mut external_tx = external_pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    sqlx::query("UPDATE sessions SET updated_at = updated_at WHERE id = ?")
        .bind(fixture.session_id.0.to_string())
        .execute(&mut *external_tx)
        .await
        .unwrap();

    let store = fixture.store.clone();
    let session_id = fixture.session_id;
    let claim = tokio::spawn(async move {
        store
            .claim_session_provider_session_ref(session_id, "provider-thread-1".into(), "test")
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!claim.is_finished());

    external_tx.commit().await.unwrap();
    claim.await.unwrap().unwrap();
}

#[tokio::test]
async fn provider_session_ref_claim_rejects_duplicate_owner() {
    let fixture = setup_session_fixture().await;
    let peer_session_id = create_peer_session(&fixture).await;

    fixture
        .store
        .claim_session_provider_session_ref(fixture.session_id, "provider-thread-1".into(), "test")
        .await
        .unwrap();

    let err = fixture
        .store
        .claim_session_provider_session_ref(peer_session_id, "provider-thread-1".into(), "test")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("owned by session")
            || err.to_string().contains("already attached to session"),
        "{err:#}"
    );
}

#[tokio::test]
async fn provider_session_ref_claim_rejects_same_session_substitution() {
    let fixture = setup_session_fixture().await;

    fixture
        .store
        .claim_session_provider_session_ref(fixture.session_id, "provider-thread-1".into(), "test")
        .await
        .unwrap();

    let err = fixture
        .store
        .claim_session_provider_session_ref(fixture.session_id, "provider-thread-2".into(), "test")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("substitution rejected"), "{err:#}");
}

fn make_turn(session_id: SessionId, run_id: RunId, turn_id: TurnId) -> SessionTurn {
    let now = Utc::now();
    SessionTurn {
        turn_id,
        session_id,
        run_id: Some(run_id),
        user_message_id: None,
        status: SessionTurnStatus::Running,
        start_seq: Some(1),
        end_seq: None,
        started_at: now,
        updated_at: now,
        assistant_partial: None,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    }
}

fn make_assistant_message(
    session_id: SessionId,
    task_id: TaskId,
    run_id: RunId,
    turn_id: TurnId,
    content: &str,
) -> Message {
    Message {
        id: MessageId::new(),
        session_id,
        task_id,
        run_id: Some(run_id),
        turn_id: Some(turn_id),
        turn_sequence: Some(1),
        order_seq: None,
        role: MessageRole::Assistant,
        content: content.to_string(),
        attachments: vec![],
        delivery: MessageDelivery::Immediate,
        delivered_at: None,
        created_at: Utc::now(),
    }
}

#[tokio::test]
async fn completed_turn_accepts_only_late_assistant_finalization_events() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let completed_turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, completed_turn_id))
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_status(
            fixture.session_id,
            completed_turn_id,
            SessionTurnStatus::Completed,
            Some(1),
            None,
            Utc::now(),
        )
        .await
        .unwrap();

    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(completed_turn_id),
            SessionEventType::AssistantComplete,
            serde_json::json!({ "full_content": "late final answer" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(completed_turn_id),
            SessionEventType::AssistantMessageInserted,
            serde_json::json!({ "content": "late final answer" }),
        )
        .await
        .unwrap();

    let err = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(completed_turn_id),
            SessionEventType::ToolCall,
            serde_json::json!({ "tool_call_id": "late-tool" }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("turn terminalization"), "{err:#}");

    let failed_turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, failed_turn_id))
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_status(
            fixture.session_id,
            failed_turn_id,
            SessionTurnStatus::Failed,
            Some(1),
            None,
            Utc::now(),
        )
        .await
        .unwrap();
    let err = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(failed_turn_id),
            SessionEventType::AssistantComplete,
            serde_json::json!({ "full_content": "late failed answer" }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("turn terminalization"), "{err:#}");
}

async fn delete_tool_projection(
    db_path: &std::path::Path,
    session_id: SessionId,
    tool_call_id: &str,
) {
    let pool = SqlitePool::connect(&sqlite_url(db_path)).await.unwrap();
    sqlx::query("DELETE FROM session_turn_tools WHERE session_id = ? AND tool_call_id = ?")
        .bind(session_id.0.to_string())
        .bind(tool_call_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn can_create_and_list_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();

    let ws = store
        .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
        .await
        .unwrap();
    assert_eq!(ws.name, "test");

    let list = store.list_workspaces().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id.0, ws.id.0);

    let got = store.get_workspace(ws.id).await.unwrap();
    assert!(got.is_some());

    store.delete_workspace(ws.id).await.unwrap();
    let list = store.list_workspaces().await.unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn can_create_task() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
        .await
        .unwrap();

    let task = store
        .create_task(ws.id, "do thing".into(), None)
        .await
        .unwrap();
    let tasks = store.list_tasks(ws.id).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id.0, task.id.0);
    assert!(tasks[0].assistant_seen_at.is_none());

    let fetched = store.get_task(task.id).await.unwrap().unwrap();
    assert_eq!(fetched.title, "do thing");
    assert!(fetched.assistant_seen_at.is_none());

    let updated_at_before = fetched.updated_at;
    store.mark_task_read(task.id).await.unwrap();
    let fetched_after_read = store.get_task(task.id).await.unwrap().unwrap();
    assert!(fetched_after_read.assistant_seen_at.is_some());
    assert_eq!(fetched_after_read.updated_at, updated_at_before);

    drop(store);
    let store = loop {
        match Store::open(&db_path).await {
            Ok(store) => break store,
            Err(err) if err.to_string().contains("database is locked") => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => panic!("failed to reopen store: {err:?}"),
        }
    };
    let fetched_after_restart = store.get_task(task.id).await.unwrap().unwrap();
    assert!(fetched_after_restart.assistant_seen_at.is_some());
    assert_eq!(fetched_after_restart.updated_at, updated_at_before);

    store.mark_task_unread(task.id).await.unwrap();
    let fetched_after_unread = store.get_task(task.id).await.unwrap().unwrap();
    assert!(fetched_after_unread.assistant_seen_at.is_none());
    assert_eq!(fetched_after_unread.updated_at, updated_at_before);

    let other = WorkspaceId::new();
    let tasks_other = store.list_tasks(other).await.unwrap();
    assert!(tasks_other.is_empty());
}

#[tokio::test]
async fn session_reasoning_effort_migration_backfills_known_suffixes_only() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    std::fs::File::create(&db_path).unwrap();
    let pool = SqlitePool::connect(&sqlite_url(&db_path)).await.unwrap();

    sqlx::query(
        r#"CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            model_id TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("INSERT INTO sessions (id, model_id) VALUES (?, ?)")
        .bind("session-1")
        .bind("openai/gpt-5/xhigh")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions (id, model_id) VALUES (?, ?)")
        .bind("session-2")
        .bind("openrouter/google/gemini-2.5-pro")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions (id, model_id) VALUES (?, ?)")
        .bind("session-3")
        .bind("vendor/highway")
        .execute(&pool)
        .await
        .unwrap();

    let migration_sql = include_str!("../migrations/0046_session_reasoning_effort.sql");
    for statement in migration_sql
        .split(";\n\n")
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }

    let rows = sqlx::query("SELECT id, model_id, reasoning_effort FROM sessions ORDER BY id ASC")
        .fetch_all(&pool)
        .await
        .unwrap();
    let session1 = &rows[0];
    assert_eq!(
        session1.try_get::<String, _>("model_id").unwrap(),
        "openai/gpt-5"
    );
    assert_eq!(
        session1
            .try_get::<Option<String>, _>("reasoning_effort")
            .unwrap()
            .as_deref(),
        Some("xhigh")
    );

    let session2 = &rows[1];
    assert_eq!(
        session2.try_get::<String, _>("model_id").unwrap(),
        "openrouter/google/gemini-2.5-pro"
    );
    assert_eq!(
        session2
            .try_get::<Option<String>, _>("reasoning_effort")
            .unwrap(),
        None
    );

    let session3 = &rows[2];
    assert_eq!(
        session3.try_get::<String, _>("model_id").unwrap(),
        "vendor/highway"
    );
    assert_eq!(
        session3
            .try_get::<Option<String>, _>("reasoning_effort")
            .unwrap(),
        None
    );

    pool.close().await;
}

#[tokio::test]
async fn concurrent_event_and_message_writes_do_not_error() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.sqlite");
        let store = Store::open(&db_path).await.unwrap();

        let ws = store
            .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
            .await
            .unwrap();
        let task = store
            .create_task(ws.id, "do thing".into(), None)
            .await
            .unwrap();
        let worktree = store
            .create_worktree(ws.id, "/tmp/test".into(), "deadbeef".into(), None)
            .await
            .unwrap();
        let session = store
            .create_session(
                task.id,
                ws.id,
                worktree.id,
                ctx_core::models::ExecutionEnvironment::Host,
                "fake".into(),
                "fake".into(),
                "implementer".into(),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let store = store.clone();
        let session_id = session.id;
        let task_id = session.task_id;

        const WORKERS: usize = 16;
        const WRITES_PER_WORKER: usize = 20;

        let barrier = Arc::new(Barrier::new(WORKERS));
        let mut handles = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let store = store.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                for i in 0..WRITES_PER_WORKER {
                    let run_id = RunId::new();
                    let turn_id = TurnId::new();
                    store
                        .append_session_event(
                            session_id,
                            Some(run_id),
                            Some(turn_id),
                            SessionEventType::Notice,
                            serde_json::json!({ "worker": worker, "i": i }),
                        )
                        .await?;
                    store
                        .insert_message(Message {
                            id: MessageId::new(),
                            session_id,
                            task_id,
                            run_id: Some(run_id),
                            turn_id: Some(turn_id),
                            turn_sequence: None,
                            order_seq: None,
                            role: MessageRole::User,
                            content: format!("hello {worker} {i}"),
                            attachments: vec![],
                            delivery: MessageDelivery::Immediate,
                            delivered_at: None,
                            created_at: chrono::Utc::now(),
                        })
                        .await?;
                }
                anyhow::Result::<()>::Ok(())
            }));
        }

        for h in handles {
            h.await.unwrap().unwrap();
        }

        let events = store.list_session_events(session_id).await.unwrap();
        assert_eq!(events.len(), WORKERS * WRITES_PER_WORKER);
        let messages = store.list_messages_for_session(session_id).await.unwrap();
        assert_eq!(messages.len(), WORKERS * WRITES_PER_WORKER);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn provider_session_binding_migration_preserves_one_canonical_owner() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    std::fs::File::create(&db_path).unwrap();
    let pool = SqlitePool::connect(&sqlite_url(&db_path)).await.unwrap();

    sqlx::query(
        r#"CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL,
            provider_session_ref TEXT,
            workspace_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            worktree_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO sessions (id, provider_id, provider_session_ref, workspace_id, task_id, worktree_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("session-a")
    .bind("fake")
    .bind("shared-ref")
    .bind("workspace")
    .bind("task")
    .bind("worktree")
    .bind("2026-04-23T10:00:00Z")
    .bind("2026-04-23T10:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, provider_id, provider_session_ref, workspace_id, task_id, worktree_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("session-b")
    .bind("fake")
    .bind("shared-ref")
    .bind("workspace")
    .bind("task")
    .bind("worktree")
    .bind("2026-04-23T10:05:00Z")
    .bind("2026-04-23T10:05:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, provider_id, provider_session_ref, workspace_id, task_id, worktree_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("session-c")
    .bind("fake")
    .bind("unique-ref")
    .bind("workspace")
    .bind("task")
    .bind("worktree")
    .bind("2026-04-23T10:10:00Z")
    .bind("2026-04-23T10:10:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let migration_sql = include_str!("../migrations/0061_provider_session_bindings.sql");
    for statement in migration_sql
        .split(";\n\n")
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }

    let canonical_ref: Option<String> =
        sqlx::query_scalar("SELECT provider_session_ref FROM sessions WHERE id = 'session-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let quarantined_ref: Option<String> =
        sqlx::query_scalar("SELECT provider_session_ref FROM sessions WHERE id = 'session-b'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let unique_ref: Option<String> =
        sqlx::query_scalar("SELECT provider_session_ref FROM sessions WHERE id = 'session-c'")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(canonical_ref.as_deref(), Some("shared-ref"));
    assert_eq!(quarantined_ref, None);
    assert_eq!(unique_ref.as_deref(), Some("unique-ref"));

    let quarantined_updated_at: String =
        sqlx::query_scalar("SELECT updated_at FROM sessions WHERE id = 'session-b'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(quarantined_updated_at.contains('T'));
    assert!(quarantined_updated_at.ends_with('Z'));
    chrono::DateTime::parse_from_rfc3339(&quarantined_updated_at).unwrap();

    let binding_owner: String = sqlx::query_scalar(
        "SELECT session_id FROM provider_session_bindings WHERE provider_id = 'fake' AND provider_session_ref = 'shared-ref'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(binding_owner, "session-a");

    let binding_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_session_bindings WHERE provider_id = 'fake'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(binding_count, 2);

    pool.close().await;
}

#[tokio::test]
async fn subagent_sessions_and_last_message_for_run() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();

    let ws = store
        .create_workspace("test".into(), "/tmp/test".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store.create_task(ws.id, "task".into(), None).await.unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/test".into(), "deadbeef".into(), None)
        .await
        .unwrap();

    let parent = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let subagent = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "subagent".into(),
            Some(parent.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();
    let _reviewer = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "reviewer".into(),
            Some(parent.id),
            Some("reviewer".into()),
            None,
        )
        .await
        .unwrap();

    let subs = store.list_subagent_sessions(parent.id).await.unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].id.0, subagent.id.0);

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_message(Message {
            id: MessageId::new(),
            session_id: subagent.id,
            task_id: subagent.task_id,
            run_id: Some(run_id),
            turn_id: Some(turn_id),
            turn_sequence: Some(1),
            order_seq: None,
            role: MessageRole::Assistant,
            content: "final response".to_string(),
            attachments: vec![],
            delivery: MessageDelivery::Immediate,
            delivered_at: None,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();

    let last = store
        .get_last_assistant_message_for_run(subagent.id, run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.content, "final response");
}

#[tokio::test]
async fn archived_subagents_are_hidden_from_active_queries_and_labels_can_be_reused() {
    let fixture = setup_session_fixture().await;
    let store = &fixture.store;
    let parent = store
        .get_session(fixture.session_id)
        .await
        .unwrap()
        .unwrap();

    let archived = store
        .create_session(
            fixture.task_id,
            fixture.workspace_id,
            fixture.worktree_id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "subagent".into(),
            Some(parent.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();
    assert!(store
        .update_session_title(archived.id, "Alpha".into())
        .await
        .unwrap());
    assert_eq!(
        store
            .count_active_subagent_sessions(parent.id)
            .await
            .unwrap(),
        1
    );
    assert!(store
        .subagent_label_exists(parent.task_id, "Alpha")
        .await
        .unwrap());

    assert!(store
        .archive_subagent_session(parent.id, archived.id)
        .await
        .unwrap());
    assert!(store
        .list_subagent_sessions(parent.id)
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .get_subagent_session_by_label(parent.id, "Alpha")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .count_active_subagent_sessions(parent.id)
            .await
            .unwrap(),
        0
    );
    assert!(!store
        .subagent_label_exists(parent.task_id, "Alpha")
        .await
        .unwrap());
    let task_sessions = store.list_sessions_for_task(parent.task_id).await.unwrap();
    assert_eq!(
        task_sessions.len(),
        1,
        "task session listings should hide archived children"
    );
    let all_task_sessions = store
        .list_all_sessions_for_task(parent.task_id)
        .await
        .unwrap();
    assert_eq!(
        all_task_sessions.len(),
        2,
        "all task session listings should retain archived children for cleanup flows"
    );
    let worktree_sessions = store
        .list_sessions_for_worktree(parent.worktree_id)
        .await
        .unwrap();
    assert_eq!(
        worktree_sessions.len(),
        1,
        "worktree session listings should hide archived children"
    );

    let replacement = store
        .create_session(
            fixture.task_id,
            fixture.workspace_id,
            fixture.worktree_id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "subagent".into(),
            Some(parent.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();
    assert!(store
        .update_session_title(replacement.id, "Alpha".into())
        .await
        .unwrap());

    let looked_up = store
        .get_subagent_session_by_label(parent.id, "Alpha")
        .await
        .unwrap()
        .expect("replacement should be active");
    assert_eq!(looked_up.id, replacement.id);
}

#[tokio::test]
async fn tool_projection_normalizes_mixed_payloads_and_rebuilds_from_event_log() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();

    let result_event = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolResult,
            serde_json::json!({
                "tool_call_id": "tool-42",
                "order_seq": 1,
                "status": "ok",
                "result": "cwd=/tmp/project"
            }),
        )
        .await
        .unwrap();
    let call_event = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolCall,
            serde_json::json!({
                "toolCallId": "tool-42",
                "order_seq": 2,
                "kind": "shell",
                "tool_label": "Run shell",
                "rawInput": { "command": "pwd" }
            }),
        )
        .await
        .unwrap();

    assert!(result_event.seq < call_event.seq);

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, turn_id, false)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .map(|event| event.payload_json["tool_call_id"].as_str())
            .collect::<Vec<_>>(),
        vec![Some("tool-42"), Some("tool-42")]
    );

    let persisted = fixture
        .store
        .get_session_turn_tool(fixture.session_id, "tool-42")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.order_seq, 1);
    assert_eq!(persisted.first_event_seq, Some(result_event.seq));
    assert_eq!(persisted.tool_kind.as_deref(), Some("shell"));
    assert_eq!(persisted.title.as_deref(), Some("Run shell"));
    assert_eq!(persisted.status.as_deref(), Some("completed"));
    assert_eq!(persisted.output_text.as_deref(), Some("cwd=/tmp/project"));
    assert_eq!(
        persisted
            .input_json
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(|value| value.as_str()),
        Some("pwd")
    );

    delete_tool_projection(&fixture.db_path, fixture.session_id, "tool-42").await;

    let rebuilt = fixture
        .store
        .list_turn_tools(fixture.session_id, turn_id)
        .await
        .unwrap();
    assert_eq!(rebuilt.len(), 1);
    let rebuilt = &rebuilt[0];
    assert_eq!(rebuilt.tool_call_id, "tool-42");
    assert_eq!(rebuilt.order_seq, 1);
    assert_eq!(rebuilt.first_event_seq, Some(result_event.seq));
    assert_eq!(rebuilt.tool_kind.as_deref(), Some("shell"));
    assert_eq!(rebuilt.title.as_deref(), Some("Run shell"));
    assert_eq!(rebuilt.status.as_deref(), Some("completed"));
    assert_eq!(rebuilt.output_text.as_deref(), Some("cwd=/tmp/project"));
    assert_eq!(
        rebuilt
            .input_json
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(|value| value.as_str()),
        Some("pwd")
    );
}

#[tokio::test]
async fn tool_projection_uses_provider_tool_name_when_title_is_missing() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();

    let call_event = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolCall,
            serde_json::json!({
                "toolCallId": "tool-43",
                "order_seq": 1,
                "kind": "execute",
                "toolCall": {
                    "name": "Bash",
                    "kind": "execute"
                },
                "rawInput": {
                    "command": "pwd",
                    "description": "Print working directory"
                }
            }),
        )
        .await
        .unwrap();
    let _result_event = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolResult,
            serde_json::json!({
                "tool_call_id": "tool-43",
                "order_seq": 1,
                "status": "completed",
                "toolCall": {
                    "name": "Bash",
                    "kind": "execute"
                },
                "result": "/tmp/project"
            }),
        )
        .await
        .unwrap();

    let events = fixture
        .store
        .list_session_events_for_turn(fixture.session_id, turn_id, false)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);

    let persisted = fixture
        .store
        .get_session_turn_tool(fixture.session_id, "tool-43")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.order_seq, 1);
    assert_eq!(persisted.first_event_seq, Some(call_event.seq));
    assert_eq!(persisted.tool_kind.as_deref(), Some("execute"));
    assert_eq!(persisted.provider_tool_name.as_deref(), Some("Bash"));
    assert_eq!(persisted.title.as_deref(), Some("Bash"));
    assert_eq!(
        persisted.subtitle.as_deref(),
        Some("Print working directory")
    );
    assert_eq!(persisted.status.as_deref(), Some("completed"));

    delete_tool_projection(&fixture.db_path, fixture.session_id, "tool-43").await;

    let rebuilt = fixture
        .store
        .list_turn_tools(fixture.session_id, turn_id)
        .await
        .unwrap();
    assert_eq!(rebuilt.len(), 1);
    let rebuilt = &rebuilt[0];
    assert_eq!(rebuilt.tool_call_id, "tool-43");
    assert_eq!(rebuilt.order_seq, 1);
    assert_eq!(rebuilt.first_event_seq, Some(call_event.seq));
    assert_eq!(rebuilt.tool_kind.as_deref(), Some("execute"));
    assert_eq!(rebuilt.provider_tool_name.as_deref(), Some("Bash"));
    assert_eq!(rebuilt.title.as_deref(), Some("Bash"));
    assert_eq!(rebuilt.subtitle.as_deref(), Some("Print working directory"));
    assert_eq!(rebuilt.status.as_deref(), Some("completed"));
}

#[tokio::test]
async fn session_head_snapshot_strips_partials_and_stream_only_events() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_partial(
            fixture.session_id,
            turn_id,
            Some("draft assistant"),
            Some("draft thought"),
            Utc::now(),
        )
        .await
        .unwrap();

    let assistant_chunk = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::AssistantChunk,
            serde_json::json!({ "text": "partial assistant" }),
        )
        .await
        .unwrap();
    let thought_chunk = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ThoughtChunk,
            serde_json::json!({ "text": "partial thought" }),
        )
        .await
        .unwrap();
    let assistant_complete = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::AssistantComplete,
            serde_json::json!({
                "full_content": "final answer",
                "message_id": "provider-msg-1",
                "order_seq": 2
            }),
        )
        .await
        .unwrap();
    let inserted_message = make_assistant_message(
        fixture.session_id,
        fixture.task_id,
        run_id,
        turn_id,
        "final answer",
    );
    let notice = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "test_checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .insert_message(inserted_message.clone())
        .await
        .unwrap();
    let inserted_event = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::AssistantMessageInserted,
            serde_json::json!({
                "message_id": inserted_message.id.0.to_string(),
                "provider_message_id": "provider-msg-1",
                "content": inserted_message.content,
                "order_seq": 2
            }),
        )
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_status(
            fixture.session_id,
            turn_id,
            SessionTurnStatus::Completed,
            Some(notice.seq),
            None,
            Utc::now(),
        )
        .await
        .unwrap();

    assert!(assistant_chunk.transient);
    assert!(assistant_chunk.seq < 0);
    assert!(thought_chunk.transient);
    assert!(thought_chunk.seq < 0);
    assert!(assistant_complete.seq > 0);

    let persisted_events = fixture
        .store
        .list_session_events(fixture.session_id)
        .await
        .unwrap();
    assert!(persisted_events
        .iter()
        .any(|event| matches!(event.event_type, SessionEventType::Notice)));

    let head = fixture
        .store
        .get_session_head_snapshot(fixture.session_id, 10, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.last_event_seq, inserted_event.seq);
    assert_eq!(head.turns.len(), 1);
    assert_eq!(head.turns[0].assistant_partial, None);
    assert_eq!(head.turns[0].thought_partial, None);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "final answer");
    assert!(head
        .events
        .iter()
        .all(|event| !matches!(event.event_type, SessionEventType::AssistantComplete)));
    assert!(head
        .events
        .iter()
        .any(|event| matches!(event.event_type, SessionEventType::Notice)));

    let active_head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_head.turns.len(), 1);
    assert_eq!(active_head.turns[0].assistant_partial, None);
    assert_eq!(active_head.turns[0].thought_partial, None);
    assert_eq!(active_head.messages.len(), 1);
    assert_eq!(active_head.messages[0].content, "final answer");
    assert!(active_head
        .events
        .iter()
        .all(|event| !matches!(event.event_type, SessionEventType::AssistantComplete)));
    assert!(active_head.events.is_empty());
}

#[tokio::test]
async fn active_snapshot_materialization_keeps_tool_projection_without_transient_updates() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_partial(
            fixture.session_id,
            turn_id,
            Some("assistant partial"),
            Some("thought partial"),
            Utc::now(),
        )
        .await
        .unwrap();
    let call = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolCall,
            serde_json::json!({
                "toolCallId": "tool-7",
                "order_seq": 2,
                "kind": "shell",
                "tool_label": "List directory",
                "rawInput": { "command": "ls -la" }
            }),
        )
        .await
        .unwrap();
    let update = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolCallUpdate,
            serde_json::json!({
                "tool_call_id": "tool-7",
                "order_seq": 2,
                "status": "running",
                "output_text": "streaming output"
            }),
        )
        .await
        .unwrap();
    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::ToolResult,
            serde_json::json!({
                "tool_call_id": "tool-7",
                "order_seq": 2,
                "status": "completed",
                "result": "done"
            }),
        )
        .await
        .unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "tool completed",
        ))
        .await
        .unwrap();

    assert!(update.transient);
    assert!(update.seq < 0);

    let active_head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_head.session.id, fixture.session_id);
    assert_eq!(active_head.turns.len(), 1);
    assert_eq!(active_head.turns[0].assistant_partial, None);
    assert_eq!(active_head.turns[0].thought_partial, None);
    assert!(active_head.events.is_empty());
    assert_eq!(active_head.messages.len(), 1);
    assert_eq!(active_head.messages[0].content, "tool completed");
    assert_eq!(active_head.tool_summaries.len(), 1);
    let tool = &active_head.tool_summaries[0];
    assert_eq!(tool.tool_call_id, "tool-7");
    assert_eq!(tool.order_seq, 2);
    assert_eq!(tool.first_event_seq, Some(call.seq));
    assert_eq!(tool.tool_kind.as_deref(), Some("shell"));
    assert_eq!(tool.title.as_deref(), Some("List directory"));
    assert_eq!(tool.status.as_deref(), Some("completed"));
    assert_eq!(tool.output_preview.as_deref(), Some("done"));
    assert_eq!(
        tool.input_preview
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(|value| value.as_str()),
        Some("ls -la")
    );

    drop(fixture.store);
    let reopened = open_store_with_retry(&fixture.db_path).await;
    let reopened_head = reopened
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened_head.session.id, fixture.session_id);
    assert_eq!(reopened_head.session.worktree_id, fixture.worktree_id);
    assert_eq!(reopened_head.tool_summaries.len(), 1);
    assert_eq!(reopened_head.tool_summaries[0].tool_call_id, "tool-7");
    assert_eq!(reopened_head.tool_summaries[0].order_seq, 2);
    assert_eq!(
        reopened_head.tool_summaries[0].status.as_deref(),
        Some("completed")
    );
}

#[tokio::test]
async fn archived_session_head_reconstructs_without_partial_buffers_after_reopen() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_partial(
            fixture.session_id,
            turn_id,
            Some("unfinished assistant"),
            Some("unfinished thought"),
            Utc::now(),
        )
        .await
        .unwrap();
    let notice = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "archive_ready", "message": "persist me" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "archived final answer",
        ))
        .await
        .unwrap();
    fixture
        .store
        .update_session_turn_status(
            fixture.session_id,
            turn_id,
            SessionTurnStatus::Completed,
            Some(notice.seq),
            None,
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(fixture.store.archive_task(fixture.task_id).await.unwrap());

    drop(fixture.store);
    let reopened = open_store_with_retry(&fixture.db_path).await;
    let head = reopened
        .get_session_head_snapshot(fixture.session_id, 10, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.turns.len(), 1);
    assert_eq!(head.turns[0].assistant_partial, None);
    assert_eq!(head.turns[0].thought_partial, None);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "archived final answer");
    assert_eq!(head.events.len(), 1);
    assert!(matches!(
        head.events[0].event_type,
        SessionEventType::Notice
    ));
    assert!(reopened
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn session_heads_preserve_latest_turn_when_it_exceeds_message_limit() {
    let fixture = setup_session_fixture().await;

    let older_run_id = RunId::new();
    let older_turn_id = TurnId::new();
    let mut older_turn = make_turn(fixture.session_id, older_run_id, older_turn_id);
    older_turn.start_seq = Some(1);
    fixture.store.insert_session_turn(older_turn).await.unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            older_run_id,
            older_turn_id,
            "older turn message",
        ))
        .await
        .unwrap();

    let latest_run_id = RunId::new();
    let latest_turn_id = TurnId::new();
    let mut latest_turn = make_turn(fixture.session_id, latest_run_id, latest_turn_id);
    latest_turn.start_seq = Some(2);
    fixture
        .store
        .insert_session_turn(latest_turn)
        .await
        .unwrap();
    for index in 0..221 {
        fixture
            .store
            .insert_message(make_assistant_message(
                fixture.session_id,
                fixture.task_id,
                latest_run_id,
                latest_turn_id,
                &format!("latest message {index}"),
            ))
            .await
            .unwrap();
    }

    let head = fixture
        .store
        .get_session_head_snapshot(fixture.session_id, 10, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.turns.len(), 1);
    assert_eq!(head.turns[0].turn_id, latest_turn_id);
    assert_eq!(head.messages.len(), 221);
    assert!(head
        .messages
        .iter()
        .all(|message| message.turn_id == Some(latest_turn_id)));
    assert!(head.has_more_turns);

    let active_head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_head.turns.len(), 1);
    assert_eq!(active_head.turns[0].turn_id, latest_turn_id);
    assert_eq!(active_head.messages.len(), 221);
    assert!(active_head
        .messages
        .iter()
        .all(|message| message.turn_id == Some(latest_turn_id)));
    assert!(active_head.has_more_turns);
}

#[tokio::test]
async fn session_heads_bound_latest_turn_tool_summaries() {
    const EXPECTED_TOOL_SUMMARY_LIMIT: usize = 96;
    const EXPECTED_HEAD_BYTE_LIMIT: i64 = 256_000;

    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    let mut turn = make_turn(fixture.session_id, run_id, turn_id);
    turn.start_seq = Some(1);
    turn.tool_total = 335;
    turn.tool_completed = 335;
    fixture.store.insert_session_turn(turn).await.unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "latest assistant content",
        ))
        .await
        .unwrap();

    let total_tools = 335;
    let now = Utc::now();
    for index in 0..total_tools {
        fixture
            .store
            .upsert_session_turn_tool(SessionTurnTool {
                session_id: fixture.session_id,
                tool_call_id: format!("tool-{index:03}"),
                turn_id,
                tool_kind: Some("execute".to_string()),
                provider_tool_name: Some("Bash".to_string()),
                title: Some(format!("Bash {index}")),
                subtitle: Some(format!("command {index}")),
                status: Some("completed".to_string()),
                input_json: Some(serde_json::json!({ "cmd": format!("echo {index}") })),
                output_text: Some(format!("output {index}")),
                order_seq: index as i64,
                first_event_seq: Some(index as i64),
                input_truncated: Some(false),
                input_original_bytes: None,
                output_truncated: Some(false),
                output_original_bytes: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .unwrap();
    }

    let head = fixture
        .store
        .get_session_head_snapshot(fixture.session_id, 10, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.turns.len(), 1);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "latest assistant content");
    assert_eq!(head.tool_summaries.len(), EXPECTED_TOOL_SUMMARY_LIMIT);
    assert_eq!(
        head.tool_summaries
            .first()
            .map(|tool| tool.tool_call_id.as_str()),
        Some("tool-239")
    );
    assert_eq!(
        head.tool_summaries
            .last()
            .map(|tool| tool.tool_call_id.as_str()),
        Some("tool-334")
    );
    assert!(head.head_window.truncated);
    assert!(head.head_window.bytes <= EXPECTED_HEAD_BYTE_LIMIT);

    let active_head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        active_head.tool_summaries.len(),
        EXPECTED_TOOL_SUMMARY_LIMIT
    );
    assert_eq!(
        active_head
            .tool_summaries
            .last()
            .map(|tool| tool.tool_call_id.as_str()),
        Some("tool-334")
    );
}

#[tokio::test]
async fn active_session_head_snapshot_uses_requested_limit_for_large_unmaterialized_heads() {
    let fixture = setup_session_fixture().await;
    let total_turns = 240_i64;
    let requested_turns = 60_u32;

    for index in 0..total_turns {
        let run_id = RunId::new();
        let turn_id = TurnId::new();
        let mut turn = make_turn(fixture.session_id, run_id, turn_id);
        turn.start_seq = Some(index + 1);
        turn.end_seq = None;
        turn.status = SessionTurnStatus::Running;
        turn.tool_total = 1;
        turn.tool_completed = 1;
        fixture.store.insert_session_turn(turn).await.unwrap();

        let notice = fixture
            .store
            .append_session_event(
                fixture.session_id,
                Some(run_id),
                Some(turn_id),
                SessionEventType::Notice,
                serde_json::json!({
                    "kind": "large_head_checkpoint",
                    "turn_index": index,
                }),
            )
            .await
            .unwrap();
        fixture
            .store
            .insert_message(make_assistant_message(
                fixture.session_id,
                fixture.task_id,
                run_id,
                turn_id,
                &format!("answer {index}"),
            ))
            .await
            .unwrap();
        let now = Utc::now();
        fixture
            .store
            .upsert_session_turn_tool(SessionTurnTool {
                session_id: fixture.session_id,
                tool_call_id: format!("tool-{index}"),
                turn_id,
                tool_kind: Some("execute".to_string()),
                provider_tool_name: Some("Bash".to_string()),
                title: Some("Bash".to_string()),
                subtitle: Some(format!("turn {index}")),
                status: Some("completed".to_string()),
                input_json: Some(serde_json::json!({ "cmd": format!("echo {index}") })),
                output_text: Some(format!("output {index}")),
                order_seq: 1,
                first_event_seq: Some(notice.seq),
                input_truncated: Some(false),
                input_original_bytes: None,
                output_truncated: Some(false),
                output_original_bytes: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .unwrap();
        fixture
            .store
            .update_session_turn_status(
                fixture.session_id,
                turn_id,
                SessionTurnStatus::Completed,
                Some(notice.seq),
                None,
                now,
            )
            .await
            .unwrap();
    }

    let active_materialization_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_head_materializations WHERE session_id = ? AND head_kind = 'active'",
    )
    .bind(fixture.session_id.0.to_string())
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    assert_eq!(
        active_materialization_count, 0,
        "active session heads should not rely on durable session_head_materializations rows"
    );

    let head = fixture
        .store
        .get_session_head_snapshot(fixture.session_id, requested_turns, true)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(head.turns.len(), requested_turns as usize);
    assert!(head.has_more_turns);
    assert_eq!(head.turns.first().unwrap().start_seq, Some(181));
    assert_eq!(head.turns.last().unwrap().start_seq, Some(total_turns));
    assert_eq!(head.messages.len(), requested_turns as usize);
    assert_eq!(head.messages.first().unwrap().content, "answer 180");
    assert_eq!(head.messages.last().unwrap().content, "answer 239");
    assert_eq!(head.tool_summaries.len(), requested_turns as usize);
    assert!(head
        .tool_summaries
        .iter()
        .any(|tool| tool.tool_call_id == "tool-239"));
    assert!(head.events.len() <= 200);

    let active_materialization_count_after_read: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM session_head_materializations WHERE session_id = ? AND head_kind = 'active'",
    )
    .bind(fixture.session_id.0.to_string())
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    assert_eq!(
        active_materialization_count_after_read, 0,
        "bounded active reads must not create active session_head_materializations rows"
    );
}

#[tokio::test]
async fn workspace_active_page_includes_primary_and_subagent_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();

    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let primary = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, primary.id, worktree.id)
        .await
        .unwrap();
    let subagent = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "reviewer".into(),
            Some(primary.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();

    let (summaries, total) = store.list_workspace_active_page(ws.id, 50).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.primary_session.session.id, primary.id);
    assert!(summary.primary_session_head.is_none());
    assert_eq!(summary.sessions.len(), 1);
    assert_eq!(summary.sessions[0].session.id, subagent.id);
}

#[tokio::test]
async fn subagent_active_snapshot_head_is_built_on_demand_without_durable_row() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let primary = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, primary.id, worktree.id)
        .await
        .unwrap();
    let subagent = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "reviewer".into(),
            Some(primary.id),
            Some("sub_agent".into()),
            None,
        )
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(subagent.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            subagent.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    store
        .insert_message(make_assistant_message(
            subagent.id,
            task.id,
            run_id,
            turn_id,
            "subagent answer",
        ))
        .await
        .unwrap();

    let persisted: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM session_active_snapshot_heads WHERE session_id = ?)",
    )
    .bind(subagent.id.0.to_string())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(persisted, 0);

    let head = store
        .get_active_snapshot_head(subagent.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.last_event_seq, notice.seq);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "subagent answer");

    let persisted_after_read: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM session_active_snapshot_heads WHERE session_id = ?)",
    )
    .bind(subagent.id.0.to_string())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(persisted_after_read, 0);
}

#[tokio::test]
async fn projection_rev_is_consistent_across_head_and_workspace_summary_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    store
        .insert_message(make_assistant_message(
            session.id,
            task.id,
            run_id,
            turn_id,
            "final answer",
        ))
        .await
        .unwrap();
    store
        .update_session_turn_status(
            session.id,
            turn_id,
            SessionTurnStatus::Completed,
            Some(notice.seq),
            None,
            Utc::now(),
        )
        .await
        .unwrap();

    let head = store
        .get_session_head_snapshot(session.id, 10, true)
        .await
        .unwrap()
        .unwrap();
    let active_head = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    let (summaries, total) = store.list_workspace_active_page(ws.id, 50).await.unwrap();

    assert_eq!(total, 1);
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert!(head.projection_rev > 0);
    assert_eq!(active_head.projection_rev, head.projection_rev);
    assert_eq!(summary.primary_session.projection_rev, head.projection_rev);
    assert_eq!(
        summary.primary_session.last_event_seq,
        Some(head.last_event_seq)
    );
}

#[tokio::test]
async fn workspace_active_head_batch_rebuilds_malformed_projection_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    store
        .insert_message(make_assistant_message(
            session.id,
            task.id,
            run_id,
            turn_id,
            "first answer",
        ))
        .await
        .unwrap();

    let _ = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        r#"UPDATE session_snapshot_summaries
           SET projection_rev = projection_rev + 1,
               updated_at = ?
           WHERE session_id = ?"#,
    )
    .bind(Utc::now().to_rfc3339())
    .bind(session.id.0.to_string())
    .execute(store.pool())
    .await
    .unwrap();

    sqlx::query(
        r#"UPDATE session_active_snapshot_heads
           SET turns_json = '{broken'
           WHERE session_id = ?"#,
    )
    .bind(session.id.0.to_string())
    .execute(store.pool())
    .await
    .unwrap();

    let expected_rev = store.get_session_projection_rev(session.id).await.unwrap();
    let heads = store
        .list_workspace_active_head_snapshots(ws.id)
        .await
        .unwrap();
    assert_eq!(heads.len(), 1);
    let head = &heads[0];
    assert_eq!(head.session.id, session.id);
    assert_eq!(head.projection_rev, expected_rev);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "first answer");
}

#[tokio::test]
async fn partial_turn_updates_do_not_advance_projection_rev() {
    let fixture = setup_session_fixture().await;
    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    let initial_rev = fixture
        .store
        .get_session_projection_rev(fixture.session_id)
        .await
        .unwrap();

    fixture
        .store
        .update_session_turn_partial(
            fixture.session_id,
            turn_id,
            Some("partial assistant"),
            Some("partial thought"),
            Utc::now(),
        )
        .await
        .unwrap();

    let refreshed_rev = fixture
        .store
        .get_session_projection_rev(fixture.session_id)
        .await
        .unwrap();
    assert_eq!(refreshed_rev, initial_rev);

    let active_head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_head.projection_rev, initial_rev);
}

#[tokio::test]
async fn active_snapshot_projection_refreshes_when_projection_rev_changes_without_new_event() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();

    let initial = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.last_event_seq, notice.seq);
    assert!(initial.messages.is_empty());

    store
        .insert_message(make_assistant_message(
            session.id,
            task.id,
            run_id,
            turn_id,
            "final answer",
        ))
        .await
        .unwrap();

    let refreshed = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.last_event_seq, notice.seq);
    assert!(refreshed.projection_rev > initial.projection_rev);
    assert_eq!(refreshed.messages.len(), 1);
    assert_eq!(refreshed.messages[0].content, "final answer");

    let pool = SqlitePool::connect(&sqlite_url(&db_path)).await.unwrap();
    let materialized_head_rev: i64 = sqlx::query_scalar(
        "SELECT head_rev FROM session_active_snapshot_heads WHERE session_id = ?",
    )
    .bind(session.id.0.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    pool.close().await;

    assert_eq!(materialized_head_rev, refreshed.projection_rev);
}

#[tokio::test]
async fn active_snapshot_projection_refreshes_when_summary_checkpoint_updates_without_new_event() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    store
        .insert_message(make_assistant_message(
            session.id,
            task.id,
            run_id,
            turn_id,
            "final answer",
        ))
        .await
        .unwrap();

    let initial = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(initial.summary_checkpoint.is_none());

    let checkpoint = ctx_core::models::SessionSummaryCheckpoint {
        session_id: session.id,
        checkpoint_id: "cp-1".to_string(),
        summary: "compacted summary".to_string(),
        last_turn_id: Some(turn_id),
        last_event_seq: Some(notice.seq),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store
        .upsert_session_summary_checkpoint(checkpoint.clone())
        .await
        .unwrap();

    let projection_rev = store.get_session_projection_rev(session.id).await.unwrap();
    assert!(
        projection_rev > initial.projection_rev,
        "summary checkpoint writes should advance projection_rev even without new events"
    );

    let refreshed = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    let summary_checkpoint = refreshed
        .summary_checkpoint
        .expect("active head should include the refreshed summary checkpoint");
    assert_eq!(summary_checkpoint.checkpoint_id, checkpoint.checkpoint_id);
    assert_eq!(summary_checkpoint.summary, checkpoint.summary);
    assert_eq!(summary_checkpoint.last_event_seq, Some(notice.seq));
    assert_eq!(refreshed.last_event_seq, notice.seq);
    assert_eq!(refreshed.projection_rev, projection_rev);

    let durable_head_rev: i64 = sqlx::query_scalar(
        "SELECT head_rev FROM session_active_snapshot_heads WHERE session_id = ?",
    )
    .bind(session.id.0.to_string())
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(durable_head_rev, projection_rev);
}

#[tokio::test]
async fn flush_active_snapshot_head_projection_queue_materializes_current_primary_head() {
    let fixture = setup_session_fixture().await;
    fixture
        .store
        .set_task_primary_session(fixture.task_id, fixture.session_id, fixture.worktree_id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    let notice = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "coalesced answer",
        ))
        .await
        .unwrap();

    fixture
        .store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    let head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.last_event_seq, notice.seq);
    assert_eq!(head.messages.len(), 1);
    assert_eq!(head.messages[0].content, "coalesced answer");

    let materialized_head_rev: i64 = sqlx::query_scalar(
        "SELECT head_rev FROM session_active_snapshot_heads WHERE session_id = ?",
    )
    .bind(fixture.session_id.0.to_string())
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    assert_eq!(materialized_head_rev, head.projection_rev);
}

#[tokio::test]
async fn flush_active_snapshot_head_projection_queue_applies_latest_state_after_multiple_writes() {
    let fixture = setup_session_fixture().await;
    fixture
        .store
        .set_task_primary_session(fixture.task_id, fixture.session_id, fixture.worktree_id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "first" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "first answer",
        ))
        .await
        .unwrap();
    let latest_notice = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "latest" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "final answer",
        ))
        .await
        .unwrap();

    fixture
        .store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    let head = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.last_event_seq, latest_notice.seq);
    assert_eq!(head.messages.len(), 2);
    assert_eq!(head.messages[0].content, "first answer");
    assert_eq!(head.messages[1].content, "final answer");
}

#[tokio::test]
async fn queued_active_head_refresh_does_not_mark_stale_row_fresh_before_single_read() {
    let fixture = setup_session_fixture().await;
    fixture
        .store
        .set_task_primary_session(fixture.task_id, fixture.session_id, fixture.worktree_id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "initial" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    let initial = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(initial.messages.is_empty());
    let initial_projection_rev = initial.projection_rev;

    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "fresh answer",
        ))
        .await
        .unwrap();
    let latest_notice = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "after message" }),
        )
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT head_rev, last_event_seq, messages_json FROM session_active_snapshot_heads WHERE session_id = ?",
    )
    .bind(fixture.session_id.0.to_string())
    .fetch_one(fixture.store.pool())
    .await
    .unwrap();
    let materialized_head_rev: i64 = row.try_get("head_rev").unwrap();
    let materialized_last_event_seq: i64 = row.try_get("last_event_seq").unwrap();
    let materialized_messages_json: String = row.try_get("messages_json").unwrap();
    assert_eq!(materialized_head_rev, initial_projection_rev);
    assert!(materialized_last_event_seq < latest_notice.seq);
    assert!(!materialized_messages_json.contains("fresh answer"));

    let refreshed = fixture
        .store
        .get_active_snapshot_head(fixture.session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(refreshed.projection_rev > initial_projection_rev);
    assert_eq!(refreshed.last_event_seq, latest_notice.seq);
    assert_eq!(refreshed.messages.len(), 1);
    assert_eq!(refreshed.messages[0].content, "fresh answer");
}

#[tokio::test]
async fn queued_active_head_refresh_does_not_blank_workspace_batch_read() {
    let fixture = setup_session_fixture().await;
    fixture
        .store
        .set_task_primary_session(fixture.task_id, fixture.session_id, fixture.worktree_id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    fixture
        .store
        .insert_session_turn(make_turn(fixture.session_id, run_id, turn_id))
        .await
        .unwrap();
    fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "initial" }),
        )
        .await
        .unwrap();
    fixture
        .store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    fixture
        .store
        .insert_message(make_assistant_message(
            fixture.session_id,
            fixture.task_id,
            run_id,
            turn_id,
            "workspace fresh answer",
        ))
        .await
        .unwrap();
    let latest_notice = fixture
        .store
        .append_session_event(
            fixture.session_id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "after message" }),
        )
        .await
        .unwrap();

    let snapshots = fixture
        .store
        .list_workspace_active_head_snapshots(fixture.workspace_id)
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].session.id, fixture.session_id);
    assert_eq!(snapshots[0].last_event_seq, latest_notice.seq);
    assert_eq!(snapshots[0].messages.len(), 1);
    assert_eq!(snapshots[0].messages[0].content, "workspace fresh answer");
}

#[tokio::test]
async fn store_open_repairs_missing_active_snapshot_head_projection() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    store
        .insert_message(make_assistant_message(
            session.id,
            task.id,
            run_id,
            turn_id,
            "final answer",
        ))
        .await
        .unwrap();

    sqlx::query("DELETE FROM session_active_snapshot_heads WHERE session_id = ?")
        .bind(session.id.0.to_string())
        .execute(store.pool())
        .await
        .unwrap();
    store.close().await;

    let reopened = Store::open(&db_path).await.unwrap();
    let repaired = reopened
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    let repaired_row_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM session_active_snapshot_heads WHERE session_id = ?)",
    )
    .bind(session.id.0.to_string())
    .fetch_one(reopened.pool())
    .await
    .unwrap();
    assert_eq!(repaired_row_exists, 1);
    assert_eq!(repaired.last_event_seq, notice.seq);
    assert_eq!(repaired.messages.len(), 1);
    assert_eq!(repaired.messages[0].content, "final answer");
}

#[tokio::test]
async fn store_open_invalidates_legacy_0052_active_snapshot_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();
    store
        .insert_message(make_assistant_message(
            session.id,
            task.id,
            run_id,
            turn_id,
            "fresh answer",
        ))
        .await
        .unwrap();
    store
        .flush_active_snapshot_head_projection_queue()
        .await
        .unwrap();

    let initial = store
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        r#"UPDATE session_active_snapshot_heads
           SET head_rev = ?,
               messages_json = ?,
               updated_at = ?
           WHERE session_id = ?"#,
    )
    .bind(initial.projection_rev)
    .bind(serde_json::to_string(&Vec::<Message>::new()).unwrap())
    .bind(Utc::now().to_rfc3339())
    .bind(session.id.0.to_string())
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ?")
        .bind(54_i64)
        .execute(store.pool())
        .await
        .unwrap();
    store.close().await;

    let reopened = Store::open(&db_path).await.unwrap();
    let repaired_row_exists_before_read: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM session_active_snapshot_heads WHERE session_id = ?)",
    )
    .bind(session.id.0.to_string())
    .fetch_one(reopened.pool())
    .await
    .unwrap();
    assert_eq!(
        repaired_row_exists_before_read, 0,
        "legacy 0052 repair should invalidate durable active-head rows before any read trusts them"
    );

    let repaired = reopened
        .get_active_snapshot_head(session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repaired.last_event_seq, notice.seq);
    assert_eq!(repaired.messages.len(), 1);
    assert_eq!(repaired.messages[0].content, "fresh answer");

    let durable_head_rev: i64 = sqlx::query_scalar(
        "SELECT head_rev FROM session_active_snapshot_heads WHERE session_id = ?",
    )
    .bind(session.id.0.to_string())
    .fetch_one(reopened.pool())
    .await
    .unwrap();
    assert_eq!(durable_head_rev, repaired.projection_rev);
}

#[tokio::test]
async fn session_head_materialization_refreshes_when_projection_rev_changes_without_new_event() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("ws".into(), "/tmp/ws".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "active".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/ws".into(), "abc123".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .set_task_primary_session(task.id, session.id, worktree.id)
        .await
        .unwrap();

    let run_id = RunId::new();
    let turn_id = TurnId::new();
    store
        .insert_session_turn(make_turn(session.id, run_id, turn_id))
        .await
        .unwrap();
    let notice = store
        .append_session_event(
            session.id,
            Some(run_id),
            Some(turn_id),
            SessionEventType::Notice,
            serde_json::json!({ "kind": "checkpoint", "message": "stable" }),
        )
        .await
        .unwrap();

    assert!(store.archive_task(task.id).await.unwrap());
    let initial = store
        .get_session_head_snapshot(session.id, 10, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial.last_event_seq, notice.seq);
    assert_eq!(initial.turns[0].status, SessionTurnStatus::Running);

    store
        .update_session_turn_status(
            session.id,
            turn_id,
            SessionTurnStatus::Completed,
            Some(notice.seq),
            None,
            Utc::now(),
        )
        .await
        .unwrap();

    let refreshed = store
        .get_session_head_snapshot(session.id, 10, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.last_event_seq, notice.seq);
    assert_eq!(refreshed.turns[0].status, SessionTurnStatus::Completed);
    assert!(refreshed.projection_rev > initial.projection_rev);
    assert_eq!(
        refreshed.activity.last_turn_status,
        Some(SessionTurnStatus::Completed)
    );

    let pool = SqlitePool::connect(&sqlite_url(&db_path)).await.unwrap();
    let mut materialized_head_rev = -1_i64;
    for _ in 0..20 {
        materialized_head_rev = sqlx::query_scalar(
            "SELECT head_rev FROM session_head_materializations WHERE session_id = ? AND head_kind = 'archived'",
        )
        .bind(session.id.0.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        if materialized_head_rev == refreshed.projection_rev {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pool.close().await;

    assert_eq!(materialized_head_rev, refreshed.projection_rev);
}

#[cfg(feature = "fault_injection")]
async fn setup_fault_fixture() -> (
    tempfile::TempDir,
    Store,
    ctx_core::ids::WorkspaceId,
    ctx_core::ids::TaskId,
    ctx_core::ids::WorktreeId,
    ctx_core::ids::SessionId,
) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let store = Store::open(&db_path).await.unwrap();
    let ws = store
        .create_workspace("fault".into(), "/tmp/fault".into(), VcsKind::Git)
        .await
        .unwrap();
    let task = store
        .create_task(ws.id, "fault task".into(), None)
        .await
        .unwrap();
    let worktree = store
        .create_worktree(ws.id, "/tmp/fault".into(), "deadbeef".into(), None)
        .await
        .unwrap();
    let session = store
        .create_session(
            task.id,
            ws.id,
            worktree.id,
            ctx_core::models::ExecutionEnvironment::Host,
            "fake".into(),
            "fake".into(),
            "implementer".into(),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    (dir, store, ws.id, task.id, worktree.id, session.id)
}

#[cfg(feature = "fault_injection")]
#[tokio::test]
async fn fault_injection_append_session_event_fails_once_then_recovers() {
    let (_dir, store, _ws_id, _task_id, _worktree_id, session_id) = setup_fault_fixture().await;
    crate::fault_injection::clear_failpoints();
    crate::fault_injection::set_failpoint("ctx_store.append_session_event", 1);

    let first = store
        .append_session_event(
            session_id,
            None,
            None,
            SessionEventType::Notice,
            serde_json::json!({"msg":"first"}),
        )
        .await;
    assert!(first.is_err(), "expected injected failure for first append");

    let second = store
        .append_session_event(
            session_id,
            None,
            None,
            SessionEventType::Notice,
            serde_json::json!({"msg":"second"}),
        )
        .await;
    assert!(second.is_ok(), "expected recovery after one-shot failpoint");
    crate::fault_injection::clear_failpoints();
}

#[cfg(feature = "fault_injection")]
#[tokio::test]
async fn fault_injection_list_session_events_page_fails_once_then_recovers() {
    let (_dir, store, _ws_id, _task_id, _worktree_id, session_id) = setup_fault_fixture().await;
    crate::fault_injection::clear_failpoints();
    store
        .append_session_event(
            session_id,
            None,
            None,
            SessionEventType::Notice,
            serde_json::json!({"msg":"seed"}),
        )
        .await
        .unwrap();

    crate::fault_injection::set_failpoint("ctx_store.list_session_events_page_by_seq", 1);
    let first = store
        .list_session_events_page_by_seq(session_id, None, None, false)
        .await;
    assert!(first.is_err(), "expected injected failure for first list");

    let second = store
        .list_session_events_page_by_seq(session_id, None, None, false)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    crate::fault_injection::clear_failpoints();
}

#[cfg(feature = "fault_injection")]
#[tokio::test]
async fn fault_injection_session_head_snapshot_fails_once_then_recovers() {
    let (_dir, store, _ws_id, _task_id, _worktree_id, session_id) = setup_fault_fixture().await;
    crate::fault_injection::clear_failpoints();
    store
        .append_session_event(
            session_id,
            None,
            None,
            SessionEventType::Notice,
            serde_json::json!({"msg":"seed"}),
        )
        .await
        .unwrap();

    crate::fault_injection::set_failpoint("ctx_store.get_session_head_snapshot", 1);
    let first = store.get_session_head_snapshot(session_id, 10, true).await;
    assert!(
        first.is_err(),
        "expected injected failure for session head snapshot"
    );

    let second = store
        .get_session_head_snapshot(session_id, 10, true)
        .await
        .unwrap();
    assert!(
        second.is_some(),
        "expected session head snapshot after recovery"
    );
    crate::fault_injection::clear_failpoints();
}
