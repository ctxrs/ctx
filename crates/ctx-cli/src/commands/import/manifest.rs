use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::commands::import::catalog::system_time_ms;

// Keep inventory writes inside the same established bounds as provider import
// transactions. Actual WAL growth and transaction time can rotate earlier.
const INVENTORY_BATCH_UNITS: usize = 64;
const INVENTORY_BATCH_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn persist_source_import_files(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
) -> Result<()> {
    let source_root = source.path.display().to_string();
    let existing = store.list_source_import_files_for_source(source.provider, &source_root)?;
    if source_import_inventory_matches(&existing, files) {
        return Ok(());
    }
    let existing_by_path = existing
        .iter()
        .map(|file| (file.source_path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let changed_files = files
        .iter()
        .filter(|file| {
            existing_by_path
                .get(file.source_path.as_str())
                .is_none_or(|existing| !source_import_file_matches_row(existing, file))
        })
        .cloned()
        .collect::<Vec<_>>();
    let current_path_set = files
        .iter()
        .map(|file| file.source_path.as_str())
        .collect::<BTreeSet<_>>();
    let missing_paths = existing
        .iter()
        .filter(|file| !current_path_set.contains(file.source_path.as_str()))
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    let mut offset = 0;
    while offset < changed_files.len() {
        offset += persist_source_import_file_slice(store, &changed_files[offset..])?;
    }

    finalize_source_import_missing_paths(
        store,
        source,
        &source_root,
        &missing_paths,
        utc_now().timestamp_millis(),
    )?;
    Ok(())
}

fn source_import_inventory_matches(
    existing: &[SourceImportFile],
    current: &[SourceImportFile],
) -> bool {
    if existing.len() != current.len() {
        return false;
    }
    let existing = existing
        .iter()
        .map(|file| (file.source_path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    current.iter().all(|file| {
        existing
            .get(file.source_path.as_str())
            .is_some_and(|row| source_import_file_matches_row(row, file))
    })
}

fn source_import_file_matches_row(existing: &SourceImportFile, current: &SourceImportFile) -> bool {
    existing.provider == current.provider
        && existing.source_format == current.source_format
        && existing.source_root == current.source_root
        && existing.file_size_bytes == current.file_size_bytes
        && existing.file_modified_at_ms == current.file_modified_at_ms
        && existing.metadata == current.metadata
}

fn finalize_source_import_missing_paths(
    store: &Store,
    source: &SourceInfo,
    source_root: &str,
    missing_paths: &[String],
    observed_at_ms: i64,
) -> Result<()> {
    let mut offset = 0;
    while offset < missing_paths.len() {
        offset += run_inventory_slice(store, |store, slice| {
            let mut units = 0;
            let mut bytes = 0_usize;
            for path in &missing_paths[offset..] {
                let unit_bytes = path.len();
                if units > 0 && bytes.saturating_add(unit_bytes) > INVENTORY_BATCH_BYTES {
                    break;
                }
                store.mark_source_import_paths_stale(
                    source.provider,
                    source_root,
                    std::slice::from_ref(path),
                    observed_at_ms,
                )?;
                units += 1;
                bytes = bytes.saturating_add(unit_bytes);
                if units >= INVENTORY_BATCH_UNITS
                    || bytes >= INVENTORY_BATCH_BYTES
                    || store.indexing_slice_should_rotate(slice)?
                {
                    break;
                }
            }
            Ok(units)
        })?;
    }
    Ok(())
}

fn run_inventory_slice<T>(
    store: &Store,
    operation: impl FnOnce(&Store, &ctx_history_store::IndexingSlice) -> Result<T>,
) -> Result<T> {
    store.begin_immediate_batch()?;
    let slice = match store.begin_indexing_slice() {
        Ok(slice) => slice,
        Err(error) => {
            let _ = store.rollback_batch();
            return Err(error.into());
        }
    };
    let value = match operation(store, &slice) {
        Ok(value) => value,
        Err(error) => {
            let _ = store.rollback_batch();
            return Err(error);
        }
    };
    store.commit_batch()?;
    store.finish_indexing_slice(slice)?;
    Ok(value)
}

fn persist_source_import_file_slice(store: &Store, files: &[SourceImportFile]) -> Result<usize> {
    store.begin_immediate_batch()?;
    let slice = match store.begin_indexing_slice() {
        Ok(slice) => slice,
        Err(error) => {
            let _ = store.rollback_batch();
            return Err(error.into());
        }
    };
    let mut units = 0;
    let mut bytes = 0_usize;
    for file in files {
        let unit_bytes = source_import_file_estimated_len(file);
        if units > 0 && bytes.saturating_add(unit_bytes) > INVENTORY_BATCH_BYTES {
            break;
        }
        if let Err(error) = store.upsert_source_import_files(std::slice::from_ref(file)) {
            let _ = store.rollback_batch();
            return Err(error.into());
        }
        units += 1;
        bytes = bytes.saturating_add(unit_bytes);
        let measured_rotation = match store.indexing_slice_should_rotate(&slice) {
            Ok(rotate) => rotate,
            Err(error) => {
                let _ = store.rollback_batch();
                return Err(error.into());
            }
        };
        if units >= INVENTORY_BATCH_UNITS || bytes >= INVENTORY_BATCH_BYTES || measured_rotation {
            break;
        }
    }
    if let Err(error) = store.commit_batch() {
        let _ = store.rollback_batch();
        return Err(error.into());
    }
    store.finish_indexing_slice(slice)?;
    Ok(units)
}

fn source_import_file_estimated_len(file: &SourceImportFile) -> usize {
    file.source_format
        .len()
        .saturating_add(file.source_root.len())
        .saturating_add(file.source_path.len())
        .saturating_add(file.metadata.to_string().len())
}

pub(crate) fn source_uses_import_file_manifest(source: &SourceInfo) -> bool {
    !matches!(
        source.source_format,
        "codex_session_jsonl_tree"
            | "openclaw_session_jsonl_tree"
            | "openhands_file_events"
            | "hermes_state_sqlite"
            | "nanoclaw_project"
            | "astrbot_data_v4_sqlite"
            | "shelley_sqlite"
            | "cline_task_directory_json"
            | "roo_task_directory_json"
            | "firebender_chat_history_sqlite"
            | "codebuddy_history_json"
    )
}

pub(crate) fn collect_source_import_files(source: &SourceInfo) -> Result<Vec<SourceImportFile>> {
    let paths = collect_source_import_paths(source)?;
    let source_root = source.path.display().to_string();
    let observed_at_ms = utc_now().timestamp_millis();
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("stat import source file {}", path.display()))?;
        files.push(SourceImportFile {
            provider: source.provider,
            source_format: source.source_format.to_owned(),
            source_root: source_root.clone(),
            source_path: path.display().to_string(),
            file_size_bytes: metadata.len(),
            file_modified_at_ms: system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
            observed_at_ms,
            metadata: json!({}),
        });
    }
    Ok(files)
}

pub(crate) fn collect_source_import_paths(source: &SourceInfo) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "symlinked provider transcript roots are rejected: {}",
            source.path.display()
        ));
    }
    if metadata.file_type().is_file() {
        return Ok(if source_import_file_matches(source, &source.path) {
            vec![source.path.clone()]
        } else {
            Vec::new()
        });
    }
    if !metadata.file_type().is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    let mut stack = vec![source.path.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("read import source directory {}", dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("read import source entry under {}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat import source entry {}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && source_import_file_matches(source, &path) {
                paths.push(path);
            }
        }
    }
    paths = preferred_source_import_paths(source, paths);
    paths.sort();
    Ok(paths)
}

