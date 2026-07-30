use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogSelection {
    ExactFile { inventory_parent: bool },
    Directory,
}

impl CatalogSelection {
    pub(super) fn tag(self) -> u8 {
        match self {
            Self::ExactFile {
                inventory_parent: false,
            } => 1,
            Self::ExactFile {
                inventory_parent: true,
            } => 2,
            Self::Directory => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteKind {
    File,
    Directory,
}

impl RouteKind {
    pub(super) fn tag(self) -> u8 {
        match self {
            Self::File => 1,
            Self::Directory => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct CatalogRoute {
    pub(super) relative_path: PathBuf,
    pub(super) display_path: PathBuf,
    pub(super) kind: RouteKind,
    pub(super) authority_fingerprint: [u8; 32],
    pub(super) frozen: Option<CodeBuddyFrozenFile>,
}

impl CatalogRoute {
    pub(super) fn observed_file(&self) -> Option<CodeBuddyObservedFile> {
        Some(CodeBuddyObservedFile {
            relative_path: self.relative_path.clone(),
            display_path: self.display_path.clone(),
            frozen: self.frozen.clone()?,
            authority_fingerprint: self.authority_fingerprint,
        })
    }
}

pub(super) fn catalog_routes(
    root: &ProviderSourceRoot,
    selected_relative_path: &Path,
    selection: CatalogSelection,
) -> Result<Vec<CatalogRoute>> {
    let mut state = DiscoveryState {
        routes: Vec::new(),
        entries: 0,
    };
    match selection {
        CatalogSelection::Directory
        | CatalogSelection::ExactFile {
            inventory_parent: true,
        } => discover_directory(root, root.directory()?, 0, &mut state)?,
        CatalogSelection::ExactFile {
            inventory_parent: false,
        } => {
            let directory = root.directory()?;
            observe_directory(root, &directory, &mut state)?;
            directory.revalidate()?;
            state.entries = 1;
            admit_file(
                root,
                selected_relative_path.to_path_buf(),
                root.open_file(selected_relative_path)?,
                &mut state,
            )?;
        }
    }
    root.revalidate()?;
    state
        .routes
        .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(state.routes)
}

struct DiscoveryState {
    routes: Vec<CatalogRoute>,
    entries: usize,
}

fn discover_directory(
    root: &ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    depth: usize,
    state: &mut DiscoveryState,
) -> Result<()> {
    if depth > CATALOG_MAX_DEPTH {
        return Err(invalid(
            &root.named_path().join(directory.relative_path()),
            "CodeBuddy source tree exceeds its depth bound",
        ));
    }
    observe_directory(root, &directory, state)?;
    let remaining = CATALOG_MAX_ENTRIES.saturating_sub(state.entries);
    let names = directory.entries(remaining.saturating_add(1))?;
    state.entries = state.entries.saturating_add(names.len());
    if state.entries > CATALOG_MAX_ENTRIES {
        return Err(invalid(
            &root.named_path().join(directory.relative_path()),
            "CodeBuddy source tree exceeds its entry bound",
        ));
    }
    for name in names {
        let relative_path = directory.relative_path().join(&name);
        validate_path(&root.named_path().join(&relative_path))?;
        match directory.open_child(&name)? {
            OpenedProviderSourcePath::Directory(child) => {
                discover_directory(root, child, depth.saturating_add(1), state)?;
            }
            OpenedProviderSourcePath::File(file) => {
                admit_file(root, relative_path, file, state)?;
            }
        }
    }
    directory.revalidate()
}

fn observe_directory(
    root: &ProviderSourceRoot,
    directory: &ProviderSourceDirectory,
    state: &mut DiscoveryState,
) -> Result<()> {
    state.routes.push(CatalogRoute {
        relative_path: directory.relative_path().to_path_buf(),
        display_path: root.named_path().join(directory.relative_path()),
        kind: RouteKind::Directory,
        authority_fingerprint: directory.authority_fingerprint(),
        frozen: None,
    });
    Ok(())
}

fn admit_file(
    root: &ProviderSourceRoot,
    relative_path: PathBuf,
    file: OpenedProviderSourceFile,
    state: &mut DiscoveryState,
) -> Result<()> {
    let display_path = root.named_path().join(&relative_path);
    let frozen = CodeBuddyFrozenFile::from_metadata(file.metadata())?;
    state.routes.push(CatalogRoute {
        relative_path,
        display_path,
        kind: RouteKind::File,
        authority_fingerprint: file.authority_fingerprint(),
        frozen: Some(frozen),
    });
    Ok(())
}
