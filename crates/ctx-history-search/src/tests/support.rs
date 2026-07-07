use super::{
    search_packet, CaptureProvider, EntityTimestamps, Fidelity, HistoryRecord, PacketOptions,
    ProviderSessionFilter, SearchFilters, SearchResultMode, SyncMetadata, SyncState, Utc, Uuid,
    Visibility,
};

fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("target/test-data")
        });
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-search-")
        .tempdir_in(root)
        .unwrap()
}

pub(super) fn fixed_time() -> chrono::DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub(super) fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: fixed_time(),
        updated_at: fixed_time(),
    }
}

pub(super) fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

pub(super) fn excluded_filter(session_id: Option<Uuid>) -> SearchFilters {
    SearchFilters {
        exclude_provider_session: Some(ProviderSessionFilter {
            provider: CaptureProvider::Codex,
            provider_session_id: "provider-session-1".into(),
            session_id,
        }),
        ..SearchFilters::default()
    }
}

pub(super) fn test_store() -> (tempfile::TempDir, ctx_history_store::Store) {
    let temp = tempdir();
    let path = temp.path().join("work.sqlite");
    let store = ctx_history_store::Store::open(path).unwrap();
    (temp, store)
}

pub(super) fn new_link_id(target_id: Uuid) -> Uuid {
    let mut bytes = *target_id.as_bytes();
    bytes[15] = bytes[15].wrapping_add(80);
    Uuid::from_bytes(bytes)
}

pub(super) fn maybe_write_synthetic_search_smoke_artifact() {
    let Ok(out_dir) = std::env::var("CTX_ARTIFACT_DIR") else {
        return;
    };

    let (_temp, store) = test_store();
    let mut records = Vec::new();
    for index in 0..48 {
        let mut record = HistoryRecord::new(
            format!("Synthetic search smoke {index:03}"),
            format!(
                "syntheticneedle generated body {index:03} {}",
                "detail ".repeat(12)
            ),
            vec!["synthetic".into(), "smoke".into()],
            "task",
            Some("/workspace/ctx".into()),
        );
        record.id =
            Uuid::parse_str(&format!("018f45d0-0000-7000-8000-00000002{index:04x}")).unwrap();
        record.created_at = fixed_time() + chrono::Duration::seconds(index);
        record.updated_at = record.created_at;
        records.push(record);
    }

    let import_started = std::time::Instant::now();
    store.upsert_records(&records).unwrap();
    let import_elapsed = import_started.elapsed();

    let options = PacketOptions {
        limit: 12,
        snippet_chars: 180,
        filters: SearchFilters::default(),
        result_mode: SearchResultMode::Sessions,
    };
    let search_started = std::time::Instant::now();
    let search = search_packet(&store, "syntheticneedle", &options).unwrap();
    let search_elapsed = search_started.elapsed();

    let import_secs = import_elapsed.as_secs_f64();
    let artifact = serde_json::json!({
        "schema_version": 1,
        "profile": "smoke",
        "corpus": {
            "records": records.len(),
            "events": records.len()
        },
        "import": {
            "duration_ms": import_elapsed.as_millis(),
            "events_per_sec": if import_secs > 0.0 {
                records.len() as f64 / import_secs
            } else {
                records.len() as f64
            }
        },
        "storage": {
            "db_bytes": std::fs::metadata(store.path()).map(|metadata| metadata.len()).unwrap_or(0)
        },
        "search": {
            "duration_ms": search_elapsed.as_millis(),
            "result_count": search.results.len(),
            "citation_count": search.results.iter().map(|result| result.citations.len()).sum::<usize>(),
            "truncation": search.truncation
        }
    });

    let out_dir = std::path::Path::new(&out_dir);
    std::fs::create_dir_all(out_dir).unwrap();
    std::fs::write(
        out_dir.join("synthetic-search-smoke.json"),
        serde_json::to_vec_pretty(&artifact).unwrap(),
    )
    .unwrap();
}
