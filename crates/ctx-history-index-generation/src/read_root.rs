use std::{
    collections::HashMap,
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[cfg(unix)]
#[path = "read_root/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "read_root/windows.rs"]
mod platform;

use crate::{GenerationError, Result};
pub(crate) use platform::OpenedDirectory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DirectoryIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume: u64, file: u64 },
}

/// An owner-private lexical generation root reached without following links.
///
/// The capability retains the opened directory (and, on Windows, its opened
/// parent route) so all read-lease and snapshot access can stay bound to the
/// object that was validated rather than reopening an attacker-replaceable
/// pathname.
#[derive(Debug, Clone)]
pub struct GenerationReadRoot {
    path: PathBuf,
    identity: DirectoryIdentity,
    opened: Arc<OpenedDirectory>,
}

impl GenerationReadRoot {
    /// Opens and validates one lexical generation root component by component.
    pub fn open_index_root(root: impl AsRef<Path>) -> Result<Self> {
        let root = normalized_absolute(root.as_ref())?;
        let opened =
            OpenedDirectory::open_absolute(&root).map_err(map_unavailable_generation_root)?;
        opened
            .verify_lease_root()
            .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
        Self::register(root, opened)
    }

    /// Opens `data_root/search/lexical` relative to retained, verified parent
    /// handles and rejects links/reparse points at every pathname component.
    pub fn open_data_root(data_root: impl AsRef<Path>) -> Result<Self> {
        let data_root = normalized_absolute(data_root.as_ref())?;
        let data =
            OpenedDirectory::open_absolute(&data_root).map_err(map_unavailable_generation_root)?;
        data.verify_private()
            .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
        #[cfg(any(test, feature = "test-support"))]
        run_traversal_hook(GenerationRootTraversalStage::DataRootOpened);

        let search = data
            .open_directory(Path::new("search"))
            .map_err(map_unavailable_generation_root)?;
        search
            .verify_private()
            .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
        #[cfg(any(test, feature = "test-support"))]
        run_traversal_hook(GenerationRootTraversalStage::SearchOpened);

        let lexical = search
            .open_directory(Path::new("lexical"))
            .map_err(map_unavailable_generation_root)?;
        lexical
            .verify_private()
            .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
        #[cfg(any(test, feature = "test-support"))]
        run_traversal_hook(GenerationRootTraversalStage::LexicalOpened);

        Self::register(data_root.join("search").join("lexical"), lexical)
    }

    fn register(original_path: PathBuf, opened: OpenedDirectory) -> Result<Self> {
        let opened = Arc::new(opened);
        let path = opened
            .stable_path(&original_path)
            .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
        let registry = READ_ROOTS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut registry = registry
            .lock()
            .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
        registry.retain(|_, roots| {
            roots.retain(|root| root.strong_count() != 0);
            !roots.is_empty()
        });
        registry
            .entry(path.clone())
            .or_default()
            .push(Arc::downgrade(&opened));
        drop(registry);
        let identity = opened.registry_identity();
        Ok(Self {
            path,
            identity,
            opened,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn opened(&self) -> &Arc<OpenedDirectory> {
        &self.opened
    }

    pub(crate) fn identity(&self) -> DirectoryIdentity {
        self.identity
    }

    pub(crate) fn open_file(&self, relative: &Path) -> io::Result<std::fs::File> {
        self.opened.open_file(relative)
    }
}

fn normalized_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(GenerationError::InvalidGenerationRetentionLease);
    }
    Ok(ctx_history_platform::platform_security::normalize_platform_namespace_alias(path))
}

fn map_unavailable_generation_root(error: io::Error) -> GenerationError {
    match error.kind() {
        io::ErrorKind::NotFound => GenerationError::MissingActiveGenerationPointer,
        _ => GenerationError::InvalidGenerationRetentionLease,
    }
}

static READ_ROOTS: OnceLock<Mutex<HashMap<PathBuf, Vec<Weak<OpenedDirectory>>>>> = OnceLock::new();
type RetainedAuthorityKey = (PathBuf, String);
static RETAINED_READ_AUTHORITIES: OnceLock<
    Mutex<HashMap<RetainedAuthorityKey, Vec<Weak<RetainedReadAuthority>>>>,
> = OnceLock::new();

thread_local! {
    static ACTIVE_READ_ROOTS: std::cell::RefCell<Vec<DirectoryIdentity>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Debug)]
pub(crate) struct RetainedReadAuthority {
    generation_id: String,
}

