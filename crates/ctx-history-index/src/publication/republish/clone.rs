use std::path::Path;

use tantivy::Index;

use crate::{IndexError, Result};

use super::super::ActiveGenerationPointer;

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "freebsd"
)))]
compile_error!("predecessor republish clone is only qualified on ctx release targets");

mod candidate;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod exact_copy;
#[cfg(any(test, target_os = "windows", target_os = "freebsd"))]
mod portable;

use candidate::CandidateAuthentication;
pub(super) use candidate::RepublishCandidate;

pub(super) const MAX_REPUBLISH_CLONE_FILES: usize = 4_096;
pub(super) const MAX_REPUBLISH_CLONE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_REPUBLISH_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_MANAGED_METADATA_BYTES: u64 = 1024 * 1024;
const REPUBLISH_HEADROOM_RESERVE_BYTES: u64 = 16 * 1024 * 1024;
const MANAGED_FILE: &str = ".managed.json";
const TANTIVY_LOCK_FILES: [&str; 2] = [".tantivy-meta.lock", ".tantivy-writer.lock"];

pub(super) fn create_authenticated_republish_candidate(
    root: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    predecessor_index: &Index,
) -> Result<RepublishCandidate> {
    #[cfg(test)]
    if portable::forced_for_test() {
        let (candidate, guard) = portable::create_authenticated_republish_candidate(
            root,
            predecessor_pointer,
            predecessor_index,
        )?;
        return Ok(RepublishCandidate::new(
            candidate,
            CandidateAuthentication::Portable(guard),
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let (candidate, guard) = unix::create_authenticated_republish_candidate(
            root,
            predecessor_pointer,
            predecessor_index,
        )?;
        Ok(RepublishCandidate::new(
            candidate,
            CandidateAuthentication::DescriptorClone(guard),
        ))
    }
    #[cfg(any(target_os = "windows", target_os = "freebsd"))]
    {
        let (candidate, guard) = portable::create_authenticated_republish_candidate(
            root,
            predecessor_pointer,
            predecessor_index,
        )?;
        Ok(RepublishCandidate::new(
            candidate,
            CandidateAuthentication::Portable(guard),
        ))
    }
}

fn validate_single_component(path: &Path) -> Result<()> {
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "managed path escapes generation directory",
        ));
    }
    Ok(())
}

