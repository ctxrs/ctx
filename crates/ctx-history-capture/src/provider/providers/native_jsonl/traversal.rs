use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::CaptureProvider;
use tempfile::TempDir;

use crate::common::io::{
    open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
    ProviderSourceDirectory,
};
use crate::{CaptureError, Result, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES};

const NATIVE_JSONL_MAX_DIRECTORY_DEPTH: usize = 128;
const NATIVE_JSONL_DIRECTORY_RUN_ENTRIES: usize = 64;
const NATIVE_JSONL_DIRECTORY_MERGE_FAN_IN: usize = 16;

#[derive(Debug, Clone)]
pub(super) struct NativeJsonlSourceFile {
    path: PathBuf,
    opened: Arc<OpenedProviderSourceFile>,
}

impl NativeJsonlSourceFile {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) fn visit_jsonl_tree_files(
    provider: CaptureProvider,
    root: &Path,
    visit: &mut dyn FnMut(NativeJsonlSourceFile) -> Result<()>,
) -> Result<usize> {
    visit_jsonl_tree_files_isolating_selected(provider, root, visit, &mut |_path, error| Err(error))
}

/// Visits a tree while containing admission/read failures for selected child
/// files. Root and directory-enumeration failures remain fatal because no
/// independent source boundary has been established for them.
pub(super) fn visit_jsonl_tree_files_isolating_selected(
    provider: CaptureProvider,
    root: &Path,
    visit: &mut dyn FnMut(NativeJsonlSourceFile) -> Result<()>,
    selected_file_error: &mut dyn FnMut(&Path, CaptureError) -> Result<()>,
) -> Result<usize> {
    match open_provider_source_path(root) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            if !super::dialect::native_jsonl_file_is_selected(provider, root, false) {
                file.revalidate()?;
                return Ok(0);
            }
            let file = NativeJsonlSourceFile {
                path: root.to_path_buf(),
                opened: Arc::new(file),
            };
            let result = visit(file.clone());
            match result {
                Ok(()) => {
                    file.opened.revalidate()?;
                    Ok(1)
                }
                Err(error) => {
                    selected_file_error(root, error)?;
                    Ok(0)
                }
            }
        }
        Ok(OpenedProviderSourcePath::Directory(directory)) => {
            let authority = directory.authority_root();
            let visited = visit_jsonl_tree_files_at_depth(
                provider,
                root,
                directory,
                visit,
                selected_file_error,
                0,
            )?;
            authority.revalidate()?;
            Ok(visited)
        }
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            Err(error.into())
        }
        Err(error) => Err(error),
    }
}

fn visit_jsonl_tree_files_at_depth(
    provider: CaptureProvider,
    path: &Path,
    directory: ProviderSourceDirectory,
    visit: &mut dyn FnMut(NativeJsonlSourceFile) -> Result<()>,
    selected_file_error: &mut dyn FnMut(&Path, CaptureError) -> Result<()>,
    depth: usize,
) -> Result<usize> {
    if depth > NATIVE_JSONL_MAX_DIRECTORY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript directory nesting exceeds the supported limit",
        });
    }

    let mut visited = 0_usize;
    visit_native_jsonl_directory_names(&directory, &mut |name| {
        let child_path = path.join(&name);
        let selected = selected_file(provider, &directory, &child_path, &name);
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            Err(error) if selected => {
                selected_file_error(&child_path, error)?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if let OpenedProviderSourcePath::Directory(child_directory) = opened {
            visited = visited.saturating_add(visit_jsonl_tree_files_at_depth(
                provider,
                &child_path,
                child_directory,
                visit,
                selected_file_error,
                depth.saturating_add(1),
            )?);
        } else if selected {
            let OpenedProviderSourcePath::File(file) = opened else {
                return Err(CaptureError::SystemInvariant(
                    "native JSONL child classification is incomplete",
                ));
            };
            let file = NativeJsonlSourceFile {
                path: child_path.clone(),
                opened: Arc::new(file),
            };
            let outcome = visit(file.clone()).and_then(|()| file.opened.revalidate());
            match outcome {
                Ok(()) => visited = visited.saturating_add(1),
                Err(error) => selected_file_error(&child_path, error)?,
            }
        }
        Ok(())
    })?;
    directory.revalidate()?;
    Ok(visited)
}

fn selected_file(
    provider: CaptureProvider,
    directory: &ProviderSourceDirectory,
    path: &Path,
    name: &OsStr,
) -> bool {
    let full_transcript_is_regular = provider == CaptureProvider::Antigravity
        && name == OsStr::new("transcript.jsonl")
        && matches!(
            directory.open_child(OsStr::new("transcript_full.jsonl")),
            Ok(OpenedProviderSourcePath::File(_))
        );
    super::dialect::native_jsonl_file_is_selected(provider, path, full_transcript_is_regular)
}