pub(crate) fn register_retained_read_authority(
    root: &GenerationReadRoot,
    generation_id: &str,
) -> Result<Arc<RetainedReadAuthority>> {
    let authority = Arc::new(RetainedReadAuthority {
        generation_id: generation_id.to_owned(),
    });
    let registry = RETAINED_READ_AUTHORITIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| GenerationError::InvalidGenerationRetentionLease)?;
    registry.retain(|_, authorities| {
        authorities.retain(|authority| authority.strong_count() != 0);
        !authorities.is_empty()
    });
    registry
        .entry((root.path().to_path_buf(), generation_id.to_owned()))
        .or_default()
        .push(Arc::downgrade(&authority));
    Ok(authority)
}

pub(crate) fn has_retained_read_authority(root: &Path, generation_id: &str) -> bool {
    let Some(registry) = RETAINED_READ_AUTHORITIES.get() else {
        return false;
    };
    let Ok(mut registry) = registry.lock() else {
        return false;
    };
    registry.retain(|_, authorities| {
        authorities.retain(|authority| authority.strong_count() != 0);
        !authorities.is_empty()
    });
    registry
        .get(&(root.to_path_buf(), generation_id.to_owned()))
        .is_some_and(|authorities| {
            authorities.iter().any(|authority| {
                authority
                    .upgrade()
                    .is_some_and(|authority| authority.generation_id == generation_id)
            })
        })
}

pub(crate) fn registered_read_directory(path: &Path) -> io::Result<Option<Arc<OpenedDirectory>>> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "anchored generation path is not normalized",
        ));
    }
    let Some(active_identity) = ACTIVE_READ_ROOTS.with(|roots| roots.borrow().last().copied())
    else {
        return Ok(None);
    };
    let Some(registry) = READ_ROOTS.get() else {
        return Ok(None);
    };
    let mut registry = registry
        .lock()
        .map_err(|_| io::Error::other("generation read-root registry is poisoned"))?;
    registry.retain(|_, roots| {
        roots.retain(|root| root.strong_count() != 0);
        !roots.is_empty()
    });
    let selected = registry
        .iter()
        .filter_map(|(root_path, roots)| {
            let relative = path.strip_prefix(root_path).ok()?;
            let root = roots
                .iter()
                .rev()
                .find_map(Weak::upgrade)
                .filter(|root| root.registry_identity() == active_identity)?;
            Some((root_path.components().count(), relative, root))
        })
        .max_by_key(|(component_count, _, _)| *component_count);
    let Some((_, relative, root)) = selected else {
        return Ok(None);
    };
    if relative.as_os_str().is_empty() {
        return Ok(Some(root));
    }
    root.open_directory(relative).map(Arc::new).map(Some)
}

pub(crate) fn has_active_read_root() -> bool {
    ACTIVE_READ_ROOTS.with(|roots| !roots.borrow().is_empty())
}

pub(crate) fn read_registered_file(root: &Path, relative: &Path) -> io::Result<Option<Vec<u8>>> {
    let Some(root) = registered_read_directory(root)? else {
        return Ok(None);
    };
    let mut file = root.open_file(relative)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub(crate) fn registered_file_metadata(
    root: &Path,
    relative: &Path,
) -> io::Result<Option<std::fs::Metadata>> {
    let Some(root) = registered_read_directory(root)? else {
        return Ok(None);
    };
    root.open_file(relative)?.metadata().map(Some)
}

pub(crate) fn with_registered_read_root<T>(
    root: &GenerationReadRoot,
    access: impl FnOnce() -> T,
) -> T {
    struct ActiveRootGuard;

    impl Drop for ActiveRootGuard {
        fn drop(&mut self) {
            ACTIVE_READ_ROOTS.with(|roots| {
                roots.borrow_mut().pop();
            });
        }
    }

    ACTIVE_READ_ROOTS.with(|roots| roots.borrow_mut().push(root.identity()));
    let _guard = ActiveRootGuard;
    access()
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRootTraversalStage {
    DataRootOpened,
    SearchOpened,
    LexicalOpened,
}

#[cfg(any(test, feature = "test-support"))]
type GenerationRootTraversalHook = Box<dyn FnMut(GenerationRootTraversalStage)>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TRAVERSAL_HOOK: std::cell::RefCell<Option<GenerationRootTraversalHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub struct GenerationRootTraversalTestHookGuard;

#[cfg(any(test, feature = "test-support"))]
impl GenerationRootTraversalTestHookGuard {
    pub fn install(hook: impl FnMut(GenerationRootTraversalStage) + 'static) -> Self {
        TRAVERSAL_HOOK.with(|slot| {
            assert!(slot.replace(Some(Box::new(hook))).is_none());
        });
        Self
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for GenerationRootTraversalTestHookGuard {
    fn drop(&mut self) {
        TRAVERSAL_HOOK.with(|slot| {
            slot.replace(None);
        });
    }
}

#[cfg(any(test, feature = "test-support"))]
fn run_traversal_hook(stage: GenerationRootTraversalStage) {
    TRAVERSAL_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(stage);
        }
    });
}
