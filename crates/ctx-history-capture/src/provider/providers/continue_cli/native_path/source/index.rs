use super::*;

pub(super) fn observe_continue_index(
    authority: &ProviderSourceRoot,
    relative_path: PathBuf,
) -> ContinueIndexSnapshot {
    let path = authority.named_path().join(&relative_path);
    match authority.open_file(&relative_path) {
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            index_without_entries(
                authority,
                relative_path,
                path,
                ContinueIndexState::Missing,
                b"missing",
                false,
            )
        }
        Err(error) => {
            let evidence = error.to_string();
            index_without_entries(
                authority,
                relative_path,
                path,
                ContinueIndexState::Unavailable,
                evidence.as_bytes(),
                false,
            )
        }
        Ok(opened) => match read_opened_exact_file(
            &path,
            Arc::new(opened),
            MAX_CONTINUE_INDEX_BYTES,
            INDEX_REVISION_DOMAIN,
        ) {
            Ok(snapshot) => match parse_index_entries(&snapshot.bytes) {
                Ok(metadata_entries) => ContinueIndexSnapshot {
                    observation: ContinueIndexObservation {
                        path,
                        state: ContinueIndexState::Ready,
                        dependency_revision: snapshot.revision,
                    },
                    authority: authority.clone(),
                    relative_path,
                    #[cfg(test)]
                    entry_count: metadata_entries.len(),
                    metadata_entries,
                    #[cfg(test)]
                    content_read: true,
                },
                Err(_) => ContinueIndexSnapshot {
                    observation: ContinueIndexObservation {
                        path,
                        state: ContinueIndexState::Malformed,
                        dependency_revision: snapshot.revision,
                    },
                    metadata_entries: Vec::new(),
                    authority: authority.clone(),
                    relative_path,
                    #[cfg(test)]
                    entry_count: 0,
                    #[cfg(test)]
                    content_read: true,
                },
            },
            Err(error) => {
                let evidence = error.to_string();
                index_without_entries(
                    authority,
                    relative_path,
                    path,
                    ContinueIndexState::Unavailable,
                    evidence.as_bytes(),
                    false,
                )
            }
        },
    }
}

pub(super) fn parse_index_entries(
    bytes: &[u8],
) -> Result<Vec<ContinueIndexEntry>, Box<dyn std::error::Error>> {
    // Parse the bounded index once, then sort the retained metadata for binary
    // search by session identity during source preparation.
    let root = validate_and_root(bytes)?;
    if root.kind() != JsonKind::Array {
        return Err("Continue index is not an array".into());
    }
    let mut entries = Vec::new();
    for entry in root.as_array()? {
        let entry = entry?;
        if entry.kind() != JsonKind::Object {
            continue;
        }
        if let Some((session_id, metadata)) = parse_index_entry(entry)? {
            if entries.len() >= MAX_CONTINUE_INDEX_ENTRIES {
                return Err("Continue index exceeds the supported entry limit".into());
            }
            entries.push(ContinueIndexEntry {
                session_id,
                metadata,
            });
        }
    }
    entries.sort_unstable_by(|left, right| left.session_id.cmp(&right.session_id));
    if entries
        .windows(2)
        .any(|pair| pair[0].session_id == pair[1].session_id)
    {
        return Err("Continue index contains duplicate session IDs".into());
    }
    Ok(entries)
}

pub(super) fn parse_index_entry(
    entry: JsonSpan<'_>,
) -> Result<Option<(String, ContinueIndexMetadata)>, Box<dyn std::error::Error>> {
    let mut session_id = None;
    let mut title = None;
    let mut date_created = None;
    let mut workspace_directory = None;
    let mut message_count = None;
    for field in entry.as_object()? {
        let (key, value) = field?;
        if key.is("sessionId") {
            session_id = decode_string(value, MAX_CONTINUE_SESSION_ID_BYTES)?;
        } else if key.is("title") {
            title = decode_string(value, MAX_CONTINUE_INDEX_STRING_BYTES)?;
        } else if key.is("dateCreated") {
            date_created = decode_string(value, 128)?;
        } else if key.is("workspaceDirectory") {
            workspace_directory = decode_string(value, MAX_CONTINUE_INDEX_STRING_BYTES)?;
        } else if key.is("messageCount") {
            message_count = decode_u64(value);
        }
        // Unknown and result-like index fields remain borrowed spans and are
        // discarded here without constructing Value or String payloads.
    }
    Ok(session_id
        .filter(|value| valid_identity_string(value, MAX_CONTINUE_SESSION_ID_BYTES))
        .map(|session_id| {
            (
                session_id,
                ContinueIndexMetadata {
                    title: title.filter(|value| valid_metadata_string(value)),
                    date_created,
                    workspace_directory: workspace_directory
                        .filter(|value| valid_metadata_string(value)),
                    message_count,
                },
            )
        }))
}

pub(super) fn index_without_entries(
    authority: &ProviderSourceRoot,
    relative_path: PathBuf,
    path: PathBuf,
    state: ContinueIndexState,
    revision_evidence: &[u8],
    _content_read: bool,
) -> ContinueIndexSnapshot {
    ContinueIndexSnapshot {
        observation: ContinueIndexObservation {
            path,
            state,
            dependency_revision: sha256_hex(INDEX_REVISION_DOMAIN, revision_evidence),
        },
        metadata_entries: Vec::new(),
        authority: authority.clone(),
        relative_path,
        #[cfg(test)]
        entry_count: 0,
        #[cfg(test)]
        content_read: _content_read,
    }
}

pub(super) fn read_opened_exact_file(
    path: &Path,
    opened: Arc<OpenedProviderSourceFile>,
    max_bytes: usize,
    revision_domain: &[u8],
) -> Result<ExactFileSnapshot, ContinueNativePathError> {
    if opened.len() > max_bytes as u64 {
        return Err(ContinueNativePathError::SourceTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
            observed: opened.len(),
        });
    }
    let file_token = opened_file_token(&opened, path)?;
    if let Some(error) = injected_io_failure(ContinueInjectedIoOperation::SourceRead, path) {
        return Err(source_io(path, "read Continue source", error));
    }
    let length =
        usize::try_from(opened.len()).map_err(|_| ContinueNativePathError::SourceTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
            observed: opened.len(),
        })?;
    let bytes = opened
        .read_exact_range(0, length, max_bytes)
        .map_err(|error| capture_source_error(path, "read Continue source", error))?;
    if opened.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return Err(ContinueNativePathError::SourceChanged {
            path: path.to_path_buf(),
        });
    }
    Ok(ExactFileSnapshot {
        path: path.to_path_buf(),
        canonical_path: path.to_path_buf(),
        file_token,
        revision: sha256_hex(revision_domain, &bytes),
        bytes: bytes.into_boxed_slice(),
        opened,
    })
}
