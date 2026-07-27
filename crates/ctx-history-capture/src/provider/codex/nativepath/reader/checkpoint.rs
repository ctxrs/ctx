use super::*;

pub(super) struct BoundedRecordRead {
    pub(super) complete: bool,
    pub(super) terminal_nul_padding: bool,
    pub(super) oversized: bool,
    pub(super) stored_len: usize,
    pub(super) byte_len: u64,
    pub(super) sha256: [u8; 32],
}

pub(super) fn read_bounded_record(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    full_hasher: &mut Sha256,
    complete_hasher: &mut Sha256,
) -> Result<Option<BoundedRecordRead>> {
    storage.clear();
    let complete_before_record = complete_hasher.clone();
    let mut record_hasher = Sha256::new();
    let mut byte_len = 0_u64;
    let mut oversized = false;
    let mut all_nul = true;

    loop {
        let (consumed, complete) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if byte_len == 0 {
                    return Ok(None);
                }
                if all_nul {
                    return Ok(Some(BoundedRecordRead {
                        complete: true,
                        terminal_nul_padding: true,
                        oversized,
                        stored_len: storage.len(),
                        byte_len,
                        sha256: [0; 32],
                    }));
                }
                *complete_hasher = complete_before_record;
                return Ok(Some(BoundedRecordRead {
                    complete: false,
                    terminal_nul_padding: false,
                    oversized,
                    stored_len: storage.len(),
                    byte_len,
                    sha256: record_hasher.finalize().into(),
                }));
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            let chunk = &available[..consumed];
            full_hasher.update(chunk);
            complete_hasher.update(chunk);
            record_hasher.update(chunk);
            all_nul &= chunk.iter().all(|byte| *byte == 0);
            byte_len =
                byte_len
                    .checked_add(u64::try_from(consumed).map_err(|_| {
                        CaptureError::SystemInvariant("Codex record chunk exceeds u64")
                    })?)
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex JSONL record length exceeds u64",
                    ))?;

            let content_len = if newline.is_some() {
                consumed.saturating_sub(1)
            } else {
                consumed
            };
            let remaining = MAX_CODEX_RECORD_BYTES.saturating_sub(storage.len());
            let copied = content_len.min(remaining);
            storage.extend_from_slice(&chunk[..copied]);
            if copied != content_len {
                oversized = true;
            }
            (consumed, newline.is_some())
        };
        reader.consume(consumed);
        if complete {
            return Ok(Some(BoundedRecordRead {
                complete: true,
                terminal_nul_padding: false,
                oversized,
                stored_len: storage.len(),
                byte_len,
                sha256: [0; 32],
            }));
        }
    }
}

pub(super) fn trim_jsonl_terminator(mut record: &[u8]) -> &[u8] {
    if record.last() == Some(&b'\r') {
        record = &record[..record.len() - 1];
    }
    record
}

pub(super) struct ValidatedCheckpoint {
    pub(super) bytes_read: u64,
    pub(super) complete_prefix_hasher: Sha256,
    pub(super) complete_prefix_ends_with_terminal_nul_padding: bool,
    pub(super) pending_tool_contexts: BTreeMap<String, CodexToolCallContext>,
    pub(super) pending_tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
}

pub(super) fn decode_pending_tool_authority(
    record: &[u8],
    authority: &CodexPendingToolAuthority,
    owner: &CodexSessionRow,
) -> Result<(String, CodexToolCallContext)> {
    let Some(record) = record.strip_suffix(b"\n") else {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority does not end at a JSONL boundary",
        ));
    };
    let record = trim_jsonl_terminator(record);
    let probe = classify_codex_record(record).map_err(|_| {
        invalid_checkpoint_proof("pending tool-call authority is not valid Codex JSON")
    })?;
    let CodexRecordClass::Retained(kind @ super::super::record::CodexRetainedKind::ToolCall) =
        probe.class
    else {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority does not identify a tool call",
        ));
    };
    let retained = parse_decoded_record(record, owner)
        .ok_or_else(|| invalid_checkpoint_proof("pending tool-call authority cannot be decoded"))?;
    let row = match build_event_row(authority.raw_ordinal, kind, &retained)? {
        Ok(row) => row,
        Err(
            CodexRetainedNonMaterialized::ValidUnmaterializable
            | CodexRetainedNonMaterialized::Malformed(_),
        ) => {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority cannot be projected",
            ));
        }
    };
    let (call_id, context) = tool_context_from_row(&row).ok_or_else(|| {
        invalid_checkpoint_proof("pending tool-call authority has no correlation identity")
    })?;
    if !authority.matches_call_id(&call_id) {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority correlation does not match checkpoint state",
        ));
    }
    Ok((call_id, bound_tool_context(context)))
}

