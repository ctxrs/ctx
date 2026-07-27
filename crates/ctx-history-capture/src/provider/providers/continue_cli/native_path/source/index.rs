use super::*;

pub(super) fn observe_continue_index(path: PathBuf) -> ContinueIndexSnapshot {
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            index_without_entries(path, ContinueIndexState::Missing, b"missing", false)
        }
        Err(error) => index_without_entries(
            path,
            ContinueIndexState::Unavailable,
            format!("io:{:?}", error.kind()).as_bytes(),
            false,
        ),
        Ok(metadata) if !metadata.file_type().is_file() => {
            index_without_entries(path, ContinueIndexState::Unavailable, b"not-regular", false)
        }
        Ok(_) => match read_exact_file(&path, MAX_CONTINUE_INDEX_BYTES, INDEX_REVISION_DOMAIN) {
            Ok(snapshot) => match parse_index_entries(&snapshot.bytes) {
                Ok(metadata_entries) => ContinueIndexSnapshot {
                    observation: ContinueIndexObservation {
                        path,
                        state: ContinueIndexState::Ready,
                        dependency_revision: snapshot.revision,
                    },
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
                    #[cfg(test)]
                    entry_count: 0,
                    #[cfg(test)]
                    content_read: true,
                },
            },
            Err(error) => index_without_entries(
                path,
                ContinueIndexState::Unavailable,
                error.to_string().as_bytes(),
                false,
            ),
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
        #[cfg(test)]
        entry_count: 0,
        #[cfg(test)]
        content_read: _content_read,
    }
}

pub(super) fn read_exact_file(
    path: &Path,
    max_bytes: usize,
    revision_domain: &[u8],
) -> Result<ExactFileSnapshot, ContinueNativePathError> {
    let ordinary_before = observe_ordinary_file(path)
        .map_err(|error| capture_source_error(path, "observe Continue source", error))?;
    if ordinary_before.len() > max_bytes as u64 {
        return Err(ContinueNativePathError::SourceTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
            observed: ordinary_before.len(),
        });
    }
    let canonical_before = fs::canonicalize(path).map_err(|error| source_access(path, error))?;
    let file = open_ordinary_file_without_following(path)
        .map_err(|error| capture_source_error(path, "open Continue source", error))?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(ordinary_before.len())
            .unwrap_or(max_bytes)
            .min(max_bytes),
    );
    if let Some(error) = injected_io_failure(ContinueInjectedIoOperation::SourceRead, path) {
        return Err(source_io(path, "read Continue source", error));
    }
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| source_access(path, error))?;
    if bytes.len() > max_bytes {
        return Err(ContinueNativePathError::SourceTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
            observed: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        });
    }
    let ordinary_after = observe_ordinary_file(path)
        .map_err(|error| capture_source_error(path, "reobserve Continue source", error))?;
    let canonical_after = fs::canonicalize(path).map_err(|error| source_access(path, error))?;
    if ordinary_before != ordinary_after
        || canonical_before != canonical_after
        || ordinary_after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    {
        return Err(ContinueNativePathError::SourceChanged {
            path: path.to_path_buf(),
        });
    }
    Ok(ExactFileSnapshot {
        path: path.to_path_buf(),
        canonical_path: canonical_after,
        ordinary_observation: ordinary_after,
        revision: sha256_hex(revision_domain, &bytes),
        bytes: bytes.into_boxed_slice(),
    })
}
