use super::*;

#[derive(Debug)]
pub(in super::super) struct ParsedItem {
    pub(in super::super) checkpoint: ClineItemCheckpoint,
    pub(in super::super) rows: Vec<ClineEventRow>,
    pub(in super::super) outputs: Vec<ProOutputObservation>,
    pub(in super::super) rejection: Option<ClineItemRejection>,
    pub(in super::super) transient_rejections: Vec<ClineItemRejection>,
    pub(in super::super) core_bytes: usize,
}

pub(in super::super) struct ClineArrayScanner {
    reader: BufReader<File>,
    observation: ClineComponentObservation,
    offset: u64,
    started: bool,
    finished: bool,
    native_index: u64,
    revision_sha256: [u8; 32],
}

pub(in super::super) enum ClineArrayScanStep {
    Item(ClineScannedItem),
    EmptyTerminal { complete_bytes: u64 },
}

pub(in super::super) struct ClineScannedItem {
    bytes: Option<Vec<u8>>,
    pub(in super::super) native_index: u64,
    pub(in super::super) byte_start: u64,
    pub(in super::super) observed_bytes: u64,
    pub(in super::super) terminal: bool,
    pub(in super::super) complete_bytes: Option<u64>,
}

impl ClineArrayScanner {
    pub(in super::super) fn open(
        observation: &ClineComponentObservation,
        stats: &mut ClinePublicationStats,
    ) -> Result<Self, ClineLocalReadError> {
        let Some(expected) = observation.stamp() else {
            return Err(local_failure(
                observation,
                super::super::normalize::ClineComponentFailureKind::LocalIo,
                "component is not available for streaming",
                true,
            ));
        };
        if let Some(error) =
            injected_io_failure(ClineInjectedIoOperation::ComponentOpen, &observation.path)
        {
            return Err(classify_io_error(
                observation,
                "open component stream",
                error,
            ));
        }
        let before = observe_ordinary_file(&observation.path).map_err(|error| {
            classify_capture_error(observation, "observe component before streaming", error)
        })?;
        if &before != expected.ordinary() {
            return Err(ClineLocalReadError::Local(source_changed_failure(
                observation,
            )));
        }
        let file = open_ordinary_file_without_following(&observation.path)
            .map_err(|error| classify_capture_error(observation, "open component stream", error))?;
        let declared_len = file
            .metadata()
            .map_err(|error| classify_io_error(observation, "stat open component stream", error))?
            .len();
        if declared_len != expected.len() {
            return Err(ClineLocalReadError::Local(source_changed_failure(
                observation,
            )));
        }
        let after_open = observe_ordinary_file(&observation.path).map_err(|error| {
            classify_capture_error(observation, "revalidate opened component stream", error)
        })?;
        if &after_open != expected.ordinary() {
            return Err(ClineLocalReadError::Local(source_changed_failure(
                observation,
            )));
        }
        let mut revision = Sha256::new();
        revision.update(b"ctx-cline-nativepath-observed-revision-v1\0");
        revision.update([observation.component as u8]);
        revision.update(expected.len().to_le_bytes());
        revision.update(expected.ordinary().token());
        stats.component_hydrations = stats.component_hydrations.saturating_add(1);
        stats.component_parse_passes = stats.component_parse_passes.saturating_add(1);
        Ok(Self {
            reader: BufReader::new(file),
            observation: observation.clone(),
            offset: 0,
            started: false,
            finished: false,
            native_index: 0,
            revision_sha256: revision.finalize().into(),
        })
    }

    pub(in super::super) fn revision_sha256(&self) -> [u8; 32] {
        self.revision_sha256
    }

    pub(in super::super) fn descriptor_matches_observation(
        &self,
    ) -> Result<bool, ClineLocalReadError> {
        let expected = self
            .observation
            .stamp()
            .ok_or_else(|| ClineLocalReadError::Local(source_changed_failure(&self.observation)))?;
        let current_len = self
            .reader
            .get_ref()
            .metadata()
            .map_err(|error| {
                classify_io_error(&self.observation, "stat pinned component stream", error)
            })?
            .len();
        Ok(current_len == expected.len())
    }