pub(super) fn validate_checkpoint_source(
    reader: &mut BufReader<File>,
    checkpoint: &CodexNativeCheckpoint,
    hydrate_pending_tools: bool,
) -> Result<ValidatedCheckpoint> {
    // The prefix proof is the sole read pass over checkpointed bytes. On
    // append, only the at-most-24 authority spans are retained long enough to
    // reconstruct transient correlation state during that same pass.
    reader.seek(SeekFrom::Start(0))?;
    let complete_prefix_end = checkpoint.complete_prefix_end();
    let mut remaining = checkpoint.observation.len;
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; CHECKPOINT_READ_BUFFER_BYTES];
    let mut full_hasher = Sha256::new();
    let mut complete_prefix_hasher = Sha256::new();
    let mut incomplete_tail_hasher = Sha256::new();
    let mut complete_records = 0_u64;
    let mut final_prefix_byte = None;
    let mut terminal_suffix_all_nul = true;
    let mut terminal_suffix_len = 0_u64;
    let mut tail_contains_newline = false;
    let mut authorities = checkpoint
        .pending_tool_authorities()
        .iter()
        .collect::<Vec<_>>();
    authorities.sort_by_key(|authority| authority.record_start);
    let mut authority_index = 0_usize;
    let mut current_record_start = 0_u64;
    let mut pending_tool_record = Vec::new();
    let mut pending_tool_contexts = BTreeMap::new();
    let mut pending_tool_authorities = BTreeMap::new();

    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(CHECKPOINT_READ_BUFFER_BYTES as u64))
            .map_err(|_| CaptureError::SystemInvariant("Codex checkpoint read exceeds usize"))?;
        let read = reader.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(invalid_checkpoint_proof(
                "checkpoint observation ends after source EOF",
            ));
        }
        let chunk = &buffer[..read];
        full_hasher.update(chunk);
        let read_u64 = u64::try_from(read)
            .map_err(|_| CaptureError::SystemInvariant("Codex checkpoint read exceeds u64"))?;
        let chunk_end = offset
            .checked_add(read_u64)
            .ok_or(CaptureError::SystemInvariant(
                "Codex checkpoint offset exceeds u64",
            ))?;

        if offset < complete_prefix_end {
            let prefix_len = usize::try_from((complete_prefix_end.min(chunk_end)) - offset)
                .map_err(|_| CaptureError::SystemInvariant("Codex prefix length exceeds usize"))?;
            let prefix = &chunk[..prefix_len];
            complete_prefix_hasher.update(prefix);
            for (index, byte) in prefix.iter().enumerate() {
                let absolute_offset = offset
                    .checked_add(u64::try_from(index).unwrap_or(u64::MAX))
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex checkpoint record offset exceeds u64",
                    ))?;
                if hydrate_pending_tools
                    && authorities.get(authority_index).is_some_and(|authority| {
                        absolute_offset >= authority.record_start
                            && absolute_offset < authority.record_end
                    })
                {
                    pending_tool_record.push(*byte);
                }
                if *byte != b'\n' {
                    terminal_suffix_all_nul &= *byte == 0;
                    terminal_suffix_len = terminal_suffix_len.saturating_add(1);
                    continue;
                }
                let record_end =
                    absolute_offset
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Codex checkpoint record boundary exceeds u64",
                        ))?;
                if let Some(authority) = authorities.get(authority_index) {
                    if authority.record_start < record_end {
                        if authority.record_start != current_record_start
                            || authority.record_end != record_end
                            || authority.raw_ordinal != complete_records
                        {
                            return Err(invalid_checkpoint_proof(
                                "pending tool-call authority does not match its JSONL record boundary",
                            ));
                        }
                        if hydrate_pending_tools {
                            let (call_id, context) = decode_pending_tool_authority(
                                &pending_tool_record,
                                authority,
                                &checkpoint.owner,
                            )?;
                            if pending_tool_contexts
                                .insert(call_id.clone(), context)
                                .is_some()
                                || pending_tool_authorities
                                    .insert(call_id, (*authority).clone())
                                    .is_some()
                            {
                                return Err(invalid_checkpoint_proof(
                                    "pending tool-call authority correlation is duplicated",
                                ));
                            }
                            pending_tool_record.clear();
                        }
                        authority_index = authority_index.saturating_add(1);
                    }
                }
                current_record_start = record_end;
                complete_records = complete_records.saturating_add(1);
                terminal_suffix_all_nul = true;
                terminal_suffix_len = 0;
            }
            final_prefix_byte = prefix.last().copied().or(final_prefix_byte);
            if prefix_len < chunk.len() {
                let tail = &chunk[prefix_len..];
                incomplete_tail_hasher.update(tail);
                tail_contains_newline |= tail.contains(&b'\n');
            }
        } else {
            incomplete_tail_hasher.update(chunk);
            tail_contains_newline |= chunk.contains(&b'\n');
        }
        offset = chunk_end;
        remaining -= read_u64;
    }

    let full_revision_sha256: [u8; 32] = full_hasher.finalize().into();
    let complete_prefix_sha256: [u8; 32] = complete_prefix_hasher.clone().finalize().into();
    let complete_prefix_ends_with_terminal_nul_padding =
        terminal_suffix_len != 0 && terminal_suffix_all_nul;
    if complete_prefix_ends_with_terminal_nul_padding {
        complete_records = complete_records.saturating_add(1);
    }
    if full_revision_sha256 != checkpoint.full_revision_sha256
        || complete_prefix_sha256 != checkpoint.complete_prefix_sha256
        || complete_records != checkpoint.next_raw_ordinal()
        || authority_index != authorities.len()
        || (complete_prefix_end != 0
            && final_prefix_byte != Some(b'\n')
            && !complete_prefix_ends_with_terminal_nul_padding)
    {
        return Err(invalid_checkpoint_proof(
            "checkpoint digest, boundary, or raw ordinal does not match source bytes",
        ));
    }

    match checkpoint.incomplete_tail() {
        None if complete_prefix_end == checkpoint.observation.len => {}
        Some((tail_len, tail_sha256))
            if !tail_contains_newline
                && tail_len == checkpoint.observation.len - complete_prefix_end
                && <[u8; 32]>::from(incomplete_tail_hasher.finalize()) == tail_sha256 => {}
        _ => {
            return Err(invalid_checkpoint_proof(
                "checkpoint incomplete-tail proof does not match source bytes",
            ));
        }
    }

    Ok(ValidatedCheckpoint {
        bytes_read: checkpoint.observation.len,
        complete_prefix_hasher,
        complete_prefix_ends_with_terminal_nul_padding,
        pending_tool_contexts,
        pending_tool_authorities,
    })
}