fn visit_native_jsonl_directory_names(
    directory: &ProviderSourceDirectory,
    visit: &mut dyn FnMut(OsString) -> Result<()>,
) -> Result<()> {
    // `ReadDir` order is not portable. Keep small directories in memory, then
    // spill only native filenames into fixed runs so wide fanout stays bounded
    // without changing the byte-stable provider visitation order.
    traversal_stats(|stats| stats.directory_read_passes += 1);
    let mut names = Vec::with_capacity(NATIVE_JSONL_DIRECTORY_RUN_ENTRIES);
    let mut spill = None;
    for name in directory.entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES)? {
        traversal_stats(|stats| stats.directory_entries_read += 1);
        if names.len() == NATIVE_JSONL_DIRECTORY_RUN_ENTRIES {
            let runs = spill.get_or_insert(NativeJsonlDirectoryRuns::new()?);
            runs.write_initial_run(&mut names)?;
        }
        names.push(native_jsonl_filename_order_key(&name));
        traversal_stats(|stats| {
            stats.max_retained_names = stats.max_retained_names.max(names.len())
        });
    }

    let Some(mut spill) = spill else {
        names.sort_unstable();
        for name in names {
            traversal_stats(|stats| stats.final_names_read += 1);
            visit(native_jsonl_filename_from_order_key(name)?)?;
        }
        return Ok(());
    };
    if !names.is_empty() {
        spill.write_initial_run(&mut names)?;
    }
    let final_run = spill.merge_to_one()?;
    let mut reader = NativeJsonlRunReader::open(final_run)?;
    while let Some(name) = reader.next_name()? {
        traversal_stats(|stats| stats.final_names_read += 1);
        visit(native_jsonl_filename_from_order_key(name)?)?;
    }
    Ok(())
}

struct NativeJsonlDirectoryRuns {
    directory: TempDir,
    initial_runs: usize,
}

impl NativeJsonlDirectoryRuns {
    fn new() -> io::Result<Self> {
        Ok(Self {
            directory: tempfile::Builder::new()
                .prefix("ctx-native-jsonl-order-")
                .tempdir()?,
            initial_runs: 0,
        })
    }

    fn run_path(&self, pass: usize, run: usize) -> PathBuf {
        self.directory.path().join(format!("pass-{pass}-run-{run}"))
    }

    fn write_initial_run(&mut self, names: &mut Vec<Vec<u8>>) -> io::Result<()> {
        names.sort_unstable();
        let path = self.run_path(0, self.initial_runs);
        write_native_jsonl_run(&path, names.drain(..))?;
        self.initial_runs += 1;
        traversal_stats(|stats| stats.initial_runs += 1);
        Ok(())
    }

    fn merge_to_one(&mut self) -> io::Result<PathBuf> {
        let mut pass = 0_usize;
        let mut run_count = self.initial_runs;
        while run_count > 1 {
            let mut output_run = 0_usize;
            for first_input in (0..run_count).step_by(NATIVE_JSONL_DIRECTORY_MERGE_FAN_IN) {
                let input_count =
                    NATIVE_JSONL_DIRECTORY_MERGE_FAN_IN.min(run_count.saturating_sub(first_input));
                let output = self.run_path(pass.saturating_add(1), output_run);
                self.merge_run_group(pass, first_input, input_count, &output)?;
                output_run += 1;
            }
            pass = pass.saturating_add(1);
            run_count = output_run;
        }
        Ok(self.run_path(pass, 0))
    }

    fn merge_run_group(
        &self,
        pass: usize,
        first_input: usize,
        input_count: usize,
        output: &Path,
    ) -> io::Result<()> {
        let mut inputs = Vec::with_capacity(input_count);
        for run in first_input..first_input.saturating_add(input_count) {
            inputs.push(NativeJsonlRunReader::open(self.run_path(pass, run))?);
        }
        traversal_stats(|stats| {
            stats.max_merge_readers = stats.max_merge_readers.max(inputs.len());
        });
        let mut writer = BufWriter::new(File::create(output)?);
        loop {
            let mut selected: Option<usize> = None;
            for (index, input) in inputs.iter().enumerate() {
                let Some(name) = input.head() else {
                    continue;
                };
                if selected.is_none_or(|current| {
                    inputs[current]
                        .head()
                        .is_some_and(|current_name| name < current_name)
                }) {
                    selected = Some(index);
                }
            }
            let Some(selected) = selected else {
                break;
            };
            let name = inputs[selected]
                .take_head()?
                .ok_or_else(|| io::Error::other("selected native JSONL run is empty"))?;
            write_native_jsonl_run_name(&mut writer, &name)?;
            traversal_stats(|stats| stats.merge_names_read += 1);
        }
        writer.flush()?;
        drop(writer);
        drop(inputs);
        for run in first_input..first_input.saturating_add(input_count) {
            fs::remove_file(self.run_path(pass, run))?;
        }
        Ok(())
    }
}

struct NativeJsonlRunReader {
    reader: BufReader<File>,
    head: Option<Vec<u8>>,
}

