use super::super::{
    vector_store_schema::{
        semantic_vector_failure_kind, SemanticVectorFailureKind, SEMANTIC_VECTOR_BACKEND_FLAT_F32,
        SEMANTIC_VECTOR_SCHEMA_VERSION,
    },
    vector_store_search::{scan_exact_generation, SEMANTIC_EXACT_TOP_K_MAX},
};
use super::*;
use crate::SourceBackedGenerationPin;
use std::path::{Path, PathBuf};

fn test_event_identity_digest(event_id: Uuid) -> Option<[u8; 32]> {
    let mut digest = [0; 32];
    digest[..16].copy_from_slice(event_id.as_bytes());
    digest[16..].copy_from_slice(event_id.as_bytes());
    Some(digest)
}

fn exact_search(
    store: &SemanticVectorStore,
    query_embedding: &[f32],
    limit: usize,
) -> Result<SemanticVectorSearch> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    let query_embeddings = vec![query_embedding.to_vec()];
    scan_exact_generation(
        &pinned,
        &query_embeddings,
        limit,
        &test_event_identity_digest,
        std::time::Instant::now(),
    )
}

fn exact_search_multi(
    store: &SemanticVectorStore,
    query_embeddings: &[Vec<f32>],
    limit: usize,
) -> Result<SemanticVectorSearch> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    scan_exact_generation(
        &pinned,
        query_embeddings,
        limit,
        &test_event_identity_digest,
        std::time::Instant::now(),
    )
}

fn active_counts(store: &SemanticVectorStore) -> Result<(usize, usize)> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    Ok((pinned.stats().active_events, pinned.stats().active_chunks))
}

fn durable_tree_snapshot(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    fn visit(root: &Path, path: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
        if path.is_file() {
            files.push((path.strip_prefix(root)?.to_path_buf(), std::fs::read(path)?));
            return Ok(());
        }
        for entry in std::fs::read_dir(path)? {
            visit(root, &entry?.path(), files)?;
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

#[test]
fn flat_store_control_catalog_has_no_vectors_or_plaintext() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    let contract = test_contract();
    assert_eq!(root, temp.path().join("search").join("semantic"));
    let store = SemanticVectorStore::open(&root, &contract)?;

    assert_eq!(store.contract(), &contract);
    assert!(root.is_dir());
    assert!(root.join("state.sqlite").is_file());
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
        SEMANTIC_VECTOR_SCHEMA_VERSION
    );
    let schema = store.conn.query_row(
        "SELECT group_concat(COALESCE(sql, ''), '\n') FROM sqlite_schema",
        [],
        |row| row.get::<_, String>(0),
    )?;
    assert!(!schema.contains("semantic_source_documents"));
    assert!(!schema.contains("locator_json"));
    assert!(!schema.contains("embedding_f32"));
    assert!(!schema.contains("event_embedding"));
    assert!(!schema.contains("chunk_text"));
    assert!(!schema.contains("USING vec"));
    let read_only = SemanticVectorStore::open_read_only(&root, &contract)?
        .expect("created semantic store must reopen read-only");
    assert_eq!(read_only.contract(), &contract);
    Ok(())
}

#[test]
fn new_store_creates_a_private_root_and_coordination_lock() -> Result<()> {
    use ctx_history_platform::platform_security::{verify_private_directory, verify_private_file};

    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    drop(SemanticVectorStore::open(&root, &test_contract())?);

    verify_private_directory(&root)?;
    verify_private_file(&root.join("flat_transaction.lock"))?;
    Ok(())
}

#[test]
fn writable_open_establishes_the_private_root_before_any_flat_artifact() -> Result<()> {
    use ctx_history_platform::platform_security::verify_private_directory;

    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let observed_root = root.clone();
    let store =
        SemanticVectorStore::open_after_private_root_ready(&root, &test_contract(), move || {
            verify_private_directory(&observed_root)
                .expect("the semantic root must be private before Flat initialization");
            assert!(
                std::fs::read_dir(&observed_root)
                    .expect("the private semantic root must be readable")
                    .next()
                    .is_none(),
                "no Flat artifact may predate the private semantic root"
            );
        })?;

    assert!(root.join("flat_transaction.lock").is_file());
    drop(store);
    Ok(())
}

#[cfg(unix)]
#[test]
fn writable_open_refuses_a_symlinked_final_root_without_creation() -> Result<()> {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    let temporary = tempfile::tempdir()?;
    let target = temporary.path().join("outside");
    let root = temporary.path().join("semantic");
    std::fs::create_dir(&target)?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))?;
    symlink(&target, &root)?;

    assert!(SemanticVectorStore::open(&root, &test_contract()).is_err());
    assert!(std::fs::symlink_metadata(&root)?.file_type().is_symlink());
    assert!(
        std::fs::read_dir(&target)?.next().is_none(),
        "private creation must not follow a symlinked final root"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn writable_open_repairs_only_an_existing_legacy_semantic_root() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir()?;
    let search = temporary.path().join("search");
    let root = search.join("semantic");
    std::fs::create_dir(&search)?;
    std::fs::create_dir(&root)?;
    std::fs::set_permissions(&search, std::fs::Permissions::from_mode(0o755))?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;

    drop(SemanticVectorStore::open(&root, &test_contract())?);

    assert_eq!(
        std::fs::metadata(&root)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&search)?.permissions().mode() & 0o777,
        0o755,
        "private root establishment must not chmod an existing ancestor"
    );
    Ok(())
}

