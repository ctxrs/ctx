use std::{
    cmp::Ordering,
    ffi::OsString,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use tempfile::TempDir;

use crate::{
    is_non_regular_source_rejection, is_symlink_source_rejection, open_provider_source_path,
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory, SourceIoError,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
};

const BOUNDED_TREE_MAX_DIRECTORY_DEPTH: usize = 128;
const BOUNDED_TREE_DIRECTORY_RUN_ENTRIES: usize = 64;
const BOUNDED_TREE_DIRECTORY_MERGE_FAN_IN: usize = 16;

/// One lexical file candidate presented before its child is opened.
///
/// The retained parent capability is present for tree children and absent
/// when the requested root is itself a file. Selection policy may use the
/// parent to inspect a sibling without returning to an ancestor pathname.
#[derive(Debug, Clone, Copy)]
pub struct BoundedTreeFileCandidate<'a> {
    path: &'a Path,
    parent: Option<&'a ProviderSourceDirectory>,
}

impl<'a> BoundedTreeFileCandidate<'a> {
    pub fn path(self) -> &'a Path {
        self.path
    }

    pub fn parent(self) -> Option<&'a ProviderSourceDirectory> {
        self.parent
    }
}

#[derive(Debug, Clone)]
pub struct BoundedTreeSourceFile {
    path: PathBuf,
    opened: Arc<OpenedProviderSourceFile>,
}

