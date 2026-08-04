use super::super::rows::{build_event_row, tool_context_from_row};
use super::*;

#[cfg(test)]
std::thread_local! {
    static AFTER_CODEX_PREFIX_HASH_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_CODEX_SECOND_PREFIX_HASH_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn install_after_codex_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_CODEX_PREFIX_HASH_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex prefix-hash hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
pub(crate) fn install_after_codex_second_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_CODEX_SECOND_PREFIX_HASH_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "Codex second prefix-hash hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_codex_prefix_hash_hook() {
    AFTER_CODEX_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
fn run_after_codex_second_prefix_hash_hook() {
    AFTER_CODEX_SECOND_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

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
    maximum_bytes: u64,
) -> Result<Option<BoundedRecordRead>> {
    if maximum_bytes == 0 {
        return Ok(None);
    }
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
                    return Err(source_changed_during_scan());
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

            let remaining = maximum_bytes.saturating_sub(byte_len);
            let bounded = usize::try_from(remaining.min(available.len() as u64))
                .map_err(|_| CaptureError::SystemInvariant("Codex record bound exceeds usize"))?;
            let newline = available[..bounded].iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(bounded, |index| index + 1);
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
            if newline.is_none() && byte_len == maximum_bytes {
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
                sha256: record_hasher.finalize().into(),
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
    pub(super) pending_continuations: BTreeMap<String, String>,
}

pub(super) fn decode_pending_tool_authority(
    record: &[u8],
    authority: &CodexPendingToolAuthority,
    owner: &CodexSessionRow,
) -> Result<(String, CodexToolCallContext)> {
    // The surrounding checkpoint walk has already matched this authority to
    // an exact JSONL boundary. The shared lineage scratch omits the delimiter;
    // the legacy pending-only scratch includes it.
    let record = record.strip_suffix(b"\n").unwrap_or(record);
    let record = trim_jsonl_terminator(record);
    let probe = classify_codex_record(record).map_err(|_| {
        invalid_checkpoint_proof("pending tool-call authority is not valid Codex JSON")
    })?;
    if probe.lineage_malformed() {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority has malformed lineage fields",
        ));
    }
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
            | CodexRetainedNonMaterialized::Malformed,
        ) => {
            return Err(invalid_checkpoint_proof(
                "pending tool-call authority cannot be projected",
            ));
        }
    };
    let (call_id, mut context) = tool_context_from_row(&row).ok_or_else(|| {
        invalid_checkpoint_proof("pending tool-call authority has no correlation identity")
    })?;
    if !authority.matches_call_id(&call_id) {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority correlation does not match checkpoint state",
        ));
    }
    if let [evidence] =
        crate::provider::codex::repository::repository_tool_evidence(&retained.payload).as_slice()
    {
        // Fresh source-backed projection redacts provider-native arguments
        // from display/Core text. Append-proof reconstruction must recover
        // that same bounded context, not revive the legacy preview.
        context.command_preview = None;
        context.arguments_preview = None;
        context.tool_name.clone_from(&evidence.tool_name);
        context.session_cwd = owner.cwd.clone();
        context.exact_command.clone_from(&evidence.command);
        context.command_too_large = evidence.command_too_large;
        context
            .declared_workdir
            .clone_from(&evidence.declared_workdir);
        context
            .continuation_cell_id
            .clone_from(&evidence.continuation_cell_id);
        if context.exact_command.is_some() || context.command_too_large {
            context.origin_call_id = Some(call_id.clone());
            context.origin_event_sequence = Some(authority.raw_ordinal);
            context.origin_occurred_at_unix_ms = Some(retained.occurred_at.timestamp_millis());
        }
    }
    context.continuation_call_id_sha256 = authority.continuation_call_id_sha256().to_vec();
    context.continuation_capacity_exceeded = authority.continuation_capacity_exceeded();
    context.correlation_ambiguous = authority.correlation_ambiguous();
    Ok((call_id, bound_tool_context(context)))
}

