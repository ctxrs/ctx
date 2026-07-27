use super::*;

pub(super) fn load_committed_source(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<KimiCommittedSource>> {
    let Some(raw) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(None);
    };
    let Ok(committed) = decode_native_path_committed_cursor(&raw.cursor) else {
        // Released pre-NativePath cursors are intentionally migration-only:
        // retain their CAS authority, but rebuild provider-owned state from byte zero.
        return Ok(Some(KimiCommittedSource {
            checkpoint: None,
            source_revision: String::new(),
        }));
    };
    let certified = CertifiedProviderCursor::decode(committed.provider_cursor())?;
    let source_revision = certified.source_revision().to_owned();
    let checkpoint = if certified.parser_revision() == KIMI_NATIVE_CAPTURE_REVISION
        && certified.policy_revision() == KIMI_NATIVE_POLICY_REVISION
        && certified.native_position().kind() == KIMI_NATIVE_POSITION_KIND
    {
        let checkpoint = certified
            .parser_checkpoint()
            .deserialize::<KimiNativeCheckpoint>()?;
        let frontier =
            serde_json::from_slice::<KimiNativeFrontier>(certified.native_position().value())?;
        (checkpoint.version == KIMI_NATIVE_CURSOR_VERSION
            && checkpoint.frontier() == frontier
            && checkpoint.rejected_records == certified.rejected_records())
        .then_some(checkpoint)
    } else {
        None
    };
    Ok(Some(KimiCommittedSource {
        checkpoint,
        source_revision,
    }))
}

pub(super) fn known_kimi_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownKimiRoute>> {
    let source_root_display = source_root.display().to_string();
    let source_root_identity = provider_path_identity(source_root)?;
    let mut routes = BTreeMap::<String, KnownKimiRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::KimiCodeCli
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(KIMI_CODE_CLI_SOURCE_FORMAT)
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let Some(persisted_source_root) = source.descriptor.source_root.as_deref() else {
            continue;
        };
        let persisted_root = Path::new(persisted_source_root);
        let persisted_root_identity = provider_path_identity(persisted_root)?;
        let current_root = persisted_root_identity == source_root_identity;
        let legacy_file_root = persisted_root_identity == provider_path_identity(&path)?
            && canonical_source_root_for_wire(&path)
                .and_then(|root| provider_path_identity(&root))
                .is_ok_and(|identity| identity == source_root_identity);
        if !current_root && !legacy_file_root {
            continue;
        }
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::KimiCodeCli,
            KIMI_CODE_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let committed = load_committed_source(store, machine_id, &stream)?;
        let checkpoint = committed.and_then(|committed| committed.checkpoint);
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = KnownKimiRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
            needs_source_root_migration: source.descriptor.source_root.as_deref()
                != Some(source_root_display.as_str()),
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Kimi persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

pub(super) fn retire_missing_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownKimiRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    for route in known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
    {
        if retire_kimi_route(store, bulk_guard, machine_id, retired_at, route, reason)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
    }
    Ok(summary)
}

pub(super) fn retire_kimi_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownKimiRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let mut checkpoint = route.checkpoint.clone().unwrap_or(KimiNativeCheckpoint {
        version: KIMI_NATIVE_CURSOR_VERSION,
        route_sha256: route_sha256(&route.locator_identity),
        physical_device: None,
        physical_inode: None,
        observed_file_len: 0,
        wire_revision: String::new(),
        auxiliary_revision: 0,
        admission_scope_revision: String::new(),
        complete_offset: 0,
        next_ordinal: 0,
        committed_prefix_sha256: initial_prefix_sha256(),
        started_at: None,
        emitted_session: false,
        accepted_events: 0,
        accepted_file_touches: 0,
        rejected_records: 0,
        rejected_outputs: 0,
        terminal: true,
        retired: true,
    });
    checkpoint.terminal = true;
    checkpoint.retired = true;
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        kimi_sync_cursor(
            machine_id,
            stream.clone(),
            &route.source_revision,
            &checkpoint,
            retired_at,
        )?,
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::KimiCodeCli,
        source_format: KIMI_CODE_CLI_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(KIMI_RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("kimi-nativepath-retirement-v1:{:x}", digest.finalize())
}