pub(super) fn invalid_checkpoint_proof(reason: &str) -> CaptureError {
    CaptureError::InvalidPayload(format!("invalid Codex append proof: {reason}"))
}

pub(super) fn observed_file(source: &CodexCatalogSource) -> Result<CodexFileObservation> {
    let observation = observe_ordinary_file(&source.source_path)?;
    let observed = CodexFileObservation::from_parts(
        observation.len(),
        observation.modified_at(),
        *observation.token(),
    );
    if observed != source.catalog_observation {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog observation changed before NativePath admission".to_owned(),
        ));
    }
    Ok(observed)
}

pub(crate) fn revalidate_codex_source_observation(
    source: &CodexCatalogSource,
    certified: &CodexFileObservation,
) -> Result<()> {
    let observed = observed_file(source)?;
    if &observed != certified {
        return Err(source_changed_during_scan());
    }
    Ok(())
}

pub(super) fn validate_open_file_metadata(
    path: &Path,
    file: &File,
    observation: &CodexFileObservation,
) -> Result<()> {
    if opened_file_observation(path, file)? != *observation {
        return Err(source_changed_during_scan());
    }
    Ok(())
}

pub(super) fn opened_file_observation(path: &Path, file: &File) -> Result<CodexFileObservation> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(source_changed_during_scan());
    }
    let platform_before = opened_file_platform_token(path, file, &metadata)?;
    let content_fingerprint = if platform_before.is_some() {
        None
    } else {
        Some(opened_file_content_fingerprint(file, &metadata)?)
    };
    let current = file.metadata()?;
    let platform_after = opened_file_platform_token(path, file, &current)?;
    if current.len() != metadata.len()
        || current.modified().ok() != metadata.modified().ok()
        || platform_after != platform_before
    {
        return Err(source_changed_during_scan());
    }
    Ok(CodexFileObservation::from_parts(
        metadata.len(),
        metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        combine_opened_file_token(platform_before, content_fingerprint),
    ))
}

