use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    mem::size_of,
    path::{Path, PathBuf},
};

use thiserror::Error;

use super::{
    validate_native_locator_value_len, CapturedBatch, CapturedBatchBuilder, CapturedBatchError,
    CapturedRecord, NativeLocator, NativePosition, ProviderRecordKind, SourceObservation,
    StructuralRejectionKind, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
    CAPTURE_BATCH_MAX_PAYLOAD_BYTES, CAPTURE_BATCH_MAX_RECORDS,
};

const WHOLE_JSON_POSITION_KIND: &str = "whole-json-item-v1";
const WHOLE_JSON_LOCATOR_KIND: &str = "whole-json-source-item-v1";

pub(crate) struct WholeJsonItem {
    ordinal: u64,
    source_item: Vec<u8>,
    observed_size: u64,
    path: PathBuf,
}

struct SourceFileMetadata {
    metadata: Metadata,
    #[cfg(windows)]
    windows_change_token: WindowsFileChangeToken,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowsFileChangeToken {
    volume_serial_number: u64,
    file_id: [u8; 16],
    last_write_time_100ns: i64,
    change_time_100ns: i64,
    size: u64,
}

#[cfg(windows)]
impl WindowsFileChangeToken {
    fn read(file: &File, metadata: &Metadata) -> io::Result<Self> {
        use std::{ffi::c_void, mem::size_of, os::windows::io::AsRawHandle};

        const FILE_BASIC_INFO_CLASS: i32 = 0;
        const FILE_ID_INFO_CLASS: i32 = 18;

        #[repr(C)]
        struct FileBasicInfo {
            _creation_time: i64,
            _last_access_time: i64,
            last_write_time: i64,
            change_time: i64,
            _file_attributes: u32,
        }

        #[repr(C)]
        struct FileIdInfo {
            volume_serial_number: u64,
            file_id: [u8; 16],
        }

        #[link(name = "Kernel32")]
        unsafe extern "system" {
            #[link_name = "GetFileInformationByHandleEx"]
            fn get_file_information_by_handle_ex(
                file: *mut c_void,
                information_class: i32,
                information: *mut c_void,
                information_size: u32,
            ) -> i32;
        }

        let handle = file.as_raw_handle();
        let mut basic_info = FileBasicInfo {
            _creation_time: 0,
            _last_access_time: 0,
            last_write_time: 0,
            change_time: 0,
            _file_attributes: 0,
        };
        let basic_result = unsafe {
            get_file_information_by_handle_ex(
                handle,
                FILE_BASIC_INFO_CLASS,
                (&mut basic_info as *mut FileBasicInfo).cast(),
                size_of::<FileBasicInfo>() as u32,
            )
        };
        if basic_result == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut id_info = FileIdInfo {
            volume_serial_number: 0,
            file_id: [0; 16],
        };
        let id_result = unsafe {
            get_file_information_by_handle_ex(
                handle,
                FILE_ID_INFO_CLASS,
                (&mut id_info as *mut FileIdInfo).cast(),
                size_of::<FileIdInfo>() as u32,
            )
        };
        if id_result == 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            volume_serial_number: id_info.volume_serial_number,
            file_id: id_info.file_id,
            last_write_time_100ns: basic_info.last_write_time,
            change_time_100ns: basic_info.change_time,
            size: metadata.len(),
        })
    }
}

impl WholeJsonItem {
    pub(crate) fn new(
        ordinal: u64,
        source_item: Vec<u8>,
        observed_size: u64,
        path: PathBuf,
    ) -> Result<Self, WholeJsonBatchError> {
        validate_whole_json_source_item(&source_item)?;
        Ok(Self {
            ordinal,
            source_item,
            observed_size,
            path,
        })
    }
}

type WholeJsonNextItem<'a> = dyn FnMut() -> Result<Option<WholeJsonItem>, WholeJsonBatchError> + 'a;

