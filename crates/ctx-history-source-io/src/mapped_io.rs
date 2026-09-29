use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{BufRead, Take},
    marker::PhantomData,
    path::Path,
    time::SystemTime,
};

use crate::{
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderJsonlInventory,
    ProviderJsonlInventoryLimits, ProviderJsonlLineRead, ProviderSourceDirectory,
    ProviderSourceRoot, SourceIoError,
};

#[macro_export]
macro_rules! define_mapped_source_io_compat {
    ($error:ty) => {
        pub(crate) type ProviderSourceRoot = $crate::MappedProviderSourceRoot<$error>;
        pub(crate) type ProviderSourceDirectory = $crate::MappedProviderSourceDirectory<$error>;
        pub(crate) type OpenedProviderSourceFile = $crate::MappedOpenedProviderSourceFile<$error>;
        pub(crate) type OpenedProviderSourcePath = $crate::MappedOpenedProviderSourcePath<$error>;

        pub(crate) fn open_provider_source_file(
            path: &std::path::Path,
        ) -> std::result::Result<OpenedProviderSourceFile, $error> {
            $crate::open_provider_source_file_mapped::<$error>(path)
        }

        pub(crate) fn open_provider_source_path(
            path: &std::path::Path,
        ) -> std::result::Result<OpenedProviderSourcePath, $error> {
            $crate::open_provider_source_path_mapped::<$error>(path)
        }

        pub(crate) fn inventory_provider_jsonl_paths(
            root: &std::path::Path,
            limits: $crate::ProviderJsonlInventoryLimits,
        ) -> std::result::Result<$crate::ProviderJsonlInventory, $error> {
            $crate::inventory_provider_jsonl_paths_mapped::<$error>(root, limits)
        }

        pub(crate) fn inventory_provider_regular_paths(
            root: &std::path::Path,
            limits: $crate::ProviderJsonlInventoryLimits,
        ) -> std::result::Result<$crate::ProviderJsonlInventory, $error> {
            $crate::inventory_provider_regular_paths_mapped::<$error>(root, limits)
        }

        #[cfg(test)]
        pub(crate) fn collect_jsonl_paths_bounded(
            root: &std::path::Path,
            paths: &mut Vec<std::path::PathBuf>,
            maximum: usize,
        ) -> std::result::Result<(), $error> {
            $crate::collect_jsonl_paths_bounded_mapped::<$error>(root, paths, maximum)
        }

        pub(crate) fn provider_regular_file_len(
            path: &std::path::Path,
        ) -> std::result::Result<u64, $error> {
            $crate::provider_regular_file_len_mapped::<$error>(path)
        }

        pub(crate) fn ensure_regular_provider_transcript_file(
            path: &std::path::Path,
        ) -> std::result::Result<(), $error> {
            $crate::ensure_regular_provider_transcript_file_mapped::<$error>(path)
        }

        pub(crate) fn ensure_provider_path_parents_are_not_symlinks(
            path: &std::path::Path,
        ) -> std::result::Result<(), $error> {
            $crate::ensure_provider_path_parents_are_not_symlinks_mapped::<$error>(path)
        }

        pub(crate) fn path_has_component(path: &std::path::Path, expected: &str) -> bool {
            $crate::path_has_component(path, expected)
        }

        pub(crate) fn provider_metadata_is_link_like(metadata: &std::fs::Metadata) -> bool {
            $crate::provider_metadata_is_link_like(metadata)
        }

        pub(crate) fn read_text_file_limited(
            path: &std::path::Path,
            maximum: usize,
            label: &str,
        ) -> std::result::Result<String, $error> {
            $crate::read_text_file_limited_mapped::<$error>(path, maximum, label)
        }

        pub(crate) fn read_provider_jsonl_line_or_skip_oversized(
            reader: &mut impl std::io::BufRead,
            buffer: &mut Vec<u8>,
        ) -> std::result::Result<$crate::ProviderJsonlLineRead, $error> {
            $crate::read_provider_jsonl_line_or_skip_oversized_mapped::<$error>(reader, buffer)
        }

        pub(crate) fn discard_provider_jsonl_line(
            reader: &mut impl std::io::BufRead,
        ) -> std::result::Result<usize, $error> {
            $crate::discard_provider_jsonl_line_mapped::<$error>(reader)
        }

        pub(crate) fn read_json_file_limited(
            path: &std::path::Path,
            maximum: usize,
            label: &str,
        ) -> std::result::Result<serde_json::Value, $error> {
            $crate::read_json_file_limited_mapped::<$error>(path, maximum, label)
        }
    };
}

