use super::*;

pub(super) fn discover_sources(
    root: &Path,
    configured_root: &Path,
    options: &ProviderImportOptions,
) -> Result<CodeBuddyInventory> {
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if root_metadata.is_none() {
        return Ok(CodeBuddyInventory {
            sources: Vec::new(),
            root_missing: true,
        });
    }

    let mut extension_paths = BTreeSet::new();
    extension_discovery::visit_codebuddy_extension_sessions(root, &mut |path| {
        extension_paths.insert(fs::canonicalize(path)?);
        Ok(())
    })?;
    let cli_paths = discover_cli_paths(root)?;
    let mut sources = Vec::with_capacity(extension_paths.len().saturating_add(cli_paths.len()));

    for (index, canonical_path) in extension_paths.into_iter().enumerate() {
        let path = canonical_path.clone();
        let session_ordinal = index.saturating_add(1);
        let (metadata, _) = codebuddy_extension_metadata(&path, session_ordinal)?;
        let Some(metadata) = metadata else {
            continue;
        };
        let mut ignored = ProviderImportSummary::default();
        let observation =
            CodeBuddyExtensionObservation::read(&metadata, session_ordinal, &mut ignored)?;
        let locator_identity = provider_path_identity(&canonical_path)?;
        let source_revision = effective_source_revision(
            &observation.source_revision,
            options.inventory_observation_token.as_deref(),
        );
        sources.push(build_source(
            CodeBuddySourceShape::Extension,
            path,
            canonical_path,
            configured_root,
            locator_identity,
            observation.source_revision,
            source_revision,
            options.inventory_observation_token.clone(),
            session_ordinal,
            None,
        )?);
    }
    let extension_count = sources.len();
    for (index, canonical_path) in cli_paths.into_iter().enumerate() {
        let frozen = CodeBuddyFrozenFile::read(&canonical_path)?;
        let base_revision =
            frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION);
        let locator_identity = provider_path_identity(&canonical_path)?;
        let source_revision = effective_source_revision(
            &base_revision,
            options.inventory_observation_token.as_deref(),
        );
        sources.push(build_source(
            CodeBuddySourceShape::Cli,
            canonical_path.clone(),
            canonical_path,
            configured_root,
            locator_identity,
            base_revision,
            source_revision,
            options.inventory_observation_token.clone(),
            extension_count.saturating_add(index).saturating_add(1),
            Some(frozen),
        )?);
    }
    sources.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
    Ok(CodeBuddyInventory {
        sources,
        root_missing: false,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_source(
    shape: CodeBuddySourceShape,
    path: PathBuf,
    canonical_path: PathBuf,
    configured_root: &Path,
    locator_identity: String,
    base_source_revision: String,
    source_revision: String,
    inventory_observation_token: Option<String>,
    session_ordinal: usize,
    frozen: Option<CodeBuddyFrozenFile>,
) -> Result<CodeBuddySource> {
    let raw_source_path = canonical_path.display().to_string();
    let source_root = configured_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "CodeBuddy NativePath source has no canonical identity",
    ))?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &locator_identity,
    );
    Ok(CodeBuddySource {
        shape,
        path,
        canonical_path,
        configured_root: configured_root.to_path_buf(),
        locator_identity,
        cursor_stream,
        proposed_source_identity,
        base_source_revision,
        source_revision,
        inventory_observation_token,
        session_ordinal,
        frozen,
        capability: None,
    })
}

pub(super) fn discover_cli_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let metadata = fs::symlink_metadata(root)?;
    let mut paths = BTreeSet::new();
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            paths.insert(fs::canonicalize(root)?);
        }
        return Ok(paths);
    }
    if !metadata.file_type().is_dir() {
        return Ok(paths);
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    let scan_root = if root.join("projects").is_dir() {
        root.join("projects")
    } else if root.file_name().and_then(|name| name.to_str()) == Some("projects")
        || root
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("projects")
    {
        root.to_path_buf()
    } else {
        return Ok(paths);
    };
    visit_cli_tree(&scan_root, &mut paths)?;
    Ok(paths)
}

