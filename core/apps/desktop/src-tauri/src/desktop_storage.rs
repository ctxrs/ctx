use super::*;
pub(super) use ctx_desktop_ipc::{
    DesktopStorageBatchOp, DesktopStorageBatchReq, DesktopStorageGetReq, DesktopStorageNotice,
    DesktopUiStateResetReason,
};

#[derive(Default)]
pub(super) struct DesktopStorage {
    pool: OnceCell<SqlitePool>,
}

const UI_KV_CREATE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS ui_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at_ms INTEGER NOT NULL)";
const UI_META_CREATE_SQL: &str =
    "CREATE TABLE IF NOT EXISTS ui_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at_ms INTEGER NOT NULL)";
const UI_STATE_RESET_NOTICE_KEY: &str = "ui_state_reset_notice";

impl DesktopStorage {
    pub(super) async fn pool(&self, app: &tauri::AppHandle) -> Result<&SqlitePool> {
        self.pool
            .get_or_try_init(|| async {
                let path = desktop_storage_path(app)?;
                open_desktop_storage_pool_at_path(&path).await
            })
            .await
    }
}

async fn open_desktop_storage_pool_at_path(path: &Path) -> Result<SqlitePool> {
    prepare_desktop_storage_sqlite_file_family(path)?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("opening desktop storage sqlite db")?;
    ensure_ui_kv_schema(&pool).await?;
    harden_existing_desktop_storage_sqlite_file_family(path)?;
    Ok(pool)
}

fn prepare_desktop_storage_sqlite_file_family(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        ctx_fs::permissions::ensure_private_dir_sync(parent)?;
    }
    let mut missing = Vec::new();
    for member in desktop_storage_sqlite_file_family(path) {
        if !validate_desktop_storage_sqlite_file_member(&member)? {
            missing.push(member);
        }
    }
    for member in missing {
        ctx_fs::permissions::write_private_file_atomic_sync(&member, b"")?;
    }
    harden_existing_desktop_storage_sqlite_file_family(path)
}

fn harden_existing_desktop_storage_sqlite_file_family(path: &Path) -> Result<()> {
    for member in desktop_storage_sqlite_file_family(path) {
        if validate_desktop_storage_sqlite_file_member(&member)? {
            ctx_fs::permissions::harden_private_file_sync(&member)?;
        }
    }
    Ok(())
}

fn validate_desktop_storage_sqlite_file_member(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if desktop_storage_metadata_is_link_or_reparse_point(&metadata) => {
            anyhow::bail!(
                "desktop storage sqlite path must not be a symlink or reparse point: {}",
                path.display()
            );
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "desktop storage sqlite path must be a regular file: {}",
                path.display()
            );
        }
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err)
            .with_context(|| format!("reading desktop storage sqlite path {}", path.display())),
    }
}

fn desktop_storage_sqlite_file_family(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.to_string_lossy())),
        PathBuf::from(format!("{}-shm", path.to_string_lossy())),
    ]
}

#[cfg(windows)]
fn desktop_storage_metadata_is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn desktop_storage_metadata_is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

async fn ensure_ui_meta_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query(UI_META_CREATE_SQL)
        .execute(pool)
        .await
        .context("creating ui_meta table")?;
    Ok(())
}

async fn read_ui_kv_schema(pool: &SqlitePool) -> Result<Vec<(String, i64, i64)>> {
    let rows = sqlx::query("PRAGMA table_info(ui_kv)")
        .fetch_all(pool)
        .await
        .context("reading ui_kv schema")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.try_get("name").context("reading ui_kv column name")?;
        let notnull: i64 = row.try_get("notnull").context("reading ui_kv notnull")?;
        let pk: i64 = row.try_get("pk").context("reading ui_kv pk")?;
        out.push((name, notnull, pk));
    }
    Ok(out)
}

fn is_current_ui_kv_schema(columns: &[(String, i64, i64)]) -> bool {
    if columns.len() != 3 {
        return false;
    }
    let (name0, _notnull0, pk0) = &columns[0];
    let (name1, notnull1, pk1) = &columns[1];
    let (name2, notnull2, pk2) = &columns[2];
    name0 == "key"
        && *pk0 == 1
        && name1 == "value"
        && *notnull1 == 1
        && *pk1 == 0
        && name2 == "updated_at_ms"
        && *notnull2 == 1
        && *pk2 == 0
}

async fn record_ui_state_reset_notice(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    reason: DesktopUiStateResetReason,
) -> Result<()> {
    let value = serde_json::to_string(&DesktopStorageNotice::UiStateReset { reason })
        .context("serializing ui state reset notice")?;
    sqlx::query(
        "INSERT INTO ui_meta (key, value, updated_at_ms) VALUES (?1, ?2, ?3) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at_ms=excluded.updated_at_ms",
    )
    .bind(UI_STATE_RESET_NOTICE_KEY)
    .bind(value)
    .bind(now_ms_i64())
    .execute(&mut **tx)
    .await
    .context("writing ui state reset notice")?;
    Ok(())
}