fn admit_clone_resource(
    files: &mut usize,
    bytes: &mut u64,
    next_bytes: u64,
    maximum_files: usize,
    maximum_bytes: u64,
) -> Result<()> {
    *files = files.checked_add(1).ok_or(IndexError::CountOverflow)?;
    if *files > maximum_files {
        return Err(IndexError::CurrentRepublishFileLimit {
            actual: *files,
            maximum: maximum_files,
        });
    }
    *bytes = bytes
        .checked_add(next_bytes)
        .ok_or(IndexError::CountOverflow)?;
    if *bytes > maximum_bytes {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: *bytes,
            maximum: maximum_bytes,
        });
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    mod guard;

    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::{CStr, CString, OsString},
        fs::{self, File},
        io::{self, Read, Seek, SeekFrom, Write},
        os::{
            fd::{AsRawFd, FromRawFd, RawFd},
            unix::{ffi::OsStringExt, fs::MetadataExt},
        },
        path::{Path, PathBuf},
    };

    use tantivy::Index;
    use uuid::Uuid;

    use crate::{
        analyzer::register_body_analyzer, durable_directory::DurableMmapDirectory,
        physical_integrity_digest, IndexError, Result,
    };

    use super::super::super::{
        generation::CandidateGeneration, lexical_index_settings, ActiveGenerationPointer,
        INDEX_GENERATIONS_DIRECTORY,
    };
    use super::exact_copy::copy_exact_authenticated_file;
    use super::{
        admit_clone_resource, validate_single_component, MANAGED_FILE, MAX_MANAGED_METADATA_BYTES,
        MAX_REPUBLISH_CLONE_BYTES, MAX_REPUBLISH_CLONE_FILES, MAX_REPUBLISH_DIRECTORY_ENTRIES,
        REPUBLISH_HEADROOM_RESERVE_BYTES, TANTIVY_LOCK_FILES,
    };
    pub(in crate::publication::republish) use guard::CandidateGuard;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FileIdentity {
        device: u64,
        inode: u64,
        bytes: u64,
        mode: u32,
    }

    impl FileIdentity {
        fn from_metadata(metadata: &fs::Metadata) -> Self {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                bytes: metadata.len(),
                mode: metadata.mode(),
            }
        }

        fn from_stat(stat: &libc::stat) -> Self {
            Self {
                device: u64::try_from(stat.st_dev).unwrap_or(u64::MAX),
                inode: stat.st_ino,
                bytes: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
                mode: u32::from(stat.st_mode),
            }
        }

        fn is_regular(self) -> bool {
            self.mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFREG)
        }

        fn is_directory(self) -> bool {
            self.mode & u32::from(libc::S_IFMT) == u32::from(libc::S_IFDIR)
        }

        fn is_same_object(self, other: Self) -> bool {
            self.device == other.device
                && self.inode == other.inode
                && (self.mode & u32::from(libc::S_IFMT))
                    == (other.mode & u32::from(libc::S_IFMT))
        }
    }

    #[derive(Debug, Clone)]
    struct PlannedFile {
        path: PathBuf,
        identity: FileIdentity,
        copy_required: bool,
    }

    struct ClonePlan {
        files: Vec<PlannedFile>,
        logical_bytes: u64,
        required_headroom: u64,
    }

    struct BoundDirectory {
        file: File,
        identity: FileIdentity,
    }

    impl BoundDirectory {
        fn open_path(path: &Path) -> Result<Self> {
            let file = open_path_nofollow(path, libc::O_RDONLY | libc::O_DIRECTORY)
                .map_err(source_topology_open_error)?;
            Self::from_file(file)
        }

        fn open_at(parent: &File, name: &Path) -> Result<Self> {
            let file =
                open_at_nofollow(parent.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)
                    .map_err(source_topology_open_error)?;
            Self::from_file(file)
        }

        fn from_file(file: File) -> Result<Self> {
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            if !identity.is_directory() {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "generation path is not a directory",
                ));
            }
            Ok(Self { file, identity })
        }
    }

    pub(super) fn create_authenticated_republish_candidate(
        root: &Path,
        predecessor_pointer: &ActiveGenerationPointer,
        predecessor_index: &Index,
    ) -> Result<(CandidateGeneration, CandidateGuard)> {
        let base = predecessor_pointer.active();
        let root_path = root.to_path_buf();
        let root_directory = BoundDirectory::open_path(root)?;
        validate_path_binding(root, root_directory.identity)?;
        let generations_name = PathBuf::from(INDEX_GENERATIONS_DIRECTORY);
        let generations_path = root.join(INDEX_GENERATIONS_DIRECTORY);
        let generations = BoundDirectory::open_at(&root_directory.file, &generations_name)?;
        validate_child_binding(
            &root_directory.file,
            &generations_name,
            generations.identity,
        )?;
        validate_path_binding(&generations_path, generations.identity)?;
        let source_name = Path::new(base.directory());
        validate_single_component(source_name)?;
        let source = BoundDirectory::open_at(&generations.file, source_name)?;
        validate_child_binding(&generations.file, source_name, source.identity)?;

        let plan = authenticated_clone_plan(&source, predecessor_index)?;
        let available = available_bytes(&generations.file)?;
        record_plan_metrics(&plan, available);
        if available < plan.required_headroom {
            return Err(IndexError::CurrentRepublishInsufficientHeadroom {
                available,
                required: plan.required_headroom,
            });
        }

        let directory_name = format!("generation-{}", Uuid::now_v7().simple());
        let destination_name = PathBuf::from(&directory_name);
        create_directory_at(&generations.file, &destination_name)?;
        let destination_path = generations_path.join(&directory_name);
        let destination = BoundDirectory::open_at(&generations.file, &destination_name)?;
        validate_child_binding(&generations.file, &destination_name, destination.identity)?;
        let guard = CandidateGuard {
            root_path,
            root: root_directory,
            generations_name,
            generations_path,
            generations,
            destination_name,
            destination,
        };
        let clone_result = (|| {
            clone_files(
                &guard.generations,
                source_name,
                &source,
                &guard.destination,
                &plan,
            )?;
            guard.generations.file.sync_all()?;
            validate_child_binding(&guard.generations.file, source_name, source.identity)?;
            guard.validate_binding()?;

            let directory = DurableMmapDirectory::open(&destination_path)
                .map_err(tantivy::TantivyError::from)?;
            let index = Index::open(directory)?;
            if index.settings() != &lexical_index_settings() {
                return Err(IndexError::IndexSettingsMismatch(
                    crate::LEXICAL_SCHEMA_VERSION,
                ));
            }
            let cloned_digest =
                physical_integrity_digest(&index, &destination_path, Some(predecessor_pointer))?;
            if cloned_digest != base.physical_integrity_digest() {
                return Err(IndexError::ChecksumMismatch);
            }
            guard.validate_binding()?;
            register_body_analyzer(&index);
            Ok(CandidateGeneration {
                directory_name: directory_name.clone(),
                index,
            })
        })();
        match clone_result {
            Ok(candidate) => Ok((candidate, guard)),
            Err(error) => {
                guard.discard();
                Err(error)
            }
        }
    }

    fn authenticated_clone_plan(source: &BoundDirectory, index: &Index) -> Result<ClonePlan> {
        let mut active = super::super::super::verification::active_index_files(index)?;
        active.insert(PathBuf::from("meta.json"));
        for path in &active {
            validate_single_component(path)?;
        }

        let mut seen_active = BTreeSet::new();
        let mut managed_seen = false;
        let mut planned = BTreeMap::new();
        let mut total_files = 0_usize;
        let mut total_bytes = 0_u64;
        for name in directory_entries(&source.file, MAX_REPUBLISH_DIRECTORY_ENTRIES)? {
            let name_text = name
                .to_str()
                .ok_or(IndexError::CurrentRepublishSourceTopology(
                    "non-UTF-8 directory entry",
                ))?;
            let relative = PathBuf::from(&name);
            validate_single_component(&relative)?;
            let file = open_regular_file_at(&source.file, &relative)?;
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            validate_file_binding(&source.file, &relative, identity)?;
            if active.contains(&relative) {
                seen_active.insert(relative.clone());
                admit_clone_resource(
                    &mut total_files,
                    &mut total_bytes,
                    identity.bytes,
                    MAX_REPUBLISH_CLONE_FILES,
                    MAX_REPUBLISH_CLONE_BYTES,
                )?;
                planned.insert(
                    relative.clone(),
                    PlannedFile {
                        copy_required: relative == Path::new("meta.json"),
                        path: relative,
                        identity,
                    },
                );
            } else if name_text == MANAGED_FILE {
                if identity.bytes > MAX_MANAGED_METADATA_BYTES {
                    return Err(IndexError::CurrentRepublishByteLimit {
                        actual: identity.bytes,
                        maximum: MAX_MANAGED_METADATA_BYTES,
                    });
                }
                managed_seen = true;
                admit_clone_resource(
                    &mut total_files,
                    &mut total_bytes,
                    identity.bytes,
                    MAX_REPUBLISH_CLONE_FILES,
                    MAX_REPUBLISH_CLONE_BYTES,
                )?;
                planned.insert(
                    relative.clone(),
                    PlannedFile {
                        path: relative,
                        identity,
                        copy_required: true,
                    },
                );
            } else if TANTIVY_LOCK_FILES.contains(&name_text) && identity.bytes == 0 {
                continue;
            } else {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "unexpected directory entry",
                ));
            }
        }
        if seen_active != active || !managed_seen {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "active or managed file missing",
            ));
        }

        let managed = planned.get(Path::new(MANAGED_FILE)).ok_or(
            IndexError::CurrentRepublishSourceTopology("managed file missing"),
        )?;
        let managed_bytes = read_bound_file(source, managed, MAX_MANAGED_METADATA_BYTES)?;
        let managed_paths: Vec<PathBuf> = serde_json::from_slice(&managed_bytes)
            .map_err(|_| IndexError::CurrentRepublishSourceTopology("invalid managed metadata"))?;
        for path in &managed_paths {
            validate_single_component(path)?;
        }
        let managed_set = managed_paths.iter().cloned().collect::<BTreeSet<_>>();
        if managed_set.len() != managed_paths.len() || managed_set != active {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "managed metadata does not match active files",
            ));
        }

        let required_headroom = total_bytes
            .checked_add(REPUBLISH_HEADROOM_RESERVE_BYTES)
            .ok_or(IndexError::CountOverflow)?;
        Ok(ClonePlan {
            files: planned.into_values().collect(),
            logical_bytes: total_bytes,
            required_headroom,
        })
    }

    fn read_bound_file(
        directory: &BoundDirectory,
        planned: &PlannedFile,
        maximum: u64,
    ) -> Result<Vec<u8>> {
        let mut file = open_regular_file_at(&directory.file, &planned.path)?;
        let before = FileIdentity::from_metadata(&file.metadata()?);
        if before != planned.identity {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file changed after authentication",
            ));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != planned.identity.bytes {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file size changed while reading",
            ));
        }
        validate_file_binding(&directory.file, &planned.path, planned.identity)?;
        Ok(bytes)
    }

    fn clone_files(
        generations: &BoundDirectory,
        source_name: &Path,
        source: &BoundDirectory,
        destination: &BoundDirectory,
        plan: &ClonePlan,
    ) -> Result<()> {
        let mut actual_copied_bytes = 0_u64;
        let mut linked_files = 0_usize;
        let mut copied_files = 0_usize;
        for planned in &plan.files {
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::BeforeFile, &planned.path)?;
            validate_child_binding(&generations.file, source_name, source.identity)?;
            let mut source_file = open_regular_file_at(&source.file, &planned.path)?;
            let before = FileIdentity::from_metadata(&source_file.metadata()?);
            if before != planned.identity {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "source file changed after authentication",
                ));
            }
            validate_file_binding(&source.file, &planned.path, before)?;

            let force_copy = planned.copy_required || force_copy_fallback();
            if !force_copy {
                clone_checkpoint(CloneStage::BeforeHardlink, &planned.path)?;
            }
            let linked = !force_copy
                && match hard_link_at(&source.file, &planned.path, &destination.file) {
                    Ok(()) => true,
                    Err(error) if hardlink_copy_fallback_error(&error) => false,
                    Err(error) => return Err(error.into()),
                };
            if linked {
                let linked_file = open_regular_file_at(&destination.file, &planned.path)?;
                let linked_identity = FileIdentity::from_metadata(&linked_file.metadata()?);
                if linked_identity != before {
                    return Err(IndexError::CurrentRepublishSourceTopology(
                        "hardlink target identity does not match authenticated source",
                    ));
                }
                linked_files = linked_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
            } else {
                clone_checkpoint(CloneStage::BeforeCopy, &planned.path)?;
                source_file.seek(SeekFrom::Start(0))?;
                let remaining_allowance = plan
                    .logical_bytes
                    .checked_sub(actual_copied_bytes)
                    .ok_or(IndexError::CurrentRepublishByteLimit {
                        actual: actual_copied_bytes,
                        maximum: plan.logical_bytes,
                    })?;
                let mut destination_file =
                    create_regular_file_at(&destination.file, &planned.path)?;
                let copied = copy_exact_authenticated_file(
                    &mut source_file,
                    &mut destination_file,
                    before.bytes,
                    remaining_allowance,
                )?;
                destination_file.flush()?;
                let destination_identity =
                    FileIdentity::from_metadata(&destination_file.metadata()?);
                if copied != before.bytes || destination_identity.bytes != before.bytes {
                    return Err(IndexError::CurrentRepublishSourceTopology(
                        "copy byte count does not match authenticated source",
                    ));
                }
                actual_copied_bytes = actual_copied_bytes
                    .checked_add(copied)
                    .ok_or(IndexError::CountOverflow)?;
                if actual_copied_bytes > MAX_REPUBLISH_CLONE_BYTES
                    || actual_copied_bytes > plan.logical_bytes
                {
                    return Err(IndexError::CurrentRepublishByteLimit {
                        actual: actual_copied_bytes,
                        maximum: plan.logical_bytes.min(MAX_REPUBLISH_CLONE_BYTES),
                    });
                }
                copied_files = copied_files
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
            }
            let after = FileIdentity::from_metadata(&source_file.metadata()?);
            if after != before {
                return Err(IndexError::CurrentRepublishSourceTopology(
                    "source file changed while cloning",
                ));
            }
            validate_file_binding(&source.file, &planned.path, after)?;
            validate_child_binding(&generations.file, source_name, source.identity)?;
            clone_checkpoint(CloneStage::AfterFile, &planned.path)?;
        }
        record_clone_metrics(actual_copied_bytes, linked_files, copied_files);
        Ok(())
    }

    fn discard_bound_directory(
        generations: &BoundDirectory,
        destination_name: &Path,
        destination: &BoundDirectory,
    ) -> Result<()> {
        for name in directory_entries(&destination.file, MAX_REPUBLISH_DIRECTORY_ENTRIES)? {
            let relative = Path::new(&name);
            validate_single_component(relative)?;
            let file = open_regular_file_at(&destination.file, relative)?;
            let identity = FileIdentity::from_metadata(&file.metadata()?);
            validate_file_binding(&destination.file, relative, identity)?;
            unlink_at(&destination.file, relative, 0)?;
        }
        validate_child_binding(&generations.file, destination_name, destination.identity)?;
        unlink_at(&generations.file, destination_name, libc::AT_REMOVEDIR)
    }

    fn unlink_at(parent: &File, path: &Path, flags: libc::c_int) -> Result<()> {
        let path = path_cstring(path)?;
        // SAFETY: the parent descriptor and NUL-terminated relative path stay
        // live for the call. Callers retain and revalidate the opened target.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), path.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    fn open_regular_file_at(directory: &File, path: &Path) -> Result<File> {
        let file = open_at_nofollow(directory.as_raw_fd(), path, libc::O_RDONLY)
            .map_err(source_topology_open_error)?;
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        if !identity.is_regular() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "non-regular directory entry",
            ));
        }
        Ok(file)
    }

    fn create_regular_file_at(directory: &File, path: &Path) -> io::Result<File> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated, the directory descriptor remains
        // open, and successful ownership is transferred into `File` exactly once.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        file_from_fd(fd)
    }

    fn open_path_nofollow(path: &Path, flags: libc::c_int) -> io::Result<File> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated and successful descriptor ownership
        // is transferred into `File` exactly once.
        let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
        file_from_fd(fd)
    }

    fn open_at_nofollow(directory: RawFd, path: &Path, flags: libc::c_int) -> io::Result<File> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated, `directory` is borrowed for the
        // call, and successful descriptor ownership transfers exactly once.
        let fd = unsafe {
            libc::openat(
                directory,
                path.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        file_from_fd(fd)
    }

    fn file_from_fd(fd: libc::c_int) -> io::Result<File> {
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: a nonnegative `open`/`openat` result is a newly owned fd.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn path_cstring(path: &Path) -> io::Result<CString> {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL")
        })
    }

    fn create_directory_at(parent: &File, path: &Path) -> Result<()> {
        let path = path_cstring(path)?;
        // SAFETY: `path` is NUL-terminated and `parent` remains open.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), path.as_ptr(), 0o700) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    fn hard_link_at(source: &File, path: &Path, destination: &File) -> io::Result<()> {
        let path = path_cstring(path)?;
        // SAFETY: both descriptors and both NUL-terminated path pointers stay
        // valid for the duration of `linkat`.
        if unsafe {
            libc::linkat(
                source.as_raw_fd(),
                path.as_ptr(),
                destination.as_raw_fd(),
                path.as_ptr(),
                0,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn hardlink_copy_fallback_error(error: &io::Error) -> bool {
        error.raw_os_error().is_some_and(|code| {
            [
                libc::EXDEV,
                libc::EPERM,
                libc::EACCES,
                libc::EMLINK,
                libc::EOPNOTSUPP,
            ]
            .contains(&code)
        })
    }

    fn validate_path_binding(path: &Path, expected: FileIdentity) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(source_topology_open_error)?;
        let actual = FileIdentity::from_metadata(&metadata);
        if !actual.is_directory() || !actual.is_same_object(expected) {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "generation parent path changed during republish",
            ));
        }
        Ok(())
    }

    fn validate_child_binding(parent: &File, path: &Path, expected: FileIdentity) -> Result<()> {
        let actual = stat_at(parent, path)?;
        if !actual.is_directory() || !actual.is_same_object(expected) {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "active generation directory changed during republish",
            ));
        }
        Ok(())
    }

    fn validate_file_binding(parent: &File, path: &Path, expected: FileIdentity) -> Result<()> {
        let actual = stat_at(parent, path)?;
        if !actual.is_regular() || actual != expected {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file changed during republish",
            ));
        }
        Ok(())
    }

    fn stat_at(parent: &File, path: &Path) -> Result<FileIdentity> {
        let path = path_cstring(path)?;
        // SAFETY: zeroed `stat` is initialized by a successful `fstatat`; the
        // descriptor and path remain valid for the call.
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                path.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            Ok(FileIdentity::from_stat(&stat))
        } else {
            Err(source_topology_open_error(io::Error::last_os_error()))
        }
    }

    fn directory_entries(directory: &File, maximum: usize) -> Result<Vec<OsString>> {
        // SAFETY: `dup` creates an independently owned descriptor.
        let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: `fdopendir` consumes `duplicate` on success.
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            // SAFETY: `fdopendir` did not consume the descriptor on failure.
            unsafe { libc::close(duplicate) };
            return Err(io::Error::last_os_error().into());
        }
        struct Stream(*mut libc::DIR);
        impl Drop for Stream {
            fn drop(&mut self) {
                // SAFETY: the stream is uniquely owned and closed once.
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Stream(stream);
        let mut entries = Vec::new();
        loop {
            set_errno(0);
            // SAFETY: `stream` remains open and `readdir`'s pointer is consumed
            // before the next call.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error.into());
                }
                break;
            }
            // SAFETY: POSIX guarantees NUL termination of `d_name`.
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let actual = entries
                .len()
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            if actual > maximum {
                return Err(IndexError::CurrentRepublishFileLimit { actual, maximum });
            }
            entries.push(OsString::from_vec(bytes.to_vec()));
        }
        entries.sort();
        Ok(entries)
    }

    #[cfg(target_os = "linux")]
    fn set_errno(value: libc::c_int) {
        // SAFETY: the returned pointer addresses this thread's errno.
        unsafe { *libc::__errno_location() = value };
    }

    #[cfg(target_os = "macos")]
    fn set_errno(value: libc::c_int) {
        // SAFETY: the returned pointer addresses this thread's errno.
        unsafe { *libc::__error() = value };
    }

    fn available_bytes(directory: &File) -> Result<u64> {
        #[cfg(test)]
        if let Some(available) = TEST_CLONE_OPTIONS.with(|options| options.borrow().available_bytes)
        {
            return Ok(available);
        }
        // SAFETY: zeroed `statvfs` is initialized by successful `fstatvfs`.
        let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
        if unsafe { libc::fstatvfs(directory.as_raw_fd(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }

    fn source_topology_open_error(error: io::Error) -> IndexError {
        if error
            .raw_os_error()
            .is_some_and(|code| [libc::ELOOP, libc::ENOTDIR].contains(&code))
        {
            IndexError::CurrentRepublishSourceTopology(
                "symlinked or non-directory republish source",
            )
        } else {
            IndexError::Io(error)
        }
    }

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum CloneStage {
        BeforeFile,
        BeforeHardlink,
        BeforeCopy,
        AfterFile,
        BeforeCleanup,
    }

    #[cfg(not(test))]
    #[derive(Debug, Clone, Copy)]
    enum CloneStage {
        BeforeFile,
        BeforeHardlink,
        BeforeCopy,
        AfterFile,
        BeforeCleanup,
    }

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, Default)]
    pub(crate) struct CloneTestOptions {
        pub(crate) force_copy: bool,
        pub(crate) available_bytes: Option<u64>,
    }

    #[cfg(test)]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct CloneMetrics {
        pub(crate) planned_files: usize,
        pub(crate) logical_bytes: u64,
        pub(crate) required_headroom: u64,
        pub(crate) available_bytes: u64,
        pub(crate) copied_bytes: u64,
        pub(crate) linked_files: usize,
        pub(crate) copied_files: usize,
    }

    #[cfg(test)]
    type CloneTestHook = Box<dyn for<'a> FnMut(CloneStage, &'a Path) -> Result<()>>;

    #[cfg(test)]
    thread_local! {
        static TEST_CLONE_OPTIONS: std::cell::RefCell<CloneTestOptions> = const {
            std::cell::RefCell::new(CloneTestOptions { force_copy: false, available_bytes: None })
        };
        static TEST_CLONE_HOOK: std::cell::RefCell<Option<CloneTestHook>> =
            std::cell::RefCell::new(None);
        static TEST_CLONE_METRICS: std::cell::Cell<CloneMetrics> = const {
            std::cell::Cell::new(CloneMetrics {
                planned_files: 0,
                logical_bytes: 0,
                required_headroom: 0,
                available_bytes: 0,
                copied_bytes: 0,
                linked_files: 0,
                copied_files: 0,
            })
        };
    }

    #[cfg(test)]
    pub(crate) struct CloneTestHookGuard {
        previous_options: CloneTestOptions,
        previous_hook: Option<CloneTestHook>,
        previous_metrics: CloneMetrics,
    }

    #[cfg(test)]
    impl CloneTestHookGuard {
        pub(crate) fn set<F>(options: CloneTestOptions, hook: F) -> Self
        where
            F: for<'a> FnMut(CloneStage, &'a Path) -> Result<()> + 'static,
        {
            let previous_options = TEST_CLONE_OPTIONS.with(|slot| slot.replace(options));
            let previous_hook = TEST_CLONE_HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
            let previous_metrics =
                TEST_CLONE_METRICS.with(|slot| slot.replace(CloneMetrics::default()));
            Self {
                previous_options,
                previous_hook,
                previous_metrics,
            }
        }

        pub(crate) fn metrics(&self) -> CloneMetrics {
            TEST_CLONE_METRICS.with(std::cell::Cell::get)
        }
    }

    #[cfg(test)]
    impl Drop for CloneTestHookGuard {
        fn drop(&mut self) {
            TEST_CLONE_OPTIONS.with(|slot| slot.replace(self.previous_options));
            TEST_CLONE_HOOK.with(|slot| slot.replace(self.previous_hook.take()));
            TEST_CLONE_METRICS.with(|slot| slot.set(self.previous_metrics));
        }
    }

    #[cfg(test)]
    fn force_copy_fallback() -> bool {
        TEST_CLONE_OPTIONS.with(|options| options.borrow().force_copy)
    }

    #[cfg(not(test))]
    fn force_copy_fallback() -> bool {
        false
    }

    #[cfg(test)]
    fn clone_checkpoint(stage: CloneStage, path: &Path) -> Result<()> {
        TEST_CLONE_HOOK.with(|hook| match hook.borrow_mut().as_mut() {
            Some(hook) => hook(stage, path),
            None => Ok(()),
        })
    }

    #[cfg(not(test))]
    fn clone_checkpoint(_stage: CloneStage, _path: &Path) -> Result<()> {
        Ok(())
    }

    #[cfg(test)]
    fn record_plan_metrics(plan: &ClonePlan, available: u64) {
        TEST_CLONE_METRICS.with(|metrics| {
            metrics.set(CloneMetrics {
                planned_files: plan.files.len(),
                logical_bytes: plan.logical_bytes,
                required_headroom: plan.required_headroom,
                available_bytes: available,
                ..metrics.get()
            });
        });
    }

    #[cfg(not(test))]
    fn record_plan_metrics(_plan: &ClonePlan, _available: u64) {}

    #[cfg(test)]
    fn record_clone_metrics(copied_bytes: u64, linked_files: usize, copied_files: usize) {
        TEST_CLONE_METRICS.with(|metrics| {
            metrics.set(CloneMetrics {
                copied_bytes,
                linked_files,
                copied_files,
                ..metrics.get()
            });
        });
    }

    #[cfg(not(test))]
    fn record_clone_metrics(_copied_bytes: u64, _linked_files: usize, _copied_files: usize) {}
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use unix::{CloneMetrics, CloneStage, CloneTestHookGuard, CloneTestOptions};

#[cfg(test)]
pub(crate) use portable::{
    PortableCloneMetrics, PortableCloneStage, PortableCloneTestGuard, PortableCloneTestOptions,
};

#[cfg(test)]
mod tests;
