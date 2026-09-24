use super::*;

pub(super) fn unchanged_report(
    stats: &Stats,
    options: &IndexOptions,
    started: std::time::Instant,
) -> Result<IndexReport> {
    let semantic_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_budget.as_ref())
        .map(|budget| budget.usage())
        .transpose()?;
    let provider_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_usage.as_ref())
        .map(|recorder| recorder.snapshot())
        .transpose()?;
    let timings = options.timing.then(|| {
        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        IndexTimings {
            detect_ms: total_ms,
            extract_ms: 0.0,
            commit_ms: 0.0,
            total_ms,
            capture_ms: None,
        }
    });
    Ok(IndexReport {
        schema_version: stats.schema_version,
        generation: stats.generation,
        parsed_files: 0,
        unchanged_files: stats.coverage.supported_files,
        deleted_files: 0,
        nodes: stats.nodes,
        edges: stats.edges,
        diagnostics: stats.diagnostics.clone(),
        semantic_usage,
        provider_usage,
        timings,
    })
}

pub(super) fn is_code(path: &str) -> bool {
    path.ends_with(".py") || languages::supports(path)
}

pub(super) fn is_robot(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("robot") || extension.eq_ignore_ascii_case("resource")
        })
}

pub(super) fn generic_json(path: &str) -> bool {
    path.ends_with(".json") || path.ends_with(".jsonc")
}

pub(super) fn content_probe(path: &str) -> bool {
    generic_json(path) || Path::new(path).extension().is_none()
}

pub(super) fn python_source_root<'a>(path: &str, options: &'a IndexOptions) -> Option<&'a str> {
    options
        .python_source_roots
        .iter()
        .filter(|root| {
            root.is_empty()
                || path
                    .strip_prefix(root.as_str())
                    .is_some_and(|tail| tail.starts_with('/'))
        })
        .max_by_key(|root| root.len())
        .map(String::as_str)
}

pub(super) fn python_inventory(
    files: &[(PathBuf, String)],
    options: &IndexOptions,
    old: &HashMap<String, String>,
    project: &ProjectContext,
    ingest_fingerprint: &str,
) -> Result<PythonInventory> {
    let possible_python = |path: &str| {
        !path.starts_with(".graf/sources/")
            && (path.ends_with(".py") || Path::new(path).extension().is_none())
    };
    let paths: BTreeSet<_> = files
        .iter()
        .map(|(_, path)| path.as_str())
        .filter(|p| possible_python(p))
        .collect();
    let old_paths: BTreeSet<_> = old
        .keys()
        .map(String::as_str)
        .filter(|p| possible_python(p))
        .collect();
    // Reuse only the stamp, never old parsed facts. If any possible Python file,
    // option, inventory member or extractor revision differs, rebuild context.
    // This still hashes source bytes, so equal timestamps cannot conceal edits.
    if !options.force && !paths.is_empty() && paths == old_paths {
        let mut inventory = PythonInventory::default();
        let mut unchanged = true;
        for (path, relative) in files.iter().filter(|(_, path)| possible_python(path)) {
            let Some(token) = old
                .get(relative)
                .and_then(|stamp| previous_python_context(stamp))
            else {
                unchanged = false;
                break;
            };
            let (hash, _) = read_source(path, MAX_SOURCE_BYTES as u64)?;
            if old.get(relative)
                != Some(&stamp(
                    relative,
                    &hash,
                    project,
                    ingest_fingerprint,
                    options,
                    token,
                ))
            {
                unchanged = false;
                break;
            }
            inventory.source_hashes.insert(relative.clone(), hash);
            inventory
                .context_tokens
                .insert(relative.clone(), token.into());
        }
        if unchanged {
            return Ok(inventory);
        }
    }
    let mut inventory = PythonInventory::default();
    let mut facts = Vec::new();
    for (path, relative) in files {
        if !possible_python(relative) {
            continue;
        }
        let (hash, bytes) = read_source(path, MAX_SOURCE_BYTES as u64)?;
        // Even an invalid source or non-Python shebang contributed to discovery.
        inventory
            .source_hashes
            .insert(relative.clone(), hash.clone());
        let Some(bytes) = bytes else { continue };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !relative.ends_with(".py")
            && languages::scripted::shebang_language(source) != Some("python")
        {
            continue;
        }
        facts.push(parse_python_with_source_root(
            relative,
            source,
            &hash,
            python_source_root(relative, options),
        )?);
    }
    let context = PythonContext::from_facts(&facts);
    let modules: BTreeSet<_> = facts
        .iter()
        .flat_map(|facts| &facts.nodes)
        .filter_map(|node| node.binding_key.as_deref())
        .filter(|key| key.starts_with("module:"))
        .map(str::to_owned)
        .collect();
    for mut facts in facts {
        context.apply(&mut facts);
        for reference in &mut facts.references {
            // An unavailable submodule is not an eligible fallback. Keeping the
            // terminal key lets Store rebind ordinary definition additions and
            // deletions; an actual submodule addition changes this file's token.
            if reference.relation == "imports"
                && let [_, fallback] = reference.candidate_keys.as_slice()
                && fallback.starts_with("module:")
                && !modules.contains(fallback)
            {
                reference.candidate_keys.pop();
            }
        }
        inventory
            .context_tokens
            .insert(facts.path.clone(), python_binding_token(&facts)?);
        inventory.facts.insert(facts.path.clone(), facts);
    }
    Ok(inventory)
}