#[test]
fn passive_snapshot_accepts_a_legacy_regular_coordination_lock() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let lock = root.join("flat_transaction.lock");
    std::fs::remove_file(&lock)?;
    std::fs::write(&lock, b"legacy")?;
    let before = durable_tree_snapshot(&root)?;

    let passive = SemanticVectorStore::open_passive_snapshot(&root, &contract)?
        .expect("a legacy regular lock must remain compatible");

    assert!(matches!(
        passive.source_backed_generation_pin_exact(&"e".repeat(64), 0)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert_eq!(durable_tree_snapshot(&root)?, before);
    Ok(())
}

#[test]
fn passive_snapshot_open_leaves_the_durable_tree_unchanged() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    let store = SemanticVectorStore::open(&root, &contract)?;
    drop(store);
    let before = durable_tree_snapshot(&root)?;

    let passive = SemanticVectorStore::open_passive_snapshot(&root, &contract)?;

    assert!(passive.is_some());
    assert_eq!(durable_tree_snapshot(&root)?, before);
    assert!(!root.join("state.sqlite-wal").exists());
    assert!(!root.join("state.sqlite-shm").exists());
    Ok(())
}

#[test]
fn passive_snapshot_refuses_a_live_wal_without_touching_it() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    let store = SemanticVectorStore::open(&root, &contract)?;
    store
        .conn
        .execute("UPDATE semantic_index_stats SET dirty_items = 0", [])?;
    let wal = root.join("state.sqlite-wal");
    assert!(wal.exists(), "fixture must retain live WAL state");
    let before = std::fs::read(&wal)?;

    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("a live WAL cannot be read through an immutable passive snapshot"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(std::fs::read(&wal)?, before);
    drop(store);
    Ok(())
}

#[test]
fn passive_snapshot_lock_blocks_writer_until_pin_and_refuses_later_wal() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    let writer = SemanticVectorStore::open(&root, &contract)?;
    assert!(!root.join("state.sqlite-wal").exists());
    let writer_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("flat_transaction.lock"))?;

    let passive =
        SemanticVectorStore::open_passive_snapshot_after_admission(&root, &contract, || {
            fs2::FileExt::try_lock_shared(&writer_lock)
                .expect("passive admission must use a shared coordination lock");
            fs2::FileExt::unlock(&writer_lock).unwrap();
            assert!(
                fs2::FileExt::try_lock_exclusive(&writer_lock).is_err(),
                "the passive admission guard must exclude a writer before open"
            );
        })?
        .expect("the initialized passive store must open");
    assert!(matches!(
        passive.source_backed_generation_pin_exact(&"a".repeat(64), 0)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert!(
        fs2::FileExt::try_lock_exclusive(&writer_lock).is_err(),
        "the passive store must retain coordination through generation pinning"
    );
    drop(passive);
    fs2::FileExt::try_lock_exclusive(&writer_lock)?;
    fs2::FileExt::unlock(&writer_lock)?;

    writer
        .flat
        .begin_source_generation_view()
        .map_err(anyhow::Error::new)?;
    writer.conn.execute(
        "INSERT INTO semantic_index_stats(id, dirty_items) VALUES (1, 7)
         ON CONFLICT(id) DO UPDATE SET dirty_items = excluded.dirty_items",
        [],
    )?;
    writer
        .flat
        .end_source_generation_view()
        .map_err(anyhow::Error::new)?;

    let ordinary = SemanticVectorStore::open_read_only(&root, &contract)?
        .expect("ordinary WAL-aware read must open");
    assert_eq!(
        ordinary.conn.query_row(
            "SELECT dirty_items FROM semantic_index_stats WHERE id = 1",
            [],
            |row| { row.get::<_, u64>(0) }
        )?,
        7
    );
    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("the committed WAL must never be ignored by an immutable reader"),
        Err(error) => error,
    };
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    Ok(())
}