async fn reset_ui_kv_state(pool: &SqlitePool, reason: DesktopUiStateResetReason) -> Result<()> {
    let mut tx = pool
        .begin()
        .await
        .context("starting ui state reset transaction")?;
    sqlx::query("DROP TABLE IF EXISTS ui_kv")
        .execute(&mut *tx)
        .await
        .context("dropping ui_kv table during reset")?;
    sqlx::query(UI_KV_CREATE_SQL)
        .execute(&mut *tx)
        .await
        .context("recreating ui_kv table during reset")?;
    sqlx::query(UI_META_CREATE_SQL)
        .execute(&mut *tx)
        .await
        .context("ensuring ui_meta table during reset")?;
    record_ui_state_reset_notice(&mut tx, reason).await?;
    tx.commit()
        .await
        .context("committing ui state reset transaction")?;
    eprintln!(
        "desktop_ui_state_db_reset reason={}",
        desktop_ui_state_reset_reason_str(reason)
    );
    Ok(())
}

fn desktop_ui_state_reset_reason_str(reason: DesktopUiStateResetReason) -> &'static str {
    match reason {
        DesktopUiStateResetReason::SchemaMismatch => "schema_mismatch",
        DesktopUiStateResetReason::InvalidUiStateDb => "invalid_ui_state_db",
    }
}

async fn ensure_ui_kv_schema(pool: &SqlitePool) -> Result<()> {
    ensure_ui_meta_schema(pool).await?;
    sqlx::query(UI_KV_CREATE_SQL)
        .execute(pool)
        .await
        .context("creating ui_kv table")?;

    let schema = match read_ui_kv_schema(pool).await {
        Ok(schema) => schema,
        Err(err) => {
            eprintln!(
                "desktop_ui_state_db_schema_read_failed reason=invalid_ui_state_db error={err:#}"
            );
            reset_ui_kv_state(pool, DesktopUiStateResetReason::InvalidUiStateDb).await?;
            return Ok(());
        }
    };
    if !is_current_ui_kv_schema(&schema) {
        reset_ui_kv_state(pool, DesktopUiStateResetReason::SchemaMismatch).await?;
    }

    Ok(())
}

async fn consume_desktop_storage_notice(pool: &SqlitePool) -> Result<Option<DesktopStorageNotice>> {
    let mut tx = pool
        .begin()
        .await
        .context("starting desktop storage notice transaction")?;
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM ui_meta WHERE key = ?1")
        .bind(UI_STATE_RESET_NOTICE_KEY)
        .fetch_optional(&mut *tx)
        .await
        .context("reading desktop storage notice")?;
    if row.is_none() {
        tx.commit()
            .await
            .context("committing desktop storage notice transaction")?;
        return Ok(None);
    }

    sqlx::query("DELETE FROM ui_meta WHERE key = ?1")
        .bind(UI_STATE_RESET_NOTICE_KEY)
        .execute(&mut *tx)
        .await
        .context("deleting desktop storage notice")?;
    tx.commit()
        .await
        .context("committing desktop storage notice transaction")?;

    let Some((raw_value,)) = row else {
        return Ok(None);
    };
    match serde_json::from_str::<DesktopStorageNotice>(&raw_value) {
        Ok(notice) => Ok(Some(notice)),
        Err(err) => {
            eprintln!("dropping invalid desktop storage notice payload: {err}");
            Ok(None)
        }
    }
}