pub(super) fn python_binding_token(facts: &FileFacts) -> Result<String> {
    // Hash the context outcome, not the corpus or target availability. Stable
    // candidate keys remain Store-owned even when their terminal targets vanish.
    let mut references: Vec<_> = facts
        .references
        .iter()
        .map(|reference| {
            (
                &reference.id,
                &reference.relation,
                &reference.candidate_keys,
            )
        })
        .collect();
    let mut bindings: Vec<_> = facts
        .nodes
        .iter()
        .map(|node| {
            (
                &node.id,
                &node.binding_key,
                &node.metadata["binding_aliases"],
            )
        })
        .collect();
    // Export references are emitted from a HashMap; enumeration is not identity.
    references.sort_by(|a, b| a.0.cmp(b.0));
    bindings.sort_by(|a, b| a.0.cmp(b.0));
    let outcome = serde_json::to_vec(&(references, bindings))?;
    Ok(format!(
        "{PYTHON_TERMINAL_CONTEXT}-{}",
        blake3::hash(&outcome)
    ))
}

pub(super) fn previous_python_context(stamp: &str) -> Option<&str> {
    let context = if let Some(rest) = stamp.strip_prefix("python-v") {
        let (_, rest) = rest.split_once(':')?;
        rest.split_once(':').map_or(rest, |(context, _)| context)
    } else if stamp.starts_with("languages-") {
        stamp.split(':').nth(2)?
    } else {
        return None;
    };
    (context == PYTHON_TERMINAL_CONTEXT
        || context.strip_prefix("terminal-v2-").is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
        }))
    .then_some(context)
}

pub(super) fn stamp(
    path: &str,
    hash: &str,
    context: &ProjectContext,
    ingest_fingerprint: &str,
    options: &IndexOptions,
    python_context: &str,
) -> String {
    let hash = if options.semantic_code {
        format!("deep:{ingest_fingerprint}:{hash}")
    } else {
        hash.to_owned()
    };
    let hash = if let Some(root) = python_source_root(path, options)
        .filter(|_| path.ends_with(".py") || Path::new(path).extension().is_none())
    {
        format!(
            "source-root:{}:{hash}",
            blake3::hash(root.as_bytes()).to_hex()
        )
    } else {
        hash
    };
    let hash = if let Some(python) = options.robot_python.as_ref().filter(|_| is_robot(path)) {
        format!(
            "robot-official-v1:{}:{hash}",
            blake3::hash(python.as_os_str().as_encoded_bytes()).to_hex()
        )
    } else {
        hash
    };
    if path.ends_with(".py") {
        format!("python-v{EXTRACTOR_REVISION}:{python_context}:{hash}")
    } else if languages::supports(path) || content_probe(path) {
        format!(
            "languages-{}:{}:{}:{hash}",
            languages::revision(),
            context.fingerprint(path),
            if Path::new(path).extension().is_none() {
                python_context
            } else {
                ""
            },
        )
    } else {
        format!("ingest-v1:{ingest_fingerprint}:{hash}")
    }
}