    pub(in super::super) fn next_step(
        &mut self,
    ) -> Result<ClineArrayScanStep, ClineLocalReadError> {
        if self.finished {
            return Err(ClineLocalReadError::Fatal(
                ClineNativePathError::Invariant {
                    message: "Cline array scanner advanced after its terminal boundary".to_owned(),
                },
            ));
        }
        if let Some(error) = injected_io_failure(
            ClineInjectedIoOperation::ComponentRead,
            &self.observation.path,
        ) {
            return Err(classify_io_error(
                &self.observation,
                "read component stream",
                error,
            ));
        }
        if !self.started {
            self.skip_whitespace()?;
            match self.read_byte()? {
                Some(b'[') => self.started = true,
                None => return Err(self.structure_failure("Cline history array is empty", true)),
                Some(_) => {
                    return Err(self.structure_failure(
                        "Cline history component is not a top-level JSON array",
                        false,
                    ));
                }
            }
        }
        self.skip_whitespace()?;
        match self.peek_byte()? {
            Some(b']') => {
                self.read_byte()?;
                self.finish_document()?;
                self.finished = true;
                Ok(ClineArrayScanStep::EmptyTerminal {
                    complete_bytes: self.offset,
                })
            }
            None => Err(self
                .structure_failure("Cline history array ended before its closing bracket", true)),
            Some(_) => self.scan_item(),
        }
    }

    fn scan_item(&mut self) -> Result<ClineArrayScanStep, ClineLocalReadError> {
        let byte_start = self.offset;
        let first = self.read_byte()?.ok_or_else(|| {
            self.structure_failure("Cline history array ended before an item", true)
        })?;
        let mut bytes = Vec::with_capacity(64 * 1024);
        retain_item_byte(&mut bytes, first);
        let complete = match first {
            b'"' => self.scan_string(&mut bytes)?,
            b'[' | b'{' => self.scan_composite(&mut bytes)?,
            _ => {
                while let Some(byte) = self.peek_byte()? {
                    if byte.is_ascii_whitespace() || matches!(byte, b',' | b']') {
                        break;
                    }
                    let byte = self.read_byte()?.ok_or_else(|| {
                        self.structure_failure("Cline scanner lost a peeked item byte", false)
                    })?;
                    retain_item_byte(&mut bytes, byte);
                }
                true
            }
        };
        if !complete {
            return Err(self.structure_failure(
                "Cline history item ended before its closing delimiter",
                true,
            ));
        }
        let byte_end_exclusive = self.offset;
        let observed_bytes = byte_end_exclusive.saturating_sub(byte_start);
        self.skip_whitespace()?;
        let terminal = match self.read_byte()? {
            Some(b',') => {
                self.skip_whitespace()?;
                if self.peek_byte()? == Some(b']') {
                    return Err(
                        self.structure_failure("Cline history array has a trailing comma", false)
                    );
                }
                false
            }
            Some(b']') => {
                self.finish_document()?;
                self.finished = true;
                true
            }
            None => {
                return Err(
                    self.structure_failure("Cline history array ended after a complete item", true)
                );
            }
            Some(_) => {
                return Err(self.structure_failure(
                    "Cline history array has an invalid item separator",
                    false,
                ));
            }
        };
        let native_index = self.native_index;
        self.native_index = self.native_index.checked_add(1).ok_or_else(|| {
            ClineLocalReadError::Fatal(ClineNativePathError::Invariant {
                message: "Cline native item index overflowed".to_owned(),
            })
        })?;
        Ok(ClineArrayScanStep::Item(ClineScannedItem {
            bytes: (u64::try_from(bytes.len()).ok() == Some(observed_bytes)).then_some(bytes),
            native_index,
            byte_start,
            observed_bytes,
            terminal,
            complete_bytes: terminal.then_some(self.offset),
        }))
    }

    fn finish_document(&mut self) -> Result<(), ClineLocalReadError> {
        self.skip_whitespace()?;
        if self.peek_byte()?.is_some() {
            return Err(self.structure_failure("Cline history array has trailing JSON data", false));
        }
        Ok(())
    }

