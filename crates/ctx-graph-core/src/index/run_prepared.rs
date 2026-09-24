use super::*;

pub(super) fn run_prepared(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    reserved: usize,
) -> Result<IndexReport> {
    let started = std::time::Instant::now();
    for source_root in &options.python_source_roots {
        ensure!(
            source_root.is_empty()
                || (!source_root.contains(['\\', ':'])
                    && source_root
                        .split('/')
                        .all(|part| !matches!(part, "" | "." | ".."))),
            "Python source roots must be normalized repository-relative directories (empty means repository root)"
        );
    }
    ensure!(
        !options.semantic_code || options.ingest.semantic.is_some(),
        "deep code extraction requires an explicit semantic provider"
    );
    ensure!(
        options.max_semantic_files <= 100_000,
        "max_semantic_files must be at most 100000"
    );
    ensure!(
        reserved <= options.max_semantic_files,
        "semantic extraction exceeds configured file budget"
    );
    let root = root
        .canonicalize()
        .context("cannot canonicalize index root")?;
    ensure!(root.is_dir(), "index root must be a directory");
    let root_text = root.to_str().context("index root must be UTF-8")?;
    let mut store = Store::create(db)?;
    let stats = store.stats()?;
    ensure!(
        stats.kind != "imported",
        "cannot index native sources into an imported graph"
    );
    ensure!(
        stats.root.as_deref().is_none_or(|old| old == root_text),
        "index root differs from the stored root"
    );
    let db = db.canonicalize()?;
    let index_options = serde_json::to_value(options)?;
    let stored_options = stored_options_value(&store)?;
    let ingest_fingerprint = ingest::config_fingerprint(&options.ingest)?;
    let mut old: HashMap<_, _> = store
        .file_stamps()?
        .into_iter()
        .map(|f| (f.path, f.hash))
        .collect();
    let manifest_path = scan_manifest_path(&db);
    let existing_manifest = read_manifest(&manifest_path);
    let database = database_identity(&db);
    let reusable = existing_manifest.as_ref().and_then(|manifest| {
        reusable_scan(
            manifest,
            root_text,
            database.as_ref(),
            stats.generation,
            &stored_options,
            &index_options,
            &ingest_fingerprint,
        )
    });
    let scan = prepare_scan(&root, &db, options, true, reusable, None)?;
    if !options.force
        && let Some(proof) = &scan.proof
        && let Some(database) = database_identity(&db)
    {
        let current = ScanManifest {
            version: SCAN_MANIFEST_VERSION,
            root: root_text.to_owned(),
            database,
            generation: stats.generation,
            stored_options: stored_options.clone(),
            index_options: index_options.clone(),
            extractor_revision: EXTRACTOR_REVISION,
            language_revision: languages::revision().into(),
            ingest_fingerprint: ingest_fingerprint.clone(),
            scan: proof.clone(),
        };
        if manifest_matches(&manifest_path, &current) {
            return unchanged_report(&stats, options, started);
        }
        if reusable.is_some_and(|previous| scan_content_matches(previous, proof)) {
            write_manifest(&manifest_path, &current)?;
            return unchanged_report(&stats, options, started);
        }
    }
    let initial_proof = scan.proof.clone();
    let PreparedScan {
        files,
        mut coverage,
        managed,
        mut cached_sources,
        mut freshly_read,
        ..
    } = scan;
    let mut context = discover_project_context(
        &root,
        &files.iter().map(|f| f.1.clone()).collect::<Vec<_>>(),
        &options.swift_modules,
    )?;
    let mut python = python_inventory(&files, options, &old, &context, &ingest_fingerprint)?;
    #[cfg(test)]
    tests::after_python_inventory();
    let detect_ms = started.elapsed().as_secs_f64() * 1000.0;
    let extracting = std::time::Instant::now();
    let mut changed = vec![];
    let mut semantic_files = reserved;
    for (path, relative) in files {
        let code = is_code(&relative) || content_probe(&relative);
        let maximum = if code && !relative.ends_with(".dmi") {
            MAX_SOURCE_BYTES as u64
        } else {
            options.ingest.max_input_bytes
        };
        let cached = cached_sources
            .as_mut()
            .and_then(|sources| sources.remove(&relative))
            .filter(|source| source_version(&path).as_ref() == Some(&source.version));
        let cached_version = cached.as_ref().map(|source| source.version.clone());
        let (content_hash, mut content) = cached
            .map(|source| Ok((source.digest, source.content)))
            .unwrap_or_else(|| read_source(&path, maximum))?;
        context.validate_source(&relative, &content_hash)?;
        python.validate_source(&relative, &content_hash)?;
        let hash = stamp(
            &relative,
            &content_hash,
            &context,
            &ingest_fingerprint,
            options,
            python.context_token(&relative),
        );
        let unchanged = old
            .remove(&relative)
            .is_some_and(|previous| previous == hash);
        if unchanged && !options.force {
            coverage.unchanged_files += 1;
            continue;
        }
        if content.is_none() {
            freshly_read.insert(relative.clone());
            let (fresh_hash, fresh_content) = read_source(&path, maximum)?;
            ensure!(
                fresh_hash == content_hash,
                "source changed during indexing; previous graph retained"
            );
            content = fresh_content;
        }
        let mut facts = if code {
            match content {
                Some(bytes) if relative.ends_with(".dmi") => {
                    languages::extended::parse_dmi(&relative, &bytes, &hash)?
                }
                Some(bytes) if relative.ends_with(".dfm") => {
                    languages::extended::parse_pascal_form_bytes(&relative, &bytes, &hash)?
                }
                Some(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(source) => {
                        let mut facts = if relative.ends_with(".py")
                            || (Path::new(&relative).extension().is_none()
                                && languages::scripted::shebang_language(source) == Some("python"))
                        {
                            let mut facts = python
                                .facts
                                .remove(&relative)
                                .context("Python inventory changed during scan; retry indexing")?;
                            facts.hash = hash.clone();
                            facts
                        } else if is_robot(&relative) && options.robot_python.is_some() {
                            let python = options.robot_python.as_ref().unwrap();
                            let python = if python.is_relative() && python.components().count() > 1
                            {
                                root.join(python)
                            } else {
                                python.clone()
                            };
                            languages::templates::parse_robot_official(
                                &relative, source, &hash, &python,
                            )?
                        } else if let Some(mut facts) =
                            context.take_cached_facts(&relative, &content_hash)?
                        {
                            facts.hash = hash.clone();
                            facts
                        } else {
                            languages::parse(&relative, source, &hash)?.unwrap_or_else(|| {
                                diagnostic(
                                    &relative,
                                    &hash,
                                    "file content does not match a supported language",
                                )
                            })
                        };
                        if options.semantic_code && !facts.nodes.is_empty() {
                            semantic_files += 1;
                            ensure!(
                                semantic_files <= options.max_semantic_files,
                                "semantic extraction exceeds configured file budget"
                            );
                            ingest::enrich_facts(&mut facts, source, &options.ingest)?;
                        }
                        facts
                    }
                    Err(_) => diagnostic(&relative, &hash, "source is not UTF-8"),
                },
                None => diagnostic(
                    &relative,
                    &hash,
                    if relative.ends_with(".dmi") {
                        "source exceeds configured input byte limit"
                    } else {
                        "source exceeds the 4 MiB limit"
                    },
                ),
            }
        } else {
            if options.ingest.semantic.is_some() {
                semantic_files += 1;
                ensure!(
                    semantic_files <= options.max_semantic_files,
                    "semantic extraction exceeds the configured file budget; increase max_semantic_files explicitly"
                );
            }
            let bytes = content
                .context("document exceeds configured input byte limit; previous graph retained")?;
            let suffix = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| format!(".{s}"))
                .unwrap_or_default();
            // Converters receive immutable task-owned bytes, not a source path
            // that could change after its fingerprint was computed.
            let mut staged = tempfile::Builder::new()
                .prefix("graf-ingest-")
                .suffix(&suffix)
                .tempfile()?;
            staged.write_all(&bytes)?;
            staged.flush()?;
            ingest::extract(staged.path(), &relative, &hash, &options.ingest)?
        };
        if let Some(version) = cached_version {
            ensure!(
                source_version(&path).as_ref() == Some(&version),
                "source changed during cached extraction; retry indexing"
            );
        }
        context.apply(&mut facts);
        add_document_aliases(&mut facts);
        changed.push(facts);
    }
    for mut facts in managed {
        coverage.supported_files += 1;
        if old
            .remove(&facts.path)
            .is_some_and(|previous| previous == facts.hash)
        {
            coverage.unchanged_files += 1;
        } else {
            add_document_aliases(&mut facts);
            changed.push(facts);
        }
    }
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    let mut deleted: Vec<_> = old.into_keys().collect();
    deleted.sort();
    #[cfg(test)]
    tests::before_publish_validation();
    if let Some(initial_proof) = &initial_proof {
        let current = prepare_scan(
            &root,
            &db,
            options,
            false,
            Some(initial_proof),
            Some(&freshly_read),
        )?;
        ensure!(
            current
                .proof
                .as_ref()
                .is_some_and(|proof| scan_content_matches(initial_proof, proof)),
            "source tree changed during indexing; previous graph retained"
        );
    }
    let extract_ms = extracting.elapsed().as_secs_f64() * 1000.0;
    let committing = std::time::Instant::now();
    let losses = store.semantic_losses(&changed)?;
    if !losses.is_empty() {
        ensure!(
            options.allow_semantic_shrink,
            "semantic extraction would remove recorded facts from {}; previous graph retained. Review the result and use --allow-semantic-shrink to accept it with a backup",
            losses.join(", ")
        );
        preserve_snapshot(&root, &store)?;
    }
    let prepared_native_write = stats.kind == "empty" && !changed.is_empty();
    if prepared_native_write {
        store.prepare_native_index_write()?;
    }
    let applied = store.apply_native_with_options(
        root_text,
        changed,
        deleted,
        coverage,
        index_options.clone(),
    );
    let mut report = if prepared_native_write {
        store.finish_native_index_write(applied)?
    } else {
        applied?
    };
    report.semantic_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_budget.as_ref())
        .map(|budget| budget.usage())
        .transpose()?;
    report.provider_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_usage.as_ref())
        .map(|recorder| recorder.snapshot())
        .transpose()?;
    if options.timing {
        report.timings = Some(IndexTimings {
            detect_ms,
            extract_ms,
            commit_ms: committing.elapsed().as_secs_f64() * 1000.0,
            total_ms: started.elapsed().as_secs_f64() * 1000.0,
            capture_ms: None,
        });
    }
    drop(store);
    if let Some(initial_proof) = initial_proof
        && let Some(database) = database_identity(&db)
    {
        let manifest = ScanManifest {
            version: SCAN_MANIFEST_VERSION,
            root: root_text.to_owned(),
            database,
            generation: report.generation,
            stored_options: index_options.clone(),
            index_options,
            extractor_revision: EXTRACTOR_REVISION,
            language_revision: languages::revision().into(),
            ingest_fingerprint,
            scan: initial_proof,
        };
        if serde_json::to_vec(&manifest)
            .is_ok_and(|bytes| bytes.len() as u64 <= MAX_SCAN_MANIFEST_BYTES)
        {
            let _ = write_manifest(&scan_manifest_path(&db), &manifest);
        }
    }
    Ok(report)
}