pub(crate) struct WholeJsonBatchProducer<'a> {
    source: SourceObservation,
    record_kind: ProviderRecordKind,
    next_item: Box<WholeJsonNextItem<'a>>,
    lookahead: Option<WholeJsonItem>,
    last_observed_ordinal: Option<u64>,
    source_exhausted: bool,
    max_record_bytes: usize,
    max_batch_payload_bytes: usize,
    poisoned: bool,
}

impl<'a> WholeJsonBatchProducer<'a> {
    pub(crate) fn new(
        source: SourceObservation,
        record_kind: ProviderRecordKind,
        next_item: impl FnMut() -> Result<Option<WholeJsonItem>, WholeJsonBatchError> + 'a,
    ) -> Result<Self, WholeJsonBatchError> {
        Ok(Self {
            source,
            record_kind,
            next_item: Box::new(next_item),
            lookahead: None,
            last_observed_ordinal: None,
            source_exhausted: false,
            max_record_bytes: CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
            max_batch_payload_bytes: CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
            poisoned: false,
        })
    }

    #[cfg(test)]
    fn with_max_record_bytes(mut self, maximum: usize) -> Self {
        self.max_record_bytes = maximum;
        self
    }

    #[cfg(test)]
    fn with_max_batch_payload_bytes(mut self, maximum: usize) -> Self {
        self.max_batch_payload_bytes = maximum;
        self
    }