pub(super) fn visit_cli_tree(root: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visit_cli_tree(&path, paths)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        {
            ensure_regular_provider_transcript_file(&path)?;
            paths.insert(fs::canonicalize(path)?);
        }
    }
    Ok(())
}

pub(super) fn effective_source_revision(base: &str, inventory_token: Option<&str>) -> String {
    let Some(token) = inventory_token else {
        return base.to_owned();
    };
    let mut digest = Sha256::new();
    digest.update(CODEBUDDY_INVENTORY_REVISION_DOMAIN);
    digest.update((base.len() as u64).to_be_bytes());
    digest.update(base.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!(
        "inventory-observation-sha256-v1:{}",
        hex(&digest.finalize())
    )
}

pub(super) fn checkpoint_time(value: Option<&str>, field: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "CodeBuddy NativePath cursor has an invalid {field}"
                ))
            })
        })
        .transpose()
}

#[derive(Debug, Clone)]
pub(super) struct KnownRoute {
    pub(super) locator_identity: String,
    pub(super) cursor_stream: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
}

pub(super) fn known_routes(
    store: &Store,
    context: &ProviderAdapterContext,
    configured_root: &Path,
) -> Result<Vec<KnownRoute>> {
    let source_root = configured_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::CodeBuddy
            || source.descriptor.machine_id != context.machine_id
            || source.descriptor.source_format.as_deref() != Some(CODEBUDDY_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let source_revision = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .or_else(|| {
                source
                    .sync
                    .metadata
                    .pointer("/source_metadata/source_revision")
                    .and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_owned();
        if source_revision.is_empty() {
            continue;
        }
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            &locator_identity,
        );
        if store
            .get_sync_cursor(None, &context.machine_id, &cursor_stream)?
            .is_none()
        {
            continue;
        }
        routes.insert(
            locator_identity.clone(),
            KnownRoute {
                locator_identity,
                cursor_stream,
                canonical_source_identity: canonical_source_identity.to_owned(),
                source_revision,
            },
        );
    }
    Ok(routes.into_values().collect())
}

pub(super) fn load_stored_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<StoredCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(StoredCursor::Native {
            cursor: CodeBuddyNativeCursor::decode(committed.provider_cursor())?,
            stored,
        });
    }

    // Released pre-v0.27 CodeBuddy cursors are accepted only as a migration
    // input.  They never become a runtime resume frontier.
    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_some() {
        return Ok(StoredCursor::ReleasedLegacy { stored });
    }
    Err(CaptureError::InvalidPayload(
        "CodeBuddy cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}

pub(super) fn plan_source(
    store: &Store,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddySourcePlan> {
    let stored = load_stored_cursor(store, &context.machine_id, &source.cursor_stream)?;
    let initial = initial_cursor(source, context)?;
    match stored {
        StoredCursor::None => Ok(CodeBuddySourcePlan {
            change: CodeBuddySourceChange::Fresh,
            expected_store_cursor: None,
            cursor: initial,
        }),
        StoredCursor::ReleasedLegacy { stored } => {
            let mut cursor = initial;
            cursor.generation = 1;
            Ok(CodeBuddySourcePlan {
                change: CodeBuddySourceChange::LegacyMigration,
                expected_store_cursor: Some(stored.cursor),
                cursor,
            })
        }
        StoredCursor::Native { stored, mut cursor } => {
            if cursor.shape != source.shape
                || cursor.canonical_path != source.canonical_path
                || cursor.source_identity != source.proposed_source_identity
            {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy NativePath cursor route does not match the selected source"
                        .to_owned(),
                ));
            }
            if cursor.source_revision == source.source_revision {
                return Ok(CodeBuddySourcePlan {
                    change: CodeBuddySourceChange::Resume,
                    expected_store_cursor: Some(stored.cursor),
                    cursor,
                });
            }

            if source.shape == CodeBuddySourceShape::Cli && cli_prefix_matches(source, &cursor)? {
                cursor.source_revision.clone_from(&source.source_revision);
                cursor.terminal = false;
                cursor.incomplete_tail = None;
                return Ok(CodeBuddySourcePlan {
                    change: CodeBuddySourceChange::Append,
                    expected_store_cursor: Some(stored.cursor),
                    cursor,
                });
            }

            let generation =
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy source generation overflowed",
                    ))?;
            let mut replacement = initial;
            replacement.generation = generation;
            Ok(CodeBuddySourcePlan {
                change: CodeBuddySourceChange::Rewrite,
                expected_store_cursor: Some(stored.cursor),
                cursor: replacement,
            })
        }
    }
}