pub(super) fn discover_project_context(
    root: &Path,
    files: &[String],
    swift_modules: &BTreeMap<String, String>,
) -> Result<ProjectContext> {
    PROJECT_CONTEXT_DISCOVERIES.with(|count| count.set(count.get() + 1));
    ProjectContext::discover_with_swift_modules(root, files, swift_modules)
}

pub(super) fn source_maximum(relative: &str, options: &IndexOptions) -> u64 {
    if (is_code(relative) || content_probe(relative)) && !relative.ends_with(".dmi") {
        MAX_SOURCE_BYTES as u64
    } else {
        options.ingest.max_input_bytes
    }
}

pub(super) fn prepare_scan(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    cache_sources: bool,
    previous: Option<&ScanProof>,
    force_read: Option<&BTreeSet<String>>,
) -> Result<PreparedScan> {
    let Discovery {
        files,
        coverage,
        ignore_files,
        context_files,
    } = discover(root, db, options)?;
    let mut cacheable = ignore_files.is_some() && context_files.is_some();
    let mut sources = BTreeMap::new();
    let previous: BTreeMap<_, _> = previous
        .into_iter()
        .flat_map(|proof| &proof.sources)
        .map(|proof| (proof.path.as_str(), proof))
        .collect();
    let mut freshly_read = BTreeSet::new();
    let source_paths: BTreeSet<_> = files.iter().map(|(_, relative)| relative.clone()).collect();
    let mut inputs = BTreeMap::new();
    for (path, relative) in files
        .iter()
        .chain(context_files.as_deref().unwrap_or_default())
    {
        inputs
            .entry(relative.clone())
            .or_insert_with(|| path.clone());
    }
    let mut cached_sources = cache_sources.then(HashMap::new);
    let mut cached_bytes = 0usize;
    for (relative, path) in inputs {
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            cacheable = false;
            continue;
        }
        let version = source_version_from_metadata(&metadata);
        let identity = manifest_source_identity(&metadata);
        let reusable = force_read
            .is_none_or(|paths| !paths.contains(&relative))
            .then(|| {
                identity.as_ref().and_then(|identity| {
                    previous
                        .get(relative.as_str())
                        .copied()
                        .filter(|proof| proof.identity.as_ref() == Some(identity))
                })
            })
            .flatten();
        let (digest, content) = if let Some(proof) = reusable {
            (proof.digest.clone(), None)
        } else {
            freshly_read.insert(relative.clone());
            read_source(&path, source_maximum(&relative, options))?
        };
        if source_paths.contains(&relative) && cached_sources.is_some() {
            cached_bytes = cached_bytes.saturating_add(content.as_ref().map_or(0, Vec::len));
            if cached_bytes <= MAX_SCAN_CACHE_BYTES
                && let Some(version) =
                    version.filter(|version| source_version(&path).as_ref() == Some(version))
            {
                cached_sources.as_mut().unwrap().insert(
                    relative.clone(),
                    CachedSource {
                        digest: digest.clone(),
                        content,
                        version,
                    },
                );
            } else if cached_bytes > MAX_SCAN_CACHE_BYTES {
                cached_sources = None;
            }
        }
        sources.insert(
            relative.clone(),
            SourceProof {
                path: relative,
                digest,
                identity,
            },
        );
    }
    let managed = crate::sources::read(root)?;
    let managed_sources = managed
        .iter()
        .map(|facts| SourceProof {
            path: facts.path.clone(),
            digest: facts.hash.clone(),
            identity: None,
        })
        .collect();
    let proof = cacheable.then(|| ScanProof {
        supported_files: coverage.supported_files + managed.len(),
        unsupported_files: coverage.unsupported_files,
        ignore_fingerprint: ignore_fingerprint(ignore_files.unwrap()),
        sources: sources.into_values().collect(),
        managed_sources,
    });
    if proof.is_none() {
        cached_sources = None;
    }
    Ok(PreparedScan {
        files,
        coverage,
        managed,
        proof,
        cached_sources,
        freshly_read,
    })
}