pub(super) fn discover_kimi_wire_files(root: &Path) -> Result<KimiInventory> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let source_root = canonical_source_root_for_wire(root)
                .or_else(|_| std::path::absolute(root).map_err(CaptureError::from))?;
            return Ok(KimiInventory {
                paths: BTreeSet::new(),
                source_root,
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript roots must not be symbolic links",
        });
    }
    if metadata.is_file() {
        KimiWireRoute::parse(root)?;
        let canonical = fs::canonicalize(root)?;
        return Ok(KimiInventory {
            paths: BTreeSet::from([canonical.clone()]),
            source_root: canonical_source_root_for_wire(&canonical)?,
            root_missing: false,
        });
    }
    if !metadata.is_dir() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript root is neither a file nor directory",
        });
    }
    let mut paths = BTreeSet::new();
    let mut source_roots = BTreeSet::new();
    let mut entries = 0_usize;
    discover_kimi_directory(root, 0, &mut entries, &mut paths, &mut source_roots)?;
    if source_roots.len() > 1 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript selection spans multiple canonical layout roots",
        });
    }
    Ok(KimiInventory {
        paths,
        source_root: source_roots
            .into_iter()
            .next()
            .unwrap_or(fs::canonicalize(root)?),
        root_missing: false,
    })
}

pub(super) fn discover_kimi_directory(
    directory: &Path,
    depth: usize,
    entries: &mut usize,
    paths: &mut BTreeSet<PathBuf>,
    source_roots: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if depth > KIMI_NATIVE_DISCOVERY_MAX_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: directory.to_path_buf(),
            reason: "Kimi transcript tree exceeds the discovery depth bound",
        });
    }
    let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        *entries = entries.saturating_add(1);
        if *entries > KIMI_NATIVE_DISCOVERY_MAX_ENTRIES {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: directory.to_path_buf(),
                reason: "Kimi transcript tree exceeds the discovery entry bound",
            });
        }
        let path = child.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            discover_kimi_directory(&path, depth.saturating_add(1), entries, paths, source_roots)?;
        } else if metadata.is_file() && KimiWireRoute::parse(&path).is_ok() {
            let canonical = fs::canonicalize(path)?;
            KimiWireRoute::parse(&canonical)?;
            source_roots.insert(canonical_source_root_for_wire(&canonical)?);
            paths.insert(canonical);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_kimi_message_locator(
    event: &mut KimiCoreEvent,
    value: &Value,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    source_revision: &str,
    path_identity: &str,
) -> Result<()> {
    if event.event_type != EventType::Message {
        return Ok(());
    }
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = kimi_event_text(record_type, value, event.event_type);
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported Kimi complete-content route has no verified profile",
        ));
    };
    let mut encoded = Vec::with_capacity(80);
    encoded.extend_from_slice(&byte_start.to_be_bytes());
    encoded.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    encoded.extend_from_slice(&domain_digest(
        SOURCE_REVISION_DIGEST_DOMAIN,
        source_revision,
    ));
    encoded.extend_from_slice(&domain_digest(PATH_IDENTITY_DIGEST_DOMAIN, path_identity));
    let native_record_id = event.legacy_provider_event_hash.clone();
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        KIMI_JSONL_LOCATOR_KIND,
        &encoded,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("Kimi verified-content locator collection is malformed"),
    )?;
    Ok(())
}

pub(super) fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    max_bytes: usize,
) -> Result<RawLine> {
    let mut bytes = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed_bytes =
            observed_bytes
                .checked_add(chunk.len() as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "Kimi JSONL line length overflowed",
                ))?;
        if bytes.len() < max_bytes.saturating_add(2) {
            let remaining = max_bytes.saturating_add(2).saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        oversized |= observed_bytes > max_bytes as u64;
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if terminated {
            break;
        }
    }
    Ok(RawLine {
        bytes,
        observed_bytes,
        terminated,
        oversized,
    })
}

pub(super) fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

pub(super) fn initial_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(KIMI_PREFIX_DOMAIN);
    hasher
}

pub(super) fn initial_prefix_sha256() -> [u8; 32] {
    prefix_digest(&initial_prefix_hasher())
}

pub(super) fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

pub(super) fn route_sha256(locator_identity: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(KIMI_ROUTE_DOMAIN);
    digest.update(locator_identity.as_bytes());
    digest.finalize().into()
}

pub(super) fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

pub(super) fn effective_source_revision(revision: &str, inventory_token: Option<&str>) -> String {
    let Some(token) = inventory_token else {
        return revision.to_owned();
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-kimi-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
}
