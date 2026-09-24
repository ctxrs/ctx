use super::*;

pub(crate) fn print(value: &impl Serialize, compact: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if compact {
        serde_json::to_writer(&mut out, value)?;
    } else {
        serde_json::to_writer_pretty(&mut out, value)?;
    }
    writeln!(out)?;
    Ok(())
}

pub(crate) fn local_database(db: Option<&Path>) -> Result<PathBuf> {
    optional_local_database(db)?
        .context("no .graf/index.db found; run ctx graph index or pass --db or --snapshot")
}

pub(crate) fn optional_local_database(db: Option<&Path>) -> Result<Option<PathBuf>> {
    crate::paths::optional_database(db)
}

pub(crate) fn optional_graph(
    snapshot: Option<&Path>,
    db: Option<&Path>,
) -> Result<Option<GraphSnapshot>> {
    ensure!(
        snapshot.is_none() || db.is_none(),
        "--snapshot conflicts with --db"
    );
    if let Some(path) = snapshot {
        return load_source(path, SourceKind::Snapshot).map(Some);
    }
    optional_local_database(db)?
        .map(|path| load_source(&path, SourceKind::Database))
        .transpose()
}

pub(crate) fn regular(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "expected a regular file, not a symlink: {}",
        path.display()
    );
    Ok(())
}

pub(crate) fn registered_source(name: &str, path: &Path, kind: SourceKind) -> Result<Registration> {
    ensure!(!name.trim().is_empty(), "project name must not be empty");
    let path = if matches!(kind, SourceKind::Database) && path.is_dir() {
        path.join(".graf/index.db")
    } else {
        path.to_owned()
    };
    regular(&path)?;
    Ok(Registration {
        name: name.into(),
        path: path.canonicalize()?,
        kind,
    })
}

pub(crate) fn load_source(path: &Path, kind: SourceKind) -> Result<GraphSnapshot> {
    regular(path)?;
    match kind {
        SourceKind::Database => Store::open_read_only(path)?.snapshot(),
        SourceKind::Snapshot => {
            let graph = snapshot::read(path)?;
            // The strict snapshot reader keeps its validated source header here.
            let header = &graph.metadata["graf_snapshot"];
            Ok(GraphSnapshot {
                schema_version: SCHEMA_VERSION,
                generation: header["generation"]
                    .as_u64()
                    .context("missing snapshot generation")?,
                kind: header["kind"]
                    .as_str()
                    .context("missing snapshot kind")?
                    .into(),
                root: header["root"].as_str().map(str::to_owned),
                metadata: header["metadata"].clone(),
                nodes: graph.nodes,
                edges: graph.edges,
            })
        }
    }
}

pub(crate) fn load(source: &SourceArgs, db: Option<&Path>) -> Result<(GraphSnapshot, PathBuf)> {
    ensure!(
        source.snapshot.is_none() || db.is_none(),
        "--snapshot conflicts with --db"
    );
    let (path, kind) = if let Some(path) = &source.snapshot {
        (path.clone(), SourceKind::Snapshot)
    } else {
        (local_database(db)?, SourceKind::Database)
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot find source {}", path.display()))?;
    Ok((load_source(&path, kind)?, path))
}

pub(crate) fn same_file(a: &Path, b: &Path) -> Result<bool> {
    if a.canonicalize()? == b.canonicalize()? {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (a.metadata()?, b.metadata()?);
        if (a.dev(), a.ino()) == (b.dev(), b.ino()) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn destination(path: &Path, sources: &[PathBuf]) -> Result<bool> {
    let exists = match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.is_file(),
                "output must be a regular file, not a symlink or directory"
            );
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    if exists {
        for source in sources {
            ensure!(
                !same_file(path, source)?,
                "output must not overwrite a source database or snapshot"
            );
        }
        let mut header = [0; 16];
        if File::open(path)?.read(&mut header)? == header.len() {
            ensure!(
                &header != b"SQLite format 3\0",
                "output must not overwrite a SQLite database"
            );
        }
    }
    protect_sidecars(path, sources)?;
    Ok(exists)
}

pub(crate) fn protect_sidecars(path: &Path, sources: &[PathBuf]) -> Result<()> {
    let full = std::path::absolute(path)?;
    let parent = full.parent().context("output has no parent")?;
    if parent.try_exists()? {
        let full = parent
            .canonicalize()?
            .join(full.file_name().context("output has no filename")?);
        for source in sources {
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut sidecar = source.as_os_str().to_os_string();
                sidecar.push(suffix);
                ensure!(
                    full != sidecar,
                    "output must not overwrite a source SQLite sidecar"
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn write_atomic(path: &Path, content: &[u8], sources: &[PathBuf]) -> Result<()> {
    let exists = destination(path, sources)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    if exists {
        let original = read_output(path)?;
        if original == content {
            return Ok(());
        }
        backup_output(parent, &original)?;
    }
    let stage = tempfile::tempdir_in(parent)?;
    crate::switch_files::protect(stage.path())?;
    let mut temp = tempfile::NamedTempFile::new_in(stage.path())?;
    crate::switch_files::protect(temp.path())?;
    if exists {
        crate::switch_files::preserve_permissions(temp.as_file(), path)?;
    }
    temp.write_all(content)?;
    temp.as_file().sync_all()?;
    ensure!(
        destination(path, sources)? == exists,
        "output appeared or disappeared while rendering; retry"
    );
    crate::switch_files::replace(temp, path, exists)
}

pub(crate) fn read_output(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 256 * 1024 * 1024,
        "existing output exceeds 256 MiB backup limit"
    );
    Ok(bytes)
}

pub(crate) fn backup_output(parent: &Path, original: &[u8]) -> Result<()> {
    let directory = parent.join(".graf/export-backups");
    for path in [parent.join(".graf"), directory.clone()] {
        match fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "export backup path must be a regular directory"
                );
            }
            Err(error) => return Err(error).context("cannot create export backup directory"),
        }
    }
    let path = directory.join(format!("{}.bak", blake3::hash(original).to_hex()));
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "export backup must be a regular file"
            );
            ensure!(
                read_output(&path)? == original,
                "existing export backup differs"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
            crate::switch_files::protect(staged.path())?;
            staged.write_all(original)?;
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&path)
                .map_err(|e| e.error)
                .context("cannot save previous export")?;
        }
        Err(error) => return Err(error.into()),
    }
    eprintln!("Saved previous output to {}", path.display());
    Ok(())
}