#[test]
fn passive_snapshot_writer_contention_fails_promptly_without_mutation() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let before = durable_tree_snapshot(&root)?;
    let writer_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("flat_transaction.lock"))?;
    fs2::FileExt::lock_exclusive(&writer_lock)?;
    // Passive admission uses try_lock_shared, so writer-first contention is an
    // ordinary typed result in this thread rather than a timing assertion.
    let result = SemanticVectorStore::open_passive_snapshot(&root, &contract);
    fs2::FileExt::unlock(&writer_lock)?;
    let error = match result {
        Ok(_) => panic!("writer contention must make the passive snapshot unavailable"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(durable_tree_snapshot(&root)?, before);
    Ok(())
}

// Darwin filesystems reject malformed UTF-8 path components with EILSEQ before
// the store can be created. Other Unix targets retain the invalid-byte oracle;
// Darwin separately exercises WAL/rollback-journal refusal and symlinked
// parent components.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn passive_snapshot_supports_non_utf8_roots_and_exact_sidecar_paths() -> Result<()> {
    use std::os::unix::ffi::OsStringExt;

    let temporary = tempfile::tempdir()?;
    let child = std::ffi::OsString::from_vec(b"semantic-\xff-root".to_vec());
    let root = temporary.path().join(child);
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    assert!(SemanticVectorStore::open_passive_snapshot(&root, &contract)?.is_some());

    let mut wal = root.join("state.sqlite").into_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    std::fs::write(&wal, b"exact non-UTF8 WAL path")?;
    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("an exact non-UTF8 WAL sidecar must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(std::fs::read(wal)?, b"exact non-UTF8 WAL path");
    Ok(())
}

#[cfg(unix)]
#[test]
fn passive_snapshot_supports_symlinked_parent_components_without_following_the_database(
) -> Result<()> {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let real_parent = temporary.path().join("real-parent");
    std::fs::create_dir(&real_parent)?;
    let alias_parent = temporary.path().join("alias-parent");
    symlink(&real_parent, &alias_parent)?;
    let real_root = real_parent.join("semantic");
    let alias_root = alias_parent.join("semantic");
    let contract = test_contract();
    drop(SemanticVectorStore::open(&real_root, &contract)?);

    let store = SemanticVectorStore::open_passive_snapshot(&alias_root, &contract)?
        .expect("canonical parent path must retain the initialized store");
    assert!(matches!(
        store.source_backed_generation_pin_exact(&"c".repeat(64), 0)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert!(
        std::fs::symlink_metadata(real_parent.join("semantic/state.sqlite"))?
            .file_type()
            .is_file()
    );

    drop(store);
    let database = real_root.join("state.sqlite");
    let real_database = real_root.join("state.sqlite.real");
    std::fs::rename(&database, &real_database)?;
    symlink(&real_database, &database)?;
    let error = match SemanticVectorStore::open_passive_snapshot(&alias_root, &contract) {
        Ok(_) => panic!("the final database symlink must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    Ok(())
}

#[test]
fn passive_snapshot_refuses_a_hot_rollback_journal_without_touching_it() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let mut control = rusqlite::Connection::open(root.join("state.sqlite"))?;
    control.pragma_update(None, "journal_mode", "DELETE")?;
    let transaction =
        control.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO semantic_index_stats(id, dirty_items) VALUES (1, 9)
         ON CONFLICT(id) DO UPDATE SET dirty_items = excluded.dirty_items",
        [],
    )?;
    let journal = root.join("state.sqlite-journal");
    let before = std::fs::read(&journal)?;

    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("a hot rollback journal must be refused"),
        Err(error) => error,
    };
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(std::fs::read(&journal)?, before);
    drop(transaction);
    Ok(())
}

#[cfg(unix)]
#[test]
fn passive_snapshot_succeeds_with_a_write_denied_valid_root() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let original = std::fs::metadata(&root)?.permissions();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500))?;
    let result = SemanticVectorStore::open_passive_snapshot(&root, &contract);
    std::fs::set_permissions(&root, original)?;
    assert!(result?.is_some());
    Ok(())
}