pub(super) fn initial_cursor(
    source: &CodeBuddySource,
    _context: &ProviderAdapterContext,
) -> Result<CodeBuddyNativeCursor> {
    let session = match source.shape {
        CodeBuddySourceShape::Cli => {
            let native_session_id = source
                .canonical_path
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("unknown-session")
                .to_owned();
            CodeBuddySessionCheckpoint {
                native_session_id,
                project_hash: cli_project_hash(&source.canonical_path),
                ..CodeBuddySessionCheckpoint::default()
            }
        }
        CodeBuddySourceShape::Extension => {
            let admitted = source
                .capability
                .as_ref()
                .and_then(|capability| capability.extension.as_ref());
            let owned;
            let metadata = if let Some(admitted) = admitted {
                &admitted.metadata
            } else {
                let (metadata, _) =
                    codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
                owned = metadata.ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: source.path.clone(),
                    reason: "CodeBuddy extension session index is unreadable",
                })?;
                &owned
            };
            CodeBuddySessionCheckpoint {
                native_session_id: metadata.native_session_id.clone(),
                project_hash: metadata.project_hash.clone(),
                cwd: None,
                started_at: metadata
                    .conversation
                    .as_ref()
                    .and_then(|value| {
                        task_json_time_field(value, &["createdAt", "created_at", "timestamp"])
                    })
                    .map(|value| value.to_rfc3339()),
                ended_at: metadata
                    .conversation
                    .as_ref()
                    .and_then(|value| {
                        task_json_time_field(
                            value,
                            &["lastMessageAt", "updatedAt", "completedAt", "last_modified"],
                        )
                    })
                    .map(|value| value.to_rfc3339()),
                generated_title_anchor: None,
                row_count: 0,
            }
        }
    };
    Ok(CodeBuddyNativeCursor {
        version: CODEBUDDY_NATIVE_CURSOR_VERSION,
        shape: source.shape,
        canonical_path: source.canonical_path.clone(),
        source_revision: source.source_revision.clone(),
        source_identity: source.proposed_source_identity.clone(),
        generation: 0,
        next_native_offset: 0,
        next_native_ordinal: 0,
        certified_prefix_sha256: sha256_hex(&[]),
        file_identity: source
            .frozen
            .as_ref()
            .map(CodeBuddyFrozenFile::identity_token),
        terminal: false,
        accepted_events: 0,
        skipped_metadata: 0,
        rejected_records: 0,
        failures: Vec::new(),
        incomplete_tail: None,
        session,
    })
}

pub(super) fn cli_prefix_matches(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
) -> Result<bool> {
    let Some(frozen) = source.frozen.as_ref() else {
        return Ok(false);
    };
    if cursor.next_native_offset > frozen.length
        || cursor.file_identity.as_deref() != Some(frozen.identity_token().as_str())
    {
        return Ok(false);
    }
    Ok(source_prefix_sha256(source, cursor.next_native_offset)?
        == cursor.certified_prefix_sha256)
}

pub(super) fn source_prefix_sha256(source: &CodeBuddySource, length: u64) -> Result<String> {
    if let Some(file) = source
        .capability
        .as_ref()
        .and_then(|capability| capability.primary.as_ref())
    {
        let mut file = file.file().try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        return reader_prefix_sha256(&mut file, length);
    }
    file_prefix_sha256(&source.path, length)
}

pub(super) fn file_prefix_sha256(path: &Path, length: u64) -> Result<String> {
    let mut file = File::open(path)?;
    reader_prefix_sha256(&mut file, length)
}

fn reader_prefix_sha256(file: &mut File, length: u64) -> Result<String> {
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    let mut digest = Sha256::new();
    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("CodeBuddy prefix length exceeds platform limits")
        })?;
        let read = file.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hex(&digest.finalize()))
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex(&digest.finalize())
}