    fn scan_string(&mut self, bytes: &mut Vec<u8>) -> Result<bool, ClineLocalReadError> {
        let mut escaped = false;
        while let Some(byte) = self.read_byte()? {
            retain_item_byte(bytes, byte);
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

    fn scan_composite(&mut self, bytes: &mut Vec<u8>) -> Result<bool, ClineLocalReadError> {
        let mut depth = 1_u64;
        let mut in_string = false;
        let mut escaped = false;
        while let Some(byte) = self.read_byte()? {
            retain_item_byte(bytes, byte);
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
                    depth = depth.checked_add(1).ok_or_else(|| {
                        ClineLocalReadError::Fatal(ClineNativePathError::Invariant {
                            message: "Cline JSON nesting depth overflowed".to_owned(),
                        })
                    })?;
                }
                b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        Ok(false)
    }

    fn skip_whitespace(&mut self) -> Result<(), ClineLocalReadError> {
        while self
            .peek_byte()?
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.read_byte()?;
        }
        Ok(())
    }

    fn peek_byte(&mut self) -> Result<Option<u8>, ClineLocalReadError> {
        self.reader
            .fill_buf()
            .map(|buffer| buffer.first().copied())
            .map_err(|error| classify_io_error(&self.observation, "read component stream", error))
    }

    fn read_byte(&mut self) -> Result<Option<u8>, ClineLocalReadError> {
        let Some(byte) = self.peek_byte()? else {
            return Ok(None);
        };
        self.reader.consume(1);
        self.offset = self.offset.checked_add(1).ok_or_else(|| {
            ClineLocalReadError::Fatal(ClineNativePathError::Invariant {
                message: "Cline component byte offset overflowed".to_owned(),
            })
        })?;
        Ok(Some(byte))
    }

    fn structure_failure(&self, message: &str, retryable: bool) -> ClineLocalReadError {
        local_failure(
            &self.observation,
            if retryable {
                super::super::normalize::ClineComponentFailureKind::IncompleteJson
            } else {
                super::super::normalize::ClineComponentFailureKind::MalformedJson
            },
            message,
            retryable,
        )
    }
}

fn retain_item_byte(bytes: &mut Vec<u8>, byte: u8) {
    if bytes.len() < MAX_ARRAY_ITEM_BYTES {
        bytes.push(byte);
    }
}

#[allow(clippy::too_many_arguments)]
pub(in super::super) fn parse_scanned_item(
    scanned: ClineScannedItem,
    source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    component: ClineEventComponent,
    profile: ClineNativeProfile,
    max_item_units: usize,
    native_id_occurrences: &mut BTreeMap<String, u64>,
    stats: &mut ClinePublicationStats,
) -> ParsedItem {
    stats.array_item_parse_attempts = stats.array_item_parse_attempts.saturating_add(1);
    stats.max_array_item_bytes_retained = stats.max_array_item_bytes_retained.max(
        scanned
            .bytes
            .as_ref()
            .map_or(MAX_ARRAY_ITEM_BYTES, Vec::len),
    );
    let Some(bytes) = scanned.bytes else {
        return rejected_item(
            component,
            scanned.native_index,
            None,
            scanned.observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            &format!(
                "Cline native item exceeds the {MAX_ARRAY_ITEM_BYTES}-byte streaming item bound"
            ),
            stats,
        );
    };
    let raw = match serde_json::from_slice::<&RawValue>(&bytes) {
        Ok(raw) => raw,
        Err(error) => {
            return rejected_item(
                component,
                scanned.native_index,
                None,
                scanned.observed_bytes,
                ClineItemRejectionKind::MalformedRecord,
                &error.to_string(),
                stats,
            );
        }
    };
    parse_item(
        raw,
        ItemParseContext {
            source,
            identity,
            component,
            profile,
            max_item_units,
        },
        scanned.native_index,
        scanned.byte_start,
        native_id_occurrences,
        stats,
    )
}
