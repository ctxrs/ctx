use super::*;

pub(super) fn discover_sources(
    root: &Path,
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
        let metadata = codebuddy_extension_metadata(&path, session_ordinal)?;
        let (observation, _) = CodeBuddyExtensionObservation::read(&metadata, session_ordinal)?;
        let source_revision = effective_source_revision(
            &observation.source_revision,
            options.inventory_observation_token.as_deref(),
        );
        sources.push(build_source(
            CodeBuddySourceShape::Extension,
            path,
            canonical_path,
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
        let source_revision = effective_source_revision(
            &base_revision,
            options.inventory_observation_token.as_deref(),
        );
        sources.push(build_source(
            CodeBuddySourceShape::Cli,
            canonical_path.clone(),
            canonical_path,
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
    base_source_revision: String,
    source_revision: String,
    inventory_observation_token: Option<String>,
    session_ordinal: usize,
    frozen: Option<CodeBuddyFrozenFile>,
) -> Result<CodeBuddySource> {
    Ok(CodeBuddySource {
        shape,
        path,
        canonical_path,
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

pub(super) fn scan_time(value: Option<&str>, field: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "CodeBuddy NativePath state has an invalid {field}"
                ))
            })
        })
        .transpose()
}

pub(super) fn initial_state(
    source: &CodeBuddySource,
    _context: &ProviderAdapterContext,
) -> Result<CodeBuddyScanState> {
    let session = match source.shape {
        CodeBuddySourceShape::Cli => {
            let native_session_id = source
                .canonical_path
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("unknown-session")
                .to_owned();
            CodeBuddySessionState {
                native_session_id,
                project_hash: cli_project_hash(&source.canonical_path),
                ..CodeBuddySessionState::default()
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
                owned = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
                &owned
            };
            CodeBuddySessionState {
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
    Ok(CodeBuddyScanState {
        shape: source.shape,
        source_revision: source.source_revision.clone(),
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
