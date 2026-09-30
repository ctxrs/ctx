use super::*;

/// Select an explicit path or the nearest ancestor's existing `.graf/index.db`.
/// This only discovers a path: it never creates, opens, or migrates a database.
pub fn discover_database(explicit: Option<&Path>) -> Result<PathBuf> {
    optional_database(explicit)?.ok_or_else(|| MissingIndex.into())
}

pub fn optional_database(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(db) = explicit {
        return Ok(Some(db.to_path_buf()));
    }
    let cwd = std::env::current_dir()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join(".graf/index.db");
        if candidate.try_exists()? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub(crate) fn database(cli: &GraphArgs) -> Result<PathBuf> {
    if let Some(db) = &cli.db {
        return Ok(db.clone());
    }
    match &cli.command {
        Command::Index { path, .. } => Ok(path.join(".graf/index.db")),
        Command::Add { project, .. } => Ok(project.join(".graf/index.db")),
        Command::Import { .. } => Ok(std::env::current_dir()?.join(".graf/index.db")),
        _ => discover_database(None),
    }
}

#[derive(Debug)]
pub(crate) struct MissingIndex;
impl std::fmt::Display for MissingIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no .graf/index.db found in this directory or its ancestors; run ctx graph index or pass --db")
    }
}
impl std::error::Error for MissingIndex {}