#[cfg(unix)]
pub(super) fn manifest_source_identity(
    metadata: &std::fs::Metadata,
) -> Option<ManifestSourceIdentity> {
    let modified_nanos = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        let changed_seconds = u128::try_from(metadata.ctime()).ok()?;
        let changed_subsecond = u128::try_from(metadata.ctime_nsec()).ok()?;
        if changed_subsecond >= 1_000_000_000 {
            return None;
        }
        let changed_nanos = changed_seconds
            .checked_mul(1_000_000_000)?
            .checked_add(changed_subsecond)?;
        let observed_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        // Timestamp metadata is a safe digest cache key only after its
        // filesystem-resolution window has closed. If a scan overlaps that
        // window, persist no identity: a later run must hash once more before
        // it can establish a reusable proof.
        if !source_identity_settled(modified_nanos, changed_nanos, observed_nanos) {
            return None;
        }
        format!(
            "{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )
    };
    Some(ManifestSourceIdentity {
        length: metadata.len(),
        modified: modified_nanos.to_string(),
        file_id,
    })
}

#[cfg(unix)]
pub(super) fn source_identity_settled(
    modified_nanos: u128,
    changed_nanos: u128,
    observed_nanos: u128,
) -> bool {
    observed_nanos.saturating_sub(changed_nanos.max(modified_nanos)) >= SOURCE_IDENTITY_SETTLE_NANOS
}