async fn desktop_storage_get_from_pool(
    pool: &SqlitePool,
    key: &str,
) -> Result<Option<serde_json::Value>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM ui_kv WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("reading ui_kv value")?;
    if let Some((value,)) = row {
        match serde_json::from_str(&value) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(err) => {
                let _ = sqlx::query("DELETE FROM ui_kv WHERE key = ?1")
                    .bind(key)
                    .execute(pool)
                    .await;
                eprintln!("dropping corrupt ui_kv value for key {key}: {err}");
                Ok(None)
            }
        }
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod desktop_storage_tests {
    use super::*;

    async fn open_memory_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_ui_kv_schema_initializes_current_schema() {
        let pool = open_memory_pool().await;
        ensure_ui_kv_schema(&pool).await.unwrap();
        let schema = read_ui_kv_schema(&pool).await.unwrap();
        assert!(is_current_ui_kv_schema(&schema));
        let notice = consume_desktop_storage_notice(&pool).await.unwrap();
        assert!(notice.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn schema_mismatch_resets_ui_state_and_notice_is_one_time() {
        let pool = open_memory_pool().await;
        sqlx::query("CREATE TABLE ui_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO ui_kv (key, value) VALUES (?1, ?2)")
            .bind("legacy")
            .bind("\"payload\"")
            .execute(&pool)
            .await
            .unwrap();

        ensure_ui_kv_schema(&pool).await.unwrap();

        let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ui_kv")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            row_count, 0,
            "ui_kv contents should be reset on schema mismatch"
        );
        let schema = read_ui_kv_schema(&pool).await.unwrap();
        assert!(is_current_ui_kv_schema(&schema));

        let notice = consume_desktop_storage_notice(&pool).await.unwrap();
        assert_eq!(
            notice,
            Some(DesktopStorageNotice::UiStateReset {
                reason: DesktopUiStateResetReason::SchemaMismatch,
            })
        );
        let second = consume_desktop_storage_notice(&pool).await.unwrap();
        assert!(
            second.is_none(),
            "reset notice must be consumed after first read"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn corrupt_key_self_heal_is_log_only_and_does_not_emit_notice() {
        let pool = open_memory_pool().await;
        ensure_ui_kv_schema(&pool).await.unwrap();
        sqlx::query("INSERT INTO ui_kv (key, value, updated_at_ms) VALUES (?1, ?2, ?3)")
            .bind("bad")
            .bind("{bad-json")
            .bind(now_ms_i64())
            .execute(&pool)
            .await
            .unwrap();

        let got = desktop_storage_get_from_pool(&pool, "bad").await.unwrap();
        assert!(got.is_none());
        let still_exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ui_kv WHERE key = ?1")
            .bind("bad")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(still_exists, 0);

        let notice = consume_desktop_storage_notice(&pool).await.unwrap();
        assert!(notice.is_none(), "corrupt-key self-heal must stay log-only");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn desktop_storage_file_pool_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("ctx-desktop-storage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("ui").join("desktop-ui-state.sqlite");

        let pool = open_desktop_storage_pool_at_path(&path).await.unwrap();
        pool.close().await;

        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn desktop_storage_prepare_reserves_sqlite_sidecars_before_open() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("ctx-desktop-storage-{}", uuid::Uuid::new_v4()));
        let path = dir.join("ui").join("desktop-ui-state.sqlite");

        prepare_desktop_storage_sqlite_file_family(&path).unwrap();

        for member in desktop_storage_sqlite_file_family(&path) {
            let metadata = std::fs::symlink_metadata(&member).unwrap();
            assert!(
                metadata.is_file(),
                "sqlite family member must be a regular file: {}",
                member.display()
            );
            assert!(
                !metadata.file_type().is_symlink(),
                "sqlite family member must not be a symlink: {}",
                member.display()
            );
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn desktop_storage_file_pool_rejects_symlinked_sqlite_sidecar_before_open() {
        for suffix in ["-wal", "-shm"] {
            let dir =
                std::env::temp_dir().join(format!("ctx-desktop-storage-{}", uuid::Uuid::new_v4()));
            let ui_dir = dir.join("ui");
            let path = ui_dir.join("desktop-ui-state.sqlite");
            let sidecar = PathBuf::from(format!("{}{suffix}", path.to_string_lossy()));
            let outside = dir.join("outside-sidecar");
            std::fs::create_dir_all(&ui_dir).unwrap();
            std::fs::write(&outside, b"outside").unwrap();
            std::os::unix::fs::symlink(&outside, &sidecar).unwrap();

            let err = open_desktop_storage_pool_at_path(&path).await.unwrap_err();

            assert!(format!("{err:#}").contains("symlink or reparse point"));
            assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
            assert!(
                !path.exists(),
                "sqlite main file should not be created after sidecar rejection"
            );
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[tauri::command]
pub(super) async fn desktop_storage_get(
    app: tauri::AppHandle,
    storage: tauri::State<'_, DesktopStorage>,
    req: DesktopStorageGetReq,
) -> Result<Option<serde_json::Value>, String> {
    let pool = storage.pool(&app).await.map_err(to_err)?;
    desktop_storage_get_from_pool(pool, &req.key)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub(super) async fn desktop_storage_batch(
    app: tauri::AppHandle,
    storage: tauri::State<'_, DesktopStorage>,
    req: DesktopStorageBatchReq,
) -> Result<(), String> {
    let ops = req.ops;
    if ops.is_empty() {
        return Ok(());
    }
    let pool = storage.pool(&app).await.map_err(to_err)?;
    let mut tx = pool.begin().await.map_err(to_err)?;
    let now = now_ms_i64();
    for op in ops {
        match op {
            DesktopStorageBatchOp::Set { key, value } => {
                let value_json = serde_json::to_string(&value).map_err(to_err)?;
                sqlx::query(
                    "INSERT INTO ui_kv (key, value, updated_at_ms) VALUES (?1, ?2, ?3) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at_ms=excluded.updated_at_ms",
                )
                .bind(&key)
                .bind(&value_json)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(to_err)?;
            }
            DesktopStorageBatchOp::Delete { key } => {
                sqlx::query("DELETE FROM ui_kv WHERE key = ?1")
                    .bind(&key)
                    .execute(&mut *tx)
                    .await
                    .map_err(to_err)?;
            }
        }
    }
    tx.commit().await.map_err(to_err)?;
    Ok(())
}

#[tauri::command]
pub(super) async fn desktop_storage_consume_notice(
    app: tauri::AppHandle,
    storage: tauri::State<'_, DesktopStorage>,
) -> Result<Option<DesktopStorageNotice>, String> {
    let pool = storage.pool(&app).await.map_err(to_err)?;
    consume_desktop_storage_notice(pool).await.map_err(to_err)
}

fn desktop_storage_path(_app: &tauri::AppHandle) -> Result<PathBuf> {
    let root = desktop_local_data_root()?;
    Ok(ctx_fs::paths::ui_root(root).join("desktop-ui-state.sqlite"))
}