impl NativeJsonlRunReader {
    fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut output = Self {
            reader: BufReader::new(File::open(path)?),
            head: None,
        };
        output.head = output.read_name()?;
        Ok(output)
    }

    fn head(&self) -> Option<&[u8]> {
        self.head.as_deref()
    }

    fn take_head(&mut self) -> io::Result<Option<Vec<u8>>> {
        let current = self.head.take();
        self.head = self.read_name()?;
        Ok(current)
    }

    fn next_name(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.take_head()
    }

    fn read_name(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut length = [0_u8; 4];
        let read = self.reader.read(&mut length)?;
        if read == 0 {
            return Ok(None);
        }
        self.reader.read_exact(&mut length[read..])?;
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| io::Error::other("native JSONL run name length does not fit usize"))?;
        let mut name = vec![0_u8; length];
        self.reader.read_exact(&mut name)?;
        Ok(Some(name))
    }
}

fn write_native_jsonl_run(path: &Path, names: impl IntoIterator<Item = Vec<u8>>) -> io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    for name in names {
        write_native_jsonl_run_name(&mut writer, &name)?;
    }
    writer.flush()
}

fn write_native_jsonl_run_name(writer: &mut impl Write, name: &[u8]) -> io::Result<()> {
    let length = u32::try_from(name.len())
        .map_err(|_| io::Error::other("provider transcript filename exceeds spill limit"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(name)
}

fn native_jsonl_filename_order_key(name: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        name.as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        name.encode_wide().flat_map(u16::to_le_bytes).collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.as_encoded_bytes().to_vec()
    }
}

fn native_jsonl_filename_from_order_key(name: Vec<u8>) -> io::Result<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        Ok(OsString::from_vec(name))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;

        let chunks = name.chunks_exact(2);
        if !chunks.remainder().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native JSONL run contains an invalid Windows filename",
            ));
        }
        Ok(OsString::from_wide(
            &chunks
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>(),
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        // SAFETY: the bytes came from `OsStr::as_encoded_bytes` in this process.
        Ok(unsafe { OsString::from_encoded_bytes_unchecked(name) })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct NativeJsonlTraversalStats {
    pub(super) directory_read_passes: usize,
    pub(super) directory_entries_read: usize,
    pub(super) max_retained_names: usize,
    pub(super) initial_runs: usize,
    pub(super) max_merge_readers: usize,
    pub(super) merge_names_read: usize,
    pub(super) final_names_read: usize,
}

#[cfg(test)]
std::thread_local! {
    static NATIVE_JSONL_TRAVERSAL_STATS: std::cell::Cell<Option<NativeJsonlTraversalStats>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn traversal_stats(update: impl FnOnce(&mut NativeJsonlTraversalStats)) {
    NATIVE_JSONL_TRAVERSAL_STATS.with(|stats| {
        if let Some(mut current) = stats.get() {
            update(&mut current);
            stats.set(Some(current));
        }
    });
}

#[cfg(not(test))]
fn traversal_stats(_update: impl FnOnce(&mut NativeJsonlTraversalStats)) {}

#[cfg(test)]
pub(super) fn count_native_jsonl_traversal_work<T>(
    operation: impl FnOnce() -> T,
) -> (T, NativeJsonlTraversalStats) {
    NATIVE_JSONL_TRAVERSAL_STATS
        .with(|stats| assert_eq!(stats.replace(Some(Default::default())), None));
    let output = operation();
    let stats = NATIVE_JSONL_TRAVERSAL_STATS.with(|stats| stats.replace(None).unwrap());
    (output, stats)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn selected_child_failure_is_isolated_from_healthy_siblings() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("tree");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a-healthy.jsonl"), b"{}\n").unwrap();
        symlink("a-healthy.jsonl", root.join("b-rejected.jsonl")).unwrap();

        let mut visited = Vec::new();
        let mut failures = Vec::new();
        let count = visit_jsonl_tree_files_isolating_selected(
            CaptureProvider::Pi,
            &root,
            &mut |source_file| {
                visited.push(source_file.path().file_name().unwrap().to_owned());
                Ok(())
            },
            &mut |path, _error| {
                failures.push(path.file_name().unwrap().to_owned());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(visited, [OsString::from("a-healthy.jsonl")]);
        assert_eq!(failures, [OsString::from("b-rejected.jsonl")]);
    }

    #[test]
    fn root_failure_is_never_downgraded_to_a_selected_file_failure() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir_all(&target).unwrap();
        let root = directory.path().join("root.jsonl");
        symlink(&target, &root).unwrap();
        let mut isolated = 0;

        let error = visit_jsonl_tree_files_isolating_selected(
            CaptureProvider::Pi,
            &root,
            &mut |_| Ok(()),
            &mut |_path, _error| {
                isolated += 1;
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { .. }
        ));
        assert_eq!(isolated, 0);
    }
}