#[cfg(not(unix))]
pub(super) fn manifest_source_identity(
    _metadata: &std::fs::Metadata,
) -> Option<ManifestSourceIdentity> {
    // The portable metadata surface has no change counter independent of mtime.
    // Keep content hashing on those platforms rather than trust a restorable timestamp.
    None
}

pub(super) fn source_version(path: &Path) -> Option<SourceVersion> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    source_version_from_metadata(&metadata)
}

pub(super) fn source_version_from_metadata(metadata: &std::fs::Metadata) -> Option<SourceVersion> {
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(windows)]
    let file_id = {
        use std::os::windows::fs::MetadataExt;
        metadata.creation_time().to_string()
    };
    #[cfg(not(any(unix, windows)))]
    let file_id = String::new();
    Some(SourceVersion {
        length: metadata.len(),
        modified: metadata.modified().ok()?,
        file_id,
    })
}

pub(super) fn ignore_fingerprint(mut files: Vec<(Vec<u8>, String)>) -> String {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash = blake3::Hasher::new();
    hash.update(b"graf-ignore-v1");
    for (path, digest) in files {
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(&path);
        hash.update(digest.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

pub(super) fn stored_options_value(store: &Store) -> Result<serde_json::Value> {
    Ok(store
        .graph_metadata()?
        .get("graf_index_options")
        .cloned()
        .unwrap_or(serde_json::to_value(IndexOptions::default())?))
}

pub(super) fn scan_manifest_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".scan-manifest.json");
    PathBuf::from(name)
}

pub(super) fn database_identity(db: &Path) -> Option<DatabaseIdentity> {
    let metadata = std::fs::symlink_metadata(db).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .to_string();
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(windows)]
    let file_id = {
        use std::os::windows::fs::MetadataExt;
        metadata.creation_time().to_string()
    };
    #[cfg(not(any(unix, windows)))]
    let file_id = modified.clone();
    Some(DatabaseIdentity {
        length: metadata.len(),
        modified,
        file_id,
    })
}

