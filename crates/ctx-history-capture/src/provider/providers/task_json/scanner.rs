use std::{
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, CapturedRecord, ProviderRecordKind, SourceObservation,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
    CAPTURE_BATCH_MAX_RECORDS,
};
use crate::{CaptureError, Result};

use super::dialect::{
    task_json_captured_batch_error, task_json_locator, task_json_native_position,
    TaskJsonArrayContainer, TaskJsonMessagePhase, TaskJsonRecordClass, TaskJsonStreamPosition,
    TASK_JSON_DONE_PHASE, TASK_JSON_TERMINAL_PHASE,
};
use super::source::TaskJsonFrozenFile;

#[derive(Debug)]
pub(super) struct TaskJsonByteReader {
    reader: BufReader<File>,
    offset: u64,
}

impl TaskJsonByteReader {
    pub(super) fn open(path: &Path, frozen: &TaskJsonFrozenFile, offset: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        if TaskJsonFrozenFile::from_metadata(&file.metadata()?)? != *frozen {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            reader: BufReader::new(file),
            offset,
        })
    }

    pub(super) fn peek_byte(&mut self) -> Result<Option<u8>> {
        Ok(self.reader.fill_buf()?.first().copied())
    }

    pub(super) fn read_byte(&mut self) -> Result<Option<u8>> {
        let Some(byte) = self.peek_byte()? else {
            return Ok(None);
        };
        self.reader.consume(1);
        self.offset = self
            .offset
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "task JSON byte offset overflowed",
            ))?;
        Ok(Some(byte))
    }

    pub(super) fn skip_whitespace(&mut self) -> Result<()> {
        while self
            .peek_byte()?
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.read_byte()?;
        }
        Ok(())
    }

    pub(super) fn scanned_value(&mut self, retain: bool) -> Result<Option<TaskJsonScannedValue>> {
        self.skip_whitespace()?;
        let start = self.offset;
        let Some(first) = self.read_byte()? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        if retain {
            task_json_retain_scanned_byte(&mut bytes, first);
        }
        let complete = match first {
            b'"' => self.scan_string(retain, &mut bytes)?,
            b'[' | b'{' => self.scan_composite(retain, &mut bytes)?,
            _ => {
                while let Some(byte) = self.peek_byte()? {
                    if byte.is_ascii_whitespace() || matches!(byte, b',' | b']' | b'}') {
                        break;
                    }
                    let byte = self.read_byte()?.ok_or(CaptureError::SystemInvariant(
                        "task JSON scanner lost a peeked byte",
                    ))?;
                    if retain {
                        task_json_retain_scanned_byte(&mut bytes, byte);
                    }
                }
                true
            }
        };
        Ok(Some(TaskJsonScannedValue {
            bytes,
            start,
            complete,
            observed_bytes: self.offset.saturating_sub(start),
        }))
    }

    fn scan_string(&mut self, retain: bool, bytes: &mut Vec<u8>) -> Result<bool> {
        let mut escaped = false;
        while let Some(byte) = self.read_byte()? {
            if retain {
                task_json_retain_scanned_byte(bytes, byte);
            }
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn scan_composite(&mut self, retain: bool, bytes: &mut Vec<u8>) -> Result<bool> {
        let mut depth = 1_u64;
        let mut in_string = false;
        let mut escaped = false;
        while let Some(byte) = self.read_byte()? {
            if retain {
                task_json_retain_scanned_byte(bytes, byte);
            }
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'[' | b'{' => {
                    depth = depth.checked_add(1).ok_or(CaptureError::SystemInvariant(
                        "task JSON nesting depth overflowed",
                    ))?;
                }
                b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        Ok(false)
    }
}

fn task_json_retain_scanned_byte(bytes: &mut Vec<u8>, byte: u8) {
    if bytes.len() < CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES {
        bytes.push(byte);
    }
}

pub(super) struct TaskJsonScannedValue {
    pub(super) bytes: Vec<u8>,
    pub(super) start: u64,
    pub(super) complete: bool,
    pub(super) observed_bytes: u64,
}

impl TaskJsonScannedValue {
    pub(super) fn retained_all(&self) -> bool {
        u64::try_from(self.bytes.len()).ok() == Some(self.observed_bytes)
    }
}

enum TaskJsonArrayLocation {
    Found(TaskJsonArrayContainer),
    NoArray,
    Malformed(String),
}

enum TaskJsonMessageRead {
    Item {
        bytes: Vec<u8>,
        start: u64,
        file_done: bool,
    },
    RejectedItem {
        reason: String,
        start: u64,
        file_done: bool,
    },
    End,
    Malformed(String),
}

struct TaskJsonMessageReader {
    bytes: TaskJsonByteReader,
    container: TaskJsonArrayContainer,
}

impl TaskJsonMessageReader {
    pub(super) fn open(
        path: &Path,
        frozen: &TaskJsonFrozenFile,
        offset: u64,
        container: TaskJsonArrayContainer,
    ) -> Result<Self> {
        Ok(Self {
            bytes: TaskJsonByteReader::open(path, frozen, offset)?,
            container,
        })
    }

    fn locate_array(&mut self) -> Result<TaskJsonArrayLocation> {
        self.bytes.skip_whitespace()?;
        match self.bytes.peek_byte()? {
            Some(b'[') => {
                self.bytes.read_byte()?;
                self.bytes.skip_whitespace()?;
                self.container = TaskJsonArrayContainer::Direct;
                Ok(TaskJsonArrayLocation::Found(self.container))
            }
            Some(b'{') => self.locate_wrapped_array(),
            Some(_) => {
                let Some(value) = self.bytes.scanned_value(true)? else {
                    return Ok(TaskJsonArrayLocation::NoArray);
                };
                if !value.complete
                    || !value.retained_all()
                    || serde_json::from_slice::<Value>(&value.bytes).is_err()
                {
                    Ok(TaskJsonArrayLocation::Malformed(
                        "task message JSON is malformed".to_owned(),
                    ))
                } else {
                    Ok(TaskJsonArrayLocation::NoArray)
                }
            }
            None => Ok(TaskJsonArrayLocation::Malformed(
                "task message JSON is empty".to_owned(),
            )),
        }
    }

    fn locate_wrapped_array(&mut self) -> Result<TaskJsonArrayLocation> {
        self.bytes.read_byte()?;
        loop {
            self.bytes.skip_whitespace()?;
            if self.bytes.peek_byte()? == Some(b'}') {
                self.bytes.read_byte()?;
                return Ok(TaskJsonArrayLocation::NoArray);
            }
            let Some(key) = self.bytes.scanned_value(true)? else {
                return Ok(TaskJsonArrayLocation::Malformed(
                    "task message JSON object ended before a key".to_owned(),
                ));
            };
            if !key.complete {
                return Ok(TaskJsonArrayLocation::Malformed(
                    "task message JSON object contains an unterminated key".to_owned(),
                ));
            }
            if !key.retained_all() {
                return Ok(TaskJsonArrayLocation::Malformed(
                    "task message JSON object key exceeds the captured-record limit".to_owned(),
                ));
            }
            let key = serde_json::from_slice::<String>(&key.bytes).ok();
            self.bytes.skip_whitespace()?;
            if self.bytes.read_byte()? != Some(b':') {
                return Ok(TaskJsonArrayLocation::Malformed(
                    "task message JSON object is missing a key separator".to_owned(),
                ));
            }
            self.bytes.skip_whitespace()?;
            let selected = matches!(key.as_deref(), Some("messages" | "history"));
            if selected {
                if self.bytes.peek_byte()? != Some(b'[') {
                    return Ok(TaskJsonArrayLocation::NoArray);
                }
                self.bytes.read_byte()?;
                self.bytes.skip_whitespace()?;
                self.container = TaskJsonArrayContainer::Wrapped;
                return Ok(TaskJsonArrayLocation::Found(self.container));
            }
            let Some(value) = self.bytes.scanned_value(false)? else {
                return Ok(TaskJsonArrayLocation::Malformed(
                    "task message JSON object ended before a value".to_owned(),
                ));
            };
            if !value.complete {
                return Ok(TaskJsonArrayLocation::Malformed(
                    "task message JSON object contains an unterminated value".to_owned(),
                ));
            }
            self.bytes.skip_whitespace()?;
            match self.bytes.read_byte()? {
                Some(b',') => continue,
                Some(b'}') => return Ok(TaskJsonArrayLocation::NoArray),
                _ => {
                    return Ok(TaskJsonArrayLocation::Malformed(
                        "task message JSON object has an invalid value separator".to_owned(),
                    ));
                }
            }
        }
    }

    fn next_item(&mut self) -> Result<TaskJsonMessageRead> {
        self.bytes.skip_whitespace()?;
        match self.bytes.peek_byte()? {
            Some(b']') => {
                self.bytes.read_byte()?;
                return Ok(TaskJsonMessageRead::End);
            }
            None => {
                return Ok(TaskJsonMessageRead::Malformed(
                    "task message JSON array ended before its closing bracket".to_owned(),
                ));
            }
            _ => {}
        }
        let Some(mut value) = self.bytes.scanned_value(true)? else {
            return Ok(TaskJsonMessageRead::Malformed(
                "task message JSON array ended before an item".to_owned(),
            ));
        };
        if !value.complete {
            task_json_retain_scanned_byte(&mut value.bytes, b'!');
            return Ok(TaskJsonMessageRead::Item {
                bytes: value.bytes,
                start: value.start,
                file_done: true,
            });
        }
        let retained_all = value.retained_all();
        self.bytes.skip_whitespace()?;
        let (file_done, separator_error) = match self.bytes.read_byte()? {
            Some(b',') => {
                self.bytes.skip_whitespace()?;
                if self.bytes.peek_byte()? == Some(b']') {
                    (
                        true,
                        Some("task message JSON array has a trailing comma".to_owned()),
                    )
                } else {
                    (false, None)
                }
            }
            Some(b']') => (true, None),
            Some(_) => (
                true,
                Some("task message JSON array has an invalid item separator".to_owned()),
            ),
            None => (
                true,
                Some("task message JSON array ended after an item".to_owned()),
            ),
        };
        if !retained_all {
            return Ok(TaskJsonMessageRead::RejectedItem {
                reason: format!(
                    "task message JSON item exceeds the {CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES} byte captured-record limit (observed {} bytes)",
                    value.observed_bytes
                ),
                start: value.start,
                file_done,
            });
        }
        if let Some(reason) = separator_error {
            return Ok(TaskJsonMessageRead::RejectedItem {
                reason,
                start: value.start,
                file_done,
            });
        }
        Ok(TaskJsonMessageRead::Item {
            bytes: value.bytes,
            start: value.start,
            file_done,
        })
    }

    fn offset(&self) -> u64 {
        self.bytes.offset
    }
}

#[derive(Clone)]
pub(super) struct TaskJsonMessageSource {
    pub(super) phase: TaskJsonMessagePhase,
    pub(super) path: PathBuf,
    pub(super) frozen: TaskJsonFrozenFile,
}

struct TaskJsonPendingRecord {
    record: CapturedRecord,
    after: TaskJsonStreamPosition,
}

pub(super) struct TaskJsonBatchProducer {
    source: SourceObservation,
    record_kind: ProviderRecordKind,
    message_sources: Vec<TaskJsonMessageSource>,
    position: TaskJsonStreamPosition,
    reader: Option<TaskJsonMessageReader>,
    lookahead: Option<TaskJsonPendingRecord>,
    source_exhausted: bool,
}

impl TaskJsonBatchProducer {
    pub(super) fn new(
        source: SourceObservation,
        record_kind: ProviderRecordKind,
        message_sources: Vec<TaskJsonMessageSource>,
        position: TaskJsonStreamPosition,
    ) -> Result<Self> {
        if position.phase < TASK_JSON_TERMINAL_PHASE {
            TaskJsonMessagePhase::decode(position.phase)?;
        }
        Ok(Self {
            source,
            record_kind,
            message_sources,
            position,
            reader: None,
            lookahead: None,
            source_exhausted: false,
        })
    }

    pub(super) fn next_batch(&mut self) -> Result<Option<CapturedBatch>> {
        let range_before = task_json_native_position(self.position)?;
        self.fill_lookahead()?;
        if self.lookahead.is_none() {
            return Ok(None);
        }
        let mut builder = CapturedBatchBuilder::new(self.source.clone(), range_before);
        loop {
            if builder.record_count() >= CAPTURE_BATCH_MAX_RECORDS
                || (!builder.is_empty()
                    && builder.retained_payload_bytes() >= CAPTURE_BATCH_MAX_PAYLOAD_BYTES)
            {
                break;
            }
            self.fill_lookahead()?;
            let Some(pending) = self.lookahead.as_ref() else {
                break;
            };
            if !builder.is_empty()
                && builder
                    .retained_payload_bytes()
                    .checked_add(pending.record.retained_bytes())
                    .is_none_or(|bytes| bytes > CAPTURE_BATCH_MAX_PAYLOAD_BYTES)
            {
                break;
            }
            if !builder.can_accept(&pending.record) {
                if builder.is_empty() {
                    return Err(CaptureError::SystemInvariant(
                        "task JSON producer created a record that cannot enter an empty batch",
                    ));
                }
                break;
            }
            let pending = self.lookahead.take().ok_or(CaptureError::SystemInvariant(
                "task JSON producer lost its lookahead record",
            ))?;
            builder
                .push(pending.record)
                .map_err(task_json_captured_batch_error)?;
            self.position = pending.after;
            if self.position.phase == TASK_JSON_DONE_PHASE {
                self.source_exhausted = true;
            }
        }
        if builder.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "task JSON producer returned an empty captured batch",
            ));
        }
        if self.source_exhausted && self.lookahead.is_none() {
            builder.mark_source_exhausted();
        }
        builder
            .finish(task_json_native_position(self.position)?)
            .map(Some)
            .map_err(task_json_captured_batch_error)
    }

    fn fill_lookahead(&mut self) -> Result<()> {
        if self.lookahead.is_none() && !self.source_exhausted {
            self.lookahead = self.next_record()?;
            self.source_exhausted = self.lookahead.is_none();
        }
        Ok(())
    }

    fn next_record(&mut self) -> Result<Option<TaskJsonPendingRecord>> {
        let mut scan = self.position;
        loop {
            if scan.phase == TASK_JSON_DONE_PHASE {
                return Ok(None);
            }
            if scan.phase == TASK_JSON_TERMINAL_PHASE {
                let ordinal = scan.ordinal;
                let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "task JSON captured record ordinal overflowed",
                ))?;
                let record = CapturedRecord::content(
                    ordinal,
                    task_json_locator(
                        TASK_JSON_TERMINAL_PHASE,
                        TaskJsonRecordClass::Terminal,
                        0,
                        0,
                    )?,
                    self.record_kind.clone(),
                    Vec::new(),
                )
                .map_err(task_json_captured_batch_error)?;
                return Ok(Some(TaskJsonPendingRecord {
                    record,
                    after: TaskJsonStreamPosition::done(next_ordinal),
                }));
            }

            let phase = TaskJsonMessagePhase::decode(scan.phase)?;
            let Some(source) = self
                .message_sources
                .iter()
                .find(|source| source.phase == phase)
                .cloned()
            else {
                scan = scan.next_phase();
                continue;
            };
            if self.reader.is_none() {
                let mut reader = TaskJsonMessageReader::open(
                    &source.path,
                    &source.frozen,
                    scan.offset,
                    scan.container,
                )?;
                if scan.offset == 0 {
                    match reader.locate_array()? {
                        TaskJsonArrayLocation::Found(container) => {
                            scan.container = container;
                            scan.offset = reader.offset();
                        }
                        TaskJsonArrayLocation::NoArray => {
                            scan = scan.next_phase();
                            continue;
                        }
                        TaskJsonArrayLocation::Malformed(reason) => {
                            return self.file_error_record(
                                &source,
                                scan,
                                format!("{}: {reason}", source.path.display()),
                            );
                        }
                    }
                }
                self.reader = Some(reader);
            }
            let read = self
                .reader
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "task JSON producer lost its message reader",
                ))?
                .next_item()?;
            match read {
                TaskJsonMessageRead::End => {
                    self.reader = None;
                    scan = scan.next_phase();
                }
                TaskJsonMessageRead::Malformed(reason) => {
                    return self.file_error_record(
                        &source,
                        scan,
                        format!("{}: {reason}", source.path.display()),
                    );
                }
                TaskJsonMessageRead::RejectedItem {
                    reason,
                    start,
                    file_done,
                } => {
                    let ordinal = scan.ordinal;
                    let native_index = scan.native_index;
                    let next_ordinal =
                        ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                            "task JSON captured record ordinal overflowed",
                        ))?;
                    let after = if file_done {
                        TaskJsonStreamPosition::terminal(next_ordinal)
                            .with_phase((phase as u8).saturating_add(1))
                    } else {
                        let offset = self
                            .reader
                            .as_ref()
                            .ok_or(CaptureError::SystemInvariant(
                                "task JSON producer lost its message reader offset",
                            ))?
                            .offset();
                        TaskJsonStreamPosition {
                            phase: phase as u8,
                            container: scan.container,
                            offset,
                            native_index: native_index.checked_add(1).ok_or(
                                CaptureError::SystemInvariant(
                                    "task JSON native message index overflowed",
                                ),
                            )?,
                            ordinal: next_ordinal,
                        }
                    };
                    let record = CapturedRecord::content(
                        ordinal,
                        task_json_locator(
                            phase as u8,
                            TaskJsonRecordClass::FileError,
                            native_index,
                            start,
                        )?,
                        self.record_kind.clone(),
                        reason.into_bytes(),
                    )
                    .map_err(task_json_captured_batch_error)?;
                    if file_done {
                        self.reader = None;
                    }
                    return Ok(Some(TaskJsonPendingRecord { record, after }));
                }
                TaskJsonMessageRead::Item {
                    bytes,
                    start,
                    file_done,
                } => {
                    let ordinal = scan.ordinal;
                    let native_index = scan.native_index;
                    let next_ordinal =
                        ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                            "task JSON captured record ordinal overflowed",
                        ))?;
                    let after = if file_done {
                        TaskJsonStreamPosition {
                            ordinal: next_ordinal,
                            ..TaskJsonStreamPosition::terminal(next_ordinal)
                        }
                        .with_phase((phase as u8).saturating_add(1))
                    } else {
                        let offset = self
                            .reader
                            .as_ref()
                            .ok_or(CaptureError::SystemInvariant(
                                "task JSON producer lost its message reader offset",
                            ))?
                            .offset();
                        TaskJsonStreamPosition {
                            phase: phase as u8,
                            container: scan.container,
                            offset,
                            native_index: native_index.checked_add(1).ok_or(
                                CaptureError::SystemInvariant(
                                    "task JSON native message index overflowed",
                                ),
                            )?,
                            ordinal: next_ordinal,
                        }
                    };
                    let record = CapturedRecord::content(
                        ordinal,
                        task_json_locator(
                            phase as u8,
                            TaskJsonRecordClass::Event,
                            native_index,
                            start,
                        )?,
                        self.record_kind.clone(),
                        bytes,
                    )
                    .map_err(task_json_captured_batch_error)?;
                    if file_done {
                        self.reader = None;
                    }
                    return Ok(Some(TaskJsonPendingRecord { record, after }));
                }
            }
        }
    }

    fn file_error_record(
        &mut self,
        source: &TaskJsonMessageSource,
        position: TaskJsonStreamPosition,
        reason: String,
    ) -> Result<Option<TaskJsonPendingRecord>> {
        let ordinal = position.ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "task JSON captured record ordinal overflowed",
        ))?;
        let record = CapturedRecord::content(
            ordinal,
            task_json_locator(
                source.phase as u8,
                TaskJsonRecordClass::FileError,
                position.native_index,
                position.offset,
            )?,
            self.record_kind.clone(),
            reason.into_bytes(),
        )
        .map_err(task_json_captured_batch_error)?;
        self.reader = None;
        Ok(Some(TaskJsonPendingRecord {
            record,
            after: TaskJsonStreamPosition::terminal(next_ordinal)
                .with_phase((source.phase as u8).saturating_add(1)),
        }))
    }
}

impl TaskJsonStreamPosition {
    fn with_phase(mut self, phase: u8) -> Self {
        self.phase = phase.min(TASK_JSON_TERMINAL_PHASE);
        self
    }

    fn next_phase(self) -> Self {
        Self::terminal(self.ordinal).with_phase(self.phase.saturating_add(1))
    }
}