#[test]
fn passive_snapshot_never_recreates_a_missing_coordination_lock() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let lock = root.join("flat_transaction.lock");
    std::fs::remove_file(&lock)?;
    let before = durable_tree_snapshot(&root)?;

    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("passive admission requires the existing store lock"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert!(!lock.exists());
    assert_eq!(durable_tree_snapshot(&root)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn passive_snapshot_refuses_a_symlinked_coordination_lock_without_touching_the_target() -> Result<()>
{
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let lock = root.join("flat_transaction.lock");
    let target = temporary.path().join("outside-lock");
    std::fs::write(&target, b"outside")?;
    std::fs::remove_file(&lock)?;
    symlink(&target, &lock)?;
    let before = std::fs::read(&target)?;

    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("a redirected passive coordination lock must be refused"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(std::fs::read(target)?, before);
    assert!(std::fs::symlink_metadata(lock)?.file_type().is_symlink());
    Ok(())
}

#[cfg(windows)]
#[test]
fn passive_snapshot_refuses_a_reparse_point_coordination_lock_without_creation() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let lock = root.join("flat_transaction.lock");
    let target = temporary.path().join("outside-lock-directory");
    std::fs::create_dir(&target)?;
    std::fs::remove_file(&lock)?;
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&lock)
        .arg(&target)
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("failed to create junction lock fixture"));
    }

    let error = match SemanticVectorStore::open_passive_snapshot(&root, &contract) {
        Ok(_) => panic!("a reparse-point passive coordination lock must be refused"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert!(std::fs::read_dir(&target)?.next().is_none());
    assert!(!lock.is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn passive_snapshot_detects_coordination_lock_replacement_after_admission() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let lock = root.join("flat_transaction.lock");
    let displaced = root.join("flat_transaction.lock.displaced");

    let result =
        SemanticVectorStore::open_passive_snapshot_after_admission(&root, &contract, || {
            std::fs::rename(&lock, &displaced).unwrap();
            std::fs::write(&lock, b"replacement").unwrap();
        });
    let error = match result {
        Ok(_) => panic!("a replaced coordination lock must invalidate passive admission"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(std::fs::read(lock)?, b"replacement");
    assert!(displaced.is_file());
    Ok(())
}

#[cfg(windows)]
#[test]
fn passive_snapshot_retained_lock_prevents_windows_path_replacement() -> Result<()> {
    use std::sync::{Arc, Mutex};

    let temporary = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temporary.path());
    let contract = test_contract();
    drop(SemanticVectorStore::open(&root, &contract)?);
    let lock = root.join("flat_transaction.lock");
    let displaced = root.join("flat_transaction.lock.displaced");
    let replacement = Arc::new(Mutex::new(None));
    let attempted = Arc::clone(&replacement);

    let passive =
        SemanticVectorStore::open_passive_snapshot_after_admission(&root, &contract, || {
            *attempted.lock().unwrap() = Some(std::fs::rename(&lock, &displaced));
        })?
        .expect("a denied replacement must leave the passive snapshot valid");
    let replacement = replacement
        .lock()
        .unwrap()
        .take()
        .expect("replacement was attempted");

    assert!(
        replacement.is_err(),
        "the retained Windows lock must deny rename/delete sharing"
    );
    assert!(lock.is_file());
    assert!(!displaced.exists());
    assert!(matches!(
        passive.source_backed_generation_pin_exact(&"d".repeat(64), 0)?,
        SourceBackedGenerationPin::NotReady
    ));
    Ok(())
}

#[cfg(windows)]
#[test]
fn passive_snapshot_refuses_a_root_junction_without_touching_its_target() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let target = temporary.path().join("semantic-target");
    let junction = temporary.path().join("semantic-junction");
    let contract = test_contract();
    drop(SemanticVectorStore::open(&target, &contract)?);
    let before = durable_tree_snapshot(&target)?;
    let status = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "failed to create semantic root junction fixture"
        ));
    }

    let error = match SemanticVectorStore::open_passive_snapshot(&junction, &contract) {
        Ok(_) => panic!("a reparse-point semantic root must be refused"),
        Err(error) => error,
    };

    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::PassiveSnapshotUnavailable)
    );
    assert_eq!(durable_tree_snapshot(&target)?, before);
    Ok(())
}