pub(super) fn validate_checkpoint_source(
    reader: &mut BufReader<File>,
    checkpoint: &CodexNativeCheckpoint,
    append_replay: bool,
    mut lineage_facts: Option<&mut CodexLineageFactsV0>,
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
    let mut lineage_record = Vec::new();
    let mut lineage_record_oversized = false;
    let mut pending_tool_contexts = BTreeMap::new();
    let mut pending_tool_authorities = BTreeMap::new();
    let mut pending_continuations = BTreeMap::new();

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
                if append_replay
                    && lineage_facts.is_none()
                    && authorities.get(authority_index).is_some_and(|authority| {
                        absolute_offset >= authority.record_start
                            && absolute_offset < authority.record_end
                    })
                {
                    pending_tool_record.push(*byte);
                }
                if lineage_facts.is_some() && *byte != b'\n' {
                    if lineage_record.len() < MAX_CODEX_RECORD_BYTES {
                        if lineage_record.len() == lineage_record.capacity() {
                            let growth = 8 * 1024;
                            lineage_record.try_reserve_exact(growth).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    CODEX_LINEAGE_EXHAUSTED_SENTINEL.to_owned(),
                                )
                            })?;
                        }
                        lineage_record.push(*byte);
                    } else {
                        lineage_record_oversized = true;
                    }
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
                        if append_replay {
                            let authority_record = if lineage_facts.is_some() {
                                lineage_record.as_slice()
                            } else {
                                pending_tool_record.as_slice()
                            };
                            let (call_id, context) = decode_pending_tool_authority(
                                authority_record,
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
                if let Some(facts) = lineage_facts.as_deref_mut() {
                    record_checkpoint_lineage(facts, &lineage_record, lineage_record_oversized)?;
                    lineage_record.clear();
                    lineage_record_oversized = false;
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
    if !append_replay && checkpoint.incomplete_tail().is_some() {
        if let Some(facts) = lineage_facts {
            // A fresh bounded scan records every unterminated tail as
            // unattributed relationship ambiguity. Exact checkpoint replay
            // must rederive that same fact from the certified boundary; the
            // tail bytes were hashed above but intentionally never parsed as
            // a complete JSONL record. Append replay resumes at that boundary,
            // so the primary scanner derives replacement evidence from the
            // tail's now-current bytes instead of retaining this old fact.
            facts.record(CodexLineageRecordEvidence::UnattributedAmbiguity)?;
        }
    }

    if append_replay {
        for (call_id, authority) in &pending_tool_authorities {
            if let Some(cell_id) = authority.continuation_cell_id() {
                if authority.continuation_conflicted() {
                    if pending_continuations
                        .insert(cell_id.to_owned(), String::new())
                        .is_some()
                    {
                        return Err(invalid_checkpoint_proof(
                            "pending conflicted continuation cell is duplicated",
                        ));
                    }
                    continue;
                }
                let Some(origin) = pending_tool_contexts.get(call_id) else {
                    return Err(invalid_checkpoint_proof(
                        "pending continuation origin context is unavailable",
                    ));
                };
                if (origin.exact_command.is_none() && !origin.command_too_large)
                    || origin.continuation_cell_id.is_some()
                {
                    return Err(invalid_checkpoint_proof(
                        "pending continuation authority is not an exact origin command",
                    ));
                }
                if pending_continuations
                    .insert(cell_id.to_owned(), call_id.clone())
                    .is_some()
                {
                    return Err(invalid_checkpoint_proof(
                        "pending continuation cell is assigned more than once",
                    ));
                }
            }
        }
        let wait_calls = pending_tool_contexts
            .iter()
            .filter_map(|(call_id, context)| {
                context
                    .continuation_cell_id
                    .as_ref()
                    .map(|cell_id| (call_id.clone(), cell_id.clone()))
            })
            .collect::<Vec<_>>();
        for (call_id, cell_id) in wait_calls {
            let Some(origin_call_id) = pending_continuations.get(&cell_id) else {
                continue;
            };
            if origin_call_id.is_empty() {
                continue;
            }
            let origin = pending_tool_contexts
                .get(origin_call_id)
                .cloned()
                .ok_or_else(|| {
                    invalid_checkpoint_proof("pending continuation origin is unavailable")
                })?;
            let context = pending_tool_contexts.get_mut(&call_id).ok_or_else(|| {
                invalid_checkpoint_proof("pending continuation wait context is unavailable")
            })?;
            context.exact_command = origin.exact_command;
            context.command_too_large = origin.command_too_large;
            context.session_cwd = origin.session_cwd;
            context.declared_workdir = origin.declared_workdir;
            context.origin_call_id = Some(origin_call_id.clone());
            context.origin_event_sequence = origin.origin_event_sequence;
            context.origin_occurred_at_unix_ms = origin.origin_occurred_at_unix_ms;
            context.continuation_call_id_sha256 = origin.continuation_call_id_sha256;
            context.continuation_capacity_exceeded = origin.continuation_capacity_exceeded;
            context.correlation_ambiguous = origin.correlation_ambiguous;
        }
    }

    Ok(ValidatedCheckpoint {
        bytes_read: checkpoint.observation.len,
        complete_prefix_hasher,
        complete_prefix_ends_with_terminal_nul_padding,
        pending_tool_contexts,
        pending_tool_authorities,
        pending_continuations,
    })
}

fn record_checkpoint_lineage(
    facts: &mut CodexLineageFactsV0,
    record: &[u8],
    oversized: bool,
) -> Result<()> {
    if record
        .iter()
        .all(|byte| *byte == 0 || byte.is_ascii_whitespace())
    {
        return Ok(());
    }
    if oversized {
        return facts.record(CodexLineageRecordEvidence::UnattributedAmbiguity);
    }
    let record = trim_jsonl_terminator(record);
    match classify_codex_record(record) {
        Ok(probe) => facts.record(codex_lineage_record_evidence(&probe)),
        Err(_) if malformed_record_may_contain_lineage(record) => {
            facts.record(CodexLineageRecordEvidence::UnattributedAmbiguity)
        }
        Err(_) => Ok(()),
    }
}

pub(super) fn invalid_checkpoint_proof(reason: &str) -> CaptureError {
    CaptureError::InvalidPayload(format!("invalid Codex append proof: {reason}"))
}

pub(super) fn observed_opened_file(
    source: &CodexCatalogSource,
    opened: &OpenedProviderSourceFile,
) -> Result<CodexFileObservation> {
    let current = opened_file_observation(&source.source_path, opened.file())?;
    opened.revalidate_same_object()?;
    if !source
        .catalog_observation
        .admits_append_only_growth(&current)
    {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog observation changed before NativePath admission".to_owned(),
        ));
    }
    let expected_prefix = source.catalog_prefix_sha256.ok_or_else(|| {
        CaptureError::SystemInvariant("Codex catalog prefix digest is unavailable")
    })?;
    revalidate_opened_prefix(
        opened.file(),
        source.catalog_observation.len,
        expected_prefix,
    )?;
    opened.revalidate_same_object()?;
    // Discovery admitted this ordinary-file identity and froze this refresh's
    // EOF. Growth after that observation is deferred to the next refresh;
    // broadening the boundary here would give one source two authorities.
    Ok(source.catalog_observation.clone())
}