#[cfg(unix)]
fn opened_file_platform_token(
    _path: &Path,
    _file: &File,
    metadata: &std::fs::Metadata,
) -> Result<Option<[u8; 32]>> {
    use std::os::unix::fs::MetadataExt;

    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_TOKEN_DOMAIN);
    hasher.update(b"unix\0");
    hasher.update(metadata.dev().to_le_bytes());
    hasher.update(metadata.ino().to_le_bytes());
    hasher.update(metadata.ctime().to_le_bytes());
    hasher.update(metadata.ctime_nsec().to_le_bytes());
    Ok(Some(hasher.finalize().into()))
}

#[cfg(target_os = "windows")]
fn opened_file_platform_token(
    path: &Path,
    file: &File,
    metadata: &std::fs::Metadata,
) -> Result<Option<[u8; 32]>> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic_info = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            &mut basic_info as *mut FILE_BASIC_INFO as *mut std::ffi::c_void,
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if basic_info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "reparse-point provider transcript files are rejected",
        });
    }

    let mut id_info = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut id_info as *mut FILE_ID_INFO as *mut std::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_TOKEN_DOMAIN);
    hasher.update(b"windows\0");
    hasher.update(id_info.VolumeSerialNumber.to_le_bytes());
    hasher.update(id_info.FileId.Identifier);
    hasher.update(basic_info.ChangeTime.to_le_bytes());
    hasher.update(basic_info.LastWriteTime.to_le_bytes());
    hasher.update(metadata.len().to_le_bytes());
    Ok(Some(hasher.finalize().into()))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn opened_file_platform_token(
    _path: &Path,
    _file: &File,
    _metadata: &std::fs::Metadata,
) -> Result<Option<[u8; 32]>> {
    Ok(None)
}

fn combine_opened_file_token(
    platform_token: Option<[u8; 32]>,
    content_fingerprint: Option<[u8; 32]>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_TOKEN_DOMAIN);
    if let Some(platform_token) = platform_token {
        hasher.update(b"platform\0");
        hasher.update(platform_token);
    } else {
        hasher.update(b"portable\0");
        match content_fingerprint {
            Some(content_fingerprint) => hasher.update(content_fingerprint),
            None => hasher.update(b"missing-content-fingerprint\0"),
        }
    }
    hasher.finalize().into()
}

fn opened_file_content_fingerprint(file: &File, metadata: &std::fs::Metadata) -> Result<[u8; 32]> {
    let len = metadata.len();
    let mut hasher = Sha256::new();
    hasher.update(ORDINARY_FILE_TOKEN_DOMAIN);
    hasher.update(len.to_le_bytes());
    let mut reader = file.try_clone()?;
    let original_position = reader.stream_position()?;
    if len <= ORDINARY_FILE_FULL_FINGERPRINT_MAX_BYTES {
        hasher.update(b"full\0");
        hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    } else {
        hasher.update(b"sparse\0");
        for offset in opened_file_sparse_sample_offsets(len) {
            let sample_len = ORDINARY_FILE_SPARSE_SAMPLE_BYTES.min(len.saturating_sub(offset));
            hasher.update(offset.to_le_bytes());
            hasher.update(sample_len.to_le_bytes());
            hash_opened_file_range(&mut reader, offset, sample_len, &mut hasher)?;
        }
    }
    reader.seek(SeekFrom::Start(original_position))?;
    Ok(hasher.finalize().into())
}

fn opened_file_sparse_sample_offsets(len: u64) -> std::collections::BTreeSet<u64> {
    let last = len.saturating_sub(ORDINARY_FILE_SPARSE_SAMPLE_BYTES);
    [0, len / 4, len / 2, len.saturating_mul(3) / 4, last]
        .into_iter()
        .map(|offset| offset.min(last))
        .collect()
}

fn hash_opened_file_range(
    file: &mut File,
    offset: u64,
    len: u64,
    hasher: &mut Sha256,
) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    let mut buffer = [0_u8; 8 * 1024];
    while remaining > 0 {
        let take = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(source_changed_during_scan());
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok(())
}

pub(super) fn validate_catalog_owner(
    catalog_owner: Option<&str>,
    scanned_owner: &str,
) -> Result<()> {
    if catalog_owner.is_some_and(|catalog_owner| catalog_owner != scanned_owner) {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog owner changed before NativePath admission".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn source_changed_during_scan() -> CaptureError {
    CaptureError::InvalidPayload("Codex source changed while NativePath was reading it".to_owned())
}