pub(super) fn discover(root: &Path, db: &Path, options: &IndexOptions) -> Result<Discovery> {
    let db = db.to_path_buf();
    let context_files = discover_context_files(root, &db, options);
    let manifest = scan_manifest_path(&db);
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect();
    let include_generated = options.include_generated;
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .parents(false)
        .git_ignore(!options.no_gitignore)
        .git_exclude(!options.no_gitignore)
        .add_custom_ignore_filename(".graphifyignore")
        .add_custom_ignore_filename(".grafignore")
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|t| t.is_symlink()) {
                return false;
            }
            if entry.path() == db
                || entry.path() == manifest
                || sidecars.iter().any(|p| p == entry.path())
            {
                return false;
            }
            !entry.file_type().is_some_and(|t| {
                t.is_dir()
                    && match entry.file_name().to_str() {
                        Some(".git" | ".graf") => true,
                        Some(
                            ".venv" | "venv" | "env" | "__pycache__" | "node_modules"
                            | "site-packages" | "target" | "build" | "dist" | ".tox" | ".nox"
                            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache",
                        ) => !include_generated,
                        _ => false,
                    }
            })
        });
    let mut coverage = Coverage::default();
    let mut files = vec![];
    let mut ignore_files = Some(vec![]);
    for entry in walker.build() {
        let entry = entry.context("cannot walk index root")?;
        if let Some(error) = entry.error() {
            bail!("cannot apply ignore rules: {error}");
        }
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            check_ignore_files(entry.path(), options.no_gitignore, &mut ignore_files)?;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("source path must be UTF-8")?
            .to_owned();
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        let recognized_content = if content_probe(&relative) && !languages::supports(&relative) {
            let (_, content) = read_source(entry.path(), MAX_SOURCE_BYTES as u64)?;
            content
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .is_some_and(|s| languages::recognizes(&relative, s))
        } else {
            false
        };
        if !is_code(&relative)
            && !recognized_content
            && (options.code_only || !ingest::supports(Path::new(&relative)))
        {
            coverage.unsupported_files += 1;
            continue;
        }
        coverage.supported_files += 1;
        files.push((entry.path().to_path_buf(), relative));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(Discovery {
        files,
        coverage,
        ignore_files,
        context_files,
    })
}

pub(super) fn discover_context_files(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
) -> Option<Vec<(PathBuf, String)>> {
    let db = db.to_path_buf();
    let manifest = scan_manifest_path(&db);
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect();
    let include_generated = options.include_generated;
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                return false;
            }
            if entry.path() == db
                || entry.path() == manifest
                || sidecars.iter().any(|path| path == entry.path())
            {
                return false;
            }
            !entry.file_type().is_some_and(|kind| {
                kind.is_dir()
                    && match entry.file_name().to_str() {
                        Some(".git" | ".graf") => true,
                        Some(
                            ".venv" | "venv" | "env" | "__pycache__" | "node_modules"
                            | "site-packages" | "target" | "build" | "dist" | ".tox" | ".nox"
                            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache",
                        ) => !include_generated,
                        _ => false,
                    }
            })
        });
    let mut files = vec![];
    for entry in walker.build() {
        let entry = entry.ok()?;
        if entry.error().is_some() {
            return None;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            return None;
        }
        let relative = entry.path().strip_prefix(root).ok()?.to_str()?.to_owned();
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        if is_code(&relative) || content_probe(&relative) {
            files.push((entry.path().to_path_buf(), relative));
        }
    }
    files.sort_by(|left, right| left.1.cmp(&right.1));
    Some(files)
}

pub(super) fn check_ignore_files(
    directory: &Path,
    no_gitignore: bool,
    manifest_files: &mut Option<Vec<(Vec<u8>, String)>>,
) -> Result<()> {
    // WalkBuilder suppresses ignore-file I/O errors, including invalid UTF-8.
    // Validate each visited directory so partial rules cannot publish a graph
    // that silently includes excluded files. Pruned subtrees need no validation.
    for name in [
        ".ignore",
        ".gitignore",
        ".git/info/exclude",
        ".grafignore",
        ".graphifyignore",
    ] {
        if no_gitignore && matches!(name, ".gitignore" | ".git/info/exclude") {
            continue;
        }
        let path = directory.join(name);
        match std::fs::metadata(&path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file(),
                "ignore rules must be a regular file: {}",
                path.display()
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("cannot inspect ignore rules"),
        }
        if let Some(error) = ignore::gitignore::GitignoreBuilder::new(directory).add(&path) {
            bail!("cannot apply ignore rules: {error}");
        }
        if manifest_files.is_some() {
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                *manifest_files = None;
                continue;
            }
            let (digest, content) = read_source(&path, MAX_IGNORE_BYTES)?;
            if content.is_none() {
                *manifest_files = None;
                continue;
            }
            manifest_files
                .as_mut()
                .unwrap()
                .push((path.as_os_str().as_encoded_bytes().to_vec(), digest));
        }
    }
    Ok(())
}

pub(super) fn diagnostic(path: &str, hash: &str, message: &str) -> FileFacts {
    let mut facts = empty_facts(path, hash);
    facts.diagnostics.push(Diagnostic {
        file: path.into(),
        line: None,
        message: message.into(),
    });
    facts
}