pub(crate) fn revalidate_codex_source_observation(
    source: &CodexCatalogSource,
    certified: &CodexFileObservation,
    certified_len: u64,
    certified_sha256: [u8; 32],
) -> Result<()> {
    let opened = open_codex_source_capability(source)?;
    let current = opened_file_observation(&source.source_path, opened.file())?;
    opened.revalidate_same_object()?;
    if !certified.admits_append_only_growth(&current) {
        return Err(source_changed_during_scan());
    }
    if current != *certified {
        revalidate_opened_prefix(opened.file(), certified_len, certified_sha256)?;
        #[cfg(test)]
        run_after_codex_prefix_hash_hook();
        let middle = opened_file_observation(&source.source_path, opened.file())?;
        opened.revalidate_same_object()?;
        if !current.admits_append_only_growth(&middle) {
            return Err(source_changed_during_scan());
        }
        // Hash the certified prefix again after observing the object. Exact
        // prefix equality plus monotonic same-object observations is enough to
        // admit a continuously appended JSONL file; waiting for a quiescent
        // metadata window makes an active session impossible to import.
        revalidate_opened_prefix(opened.file(), certified_len, certified_sha256)?;
        #[cfg(test)]
        run_after_codex_second_prefix_hash_hook();
        let after = opened_file_observation(&source.source_path, opened.file())?;
        opened.revalidate_same_object()?;
        if !middle.admits_append_only_growth(&after) {
            return Err(source_changed_during_scan());
        }
        // End on content proof. The preceding observation establishes
        // monotonic same-object growth and this final hash rejects a
        // rewrite-plus-append that raced after the prior proof.
        revalidate_opened_prefix(opened.file(), certified_len, certified_sha256)?;
    }
    Ok(())
}

pub(super) fn revalidate_opened_prefix(
    file: &File,
    len: u64,
    expected_sha256: [u8; 32],
) -> Result<()> {
    let mut hasher = Sha256::new();
    let mut reader = file.try_clone()?;
    hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    if <[u8; 32]>::from(hasher.finalize()) != expected_sha256 {
        return Err(source_changed_during_scan());
    }
    Ok(())
}