#[test]
#[ignore = "manual isolated syscall probe; set CTX_PASSIVE_SNAPSHOT_PROBE_ROOT and mode"]
fn passive_snapshot_external_syscall_probe() -> Result<()> {
    let root = PathBuf::from(
        std::env::var_os("CTX_PASSIVE_SNAPSHOT_PROBE_ROOT")
            .ok_or_else(|| anyhow::anyhow!("CTX_PASSIVE_SNAPSHOT_PROBE_ROOT is required"))?,
    );
    let contract = test_contract();
    match std::env::var("CTX_PASSIVE_SNAPSHOT_PROBE_MODE").as_deref() {
        Ok("setup") => drop(SemanticVectorStore::open(&root, &contract)?),
        Ok("probe") => {
            let store = SemanticVectorStore::open_passive_snapshot(&root, &contract)?
                .ok_or_else(|| anyhow::anyhow!("passive syscall fixture is missing"))?;
            assert!(matches!(
                store.source_backed_generation_pin_exact(&"b".repeat(64), 0)?,
                SourceBackedGenerationPin::NotReady
            ));
        }
        _ => {
            return Err(anyhow::anyhow!(
                "CTX_PASSIVE_SNAPSHOT_PROBE_MODE must be setup or probe"
            ))
        }
    }
    Ok(())
}

#[test]
fn flat_search_is_unique_deterministic_bounded_and_exact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let contract = test_contract();
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), &contract)?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let first_chunk = test_chunk_at(first, 2, "first", 0, 2);
    let first_hash = first_chunk.source_text_hash.clone();
    store.publish_chunk_replacements(
        &[
            (first_chunk, test_embedding(&contract, 1.0, 0.0)),
            (
                test_chunk_at(first, 2, "first", 1, 2),
                test_embedding(&contract, 0.8, 0.6),
            ),
            (
                test_chunk(second, 1, "second"),
                test_embedding(&contract, 1.0, 0.0),
            ),
        ],
        &[],
    )?;

    let search = exact_search(&store, &test_embedding(&contract, 1.0, 0.0), 2)?;
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(
        search
            .hits
            .iter()
            .map(|hit| hit.event_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(search.stats.backend, Some(SEMANTIC_VECTOR_BACKEND_FLAT_F32));
    assert_eq!(search.stats.chunks_scanned, 3);
    assert_eq!(search.stats.events_scored, 2);
    assert_eq!(
        search
            .hits
            .iter()
            .find(|hit| hit.event_id == first)
            .expect("first event")
            .source_text_hash,
        first_hash
    );
    let error = exact_search(
        &store,
        &test_embedding(&contract, 1.0, 0.0),
        SEMANTIC_EXACT_TOP_K_MAX + 1,
    )
    .err()
    .expect("unsafe exact top-k must be refused");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::Unavailable)
    );
    Ok(())
}

