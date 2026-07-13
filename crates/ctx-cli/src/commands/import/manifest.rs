use std::collections::BTreeMap;

use super::*;
use crate::commands::import::catalog::system_time_ms;

pub(crate) fn persist_source_import_files(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
) -> Result<()> {
    let source_root = source.path.display().to_string();
    let current_paths = files
        .iter()
        .map(|file| file.source_path.clone())
        .collect::<Vec<_>>();
    let observed_at_ms = utc_now().timestamp_millis();
    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        store.upsert_source_import_files(files)?;
        store.mark_source_import_missing_paths_stale(
            source.provider,
            &source_root,
            &current_paths,
            observed_at_ms,
        )?;
        Ok(())
    })();
    match persist {
        Ok(()) => store.commit_batch()?,
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    Ok(())
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
            import_revision: source.import_revision,
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