impl BoundedTreeSourceFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn visit_bounded_tree_files<E, Selected>(
    root: &Path,
    selected: &mut Selected,
    visit: &mut dyn FnMut(BoundedTreeSourceFile) -> std::result::Result<(), E>,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
    Selected: FnMut(BoundedTreeFileCandidate<'_>) -> bool,
{
    visit_bounded_tree_files_isolating_selected(root, selected, visit, &mut |_path, error| {
        Err(error)
    })
}

/// Visits a tree while containing admission/read failures for selected files.
///
/// Unselected symlink, reparse, and non-regular children are never followed and
/// are skipped. Other unselected open failures stay fatal because they may hide
/// a genuine directory subtree.
///
/// Root-open and directory-enumeration failures stay fatal. Neither names a
/// selected child to contain, and an enumeration that stops early would drop
/// files silently.
pub fn visit_bounded_tree_files_isolating_selected<E, Selected>(
    root: &Path,
    selected: &mut Selected,
    visit: &mut dyn FnMut(BoundedTreeSourceFile) -> std::result::Result<(), E>,
    selected_file_error: &mut dyn FnMut(&Path, E) -> std::result::Result<(), E>,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
    Selected: FnMut(BoundedTreeFileCandidate<'_>) -> bool,
{
    visit_bounded_tree_files_with_observation(root, selected, visit, selected_file_error, false)
}

/// Visits bounded opening name snapshots while files may grow. The caller
/// must authenticate retained bytes and validate logical membership before
/// publication; this walk alone never grants deletion or alias authority.
pub fn visit_bounded_tree_files_frozen<E, Selected>(
    root: &Path,
    selected: &mut Selected,
    visit: &mut dyn FnMut(BoundedTreeSourceFile) -> std::result::Result<(), E>,
    selected_file_error: &mut dyn FnMut(&Path, E) -> std::result::Result<(), E>,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
    Selected: FnMut(BoundedTreeFileCandidate<'_>) -> bool,
{
    visit_bounded_tree_files_with_observation(root, selected, visit, selected_file_error, true)
}

fn visit_bounded_tree_files_with_observation<E, Selected>(
    root: &Path,
    selected: &mut Selected,
    visit: &mut dyn FnMut(BoundedTreeSourceFile) -> std::result::Result<(), E>,
    selected_file_error: &mut dyn FnMut(&Path, E) -> std::result::Result<(), E>,
    frozen: bool,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
    Selected: FnMut(BoundedTreeFileCandidate<'_>) -> bool,
{
    match open_provider_source_path(root).map_err(E::from) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            if !selected(BoundedTreeFileCandidate {
                path: root,
                parent: None,
            }) {
                file.revalidate().map_err(E::from)?;
                return Ok(0);
            }
            let file = BoundedTreeSourceFile {
                path: root.to_path_buf(),
                opened: Arc::new(file),
            };
            let result = visit(file.clone());
            match result {
                Ok(()) => {
                    revalidate_tree_file(&file.opened, frozen).map_err(E::from)?;
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
            let visited = visit_bounded_tree_files_at_depth(
                root,
                directory,
                selected,
                visit,
                selected_file_error,
                0,
                frozen,
            )?;
            if frozen {
                authority.revalidate_same_object()
            } else {
                authority.revalidate()
            }
            .map_err(E::from)?;
            Ok(visited)
        }
        Err(error) => Err(error),
    }
}

fn visit_bounded_tree_files_at_depth<E, Selected>(
    path: &Path,
    directory: ProviderSourceDirectory,
    selected: &mut Selected,
    visit: &mut dyn FnMut(BoundedTreeSourceFile) -> std::result::Result<(), E>,
    selected_file_error: &mut dyn FnMut(&Path, E) -> std::result::Result<(), E>,
    depth: usize,
    frozen: bool,
) -> std::result::Result<usize, E>
where
    E: From<SourceIoError>,
    Selected: FnMut(BoundedTreeFileCandidate<'_>) -> bool,
{
    if depth > BOUNDED_TREE_MAX_DIRECTORY_DEPTH {
        return Err(SourceIoError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider transcript directory nesting exceeds the supported limit",
        }
        .into());
    }

    let mut visited = 0_usize;
    visit_bounded_tree_directory_names::<E>(&directory, frozen, &mut |name| {
        let child_path = path.join(&name);
        let is_selected = selected(BoundedTreeFileCandidate {
            path: &child_path,
            parent: Some(&directory),
        });
        let opened = match directory.open_child(&name) {
            Ok(opened) => opened,
            Err(error) if is_selected => {
                selected_file_error(&child_path, error.into())?;
                return Ok(());
            }
            Err(error)
                if is_symlink_source_rejection(&error)
                    || is_non_regular_source_rejection(&error) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        if let OpenedProviderSourcePath::Directory(child_directory) = opened {
            visited = visited.saturating_add(visit_bounded_tree_files_at_depth(
                &child_path,
                child_directory,
                selected,
                visit,
                selected_file_error,
                depth.saturating_add(1),
                frozen,
            )?);
        } else if is_selected {
            let OpenedProviderSourcePath::File(file) = opened else {
                return Err(SourceIoError::SystemInvariant(
                    "native JSONL child classification is incomplete",
                )
                .into());
            };
            let file = BoundedTreeSourceFile {
                path: child_path.clone(),
                opened: Arc::new(file),
            };
            let outcome = visit(file.clone())
                .and_then(|()| revalidate_tree_file(&file.opened, frozen).map_err(E::from));
            match outcome {
                Ok(()) => visited = visited.saturating_add(1),
                Err(error) => selected_file_error(&child_path, error)?,
            }
        }
        Ok(())
    })?;
    if frozen {
        directory.revalidate_same_object()
    } else {
        directory.revalidate()
    }
    .map_err(E::from)?;
    Ok(visited)
}

fn visit_bounded_tree_directory_names<E>(
    directory: &ProviderSourceDirectory,
    frozen: bool,
    visit: &mut dyn FnMut(OsString) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    E: From<SourceIoError>,
{
    // `ReadDir` order is not portable. Keep small directories in memory, then
    // spill only native filenames into fixed runs so wide fanout stays bounded
    // without changing the byte-stable provider visitation order.
    traversal_stats(|stats| stats.directory_read_passes += 1);
    let mut names = BoundedTreeNames::new(native_bounded_tree_run_order());
    let mut collect = |name| -> std::result::Result<(), E> {
        traversal_stats(|stats| stats.directory_entries_read += 1);
        names
            .push(name)
            .map_err(|error| E::from(SourceIoError::Io(error)))?;
        Ok(())
    };
    if frozen {
        directory
            .visit_entries_snapshot(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, &mut collect)?;
    } else {
        directory.visit_entries(PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, &mut collect)?;
    }
    names.visit(visit)
}

fn revalidate_tree_file(file: &OpenedProviderSourceFile, frozen: bool) -> crate::Result<()> {
    if frozen {
        file.revalidate_same_object()
    } else {
        file.revalidate()
    }
}

struct BoundedTreeNames {
    order: BoundedTreeRunOrder,
    names: Vec<Vec<u8>>,
    spill: Option<BoundedTreeDirectoryRuns>,
}

impl BoundedTreeNames {
    fn new(order: BoundedTreeRunOrder) -> Self {
        Self {
            order,
            names: Vec::with_capacity(BOUNDED_TREE_DIRECTORY_RUN_ENTRIES),
            spill: None,
        }
    }

    fn push(&mut self, name: OsString) -> io::Result<()> {
        self.push_key(native_filename_order_key(name))
    }

    fn push_key(&mut self, name: Vec<u8>) -> io::Result<()> {
        if self.names.len() == BOUNDED_TREE_DIRECTORY_RUN_ENTRIES {
            let runs = match self.spill.as_mut() {
                Some(runs) => runs,
                None => self
                    .spill
                    .insert(BoundedTreeDirectoryRuns::new(self.order)?),
            };
            runs.write_initial_run(&mut self.names)?;
        }
        self.names.push(name);
        traversal_stats(|stats| {
            stats.max_retained_names = stats.max_retained_names.max(self.names.len())
        });
        Ok(())
    }

    fn visit<E>(
        self,
        visit: &mut dyn FnMut(OsString) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>
    where
        E: From<SourceIoError>,
    {
        self.visit_keys(&mut |name| {
            visit(
                native_filename_from_order_key(name)
                    .map_err(|error| E::from(SourceIoError::Io(error)))?,
            )
        })
    }

    fn visit_keys<E>(
        mut self,
        visit: &mut dyn FnMut(Vec<u8>) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>
    where
        E: From<SourceIoError>,
    {
        let Some(mut spill) = self.spill else {
            self.order.sort_and_deduplicate(&mut self.names);
            if self.order.requires_native_resort() {
                self.names.sort_unstable();
            }
            for name in self.names {
                traversal_stats(|stats| stats.final_names_read += 1);
                visit(name)?;
            }
            return Ok(());
        };

        if !self.names.is_empty() {
            spill
                .write_initial_run(&mut self.names)
                .map_err(|error| E::from(SourceIoError::Io(error)))?;
        }
        let final_run = spill
            .merge_to_one()
            .map_err(|error| E::from(SourceIoError::Io(error)))?;
        let mut reader = BoundedTreeRunReader::open(&final_run)
            .map_err(|error| E::from(SourceIoError::Io(error)))?;
        if self.order.requires_native_resort() {
            // Windows aliases must first become one authority under folded
            // order, then retained representatives regain native traversal
            // order through the same bounded sorter. The directory is not
            // enumerated again.
            let mut native_names = Self {
                order: BoundedTreeRunOrder::Native,
                names: self.names,
                spill: None,
            };
            let mut reusable_spill = Some(spill);
            let resort_result = (|| -> std::result::Result<(), E> {
                while let Some(name) = reader
                    .next_name()
                    .map_err(|error| E::from(SourceIoError::Io(error)))?
                {
                    if native_names.names.len() == BOUNDED_TREE_DIRECTORY_RUN_ENTRIES
                        && native_names.spill.is_none()
                    {
                        let mut native_spill = reusable_spill.take().ok_or_else(|| {
                            E::from(SourceIoError::SystemInvariant(
                                "bounded Windows filename spill is unavailable",
                            ))
                        })?;
                        native_spill.restart(BoundedTreeRunOrder::Native);
                        native_names.spill = Some(native_spill);
                    }
                    native_names
                        .push_key(name)
                        .map_err(|error| E::from(SourceIoError::Io(error)))?;
                }
                Ok(())
            })();
            drop(reader);
            resort_result?;
            if native_names.spill.is_some() {
                fs::remove_file(final_run).map_err(|error| E::from(SourceIoError::Io(error)))?;
            }
            drop(reusable_spill);
            return native_names.visit_keys(visit);
        }
        while let Some(name) = reader
            .next_name()
            .map_err(|error| E::from(SourceIoError::Io(error)))?
        {
            traversal_stats(|stats| stats.final_names_read += 1);
            visit(name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundedTreeRunOrder {
    Native,
    #[cfg(any(windows, test))]
    WindowsAsciiCaseEquivalent,
}

impl BoundedTreeRunOrder {
    fn compare(self, left: &[u8], right: &[u8]) -> Ordering {
        match self {
            Self::Native => left.cmp(right),
            #[cfg(any(windows, test))]
            Self::WindowsAsciiCaseEquivalent => windows_ascii_case_cmp(left, right),
        }
    }

    fn equivalent(self, _left: &[u8], _right: &[u8]) -> bool {
        match self {
            Self::Native => false,
            #[cfg(any(windows, test))]
            Self::WindowsAsciiCaseEquivalent => {
                windows_ascii_case_cmp(_left, _right) == Ordering::Equal
            }
        }
    }

    fn requires_native_resort(self) -> bool {
        self != Self::Native
    }

    fn sort_and_deduplicate(self, names: &mut Vec<Vec<u8>>) {
        if self.requires_native_resort() {
            // The former Windows collector used a stable folded-name sort, so
            // the first enumerated spelling represented each alias class.
            // Preserve that choice before the bounded run is reordered.
            let mut current = 0;
            while current < names.len() {
                if names[..current]
                    .iter()
                    .any(|name| self.equivalent(name, &names[current]))
                {
                    names.remove(current);
                } else {
                    current += 1;
                }
            }
        }
        names.sort_unstable_by(|left, right| self.compare(left, right));
    }
}

fn native_bounded_tree_run_order() -> BoundedTreeRunOrder {
    #[cfg(windows)]
    {
        BoundedTreeRunOrder::WindowsAsciiCaseEquivalent
    }
    #[cfg(not(windows))]
    {
        BoundedTreeRunOrder::Native
    }
}

#[cfg(any(windows, test))]
fn windows_ascii_case_cmp(left: &[u8], right: &[u8]) -> Ordering {
    windows_filename_units(left)
        .map(windows_ascii_lower_u16)
        .cmp(windows_filename_units(right).map(windows_ascii_lower_u16))
}

#[cfg(any(windows, test))]
fn windows_filename_units(name: &[u8]) -> impl Iterator<Item = u16> + '_ {
    name.chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
}

#[cfg(any(windows, test))]
fn windows_ascii_lower_u16(value: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&value) {
        value + u16::from(b'a' - b'A')
    } else {
        value
    }
}

struct BoundedTreeDirectoryRuns {
    directory: TempDir,
    initial_runs: usize,
    order: BoundedTreeRunOrder,
}

impl BoundedTreeDirectoryRuns {
    fn new(order: BoundedTreeRunOrder) -> io::Result<Self> {
        Ok(Self {
            directory: tempfile::Builder::new()
                .prefix("ctx-native-jsonl-order-")
                .tempdir()?,
            initial_runs: 0,
            order,
        })
    }

    fn run_path(&self, pass: usize, run: usize) -> PathBuf {
        self.directory.path().join(format!("pass-{pass}-run-{run}"))
    }

    fn restart(&mut self, order: BoundedTreeRunOrder) {
        self.initial_runs = 0;
        self.order = order;
    }

    fn write_initial_run(&mut self, names: &mut Vec<Vec<u8>>) -> io::Result<()> {
        self.order.sort_and_deduplicate(names);
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
            for first_input in (0..run_count).step_by(BOUNDED_TREE_DIRECTORY_MERGE_FAN_IN) {
                let input_count =
                    BOUNDED_TREE_DIRECTORY_MERGE_FAN_IN.min(run_count.saturating_sub(first_input));
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
            inputs.push(BoundedTreeRunReader::open(self.run_path(pass, run))?);
        }
        traversal_stats(|stats| {
            stats.max_merge_readers = stats.max_merge_readers.max(inputs.len());
        });
        let mut writer = BufWriter::new(File::create(output)?);
        let mut previous: Option<Vec<u8>> = None;
        loop {
            let mut selected: Option<usize> = None;
            for (index, input) in inputs.iter().enumerate() {
                let Some(name) = input.head() else {
                    continue;
                };
                if selected.is_none_or(|current| {
                    inputs[current].head().is_some_and(|current_name| {
                        self.order.compare(name, current_name) == Ordering::Less
                    })
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
            traversal_stats(|stats| stats.merge_names_read += 1);
            let duplicate = previous
                .as_ref()
                .is_some_and(|previous| self.order.equivalent(previous, &name));
            if !duplicate {
                write_native_jsonl_run_name(&mut writer, &name)?;
                if self.order.requires_native_resort() {
                    previous = Some(name);
                }
            }
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

struct BoundedTreeRunReader {
    reader: BufReader<File>,
    head: Option<Vec<u8>>,
}

impl BoundedTreeRunReader {
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

fn native_filename_order_key(name: OsString) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        name.into_vec()
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

fn native_filename_from_order_key(name: Vec<u8>) -> io::Result<OsString> {
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
struct BoundedTreeTraversalStats {
    directory_read_passes: usize,
    directory_entries_read: usize,
    max_retained_names: usize,
    initial_runs: usize,
    max_merge_readers: usize,
    merge_names_read: usize,
    final_names_read: usize,
}

#[cfg(test)]
std::thread_local! {
    static BOUNDED_TREE_TRAVERSAL_STATS: std::cell::Cell<Option<BoundedTreeTraversalStats>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn traversal_stats(update: impl FnOnce(&mut BoundedTreeTraversalStats)) {
    BOUNDED_TREE_TRAVERSAL_STATS.with(|stats| {
        if let Some(mut current) = stats.get() {
            update(&mut current);
            stats.set(Some(current));
        }
    });
}

#[cfg(not(test))]
fn traversal_stats(_update: impl FnOnce(&mut BoundedTreeTraversalStats)) {}

#[cfg(test)]
fn count_bounded_tree_traversal_work<T>(
    operation: impl FnOnce() -> T,
) -> (T, BoundedTreeTraversalStats) {
    BOUNDED_TREE_TRAVERSAL_STATS
        .with(|stats| assert_eq!(stats.replace(Some(Default::default())), None));
    let output = operation();
    let stats = BOUNDED_TREE_TRAVERSAL_STATS.with(|stats| stats.replace(None).unwrap());
    (output, stats)
}

#[cfg(test)]
mod tests;