#[test]
fn thirty_two_query_vectors_touch_each_flat_chunk_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let contract = test_contract();
    let mut store =
        SemanticVectorStore::open(&source_backed_semantic_vector_path(temp.path()), &contract)?;
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    store.publish_chunk_replacements(
        &[
            (
                test_chunk_at(first, 1, "first", 0, 2),
                test_embedding(&contract, 1.0, 0.0),
            ),
            (
                test_chunk_at(first, 1, "first", 1, 2),
                test_embedding(&contract, 0.8, 0.6),
            ),
            (
                test_chunk(second, 2, "second"),
                test_embedding(&contract, 0.0, 1.0),
            ),
        ],
        &[],
    )?;
    let queries = (0..32)
        .map(|index| {
            if index % 2 == 0 {
                test_embedding(&contract, 1.0, 0.0)
            } else {
                test_embedding(&contract, 0.0, 1.0)
            }
        })
        .collect::<Vec<_>>();

    let search = exact_search_multi(&store, &queries, 2)?;

    assert_eq!(search.stats.query_vectors, 32);
    assert_eq!(search.stats.vector_passes, 1);
    assert_eq!(search.stats.chunks_scanned, 3);
    assert_eq!(search.stats.events_scored, 2);
    assert_eq!(search.stats.dot_products, 96);
    assert_eq!(
        search.stats.vector_bytes_read,
        3 * contract.dimensions() * std::mem::size_of::<f32>()
    );
    assert_eq!(
        search
            .hits
            .iter()
            .find(|hit| hit.event_id == first)
            .expect("first event")
            .query_ordinal,
        0
    );
    assert_eq!(
        search
            .hits
            .iter()
            .find(|hit| hit.event_id == second)
            .expect("second event")
            .query_ordinal,
        1
    );
    Ok(())
}

#[test]
fn flat_rewrite_truncation_delete_and_restart_do_not_resurrect_chunks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    let contract = test_contract();
    let rewritten = Uuid::new_v4();
    let deleted = Uuid::new_v4();
    let rewritten_chunk = test_chunk(rewritten, 4, "rewrite-new");
    let rewritten_hash = rewritten_chunk.source_text_hash.clone();
    {
        let mut store = SemanticVectorStore::open(&root, &contract)?;
        store.publish_chunk_replacements(
            &[
                (
                    test_chunk_at(rewritten, 4, "rewrite-old", 0, 2),
                    test_embedding(&contract, 1.0, 0.0),
                ),
                (
                    test_chunk_at(rewritten, 4, "rewrite-old", 1, 2),
                    test_embedding(&contract, 1.0, 0.0),
                ),
                (
                    test_chunk(deleted, 5, "delete-me"),
                    test_embedding(&contract, 0.0, 1.0),
                ),
            ],
            &[],
        )?;
        store.publish_chunk_replacements(
            &[(rewritten_chunk, test_embedding(&contract, 0.0, 1.0))],
            &[],
        )?;
        assert_eq!(active_counts(&store)?, (2, 2));
        let rewritten_hit = exact_search(&store, &test_embedding(&contract, 1.0, 0.0), 2)?
            .hits
            .into_iter()
            .find(|hit| hit.event_id == rewritten)
            .expect("rewritten event");
        assert_eq!(rewritten_hit.similarity, 0.0);
        assert_eq!(rewritten_hit.source_text_hash, rewritten_hash);
        assert_eq!(store.delete_events(&[deleted])?, 1);
    }

    let store = SemanticVectorStore::open(&root, &contract)?;
    assert_eq!(active_counts(&store)?, (1, 1));
    let hits = exact_search(&store, &test_embedding(&contract, 1.0, 0.0), 10)?.hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event_id, rewritten);
    assert_eq!(hits[0].similarity, 0.0);
    assert_eq!(hits[0].source_text_hash, rewritten_hash);
    Ok(())
}

#[test]
fn future_control_schema_prevents_writable_flat_recovery_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = source_backed_semantic_vector_path(temp.path());
    let contract = test_contract();
    let mut store = SemanticVectorStore::open(&root, &contract)?;
    store.publish_chunk_replacements(
        &[(
            test_chunk(Uuid::new_v4(), 1, "future-schema"),
            test_embedding(&contract, 1.0, 0.0),
        )],
        &[],
    )?;
    drop(store);

    let recovery_artifact = root
        .join("flat_segments")
        .join(".flat-tmp-future-control-schema");
    fs::write(&recovery_artifact, b"must survive rejected writable open")?;
    let control = rusqlite::Connection::open(root.join("state.sqlite"))?;
    control.pragma_update(None, "user_version", SEMANTIC_VECTOR_SCHEMA_VERSION + 1)?;
    drop(control);

    let error = SemanticVectorStore::open(&root, &contract)
        .err()
        .expect("a future control schema must reject writable open");
    assert_eq!(
        semantic_vector_failure_kind(&error),
        Some(SemanticVectorFailureKind::NewerSchema)
    );
    assert_eq!(
        fs::read(recovery_artifact)?,
        b"must survive rejected writable open"
    );
    Ok(())
}