#[macro_export]
macro_rules! define_mapped_ordinary_io_compat {
    ($error:ty) => {
        pub(crate) fn open_ordinary_file_without_following(
            path: &std::path::Path,
        ) -> std::result::Result<std::fs::File, $error> {
            $crate::open_ordinary_file_without_following_mapped::<$error>(path)
        }

        pub fn observe_ordinary_file(
            path: impl AsRef<std::path::Path>,
        ) -> std::result::Result<$crate::OrdinaryFileObservation, $error> {
            $crate::observe_ordinary_file_mapped::<$error>(path)
        }
    };
}

/// Zero-cost source authority adapter for callers with their own error type.
#[derive(Debug)]
pub struct MappedProviderSourceRoot<E>(ProviderSourceRoot, PhantomData<fn() -> E>);

impl<E> Clone for MappedProviderSourceRoot<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

pub fn inventory_provider_jsonl_paths_mapped<E>(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory, E>
where
    E: From<SourceIoError>,
{
    crate::inventory_provider_jsonl_paths(root, limits).map_err(Into::into)
}

pub fn inventory_provider_regular_paths_mapped<E>(
    root: &Path,
    limits: ProviderJsonlInventoryLimits,
) -> Result<ProviderJsonlInventory, E>
where
    E: From<SourceIoError>,
{
    crate::inventory_provider_regular_paths(root, limits).map_err(Into::into)
}

#[cfg(any(test, feature = "test-support"))]
pub fn collect_jsonl_paths_bounded_mapped<E>(
    root: &Path,
    paths: &mut Vec<std::path::PathBuf>,
    maximum: usize,
) -> Result<(), E>
where
    E: From<SourceIoError>,
{
    crate::collect_jsonl_paths_bounded(root, paths, maximum).map_err(Into::into)
}

pub fn provider_regular_file_len_mapped<E>(path: &Path) -> Result<u64, E>
where
    E: From<SourceIoError>,
{
    crate::provider_regular_file_len(path).map_err(Into::into)
}

pub fn ensure_regular_provider_transcript_file_mapped<E>(path: &Path) -> Result<(), E>
where
    E: From<SourceIoError>,
{
    crate::ensure_regular_provider_transcript_file(path).map_err(Into::into)
}

pub fn ensure_provider_path_parents_are_not_symlinks_mapped<E>(path: &Path) -> Result<(), E>
where
    E: From<SourceIoError>,
{
    crate::ensure_provider_path_parents_are_not_symlinks(path).map_err(Into::into)
}

pub fn read_text_file_limited_mapped<E>(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<String, E>
where
    E: From<SourceIoError>,
{
    crate::read_text_file_limited(path, maximum, label).map_err(Into::into)
}

pub fn read_provider_jsonl_line_or_skip_oversized_mapped<E>(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
) -> Result<ProviderJsonlLineRead, E>
where
    E: From<SourceIoError>,
{
    crate::read_provider_jsonl_line_or_skip_oversized(reader, buffer).map_err(Into::into)
}

pub fn discard_provider_jsonl_line_mapped<E>(reader: &mut impl BufRead) -> Result<usize, E>
where
    E: From<SourceIoError>,
{
    crate::discard_provider_jsonl_line(reader).map_err(Into::into)
}

pub fn read_json_file_limited_mapped<E>(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<serde_json::Value, E>
where
    E: From<SourceIoError>,
{
    crate::read_json_file_limited(path, maximum, label).map_err(Into::into)
}

pub fn open_ordinary_file_without_following_mapped<E>(path: &Path) -> Result<File, E>
where
    E: From<SourceIoError>,
{
    crate::open_ordinary_file_without_following(path).map_err(Into::into)
}

pub fn observe_ordinary_file_mapped<E>(
    path: impl AsRef<Path>,
) -> Result<crate::OrdinaryFileObservation, E>
where
    E: From<SourceIoError>,
{
    crate::observe_ordinary_file(path).map_err(Into::into)
}

pub fn open_provider_source_file_mapped<E>(
    path: &Path,
) -> Result<MappedOpenedProviderSourceFile<E>, E>
where
    E: From<SourceIoError>,
{
    MappedOpenedProviderSourceFile::open(path)
}

pub fn open_provider_source_path_mapped<E>(
    path: &Path,
) -> Result<MappedOpenedProviderSourcePath<E>, E>
where
    E: From<SourceIoError>,
{
    MappedOpenedProviderSourcePath::open(path)
}

#[derive(Debug)]
pub struct MappedProviderSourceDirectory<E>(ProviderSourceDirectory, PhantomData<fn() -> E>);

#[derive(Debug)]
pub struct MappedOpenedProviderSourceFile<E>(OpenedProviderSourceFile, PhantomData<fn() -> E>);

#[derive(Debug)]
pub enum MappedOpenedProviderSourcePath<E> {
    File(MappedOpenedProviderSourceFile<E>),
    Directory(MappedProviderSourceDirectory<E>),
}

impl<E> From<OpenedProviderSourcePath> for MappedOpenedProviderSourcePath<E> {
    fn from(opened: OpenedProviderSourcePath) -> Self {
        match opened {
            OpenedProviderSourcePath::File(file) => {
                Self::File(MappedOpenedProviderSourceFile(file, PhantomData))
            }
            OpenedProviderSourcePath::Directory(directory) => {
                Self::Directory(MappedProviderSourceDirectory(directory, PhantomData))
            }
        }
    }
}

impl<E> MappedOpenedProviderSourcePath<E>
where
    E: From<SourceIoError>,
{
    pub fn authority_fingerprint(&self) -> [u8; 32] {
        match self {
            Self::File(file) => file.authority_fingerprint(),
            Self::Directory(directory) => directory.authority_fingerprint(),
        }
    }
}

impl<E> MappedProviderSourceRoot<E>
where
    E: From<SourceIoError>,
{
    pub fn open(path: &Path) -> Result<Self, E> {
        ProviderSourceRoot::open(path)
            .map(|root| Self(root, PhantomData))
            .map_err(Into::into)
    }

    pub fn named_path(&self) -> &Path {
        self.0.named_path()
    }

    pub fn authority_fingerprint(&self) -> [u8; 32] {
        self.0.authority_fingerprint()
    }

    pub fn same_object_as(&self, other: &Self) -> bool {
        self.0.same_object_as(&other.0)
    }

    pub fn directory(&self) -> Result<MappedProviderSourceDirectory<E>, E> {
        self.0
            .directory()
            .map(|directory| MappedProviderSourceDirectory(directory, PhantomData))
            .map_err(Into::into)
    }

    pub fn open_path(&self, path: &Path) -> Result<MappedOpenedProviderSourcePath<E>, E> {
        self.0.open_path(path).map(Into::into).map_err(Into::into)
    }

    pub fn open_file(&self, path: &Path) -> Result<MappedOpenedProviderSourceFile<E>, E> {
        self.0
            .open_file(path)
            .map(|file| MappedOpenedProviderSourceFile(file, PhantomData))
            .map_err(Into::into)
    }

    pub fn open_directory(&self, path: &Path) -> Result<MappedProviderSourceDirectory<E>, E> {
        self.0
            .open_directory(path)
            .map(|directory| MappedProviderSourceDirectory(directory, PhantomData))
            .map_err(Into::into)
    }

    pub fn revalidate(&self) -> Result<(), E> {
        self.0.revalidate().map_err(Into::into)
    }

    pub fn revalidate_same_object(&self) -> Result<(), E> {
        self.0.revalidate_same_object().map_err(Into::into)
    }
}

impl<E> MappedProviderSourceDirectory<E>
where
    E: From<SourceIoError>,
{
    pub fn authority_root(&self) -> MappedProviderSourceRoot<E> {
        MappedProviderSourceRoot(self.0.authority_root(), PhantomData)
    }

    pub fn relative_path(&self) -> &Path {
        self.0.relative_path()
    }

    pub fn authority_fingerprint(&self) -> [u8; 32] {
        self.0.authority_fingerprint()
    }

    pub fn try_clone_authority_handle(&self) -> std::io::Result<File> {
        self.0.try_clone_authority_handle()
    }

    pub fn entries(&self, maximum: usize) -> Result<Vec<OsString>, E> {
        self.0.entries(maximum).map_err(Into::into)
    }

    pub fn entries_snapshot(&self, maximum: usize) -> Result<Vec<OsString>, E> {
        self.0.entries_snapshot(maximum).map_err(Into::into)
    }

    pub fn visit_entries(
        &self,
        maximum: usize,
        visit: impl FnMut(OsString) -> Result<(), E>,
    ) -> Result<(), E> {
        self.0.visit_entries(maximum, visit)
    }

    pub fn open_child(&self, name: &OsStr) -> Result<MappedOpenedProviderSourcePath<E>, E> {
        self.0.open_child(name).map(Into::into).map_err(Into::into)
    }

    pub fn revalidate_same_object(&self) -> Result<(), E> {
        self.0.revalidate_same_object().map_err(Into::into)
    }

    pub fn revalidate(&self) -> Result<(), E> {
        self.0.revalidate().map_err(Into::into)
    }
}

impl<E> MappedOpenedProviderSourceFile<E>
where
    E: From<SourceIoError>,
{
    pub fn open(path: &Path) -> Result<Self, E> {
        crate::open_provider_source_file(path)
            .map(|file| Self(file, PhantomData))
            .map_err(Into::into)
    }

    pub fn len(&self) -> u64 {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn modified(&self) -> std::io::Result<SystemTime> {
        self.0.modified()
    }

    pub fn metadata(&self) -> &Metadata {
        self.0.metadata()
    }

    pub fn authority_fingerprint(&self) -> [u8; 32] {
        self.0.authority_fingerprint()
    }

    pub fn ordinary_file_token(&self) -> [u8; 32] {
        self.0.ordinary_file_token()
    }

    pub fn current_ordinary_file_token(&self) -> Result<[u8; 32], E> {
        self.0.current_ordinary_file_token().map_err(Into::into)
    }

    pub fn file(&self) -> &File {
        self.0.file()
    }

    pub fn reopen_same_object(&self) -> Result<File, E> {
        self.0.reopen_same_object().map_err(Into::into)
    }

    pub fn bounded_reader(&self, maximum: u64) -> Result<Take<File>, E> {
        self.0.bounded_reader(maximum).map_err(Into::into)
    }

    pub fn read_all_bounded(&self, maximum: usize) -> Result<Vec<u8>, E> {
        self.0.read_all_bounded(maximum).map_err(Into::into)
    }

    pub fn read_exact_range(
        &self,
        offset: u64,
        length: usize,
        maximum: usize,
    ) -> Result<Vec<u8>, E> {
        self.0
            .read_exact_range(offset, length, maximum)
            .map_err(Into::into)
    }

    pub fn read_exact_range_allow_append(
        &self,
        offset: u64,
        length: usize,
        maximum: usize,
    ) -> Result<Vec<u8>, E> {
        self.0
            .read_exact_range_allow_append(offset, length, maximum)
            .map_err(Into::into)
    }

    pub fn revalidate_leaf(&self) -> Result<(), E> {
        self.0.revalidate_leaf().map_err(Into::into)
    }

    pub fn revalidate_leaf_with_same_object_root(&self) -> Result<(), E> {
        self.0
            .revalidate_leaf_with_same_object_root()
            .map_err(Into::into)
    }

    pub fn revalidate_same_object_leaf(&self) -> Result<(), E> {
        self.0.revalidate_same_object_leaf().map_err(Into::into)
    }

    pub fn revalidate_same_object(&self) -> Result<(), E> {
        self.0.revalidate_same_object().map_err(Into::into)
    }

    pub fn revalidate(&self) -> Result<(), E> {
        self.0.revalidate().map_err(Into::into)
    }
}

impl<E> MappedOpenedProviderSourcePath<E>
where
    E: From<SourceIoError>,
{
    pub fn open(path: &Path) -> Result<Self, E> {
        crate::open_provider_source_path(path)
            .map(Into::into)
            .map_err(Into::into)
    }
}
