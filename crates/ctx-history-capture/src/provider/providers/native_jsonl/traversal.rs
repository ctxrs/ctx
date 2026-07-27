use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::{CaptureError, Result};

const NATIVE_JSONL_MAX_DIRECTORY_DEPTH: usize = 128;
const NATIVE_JSONL_DIRECTORY_RUN_ENTRIES: usize = 64;
const NATIVE_JSONL_DIRECTORY_MERGE_FAN_IN: usize = 16;

pub(super) fn visit_jsonl_tree_files(
    root: &Path,
    is_selected: &dyn Fn(&Path) -> bool,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    visit_jsonl_tree_files_at_depth(root, is_selected, visit, 0)
}

fn visit_jsonl_tree_files_at_depth(
    root: &Path,
    is_selected: &dyn Fn(&Path) -> bool,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
    depth: usize,
) -> Result<usize> {
    if depth > NATIVE_JSONL_MAX_DIRECTORY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "provider transcript directory nesting exceeds the supported limit",
        });
    }
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        if is_selected(root) {
            ensure_regular_provider_transcript_file(root)?;
            visit(root)?;
            return Ok(1);
        }
        return Ok(0);
    }
    if !file_type.is_dir() {
        return Ok(0);
    }

    let mut visited = 0_usize;
    visit_native_jsonl_directory_names(root, &mut |name| {
        let path = root.join(name);
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            visited = visited.saturating_add(visit_jsonl_tree_files_at_depth(
                &path,
                is_selected,
                visit,
                depth.saturating_add(1),
            )?);
        } else if (file_type.is_file() || file_type.is_symlink()) && is_selected(&path) {
            ensure_regular_provider_transcript_file(&path)?;
            visit(&path)?;
            visited = visited.saturating_add(1);
        }
        Ok(())
    })?;
    Ok(visited)
}

fn visit_native_jsonl_directory_names(
    root: &Path,
    visit: &mut dyn FnMut(OsString) -> Result<()>,
) -> Result<()> {
    // `ReadDir` order is not portable. Keep small directories in memory, then
    // spill only native filenames into fixed runs so wide fanout stays bounded
    // without changing the byte-stable provider visitation order.
    traversal_stats(|stats| stats.directory_read_passes += 1);
    let mut names = Vec::with_capacity(NATIVE_JSONL_DIRECTORY_RUN_ENTRIES);
    let mut spill = None;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        traversal_stats(|stats| stats.directory_entries_read += 1);
        if names.len() == NATIVE_JSONL_DIRECTORY_RUN_ENTRIES {
            let runs = spill.get_or_insert(NativeJsonlDirectoryRuns::new()?);
            runs.write_initial_run(&mut names)?;
        }
        names.push(native_jsonl_filename_order_key(&entry.file_name()));
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