fn preferred_source_import_paths(source: &SourceInfo, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    match source.provider {
        CaptureProvider::Antigravity => antigravity_preferred_import_paths(paths),
        _ => paths,
    }
}

fn antigravity_preferred_import_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_session: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in paths {
        let session = antigravity_session_key_from_path(&path);
        let prefer_new =
            path.file_name().and_then(|name| name.to_str()) == Some("transcript_full.jsonl");
        let replace = by_session
            .get(&session)
            .map(|current| {
                prefer_new
                    && current.file_name().and_then(|name| name.to_str())
                        != Some("transcript_full.jsonl")
            })
            .unwrap_or(true);
        if replace {
            by_session.insert(session, path);
        }
    }
    by_session.into_values().collect()
}

fn antigravity_session_key_from_path(path: &Path) -> String {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect::<Vec<_>>();
    components
        .windows(2)
        .find_map(|window| {
            (window[0] == "brain" && !window[1].trim().is_empty()).then(|| window[1].clone())
        })
        .or_else(|| {
            components.windows(2).find_map(|window| {
                (window[1] == ".system_generated" && !window[0].trim().is_empty())
                    .then(|| window[0].clone())
            })
        })
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.display().to_string())
}

pub(crate) fn source_import_file_matches(source: &SourceInfo, path: &Path) -> bool {
    match source.provider {
        CaptureProvider::Codex | CaptureProvider::Pi | CaptureProvider::FactoryAiDroid => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        }
        CaptureProvider::Claude => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::MiMoCode
        | CaptureProvider::KiroCli
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::Crush
        | CaptureProvider::Goose
        | CaptureProvider::Lingma
        | CaptureProvider::Warp
        | CaptureProvider::Zed => path == source.path,
        CaptureProvider::MistralVibe => {
            path == source.path
                || (path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                    && path.starts_with(&source.path))
        }
        CaptureProvider::Mux => {
            path == source.path
                || (matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("chat.jsonl" | "partial.json")
                ) && path.starts_with(&source.path))
        }
        CaptureProvider::RovoDev => {
            path.file_name().and_then(|name| name.to_str()) == Some("session_context.json")
        }
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::Antigravity => matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("transcript_full.jsonl" | "transcript.jsonl")
        ),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::Cursor => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agent-transcripts")
        }
        CaptureProvider::Windsurf => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
        CaptureProvider::Qoder => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "transcript")
        }
        CaptureProvider::Continue => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
        }
        CaptureProvider::QwenCode => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::CodeBuddy => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "history")
        }
        CaptureProvider::Trae => {
            path.file_name().and_then(|name| name.to_str()) == Some("state.vscdb")
                && (path == source.path || path.starts_with(&source.path))
        }
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        CaptureProvider::Auggie => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Junie => {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "events.jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Firebender => {
            path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db")
                && (path == source.path || path.starts_with(&source.path))
        }
        CaptureProvider::OpenClaw => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Hermes
        | CaptureProvider::NanoClaw
        | CaptureProvider::AstrBot
        | CaptureProvider::Shelley
        | CaptureProvider::OpenHands
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::explicit_path_source;
    use ctx_history_store::{IndexingAdmission, IndexingWorkClass, WAL_TRUNCATE_MIN_BYTES};

    #[test]
    fn manifest_noop_and_one_change_do_not_rewrite_unchanged_rows() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("claude-projects");
        fs::create_dir_all(&root).unwrap();
        let source = explicit_path_source(CaptureProvider::Claude, root.clone());
        let db_path = temp.path().join("work.sqlite");
        let admission =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let store = Store::open_admitted(&db_path, &admission).unwrap();
        let baseline = manifest_files(&source, 130, 1);
        persist_source_import_files(&store, &source, &baseline).unwrap();
        let baseline_db = fs::read(&db_path).unwrap();

        let mut noop = baseline.clone();
        for file in &mut noop {
            file.observed_at_ms = 2;
        }
        persist_source_import_files(&store, &source, &noop).unwrap();
        assert_eq!(
            fs::read(&db_path).unwrap(),
            baseline_db,
            "a changed observation time alone rewrote the main database"
        );

        let mut one_change = noop;
        one_change[17].file_size_bytes += 1;
        one_change[17].file_modified_at_ms = 3;
        one_change[17].observed_at_ms = 3;
        persist_source_import_files(&store, &source, &one_change).unwrap();
        let persisted = store
            .list_source_import_files_for_source(source.provider, &root.display().to_string())
            .unwrap();
        assert_eq!(
            persisted
                .iter()
                .filter(|file| file.observed_at_ms == 3)
                .count(),
            1,
            "one changed manifest path must rewrite only its row"
        );
        assert_eq!(
            persisted
                .iter()
                .find(|file| file.source_path.ends_with("session-18.jsonl"))
                .unwrap()
                .observed_at_ms,
            1,
            "an unchanged manifest row was restamped"
        );
    }

    #[test]
    fn manifest_changed_rows_are_chunked_before_missing_paths_are_staled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("claude-projects");
        fs::create_dir_all(&root).unwrap();
        let source = explicit_path_source(CaptureProvider::Claude, root.clone());
        let db_path = temp.path().join("work.sqlite");
        let admission =
            IndexingAdmission::acquire(&db_path, IndexingWorkClass::Background).unwrap();
        let store = Store::open_admitted(&db_path, &admission).unwrap();
        let baseline = manifest_files(&source, 130, 1);
        persist_source_import_files(&store, &source, &baseline).unwrap();
        assert_eq!(store.source_import_file_counts().unwrap().stale, 0);
        assert!(store.wal_size_bytes().unwrap() <= WAL_TRUNCATE_MIN_BYTES);

        let changed = manifest_files(&source, 129, 2);
        store.begin_immediate_batch().unwrap();
        store.upsert_source_import_files(&changed[..64]).unwrap();
        store.commit_batch().unwrap();
        assert_eq!(
            store.source_import_file_counts().unwrap().stale,
            0,
            "an interrupted data slice must not stale unseen manifest rows"
        );

        persist_source_import_files(&store, &source, &changed).unwrap();
        let counts = store.source_import_file_counts().unwrap();
        assert_eq!(counts.total, 129);
        assert_eq!(counts.stale, 1);
        assert!(store.wal_size_bytes().unwrap() <= WAL_TRUNCATE_MIN_BYTES);
    }

    fn manifest_files(
        source: &SourceInfo,
        count: usize,
        file_modified_at_ms: i64,
    ) -> Vec<SourceImportFile> {
        let source_root = source.path.display().to_string();
        (0..count)
            .map(|index| SourceImportFile {
                provider: source.provider,
                source_format: source.source_format.to_owned(),
                source_root: source_root.clone(),
                source_path: source
                    .path
                    .join(format!("session-{index}.jsonl"))
                    .display()
                    .to_string(),
                file_size_bytes: 128,
                file_modified_at_ms,
                observed_at_ms: file_modified_at_ms,
                metadata: json!({}),
            })
            .collect()
    }
}