    pub(crate) fn next_batch(&mut self) -> Result<Option<CapturedBatch>, WholeJsonBatchError> {
        if self.poisoned {
            return Err(WholeJsonBatchError::ProducerPoisoned);
        }

        let result = self.next_batch_unpoisoned();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn next_batch_unpoisoned(&mut self) -> Result<Option<CapturedBatch>, WholeJsonBatchError> {
        self.fill_lookahead()?;
        let Some(first_ordinal) = self.lookahead.as_ref().map(|item| item.ordinal) else {
            return Ok(None);
        };
        let mut builder =
            CapturedBatchBuilder::new(self.source.clone(), whole_json_position(first_ordinal)?);
        let mut last_ordinal = None;

        loop {
            if builder.record_count() >= CAPTURE_BATCH_MAX_RECORDS
                || (!builder.is_empty()
                    && builder.retained_payload_bytes() >= self.max_batch_payload_bytes)
            {
                break;
            }
            self.fill_lookahead()?;
            let Some(item) = self.lookahead.as_ref() else {
                break;
            };
            if !self.item_fits_without_reading(&builder, item)? {
                break;
            }

            let item = self
                .lookahead
                .take()
                .ok_or(WholeJsonBatchError::MissingLookahead)?;
            let record = capture_path_item(&item, &self.record_kind, self.max_record_bytes)?;
            if !builder.can_accept(&record) {
                return Err(WholeJsonBatchError::Batch(CapturedBatchError::BatchFull));
            }
            builder.push(record)?;
            last_ordinal = Some(item.ordinal);
        }

        if builder.is_empty() {
            return Ok(None);
        }
        self.fill_lookahead()?;
        let range_end = match self.lookahead.as_ref() {
            Some(item) => item.ordinal,
            None => last_ordinal
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or(WholeJsonBatchError::LengthOverflow)?,
        };
        if self.source_exhausted && self.lookahead.is_none() {
            builder.mark_source_exhausted();
        }
        let batch = builder.finish(whole_json_position(range_end)?)?;
        Ok(Some(batch))
    }

    fn fill_lookahead(&mut self) -> Result<(), WholeJsonBatchError> {
        if self.lookahead.is_some() || self.source_exhausted {
            return Ok(());
        }
        let Some(item) = (self.next_item)()? else {
            self.source_exhausted = true;
            return Ok(());
        };
        if item.ordinal == u64::MAX {
            return Err(WholeJsonBatchError::LengthOverflow);
        }
        if self
            .last_observed_ordinal
            .is_some_and(|previous| previous >= item.ordinal)
        {
            return Err(WholeJsonBatchError::NonIncreasingOrdinals);
        }
        self.last_observed_ordinal = Some(item.ordinal);
        self.lookahead = Some(item);
        Ok(())
    }

    fn item_fits_without_reading(
        &self,
        builder: &CapturedBatchBuilder,
        item: &WholeJsonItem,
    ) -> Result<bool, WholeJsonBatchError> {
        let max_record_bytes = u64::try_from(self.max_record_bytes)
            .map_err(|_| WholeJsonBatchError::LengthOverflow)?;
        if item.observed_size > max_record_bytes || builder.is_empty() {
            return Ok(true);
        }
        let item_bytes =
            usize::try_from(item.observed_size).map_err(|_| WholeJsonBatchError::LengthOverflow)?;
        Ok(builder
            .retained_payload_bytes()
            .checked_add(item_bytes)
            .is_some_and(|total| total <= self.max_batch_payload_bytes))
    }
}

fn capture_path_item(
    item: &WholeJsonItem,
    record_kind: &ProviderRecordKind,
    max_record_bytes: usize,
) -> Result<CapturedRecord, WholeJsonBatchError> {
    let locator = whole_json_locator(&item.source_item)?;
    let before = source_path_metadata(&item.path, item.observed_size)?;
    let max_record_bytes =
        u64::try_from(max_record_bytes).map_err(|_| WholeJsonBatchError::LengthOverflow)?;
    if item.observed_size > max_record_bytes {
        let after = source_path_metadata(&item.path, item.observed_size)?;
        if !same_source_metadata(&before, &after) {
            return Err(WholeJsonBatchError::SourceMetadataChangedDuringRead);
        }
        return Ok(CapturedRecord::structural_rejection(
            item.ordinal,
            locator,
            record_kind.clone(),
            StructuralRejectionKind::OversizeRecord,
            item.observed_size,
        ));
    }

    let expected =
        usize::try_from(item.observed_size).map_err(|_| WholeJsonBatchError::LengthOverflow)?;
    let mut file = File::open(&item.path)?;
    let opened = source_open_file_metadata(&file, item.observed_size)?;
    if !same_source_metadata(&before, &opened) {
        return Err(WholeJsonBatchError::SourceMetadataChangedDuringRead);
    }

    let read_limit = item
        .observed_size
        .checked_add(1)
        .ok_or(WholeJsonBatchError::LengthOverflow)?;
    let mut payload = Vec::with_capacity(expected);
    (&mut file).take(read_limit).read_to_end(&mut payload)?;
    let actual = u64::try_from(payload.len()).map_err(|_| WholeJsonBatchError::LengthOverflow)?;
    if actual != item.observed_size {
        return Err(WholeJsonBatchError::SourceSizeChanged {
            expected: item.observed_size,
            actual,
        });
    }

    let opened_after = source_open_file_metadata(&file, item.observed_size)?;
    let path_after = source_path_metadata(&item.path, item.observed_size)?;
    if !same_source_metadata(&opened, &opened_after)
        || !same_source_metadata(&opened_after, &path_after)
    {
        return Err(WholeJsonBatchError::SourceMetadataChangedDuringRead);
    }
    Ok(CapturedRecord::content(
        item.ordinal,
        locator,
        record_kind.clone(),
        payload,
    )?)
}

fn source_path_metadata(
    path: &Path,
    observed_size: u64,
) -> Result<SourceFileMetadata, WholeJsonBatchError> {
    ensure_no_symlink_parents(path)?;
    let metadata = fs::symlink_metadata(path)?;
    validate_source_metadata(&metadata, observed_size)?;

    #[cfg(windows)]
    {
        // Query the same strong handle token used by OpenHands inventory. A
        // second path open is intentional: the returned file ID and ChangeTime
        // freeze the path observation even when size and mtime are preserved.
        let file = File::open(path)?;
        source_open_file_metadata(&file, observed_size)
    }

    #[cfg(not(windows))]
    Ok(SourceFileMetadata { metadata })
}

fn source_open_file_metadata(
    file: &File,
    observed_size: u64,
) -> Result<SourceFileMetadata, WholeJsonBatchError> {
    let metadata = file.metadata()?;
    validate_source_metadata(&metadata, observed_size)?;

    #[cfg(windows)]
    let windows_change_token = WindowsFileChangeToken::read(file, &metadata)?;

    Ok(SourceFileMetadata {
        metadata,
        #[cfg(windows)]
        windows_change_token,
    })
}

fn ensure_no_symlink_parents(path: &Path) -> Result<(), WholeJsonBatchError> {
    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(WholeJsonBatchError::SymlinkSourceItem);
        }
    }
    Ok(())
}

