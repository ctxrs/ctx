use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use ctx_history_store::Store;

use crate::{
    common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead},
    CaptureError, CodexSessionImportOptions, ProviderImportSummary, Result,
};

pub fn import_codex_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    super::nativepath::import_codex_native_session_files(
        vec![path.as_ref().to_path_buf()],
        store,
        options,
    )
}

pub fn import_codex_session_jsonl_tail(
    path: impl AsRef<Path>,
    start_offset: u64,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    if let Some(summary) = validate_codex_tail_start(path, start_offset)? {
        return Ok(summary);
    }
    import_codex_session_jsonl(path, store, options)
}

fn validate_codex_tail_start(path: &Path, offset: u64) -> Result<Option<ProviderImportSummary>> {
    let mut file = File::open(path)?;
    if offset == 0 {
        return Ok(None);
    }
    if offset > file.metadata()?.len() {
        return Err(CaptureError::InvalidPayload(
            "Codex tail offset is not a complete JSONL record boundary".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(offset - 1))?;
    let mut previous = [0_u8; 1];
    file.read_exact(&mut previous)?;
    if previous[0] != b'\n' {
        return Err(CaptureError::InvalidPayload(
            "Codex tail offset is not a complete JSONL record boundary".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut header = Vec::new();
    if matches!(
        read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut header)?,
        ProviderJsonlLineRead::Oversized { .. }
    ) {
        return Ok(Some(ProviderImportSummary {
            skipped: 1,
            skipped_sessions: 1,
            ..ProviderImportSummary::default()
        }));
    }
    Ok(None)
}

pub fn import_codex_session_paths(
    paths: Vec<PathBuf>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    super::nativepath::import_codex_native_session_files(paths, store, options)
}

pub fn import_codex_session_tree(
    root: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<ProviderImportSummary> {
    super::nativepath::import_codex_native_session_root(root.as_ref(), store, options)
}