pub(crate) fn opened_file_prefix_sha256(file: &File, len: u64) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut reader = file.try_clone()?;
    hash_opened_file_range(&mut reader, 0, len, &mut hasher)?;
    Ok(hasher.finalize().into())
}

pub(crate) fn open_codex_source_capability(
    source: &CodexCatalogSource,
) -> Result<Arc<OpenedProviderSourceFile>> {
    if let Some(opened) = source.opened.as_ref() {
        return Ok(Arc::clone(opened));
    }
    match (
        source.authority_root.as_ref(),
        source.authority_relative_path.as_ref(),
    ) {
        (Some(root), Some(relative_path)) => Ok(Arc::new(root.open_file(relative_path)?)),
        (None, None) => {
            let authority_path = std::path::absolute(&source.source_path)?;
            Ok(Arc::new(open_provider_source_file(&authority_path)?))
        }
        _ => Err(CaptureError::SystemInvariant(
            "Codex source route authority is incomplete",
        )),
    }
}

pub(crate) fn opened_file_observation(path: &Path, file: &File) -> Result<CodexFileObservation> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(source_changed_during_scan());
    }
    let platform_before = opened_file_platform_tokens(path, file, &metadata)?;
    let content_fingerprint = if platform_before.is_some() {
        None
    } else {
        Some(opened_file_content_fingerprint(file, &metadata)?)
    };
    let current = file.metadata()?;
    let platform_after = opened_file_platform_tokens(path, file, &current)?;
    if current.len() != metadata.len()
        || current.modified().ok() != metadata.modified().ok()
        || platform_after != platform_before
    {
        return Err(source_changed_during_scan());
    }
    Ok(CodexFileObservation::from_parts(
        metadata.len(),
        metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        platform_before.map(|tokens| tokens.stable),
        combine_opened_file_token(
            platform_before.map(|tokens| tokens.change),
            content_fingerprint,
        ),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenedFilePlatformTokens {
    stable: [u8; 32],
    change: [u8; 32],
}

#[cfg(unix)]
fn opened_file_platform_tokens(
    _path: &Path,
    _file: &File,
    metadata: &std::fs::Metadata,
) -> Result<Option<OpenedFilePlatformTokens>> {
    use std::os::unix::fs::MetadataExt;

    let mut stable = Sha256::new();
    stable.update(ORDINARY_FILE_TOKEN_DOMAIN);
    stable.update(b"unix-stable\0");
    stable.update(metadata.dev().to_le_bytes());
    stable.update(metadata.ino().to_le_bytes());
    stable.update(metadata.mode().to_le_bytes());
    let mut change = Sha256::new();
    change.update(ORDINARY_FILE_TOKEN_DOMAIN);
    change.update(b"unix-change\0");
    change.update(metadata.dev().to_le_bytes());
    change.update(metadata.ino().to_le_bytes());
    change.update(metadata.ctime().to_le_bytes());
    change.update(metadata.ctime_nsec().to_le_bytes());
    Ok(Some(OpenedFilePlatformTokens {
        stable: stable.finalize().into(),
        change: change.finalize().into(),
    }))
}

#[cfg(target_os = "windows")]
fn opened_file_platform_tokens(
    path: &Path,
    file: &File,
    metadata: &std::fs::Metadata,
) -> Result<Option<OpenedFilePlatformTokens>> {
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

    let mut stable = Sha256::new();
    stable.update(ORDINARY_FILE_TOKEN_DOMAIN);
    stable.update(b"windows-stable\0");
    stable.update(id_info.VolumeSerialNumber.to_le_bytes());
    stable.update(id_info.FileId.Identifier);
    stable.update(basic_info.CreationTime.to_le_bytes());
    let mut change = Sha256::new();
    change.update(ORDINARY_FILE_TOKEN_DOMAIN);
    change.update(b"windows-change\0");
    change.update(id_info.VolumeSerialNumber.to_le_bytes());
    change.update(id_info.FileId.Identifier);
    change.update(basic_info.ChangeTime.to_le_bytes());
    change.update(basic_info.LastWriteTime.to_le_bytes());
    change.update(metadata.len().to_le_bytes());
    Ok(Some(OpenedFilePlatformTokens {
        stable: stable.finalize().into(),
        change: change.finalize().into(),
    }))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn opened_file_platform_tokens(
    _path: &Path,
    _file: &File,
    _metadata: &std::fs::Metadata,
) -> Result<Option<OpenedFilePlatformTokens>> {
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