fn validate_source_metadata(
    metadata: &Metadata,
    observed_size: u64,
) -> Result<(), WholeJsonBatchError> {
    if metadata.file_type().is_symlink() {
        return Err(WholeJsonBatchError::SymlinkSourceItem);
    }
    if !metadata.is_file() {
        return Err(WholeJsonBatchError::NonRegularSourceItem);
    }
    if metadata.len() != observed_size {
        return Err(WholeJsonBatchError::SourceSizeChanged {
            expected: observed_size,
            actual: metadata.len(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn same_source_metadata(before: &SourceFileMetadata, after: &SourceFileMetadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.metadata.dev() == after.metadata.dev()
        && before.metadata.ino() == after.metadata.ino()
        && before.metadata.len() == after.metadata.len()
        && before.metadata.mtime() == after.metadata.mtime()
        && before.metadata.mtime_nsec() == after.metadata.mtime_nsec()
        && before.metadata.ctime() == after.metadata.ctime()
        && before.metadata.ctime_nsec() == after.metadata.ctime_nsec()
}

#[cfg(windows)]
fn same_source_metadata(before: &SourceFileMetadata, after: &SourceFileMetadata) -> bool {
    let before_token = before.windows_change_token;
    let after_token = after.windows_change_token;
    before.metadata.len() == after.metadata.len()
        && before_token.volume_serial_number == after_token.volume_serial_number
        && before_token.file_id == after_token.file_id
        && before_token.last_write_time_100ns == after_token.last_write_time_100ns
        && before_token.change_time_100ns == after_token.change_time_100ns
        && before_token.size == after_token.size
}

#[cfg(not(any(unix, windows)))]
fn same_source_metadata(before: &SourceFileMetadata, after: &SourceFileMetadata) -> bool {
    // Targets without a stable file identity and metadata ChangeTime use the
    // strongest metadata-only fallback exposed by std. Unix and Windows use
    // exact identity/change tokens above.
    before.metadata.len() == after.metadata.len()
        && before.metadata.modified().ok() == after.metadata.modified().ok()
}

fn whole_json_position(ordinal: u64) -> Result<NativePosition, CapturedBatchError> {
    NativePosition::new(WHOLE_JSON_POSITION_KIND, ordinal.to_be_bytes().to_vec())
}

fn whole_json_locator(source_item: &[u8]) -> Result<NativeLocator, WholeJsonBatchError> {
    let value_len = validate_whole_json_source_item(source_item)?;
    let source_len =
        u32::try_from(source_item.len()).map_err(|_| WholeJsonBatchError::LengthOverflow)?;
    let mut value = Vec::with_capacity(value_len);
    value.extend_from_slice(&source_len.to_be_bytes());
    value.extend_from_slice(source_item);
    Ok(NativeLocator::new(WHOLE_JSON_LOCATOR_KIND, value)?)
}

fn validate_whole_json_source_item(source_item: &[u8]) -> Result<usize, WholeJsonBatchError> {
    let value_len = size_of::<u32>()
        .checked_add(source_item.len())
        .ok_or(WholeJsonBatchError::LengthOverflow)?;
    validate_native_locator_value_len(value_len)?;
    Ok(value_len)
}

#[derive(Debug, Error)]
pub(crate) enum WholeJsonBatchError {
    #[error("whole-JSON capture length overflow")]
    LengthOverflow,
    #[error("whole-JSON items must have strictly increasing ordinals")]
    NonIncreasingOrdinals,
    #[error("whole-JSON source size changed: expected {expected} bytes, observed {actual}")]
    SourceSizeChanged { expected: u64, actual: u64 },
    #[error("whole-JSON source metadata changed during read")]
    SourceMetadataChangedDuringRead,
    #[error("whole-JSON source item path contains a symlink")]
    SymlinkSourceItem,
    #[error("whole-JSON source item path is not a regular file")]
    NonRegularSourceItem,
    #[error("whole-JSON batch producer is poisoned after a capture failure")]
    ProducerPoisoned,
    #[error("whole-JSON batch producer lost its retained lookahead item")]
    MissingLookahead,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Batch(#[from] CapturedBatchError),
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs, path::Path, rc::Rc};

    use ctx_history_core::CaptureProvider;

    use super::*;
    use crate::captured_batch::CapturedRecordPayload;

    fn observation(size: u64) -> SourceObservation {
        SourceObservation::new(
            CaptureProvider::Cline,
            "cline_task_directory_json",
            "task:abc",
            format!("size:{size}"),
            "provider:cline:cline_task_directory_json:source:test",
            1,
            1,
            None,
        )
        .unwrap()
    }

    fn record_kind() -> ProviderRecordKind {
        ProviderRecordKind::new("cline-task-json-v1").unwrap()
    }

    fn write_item(
        root: &Path,
        ordinal: u64,
        source_item: Vec<u8>,
        payload: &[u8],
    ) -> WholeJsonItem {
        let path = root.join(format!("item-{ordinal}.json"));
        fs::write(&path, payload).unwrap();
        WholeJsonItem::new(ordinal, source_item, payload.len() as u64, path).unwrap()
    }

    fn producer_from_items(
        source: SourceObservation,
        record_kind: ProviderRecordKind,
        items: Vec<WholeJsonItem>,
    ) -> WholeJsonBatchProducer<'static> {
        let mut items = items.into_iter();
        WholeJsonBatchProducer::new(source, record_kind, move || Ok(items.next())).unwrap()
    }

    #[test]
    fn producer_partitions_sixty_five_items_with_continuous_source_observation() {
        let directory = crate::test_support_paths::tempdir().unwrap();
        let source = observation(65 * 2);
        let items = (10..75)
            .map(|ordinal| {
                write_item(
                    directory.path(),
                    ordinal,
                    format!("task-{ordinal}.json").into_bytes(),
                    b"{}",
                )
            })
            .collect();
        let mut producer = producer_from_items(source.clone(), record_kind(), items);

        let first = producer.next_batch().unwrap().unwrap();
        let second = producer.next_batch().unwrap().unwrap();

        assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
        assert_eq!(second.records().len(), 1);
        assert_eq!(first.source(), &source);
        assert_eq!(second.source(), &source);
        assert_eq!(first.range_before().value(), 10_u64.to_be_bytes());
        assert_eq!(first.range_end(), second.range_before());
        assert_eq!(second.range_end().value(), 75_u64.to_be_bytes());
        assert!(producer.next_batch().unwrap().is_none());
    }

    #[test]
    fn producer_defers_non_fitting_item_without_opening_or_retaining_it() {
        let directory = crate::test_support_paths::tempdir().unwrap();
        let first_path = directory.path().join("first.json");
        let deferred_path = directory.path().join("deferred.json");
        fs::write(&first_path, b"1234567").unwrap();
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        let deferred_item_path = deferred_path.clone();
        let mut producer = WholeJsonBatchProducer::new(observation(9), record_kind(), move || {
            let call = callback_calls.get();
            callback_calls.set(call + 1);
            match call {
                0 => WholeJsonItem::new(3, b"first.json".to_vec(), 7, first_path.clone()).map(Some),
                1 => {
                    WholeJsonItem::new(4, b"deferred.json".to_vec(), 2, deferred_item_path.clone())
                        .map(Some)
                }
                2 => Ok(None),
                _ => panic!("whole-JSON producer read beyond one lookahead item"),
            }
        })
        .unwrap()
        .with_max_batch_payload_bytes(8);

        let first = producer.next_batch().unwrap().unwrap();
        assert_eq!(first.records().len(), 1);
        assert_eq!(first.retained_payload_bytes(), 7);
        assert_eq!(calls.get(), 2);
        assert_eq!(
            producer.lookahead.as_ref().map(|item| item.ordinal),
            Some(4)
        );

        fs::write(deferred_path, b"{}").unwrap();
        let second = producer.next_batch().unwrap().unwrap();
        assert_eq!(second.records().len(), 1);
        assert_eq!(second.records()[0].ordinal(), 4);
        assert_eq!(second.retained_payload_bytes(), 2);
    }

    #[test]
    fn producer_validates_lazy_ordinals_and_poisoning() {
        let directory = crate::test_support_paths::tempdir().unwrap();
        let items = vec![
            write_item(directory.path(), 8, b"first.json".to_vec(), b"{}"),
            write_item(directory.path(), 8, b"duplicate.json".to_vec(), b"{}"),
        ];
        let mut producer = producer_from_items(observation(4), record_kind(), items);

        assert!(matches!(
            producer.next_batch().unwrap_err(),
            WholeJsonBatchError::NonIncreasingOrdinals
        ));
        assert!(matches!(
            producer.next_batch().unwrap_err(),
            WholeJsonBatchError::ProducerPoisoned
        ));
    }

    #[test]
    fn producer_rejects_oversize_and_fails_closed_on_short_reads() {
        let directory = crate::test_support_paths::tempdir().unwrap();
        let oversize = write_item(directory.path(), 0, b"oversize.json".to_vec(), b"123456789");
        let mut producer = producer_from_items(observation(9), record_kind(), vec![oversize])
            .with_max_record_bytes(8);
        let batch = producer.next_batch().unwrap().unwrap();
        assert!(matches!(
            batch.records()[0].payload(),
            CapturedRecordPayload::StructuralRejection {
                kind: StructuralRejectionKind::OversizeRecord,
                observed_bytes: 9,
            }
        ));

        let short_path = directory.path().join("short.json");
        fs::write(&short_path, b"short").unwrap();
        let short = WholeJsonItem::new(1, b"short.json".to_vec(), 10, short_path).unwrap();
        let mut producer = producer_from_items(observation(10), record_kind(), vec![short]);
        assert!(matches!(
            producer.next_batch().unwrap_err(),
            WholeJsonBatchError::SourceSizeChanged {
                expected: 10,
                actual: 5,
            }
        ));
        assert!(matches!(
            producer.next_batch().unwrap_err(),
            WholeJsonBatchError::ProducerPoisoned
        ));
    }

    #[test]
    fn producer_preserves_caller_ordinals_and_stable_locator_bytes() {
        let directory = crate::test_support_paths::tempdir().unwrap();
        let first_source_item = b"task-a/messages.json".to_vec();
        let second_source_item = b"task-z/messages.json".to_vec();
        let items = vec![
            write_item(
                directory.path(),
                7,
                first_source_item.clone(),
                b"{\"id\":7}",
            ),
            write_item(
                directory.path(),
                42,
                second_source_item.clone(),
                b"{\"id\":42}",
            ),
        ];
        let mut producer = producer_from_items(observation(19), record_kind(), items);
        let batch = producer.next_batch().unwrap().unwrap();

        assert!(batch.source_exhausted());
        assert_eq!(batch.records()[0].ordinal(), 7);
        assert_eq!(batch.records()[1].ordinal(), 42);
        assert_eq!(
            batch.records()[0].locator().value(),
            whole_json_locator(&first_source_item).unwrap().value()
        );
        assert_eq!(
            batch.records()[1].locator().value(),
            whole_json_locator(&second_source_item).unwrap().value()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_metadata_token_rejects_same_size_replacement_with_preserved_mtime() {
        use std::fs::FileTimes;

        let directory = crate::test_support_paths::tempdir().unwrap();
        let path = directory.path().join("item.json");
        let replacement = directory.path().join("replacement.json");
        fs::write(&path, b"old").unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let before = source_path_metadata(&path, 3).unwrap();

        fs::write(&replacement, b"new").unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();
        fs::rename(&replacement, &path).unwrap();

        let after = source_path_metadata(&path, 3).unwrap();
        assert_eq!(before.metadata.len(), after.metadata.len());
        assert_eq!(before.metadata.modified().unwrap(), original_modified);
        assert_eq!(after.metadata.modified().unwrap(), original_modified);
        assert!(!same_source_metadata(&before, &after));
    }

    #[cfg(windows)]
    #[test]
    fn windows_metadata_token_rejects_same_size_replacement_by_file_identity() {
        use std::fs::FileTimes;

        let directory = crate::test_support_paths::tempdir().unwrap();
        let path = directory.path().join("item.json");
        let replacement = directory.path().join("replacement.json");
        fs::write(&path, b"old").unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let before = source_path_metadata(&path, 3).unwrap();

        fs::write(&replacement, b"new").unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();
        fs::remove_file(&path).unwrap();
        fs::rename(&replacement, &path).unwrap();

        let after = source_path_metadata(&path, 3).unwrap();
        assert_eq!(before.metadata.len(), after.metadata.len());
        assert_eq!(before.metadata.modified().unwrap(), original_modified);
        assert_eq!(after.metadata.modified().unwrap(), original_modified);
        assert_eq!(
            before.windows_change_token.volume_serial_number,
            after.windows_change_token.volume_serial_number
        );
        assert_ne!(
            before.windows_change_token.file_id,
            after.windows_change_token.file_id
        );
        assert!(!same_source_metadata(&before, &after));
    }

    #[cfg(windows)]
    #[test]
    fn windows_metadata_token_rejects_in_place_rewrite_by_change_time() {
        use std::fs::FileTimes;

        let directory = crate::test_support_paths::tempdir().unwrap();
        let path = directory.path().join("item.json");
        fs::write(&path, b"old").unwrap();
        let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let before = source_path_metadata(&path, 3).unwrap();

        fs::write(&path, b"new").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();

        let after = source_path_metadata(&path, 3).unwrap();
        assert_eq!(before.metadata.len(), after.metadata.len());
        assert_eq!(before.metadata.modified().unwrap(), original_modified);
        assert_eq!(after.metadata.modified().unwrap(), original_modified);
        assert_eq!(
            before.windows_change_token.volume_serial_number,
            after.windows_change_token.volume_serial_number
        );
        assert_eq!(
            before.windows_change_token.file_id,
            after.windows_change_token.file_id
        );
        assert_eq!(
            before.windows_change_token.last_write_time_100ns,
            after.windows_change_token.last_write_time_100ns
        );
        assert_ne!(
            before.windows_change_token.change_time_100ns,
            after.windows_change_token.change_time_100ns
        );
        assert!(!same_source_metadata(&before, &after));
    }

    #[cfg(unix)]
    #[test]
    fn producer_rejects_symlinked_source_items() {
        use std::os::unix::fs::symlink;

        let directory = crate::test_support_paths::tempdir().unwrap();
        let target = directory.path().join("target.json");
        let link = directory.path().join("link.json");
        fs::write(&target, b"{}").unwrap();
        symlink(target, &link).unwrap();
        let item = WholeJsonItem::new(0, b"link.json".to_vec(), 2, link).unwrap();
        let mut producer = producer_from_items(observation(2), record_kind(), vec![item]);

        assert!(matches!(
            producer.next_batch().unwrap_err(),
            WholeJsonBatchError::SymlinkSourceItem
        ));
    }
}