pub(super) fn manifest_matches(path: &Path, expected: &ScanManifest) -> bool {
    read_manifest(path).is_some_and(|actual| actual == *expected)
}

pub(super) fn scan_content_matches(left: &ScanProof, right: &ScanProof) -> bool {
    fn sources_match(left: &[SourceProof], right: &[SourceProof]) -> bool {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| left.path == right.path && left.digest == right.digest)
    }

    left.supported_files == right.supported_files
        && left.unsupported_files == right.unsupported_files
        && left.ignore_fingerprint == right.ignore_fingerprint
        && sources_match(&left.sources, &right.sources)
        && sources_match(&left.managed_sources, &right.managed_sources)
}

pub(super) fn read_manifest(path: &Path) -> Option<ScanManifest> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return None;
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_SCAN_MANIFEST_BYTES
    {
        return None;
    }
    let Ok((_, Some(bytes))) = read_source(path, MAX_SCAN_MANIFEST_BYTES) else {
        return None;
    };
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn reusable_scan<'a>(
    manifest: &'a ScanManifest,
    root: &str,
    database: Option<&DatabaseIdentity>,
    generation: u64,
    stored_options: &serde_json::Value,
    index_options: &serde_json::Value,
    ingest_fingerprint: &str,
) -> Option<&'a ScanProof> {
    (manifest.version == SCAN_MANIFEST_VERSION
        && manifest.root == root
        && Some(&manifest.database) == database
        && manifest.generation == generation
        && &manifest.stored_options == stored_options
        && &manifest.index_options == index_options
        && manifest.extractor_revision == EXTRACTOR_REVISION
        && manifest.language_revision == languages::revision()
        && manifest.ingest_fingerprint == ingest_fingerprint)
        .then_some(&manifest.scan)
}

pub(super) fn write_manifest(path: &Path, manifest: &ScanManifest) -> Result<()> {
    let bytes = serde_json::to_vec(manifest)?;
    ensure!(
        bytes.len() as u64 <= MAX_SCAN_MANIFEST_BYTES,
        "scan manifest exceeds size limit"
    );
    let parent = path.parent().context("scan manifest has no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .map_err(|error| error.error)
        .context("cannot publish scan manifest")?;
    Ok(())
}
