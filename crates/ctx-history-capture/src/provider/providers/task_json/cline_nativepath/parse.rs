use std::{
    fmt,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};

use chrono::DateTime;
use serde::{
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{value::RawValue, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::file_touches::{
        visit_provider_file_touch_drafts_with_limit, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
    },
    provider_sources::{observe_ordinary_file, open_ordinary_file_without_following},
    OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceLocator, ProOutputObservation,
};

use super::{
    bounded::BoundedString,
    normalize::{
        estimated_event_bytes, estimated_output_bytes, ClineCatalogEntry, ClineEventComponent,
        ClineEventContext, ClineEventKind, ClineEventRole, ClineEventRow, ClineFileSourceIdentity,
        ClineFileTouch, ClineItemCheckpoint, ClineItemRejection, ClineItemRejectionKind,
        ClineMetadataCheckpoint, ClineNativeItemKey, ClineNativeProfile, ClinePublicationStats,
        ClineSessionRow, ClineSparseOutputDiagnostic, ClineTaskIdentity, ClineTaskIdentityOrigin,
        CLINE_NATIVE_CORE_PAGE_MAX_BYTES, CLINE_NATIVE_MAX_FAILURE_PREVIEW_BYTES,
        CLINE_NATIVE_MAX_REJECTIONS, CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES,
        CLINE_NATIVE_PAGE_MAX_UNITS, CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES,
    },
    source::{
        capture_source_error, injected_io_failure, is_component_local_error, source_io,
        ClineComponent, ClineComponentObservation, ClineInjectedIoOperation,
    },
    ClineNativePathError,
};

const COMPONENT_REVISION_DOMAIN: &[u8] = b"ctx-cline-nativepath-component-v2\0";
const MAX_METADATA_JSON_BYTES: usize = 1024 * 1024;
const MAX_ROOT_INDEX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_NATIVE_ID_BYTES: usize = 512;
const MAX_SMALL_FIELD_BYTES: usize = 4 * 1024;
const MAX_METADATA_TEXT_BYTES: usize = 64 * 1024;
const MAX_JSON_KEY_BYTES: usize = 128;
const MAX_OUTPUT_BODY_RAW_BYTES: usize = CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES;
const MAX_ARRAY_ITEM_BYTES: usize = crate::MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_EXPLICIT_RESULT_DEPTH: usize = 64;

#[derive(Debug)]
pub(super) struct HydratedComponent {
    pub(super) bytes: Vec<u8>,
    pub(super) content_sha256: [u8; 32],
    pinned_file: File,
}

pub(super) struct ClinePinnedContentAuthority {
    file: File,
    observation: ClineComponentObservation,
    content_sha256: [u8; 32],
}

impl ClinePinnedContentAuthority {
    pub(super) fn verify_content(&mut self) -> Result<bool, ClineLocalReadError> {
        let Some(expected) = self.observation.stamp() else {
            return Ok(false);
        };
        let before_len = self
            .file
            .metadata()
            .map_err(|error| {
                classify_io_error(&self.observation, "stat pinned metadata component", error)
            })?
            .len();
        if before_len != expected.len() {
            return Ok(false);
        }
        self.file.seek(SeekFrom::Start(0)).map_err(|error| {
            classify_io_error(&self.observation, "seek pinned metadata component", error)
        })?;
        let mut hasher = Sha256::new();
        hasher.update(COMPONENT_REVISION_DOMAIN);
        hasher.update([self.observation.component as u8]);
        let mut observed = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            if let Some(error) = injected_io_failure(
                ClineInjectedIoOperation::ComponentRead,
                &self.observation.path,
            ) {
                return Err(classify_io_error(
                    &self.observation,
                    "certify pinned metadata component",
                    error,
                ));
            }
            let read = self.file.read(&mut buffer).map_err(|error| {
                classify_io_error(
                    &self.observation,
                    "certify pinned metadata component",
                    error,
                )
            })?;
            if read == 0 {
                break;
            }
            observed = observed.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            if observed > expected.len() {
                return Ok(false);
            }
            hasher.update(&buffer[..read]);
        }
        let after_len = self
            .file
            .metadata()
            .map_err(|error| {
                classify_io_error(&self.observation, "restat pinned metadata component", error)
            })?
            .len();
        Ok(observed == expected.len()
            && after_len == expected.len()
            && <[u8; 32]>::from(hasher.finalize()) == self.content_sha256)
    }
}

impl HydratedComponent {
    pub(super) fn into_pinned_authority(
        self,
        observation: &ClineComponentObservation,
    ) -> ClinePinnedContentAuthority {
        ClinePinnedContentAuthority {
            file: self.pinned_file,
            observation: observation.clone(),
            content_sha256: self.content_sha256,
        }
    }
}

#[derive(Debug)]
pub(super) enum ClineLocalReadError {
    Local(super::normalize::ClineComponentFailure),
    Fatal(ClineNativePathError),
}

impl From<ClineNativePathError> for ClineLocalReadError {
    fn from(error: ClineNativePathError) -> Self {
        Self::Fatal(error)
    }
}

pub(super) fn hydrate_component(
    observation: &ClineComponentObservation,
    stats: &mut ClinePublicationStats,
) -> Result<HydratedComponent, ClineLocalReadError> {
    let Some(expected) = observation.stamp() else {
        return Err(local_failure(
            observation,
            super::normalize::ClineComponentFailureKind::LocalIo,
            "component is not available for hydration",
            true,
        ));
    };
    let max_bytes = match observation.component {
        ClineComponent::TaskMetadata | ClineComponent::HistoryItem | ClineComponent::TaskIndex => {
            MAX_METADATA_JSON_BYTES
        }
        ClineComponent::RootIndex => MAX_ROOT_INDEX_JSON_BYTES,
        ClineComponent::ApiHistory
        | ClineComponent::UiMessages
        | ClineComponent::FallbackHistory => {
            return Err(ClineLocalReadError::Fatal(
                ClineNativePathError::Invariant {
                    message: "Cline history arrays must use the pinned streaming component reader"
                        .to_owned(),
                },
            ));
        }
    };
    if expected.len() > max_bytes as u64 {
        return Err(local_failure(
            observation,
            super::normalize::ClineComponentFailureKind::AuthorityBound,
            &format!("component exceeds the fixed {max_bytes}-byte authority bound"),
            false,
        ));
    }
    if let Some(error) =
        injected_io_failure(ClineInjectedIoOperation::ComponentOpen, &observation.path)
    {
        return Err(classify_io_error(observation, "open component", error));
    }
    let before = observe_ordinary_file(&observation.path).map_err(|error| {
        classify_capture_error(observation, "observe component before hydration", error)
    })?;
    if &before != expected.ordinary() {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    let mut file = open_ordinary_file_without_following(&observation.path)
        .map_err(|error| classify_capture_error(observation, "open component", error))?;
    let declared_len = file
        .metadata()
        .map_err(|error| classify_io_error(observation, "stat open component", error))?
        .len();
    if declared_len != expected.len() {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    let capacity = usize::try_from(declared_len)
        .unwrap_or(max_bytes)
        .min(max_bytes);
    let mut bytes = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if let Some(error) =
            injected_io_failure(ClineInjectedIoOperation::ComponentRead, &observation.path)
        {
            return Err(classify_io_error(observation, "read component", error));
        }
        let read = file
            .read(&mut buffer)
            .map_err(|error| classify_io_error(observation, "read component", error))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > max_bytes {
            return Err(local_failure(
                observation,
                super::normalize::ClineComponentFailureKind::AuthorityBound,
                &format!("component exceeds the fixed {max_bytes}-byte authority bound"),
                false,
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let after = observe_ordinary_file(&observation.path).map_err(|error| {
        classify_capture_error(observation, "observe component after hydration", error)
    })?;
    if after != before || bytes.len() as u64 != declared_len {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    let mut hasher = Sha256::new();
    hasher.update(COMPONENT_REVISION_DOMAIN);
    hasher.update([observation.component as u8]);
    hasher.update(&bytes);
    stats.component_hydrations = stats.component_hydrations.saturating_add(1);
    Ok(HydratedComponent {
        bytes,
        content_sha256: hasher.finalize().into(),
        pinned_file: file,
    })
}

pub(super) fn pin_component_content(
    observation: &ClineComponentObservation,
    content_sha256: [u8; 32],
) -> Result<ClinePinnedContentAuthority, ClineLocalReadError> {
    let Some(expected) = observation.stamp() else {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    };
    let before = observe_ordinary_file(&observation.path).map_err(|error| {
        classify_capture_error(
            observation,
            "observe metadata component before authority pin",
            error,
        )
    })?;
    if &before != expected.ordinary() {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    if let Some(error) =
        injected_io_failure(ClineInjectedIoOperation::ComponentOpen, &observation.path)
    {
        return Err(classify_io_error(
            observation,
            "pin metadata component authority",
            error,
        ));
    }
    let file = open_ordinary_file_without_following(&observation.path).map_err(|error| {
        classify_capture_error(observation, "pin metadata component authority", error)
    })?;
    if file
        .metadata()
        .map_err(|error| classify_io_error(observation, "stat pinned metadata component", error))?
        .len()
        != expected.len()
    {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    let after = observe_ordinary_file(&observation.path).map_err(|error| {
        classify_capture_error(observation, "revalidate pinned metadata component", error)
    })?;
    if after != before {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    let mut authority = ClinePinnedContentAuthority {
        file,
        observation: observation.clone(),
        content_sha256,
    };
    if !authority.verify_content()? {
        return Err(ClineLocalReadError::Local(source_changed_failure(
            observation,
        )));
    }
    Ok(authority)
}

fn classify_capture_error(
    observation: &ClineComponentObservation,
    operation: &'static str,
    error: crate::CaptureError,
) -> ClineLocalReadError {
    let error = capture_source_error(&observation.path, operation, error);
    classify_native_error(observation, error)
}

fn classify_io_error(
    observation: &ClineComponentObservation,
    operation: &'static str,
    error: std::io::Error,
) -> ClineLocalReadError {
    classify_native_error(observation, source_io(&observation.path, operation, error))
}

fn classify_native_error(
    observation: &ClineComponentObservation,
    error: ClineNativePathError,
) -> ClineLocalReadError {
    if is_component_local_error(&error) {
        local_failure(
            observation,
            super::normalize::ClineComponentFailureKind::LocalIo,
            &error.to_string(),
            true,
        )
    } else {
        ClineLocalReadError::Fatal(error)
    }
}

fn local_failure(
    observation: &ClineComponentObservation,
    kind: super::normalize::ClineComponentFailureKind,
    message: &str,
    retryable: bool,
) -> ClineLocalReadError {
    ClineLocalReadError::Local(super::normalize::ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind,
        message: bounded_detail(message),
        retryable,
    })
}

fn source_changed_failure(
    observation: &ClineComponentObservation,
) -> super::normalize::ClineComponentFailure {
    super::normalize::ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind: super::normalize::ClineComponentFailureKind::SourceChanged,
        message: "component changed before publication authority was certified".into(),
        retryable: true,
    }
}

pub(super) fn parse_metadata(
    hydrated: &HydratedComponent,
    observation: &ClineComponentObservation,
    directory_task_id: &str,
    stats: &mut ClinePublicationStats,
) -> Result<ClineMetadataCheckpoint, super::normalize::ClineComponentFailure> {
    stats.component_parse_passes = stats.component_parse_passes.saturating_add(1);
    let parsed = serde_json::from_slice::<RawMetadata>(&hydrated.bytes)
        .map_err(|error| parse_failure(observation, &error, "malformed Cline task metadata"))?;
    if parsed.task_id.as_ref().is_some_and(|bounded| bounded.1) {
        return Err(super::normalize::ClineComponentFailure {
            component: observation.component,
            path: observation.path.clone(),
            kind: super::normalize::ClineComponentFailureKind::AuthorityBound,
            message: "Cline task metadata identity exceeds the 512-byte authority bound".into(),
            retryable: false,
        });
    }
    let metadata_task_id = parsed
        .task_id
        .and_then(|value| value.0)
        .filter(|value| valid_identity(value));
    let identity_origin = if metadata_task_id.is_some() {
        ClineTaskIdentityOrigin::TaskMetadata
    } else {
        ClineTaskIdentityOrigin::DirectoryNameDegraded
    };
    let task_id = metadata_task_id.unwrap_or_else(|| directory_task_id.to_owned());
    let session = ClineSessionRow::new(
        ClineTaskIdentity::new(task_id.into_boxed_str()),
        identity_origin,
        bounded_metadata(parsed.title),
        bounded_metadata(parsed.workspace_directory),
        bounded_metadata(parsed.created_at),
        bounded_metadata(parsed.last_modified),
        bounded_small(parsed.model_id),
        bounded_small(parsed.model_provider),
        parsed.tokens_input,
        parsed.tokens_output,
    );
    Ok(ClineMetadataCheckpoint {
        observation: observation.clone(),
        content_sha256: Some(hydrated.content_sha256),
        session,
    })
}

#[derive(Deserialize)]
struct RawMetadata {
    #[serde(default, alias = "taskId", alias = "id")]
    task_id: Option<BoundedString<MAX_NATIVE_ID_BYTES>>,
    #[serde(default, alias = "task", alias = "summary", alias = "name")]
    title: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>,
    #[serde(default, alias = "workspaceDirectory", alias = "cwd")]
    workspace_directory: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>,
    #[serde(default, alias = "createdAt", alias = "ts")]
    created_at: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>,
    #[serde(default, alias = "lastModified", alias = "updatedAt")]
    last_modified: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>,
    #[serde(default, alias = "modelId", alias = "model")]
    model_id: Option<BoundedString<MAX_SMALL_FIELD_BYTES>>,
    #[serde(default, alias = "modelProvider", alias = "provider")]
    model_provider: Option<BoundedString<MAX_SMALL_FIELD_BYTES>>,
    #[serde(default, alias = "inputTokens", alias = "tokensIn")]
    tokens_input: Option<u64>,
    #[serde(default, alias = "outputTokens", alias = "tokensOut")]
    tokens_output: Option<u64>,
}

fn bounded_metadata(value: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>) -> Option<Box<str>> {
    value
        .and_then(|value| value.0)
        .filter(|value| !value.contains('\0'))
        .map(String::into_boxed_str)
}

fn bounded_small(value: Option<BoundedString<MAX_SMALL_FIELD_BYTES>>) -> Option<Box<str>> {
    value
        .and_then(|value| value.0)
        .filter(|value| !value.chars().any(char::is_control))
        .map(String::into_boxed_str)
}

#[derive(Debug)]
pub(super) struct ParsedItem {
    pub(super) checkpoint: ClineItemCheckpoint,
    pub(super) rows: Vec<ClineEventRow>,
    pub(super) outputs: Vec<ProOutputObservation>,
    pub(super) rejection: Option<ClineItemRejection>,
    pub(super) transient_rejections: Vec<ClineItemRejection>,
    pub(super) core_bytes: usize,
}

pub(super) struct ClineArrayScanner {
    reader: BufReader<File>,
    observation: ClineComponentObservation,
    offset: u64,
    started: bool,
    finished: bool,
    native_index: u64,
    revision_sha256: [u8; 32],
}

pub(super) enum ClineArrayScanStep {
    Item(ClineScannedItem),
    EmptyTerminal { complete_bytes: u64 },
}

pub(super) struct ClineScannedItem {
    bytes: Option<Vec<u8>>,
    pub(super) native_index: u64,
    pub(super) byte_start: u64,
    pub(super) observed_bytes: u64,
    pub(super) terminal: bool,
    pub(super) complete_bytes: Option<u64>,
}

impl ClineArrayScanner {
    pub(super) fn open(
        observation: &ClineComponentObservation,
        stats: &mut ClinePublicationStats,
    ) -> Result<Self, ClineLocalReadError> {
        let Some(expected) = observation.stamp() else {
            return Err(local_failure(
                observation,
                super::normalize::ClineComponentFailureKind::LocalIo,
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

    pub(super) fn revision_sha256(&self) -> [u8; 32] {
        self.revision_sha256
    }

    pub(super) fn descriptor_matches_observation(&self) -> Result<bool, ClineLocalReadError> {
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

    pub(super) fn next_step(&mut self) -> Result<ClineArrayScanStep, ClineLocalReadError> {
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
                super::normalize::ClineComponentFailureKind::IncompleteJson
            } else {
                super::normalize::ClineComponentFailureKind::MalformedJson
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

pub(super) fn parse_scanned_item(
    scanned: ClineScannedItem,
    source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    component: ClineEventComponent,
    profile: ClineNativeProfile,
    max_item_units: usize,
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
        stats,
    )
}

#[derive(Clone, Copy)]
struct ItemParseContext<'a> {
    source: &'a ClineFileSourceIdentity,
    identity: &'a ClineTaskIdentity,
    component: ClineEventComponent,
    profile: ClineNativeProfile,
    max_item_units: usize,
}

#[derive(Default)]
struct RawEnvelope<'a> {
    native_id: Option<String>,
    role: Option<String>,
    item_type: Option<String>,
    kind: Option<String>,
    say: Option<String>,
    ask: Option<String>,
    name: Option<String>,
    call_id: Option<String>,
    occurred_at_millis: Option<i64>,
    content: Option<&'a RawValue>,
    text: Option<&'a RawValue>,
    message: Option<&'a RawValue>,
    output: Option<&'a RawValue>,
    result: Option<&'a RawValue>,
    response: Option<&'a RawValue>,
    timed_out: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    explicit_failure: bool,
    explicit_success: bool,
    status: Option<String>,
    conflicting_discriminator: bool,
    oversized_discriminator: bool,
}

impl<'a> RawEnvelope<'a> {
    fn direct_result_body(&self) -> Option<&'a RawValue> {
        self.output
            .or(self.result)
            .or(self.text)
            .or(self.content)
            .or(self.message)
            .or(self.response)
    }

    fn block_result_body(&self) -> Option<&'a RawValue> {
        self.content
            .or(self.result)
            .or(self.output)
            .or(self.text)
            .or(self.response)
            .or(self.message)
    }

    fn retained_body(&self) -> Option<&'a RawValue> {
        self.text.or(self.message).or(self.content)
    }

    fn normalized_discriminators(&self) -> impl Iterator<Item = String> + '_ {
        [
            self.role.as_deref(),
            self.item_type.as_deref(),
            self.kind.as_deref(),
            self.say.as_deref(),
            self.ask.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(normalize_discriminator)
    }

    fn outcome(&self) -> OutputOutcomeMetadata {
        let status = self.status.as_deref().map(normalize_discriminator);
        let outcome = if self.timed_out
            || status
                .as_deref()
                .is_some_and(|value| matches!(value, "timeout" | "timedout"))
        {
            OutputOutcome::Timeout
        } else if self.exit_code.is_some_and(|code| code != 0)
            || self.explicit_failure
            || status.as_deref().is_some_and(status_is_failure)
        {
            OutputOutcome::Failure
        } else if self.exit_code == Some(0)
            || self.explicit_success
            || status.as_deref().is_some_and(status_is_success)
        {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        };
        OutputOutcomeMetadata {
            outcome,
            exit_code: self.exit_code,
            duration_ms: self.duration_ms,
        }
    }
}

impl<'de> Deserialize<'de> for RawEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawEnvelopeVisitor)
    }
}

struct RawEnvelopeVisitor;

impl<'de> Visitor<'de> for RawEnvelopeVisitor {
    type Value = RawEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline native item object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut envelope = RawEnvelope::default();
        let mut seen_id = false;
        let mut seen_role = false;
        let mut seen_type = false;
        let mut seen_kind = false;
        let mut seen_say = false;
        let mut seen_ask = false;
        while let Some(BoundedString(field, _)) =
            map.next_key::<BoundedString<MAX_JSON_KEY_BYTES>>()?
        {
            let Some(field) = field else {
                map.next_value::<IgnoredAny>()?;
                continue;
            };
            match field.as_str() {
                "id" | "uuid" | "messageId" => {
                    let value = map.next_value::<BoundedString<MAX_NATIVE_ID_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    let value = value.0;
                    if seen_id && envelope.native_id != value {
                        envelope.native_id = None;
                    } else if !seen_id {
                        envelope.native_id = value;
                    }
                    seen_id = true;
                }
                "role" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_role;
                    seen_role = true;
                    envelope.role = value.0;
                }
                "type" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_type;
                    seen_type = true;
                    envelope.item_type = value.0;
                }
                "kind" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_kind;
                    seen_kind = true;
                    envelope.kind = value.0;
                }
                "say" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_say;
                    seen_say = true;
                    envelope.say = value.0;
                }
                "ask" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_ask;
                    seen_ask = true;
                    envelope.ask = value.0;
                }
                "name" | "tool" | "tool_name" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    if envelope.name.is_none() {
                        envelope.name = value.0;
                    }
                }
                "tool_use_id" | "toolUseId" | "call_id" | "callId" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    if envelope.call_id.is_none() {
                        envelope.call_id = value.0;
                    }
                }
                "ts" | "timestamp" | "createdAt" => {
                    envelope.occurred_at_millis = map.next_value::<LooseTimestamp>()?.0;
                }
                "content" => set_raw_once(
                    &mut envelope.content,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "text" => set_raw_once(
                    &mut envelope.text,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "message" => set_raw_once(
                    &mut envelope.message,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "output" => set_raw_once(
                    &mut envelope.output,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "result" => set_raw_once(
                    &mut envelope.result,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "response" => set_raw_once(
                    &mut envelope.response,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "timed_out" | "timedOut" | "timeout" => {
                    envelope.timed_out |= map.next_value::<LooseBool>()?.0.unwrap_or(false);
                }
                "exit_code" | "exitCode" => {
                    envelope.exit_code = map.next_value::<LooseI32>()?.0;
                }
                "duration_ms" | "durationMs" => {
                    envelope.duration_ms = map.next_value::<LooseU64>()?.0;
                }
                "success" | "ok" => match map.next_value::<LooseBool>()?.0 {
                    Some(true) => envelope.explicit_success = true,
                    Some(false) => envelope.explicit_failure = true,
                    None => {}
                },
                "isError" | "is_error" | "failed" => {
                    envelope.explicit_failure |= map.next_value::<LooseBool>()?.0.unwrap_or(false);
                }
                "status" | "state" | "outcome" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.status = value.0;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }
}

fn set_raw_once<'a>(slot: &mut Option<&'a RawValue>, value: &'a RawValue, duplicate: &mut bool) {
    if slot.replace(value).is_some() {
        *duplicate = true;
    }
}

struct OutputCandidate<'a> {
    kind: OutputObservationKind,
    sub_index: u32,
    call_id: Option<String>,
    outcome: OutputOutcomeMetadata,
    body: Option<&'a RawValue>,
    occurred_at_millis: Option<i64>,
    byte_start: u64,
    byte_end_exclusive: u64,
}

struct OutputCandidateContext {
    kind: OutputObservationKind,
    base_sub_index: u32,
    call_id: Option<String>,
    outcome: OutputOutcomeMetadata,
    occurred_at_millis: Option<i64>,
    item_start: u64,
    fallback_start: u64,
}

fn push_explicit_outputs<'a>(
    raw_item: &'a RawValue,
    selected: Option<&'a RawValue>,
    context: OutputCandidateContext,
    outputs: &mut Vec<OutputCandidate<'a>>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let mut leaves = Vec::new();
    if let Some(selected) = selected {
        collect_explicit_output_leaves(selected, &mut leaves, 0)?;
    }
    for (inner_index, leaf) in leaves.into_iter().enumerate() {
        if inner_index >= CLINE_NATIVE_PAGE_MAX_UNITS {
            return Err((
                ClineItemRejectionKind::UnsupportedShape,
                "Cline result has more than 64 explicit inner outputs".to_owned(),
            ));
        }
        let leaf_start = (leaf.get().as_ptr() as usize)
            .checked_sub(raw_item.get().as_ptr() as usize)
            .and_then(|offset| context.item_start.checked_add(offset as u64))
            .unwrap_or(context.fallback_start);
        let leaf_end = leaf_start.saturating_add(leaf.get().len() as u64);
        outputs.push(OutputCandidate {
            kind: context.kind,
            sub_index: context
                .base_sub_index
                .saturating_add(u32::try_from(inner_index).unwrap_or(u32::MAX)),
            call_id: context.call_id.clone(),
            outcome: context.outcome.clone(),
            body: Some(leaf),
            occurred_at_millis: context.occurred_at_millis,
            byte_start: leaf_start,
            byte_end_exclusive: leaf_end,
        });
    }
    Ok(())
}

fn push_explicit_result_blocks<'a>(
    raw_item: &'a RawValue,
    content: &'a RawValue,
    kind: OutputObservationKind,
    outer: &RawEnvelope<'a>,
    item_byte_start: u64,
    outputs: &mut Vec<OutputCandidate<'a>>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let blocks = deserialize_bounded_raw_array(content, "Cline explicit result block array")?;
    for (index, raw_block) in blocks.into_iter().enumerate() {
        if !raw_block.get().trim_start().starts_with('{') {
            continue;
        }
        let block = serde_json::from_str::<RawEnvelope<'_>>(raw_block.get()).map_err(|error| {
            (
                ClineItemRejectionKind::MalformedRecord,
                format!("malformed Cline explicit result block: {error}"),
            )
        })?;
        if block.conflicting_discriminator || block.oversized_discriminator {
            return Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline explicit result block has conflicting or oversized discriminator fields"
                    .to_owned(),
            ));
        }
        if !block
            .normalized_discriminators()
            .any(|value| is_result_discriminator(&value))
        {
            continue;
        }
        let block_outcome = block.outcome();
        let outcome = if block_outcome.outcome == OutputOutcome::Unknown
            && block_outcome.exit_code.is_none()
            && block_outcome.duration_ms.is_none()
        {
            outer.outcome()
        } else {
            block_outcome
        };
        let block_start = (raw_block.get().as_ptr() as usize)
            .checked_sub(raw_item.get().as_ptr() as usize)
            .and_then(|offset| item_byte_start.checked_add(offset as u64))
            .unwrap_or(item_byte_start);
        push_explicit_outputs(
            raw_item,
            block.block_result_body(),
            OutputCandidateContext {
                kind,
                base_sub_index: u32::try_from(index)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(1_024),
                call_id: block.call_id.clone().or_else(|| outer.call_id.clone()),
                outcome,
                occurred_at_millis: block.occurred_at_millis.or(outer.occurred_at_millis),
                item_start: item_byte_start,
                fallback_start: block_start,
            },
            outputs,
        )?;
    }
    Ok(())
}

fn collect_explicit_output_leaves<'a>(
    raw: &'a RawValue,
    leaves: &mut Vec<&'a RawValue>,
    depth: usize,
) -> Result<(), (ClineItemRejectionKind, String)> {
    if depth >= MAX_EXPLICIT_RESULT_DEPTH {
        return Err((
            ClineItemRejectionKind::UnsupportedShape,
            "Cline explicit result exceeds the bounded nesting depth".to_owned(),
        ));
    }
    let text = raw.get().trim_start();
    if text == "null" {
        return Ok(());
    }
    if text.starts_with('[') {
        let items = deserialize_bounded_raw_array(raw, "explicit Cline result array")?;
        for item in items {
            if leaves.len() > CLINE_NATIVE_PAGE_MAX_UNITS {
                break;
            }
            let selected = if item.get().trim_start().starts_with('{') {
                serde_json::from_str::<RawExplicitInner<'_>>(item.get())
                    .map_err(|error| {
                        (
                            ClineItemRejectionKind::MalformedRecord,
                            format!("malformed explicit Cline result value: {error}"),
                        )
                    })?
                    .selected()
                    .unwrap_or(item)
            } else {
                item
            };
            collect_explicit_output_leaves(selected, leaves, depth.saturating_add(1))?;
        }
        return Ok(());
    }
    leaves.push(raw);
    Ok(())
}

fn deserialize_bounded_raw_array<'a>(
    raw: &'a RawValue,
    context: &'static str,
) -> Result<Vec<&'a RawValue>, (ClineItemRejectionKind, String)> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let values = deserializer
        .deserialize_seq(BoundedRawArrayVisitor)
        .map_err(|error| {
            let kind = if error.to_string().contains("more than 64") {
                ClineItemRejectionKind::UnsupportedShape
            } else {
                ClineItemRejectionKind::MalformedRecord
            };
            (kind, format!("malformed {context}: {error}"))
        })?;
    deserializer.end().map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("trailing {context} data: {error}"),
        )
    })?;
    Ok(values)
}

struct BoundedRawArrayVisitor;

impl<'de> Visitor<'de> for BoundedRawArrayVisitor {
    type Value = Vec<&'de RawValue>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline array with no more than 64 values")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(CLINE_NATIVE_PAGE_MAX_UNITS);
        while values.len() < CLINE_NATIVE_PAGE_MAX_UNITS {
            let Some(value) = sequence.next_element::<&RawValue>()? else {
                return Ok(values);
            };
            values.push(value);
        }
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Cline array has more than 64 independently publishable values",
            ));
        }
        Ok(values)
    }
}

#[derive(Default)]
struct RawExplicitInner<'a> {
    text: Option<&'a RawValue>,
    content: Option<&'a RawValue>,
    output: Option<&'a RawValue>,
    result: Option<&'a RawValue>,
}

impl<'a> RawExplicitInner<'a> {
    fn selected(&self) -> Option<&'a RawValue> {
        self.text.or(self.content).or(self.output).or(self.result)
    }
}

impl<'de> Deserialize<'de> for RawExplicitInner<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawExplicitInnerVisitor)
    }
}

struct RawExplicitInnerVisitor;

impl<'de> Visitor<'de> for RawExplicitInnerVisitor {
    type Value = RawExplicitInner<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an explicit Cline result object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut inner = RawExplicitInner::default();
        while let Some(BoundedString(field, _)) =
            map.next_key::<BoundedString<MAX_JSON_KEY_BYTES>>()?
        {
            match field.as_deref() {
                Some("text") if inner.text.is_none() => {
                    inner.text = Some(map.next_value::<&RawValue>()?);
                }
                Some("content") if inner.content.is_none() => {
                    inner.content = Some(map.next_value::<&RawValue>()?);
                }
                Some("output") if inner.output.is_none() => {
                    inner.output = Some(map.next_value::<&RawValue>()?);
                }
                Some("result") if inner.result.is_none() => {
                    inner.result = Some(map.next_value::<&RawValue>()?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(inner)
    }
}

fn parse_item(
    raw: &RawValue,
    context: ItemParseContext<'_>,
    native_index: u64,
    byte_start: u64,
    stats: &mut ClinePublicationStats,
) -> ParsedItem {
    let ItemParseContext {
        source,
        identity,
        component,
        profile,
        max_item_units,
    } = context;
    let observed_bytes = u64::try_from(raw.get().len()).unwrap_or(u64::MAX);
    let envelope = match serde_json::from_str::<RawEnvelope<'_>>(raw.get()) {
        Ok(envelope) => envelope,
        Err(error) => {
            return rejected_item(
                component,
                native_index,
                None,
                observed_bytes,
                ClineItemRejectionKind::MalformedRecord,
                &error.to_string(),
                stats,
            );
        }
    };
    let native_key = native_key(envelope.native_id.as_deref(), native_index);
    if envelope.conflicting_discriminator || envelope.oversized_discriminator {
        let kind = if envelope.conflicting_discriminator {
            ClineItemRejectionKind::ConflictingDiscriminator
        } else {
            ClineItemRejectionKind::OversizedRetainedItem
        };
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            kind,
            "Cline item has conflicting or oversized discriminator fields",
            native_key,
            stats,
        );
    }

    let parsed = match component {
        ClineEventComponent::ApiHistory | ClineEventComponent::FallbackHistory => {
            parse_api_projection(
                raw,
                &envelope,
                source,
                identity,
                &native_key,
                native_index,
                byte_start,
            )
        }
        ClineEventComponent::UiMessages => parse_ui_projection(
            raw,
            &envelope,
            source,
            identity,
            &native_key,
            native_index,
            byte_start,
        ),
    };
    let mut projection = match parsed {
        Ok(projection) => projection,
        Err((kind, detail)) => {
            return rejected_item_with_key(
                component,
                native_index,
                envelope.native_id,
                observed_bytes,
                kind,
                &detail,
                native_key,
                stats,
            );
        }
    };
    let failure_rows = projection
        .outputs
        .iter()
        .filter(|output| {
            matches!(
                output.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            )
        })
        .count();
    let potential_units = projection
        .rows
        .len()
        .saturating_add(projection.outputs.len())
        .saturating_add(failure_rows)
        .saturating_add(projection.rows.iter().fold(0_usize, |count, row| {
            count.saturating_add(row.file_touches.len())
        }));
    if potential_units > max_item_units {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::UnsupportedShape,
            "Cline item exceeds its activation-invariant page unit budget",
            native_key,
            stats,
        );
    }
    let retained_body_bytes = projection
        .rows
        .iter()
        .map(|row| row.body.as_deref().map_or(0, str::len))
        .sum::<usize>();
    if retained_body_bytes > CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline retained item exceeds the 64 KiB Core item bound",
            native_key,
            stats,
        );
    }

    let mut outputs = Vec::new();
    let mut transient_rejections = Vec::new();
    let mut transient_bytes = 0_usize;
    let mut output_outcomes = Vec::with_capacity(projection.outputs.len());
    for output in projection.outputs {
        stats.output_outcomes_observed = stats.output_outcomes_observed.saturating_add(1);
        output_outcomes.push(output.outcome.clone());
        let content = if profile.wants_outputs() {
            match decode_output_body(output.body) {
                Ok(content) => {
                    stats.output_bodies_hydrated = stats.output_bodies_hydrated.saturating_add(1);
                    stats.output_body_bytes_hydrated = stats
                        .output_body_bytes_hydrated
                        .saturating_add(content.len());
                    Some(content)
                }
                Err(detail) => {
                    push_transient_rejection(
                        &mut transient_rejections,
                        item_rejection(
                            component,
                            native_index,
                            envelope.native_id.as_deref(),
                            ClineItemRejectionKind::OversizedTransientOutput,
                            observed_bytes,
                            detail,
                        ),
                    );
                    None
                }
            }
        } else {
            None
        };
        let preview = if matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        ) {
            decode_failure_preview(output.body)
        } else {
            None
        };
        if matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        ) {
            projection.rows.push(ClineEventRow::sparse_output(
                ClineEventContext {
                    task: identity,
                    component,
                    item: &native_key,
                    item_index: native_index,
                    role: ClineEventRole::Unknown,
                    occurred_at_millis: projection.occurred_at_millis,
                },
                output.sub_index,
                match output.kind {
                    OutputObservationKind::Command => ClineEventKind::CommandOutput,
                    OutputObservationKind::Tool => ClineEventKind::ToolOutput,
                },
                ClineSparseOutputDiagnostic {
                    outcome: output.outcome.outcome,
                    exit_code: output.outcome.exit_code,
                    duration_ms: output.outcome.duration_ms,
                    output_bytes: output.body.map_or(0, |body| body.get().len()),
                    preview,
                    call_id: output.call_id.clone().map(String::into_boxed_str),
                },
            ));
        }
        if let Some(content) = content {
            let observation =
                build_output_observation(source, identity, native_index, output, content);
            let bytes = estimated_output_bytes(&observation);
            if transient_bytes.saturating_add(bytes) > CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES {
                push_transient_rejection(
                    &mut transient_rejections,
                    item_rejection(
                        component,
                        native_index,
                        envelope.native_id.as_deref(),
                        ClineItemRejectionKind::OversizedTransientOutput,
                        observed_bytes,
                        "Cline transient outputs exceed their independent 4 MiB page lane",
                    ),
                );
            } else {
                transient_bytes = transient_bytes.saturating_add(bytes);
                outputs.push(observation);
            }
        }
    }
    projection
        .rows
        .sort_by_key(|row| (row.native_order.item_index, row.native_order.sub_index));
    let core_bytes = projection
        .rows
        .iter()
        .map(estimated_event_bytes)
        .sum::<usize>();
    if core_bytes > CLINE_NATIVE_CORE_PAGE_MAX_BYTES {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline Core projection exceeds its independent 4 MiB page lane",
            native_key,
            stats,
        );
    }
    let checkpoint = ClineItemCheckpoint::new(native_key, &projection.rows, &output_outcomes, None);
    stats.core_rows = stats.core_rows.saturating_add(projection.rows.len());
    ParsedItem {
        checkpoint,
        rows: projection.rows,
        outputs,
        rejection: None,
        transient_rejections,
        core_bytes,
    }
}

struct ParsedProjection<'a> {
    rows: Vec<ClineEventRow>,
    outputs: Vec<OutputCandidate<'a>>,
    occurred_at_millis: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
fn parse_api_projection<'a>(
    raw_item: &'a RawValue,
    envelope: &RawEnvelope<'a>,
    source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
    item_byte_start: u64,
) -> Result<ParsedProjection<'a>, (ClineItemRejectionKind, String)> {
    let discriminators = envelope.normalized_discriminators().collect::<Vec<_>>();
    let top_result = discriminators
        .iter()
        .any(|value| is_result_discriminator(value));
    let positive_conversation = discriminators.iter().any(|value| {
        matches!(
            value.as_str(),
            "user" | "assistant" | "system" | "developer" | "message" | "text"
        )
    });
    let role = role_from_discriminators(&discriminators);
    let context = ClineEventContext {
        task: identity,
        component: match source.component {
            ClineComponent::FallbackHistory => ClineEventComponent::FallbackHistory,
            _ => ClineEventComponent::ApiHistory,
        },
        item: native_key,
        item_index: native_index,
        role,
        occurred_at_millis: envelope.occurred_at_millis,
    };
    let mut projection = ParsedProjection {
        rows: Vec::new(),
        outputs: Vec::new(),
        occurred_at_millis: envelope.occurred_at_millis,
    };
    if top_result {
        if envelope
            .content
            .is_some_and(|content| content.get().trim_start().starts_with('['))
        {
            let content = envelope.content.expect("checked Cline result content");
            push_explicit_result_blocks(
                raw_item,
                content,
                OutputObservationKind::Tool,
                envelope,
                item_byte_start,
                &mut projection.outputs,
            )?;
            return Ok(projection);
        }
        push_explicit_outputs(
            raw_item,
            envelope.direct_result_body(),
            OutputCandidateContext {
                kind: OutputObservationKind::Tool,
                base_sub_index: 0,
                call_id: envelope.call_id.clone(),
                outcome: envelope.outcome(),
                occurred_at_millis: envelope.occurred_at_millis,
                item_start: item_byte_start,
                fallback_start: item_byte_start,
            },
            &mut projection.outputs,
        )?;
        return Ok(projection);
    }
    let Some(content) = envelope.content else {
        return Ok(projection);
    };
    let content_text = content.get().trim_start();
    if content_text.starts_with('"') {
        if positive_conversation {
            if let Some(body) = decode_retained_text(content)? {
                if !body.trim().is_empty() {
                    projection.rows.push(ClineEventRow::message(
                        context,
                        0,
                        ClineEventKind::Message,
                        body,
                    ));
                }
            }
        }
        return Ok(projection);
    }
    if content_text.starts_with('[') {
        let blocks = deserialize_bounded_raw_array(content, "Cline API content array")?;
        for (index, block) in blocks.into_iter().enumerate() {
            let sub_index = u32::try_from(index).unwrap_or(u32::MAX);
            parse_api_block(
                raw_item,
                block,
                context,
                sub_index,
                item_byte_start,
                positive_conversation,
                envelope,
                &mut projection,
            )?;
        }
        return Ok(projection);
    }
    if content_text.starts_with('{') {
        parse_api_block(
            raw_item,
            content,
            context,
            0,
            item_byte_start,
            positive_conversation,
            envelope,
            &mut projection,
        )?;
        return Ok(projection);
    }
    Err((
        ClineItemRejectionKind::UnsupportedShape,
        "Cline API content is not text, an object, or an array".to_owned(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn parse_api_block<'a>(
    raw_item: &'a RawValue,
    raw_block: &'a RawValue,
    context: ClineEventContext<'_>,
    sub_index: u32,
    item_byte_start: u64,
    retain_text: bool,
    outer: &RawEnvelope<'a>,
    projection: &mut ParsedProjection<'a>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let row_sub_index = sub_index.saturating_mul(1_024);
    if raw_block.get().trim_start().starts_with('"') {
        if !retain_text {
            return Ok(());
        }
        if let Some(body) = decode_retained_text(raw_block)? {
            if !body.trim().is_empty() {
                projection.rows.push(ClineEventRow::message(
                    context,
                    row_sub_index,
                    ClineEventKind::Message,
                    body,
                ));
            }
        }
        return Ok(());
    }
    if !raw_block.get().trim_start().starts_with('{') {
        return Ok(());
    }
    let block = serde_json::from_str::<RawEnvelope<'_>>(raw_block.get()).map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("malformed Cline API content block: {error}"),
        )
    })?;
    if block.conflicting_discriminator || block.oversized_discriminator {
        return Err((
            ClineItemRejectionKind::ConflictingDiscriminator,
            "Cline API block has conflicting or oversized discriminator fields".to_owned(),
        ));
    }
    let discriminators = block.normalized_discriminators().collect::<Vec<_>>();
    let is_result = discriminators
        .iter()
        .any(|value| is_result_discriminator(value));
    let is_text = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "text" | "message"));
    let is_call = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "tooluse" | "functioncall" | "toolcall"));
    let block_start = (raw_block.get().as_ptr() as usize)
        .checked_sub(raw_item.get().as_ptr() as usize)
        .and_then(|offset| item_byte_start.checked_add(offset as u64))
        .unwrap_or(item_byte_start);
    if is_result {
        let block_outcome = block.outcome();
        let outcome = if block_outcome.outcome == OutputOutcome::Unknown
            && block_outcome.exit_code.is_none()
            && block_outcome.duration_ms.is_none()
        {
            outer.outcome()
        } else {
            block_outcome
        };
        push_explicit_outputs(
            raw_item,
            block.block_result_body(),
            OutputCandidateContext {
                kind: OutputObservationKind::Tool,
                base_sub_index: sub_index.saturating_mul(1_024),
                call_id: block.call_id.clone().or_else(|| outer.call_id.clone()),
                outcome,
                occurred_at_millis: block.occurred_at_millis.or(context.occurred_at_millis),
                item_start: item_byte_start,
                fallback_start: block_start,
            },
            &mut projection.outputs,
        )?;
    } else if is_call {
        let file_touches = extract_file_touches(raw_block)?;
        let mut row = ClineEventRow::tool_call(context, row_sub_index, block.call_id, block.name);
        row.attach_file_touches(file_touches);
        projection.rows.push(row);
    } else if is_text && retain_text {
        if let Some(body) = block
            .retained_body()
            .map(decode_retained_text)
            .transpose()?
        {
            if let Some(body) = body.filter(|body| !body.trim().is_empty()) {
                projection.rows.push(ClineEventRow::message(
                    context,
                    row_sub_index,
                    ClineEventKind::Message,
                    body,
                ));
            }
        }
    }
    Ok(())
}

fn extract_file_touches(
    raw_call: &RawValue,
) -> Result<Vec<ClineFileTouch>, (ClineItemRejectionKind, String)> {
    let raw_value = serde_json::from_str::<Value>(raw_call.get())
        .map_err(|error| (ClineItemRejectionKind::MalformedRecord, error.to_string()))?;
    let mut file_touches = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        &raw_value,
        true,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| -> std::result::Result<(), ()> {
            file_touches.push(ClineFileTouch {
                path: touch.path.into_boxed_str(),
                old_path: touch.old_path.map(String::into_boxed_str),
                change_kind: touch.change_kind,
                confidence: touch.confidence,
                metadata: touch.metadata,
            });
            Ok(())
        },
    )
    .unwrap_or_else(|()| unreachable!("the file-touch collector is infallible"));
    if outcome.limit_exceeded() {
        return Err((
            ClineItemRejectionKind::UnsupportedShape,
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        ));
    }
    Ok(file_touches)
}

#[allow(clippy::too_many_arguments)]
fn parse_ui_projection<'a>(
    raw_item: &'a RawValue,
    envelope: &RawEnvelope<'a>,
    _source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    native_key: &ClineNativeItemKey,
    native_index: u64,
    byte_start: u64,
) -> Result<ParsedProjection<'a>, (ClineItemRejectionKind, String)> {
    let discriminators = envelope.normalized_discriminators().collect::<Vec<_>>();
    let command = discriminators.iter().any(|value| {
        is_result_discriminator(value) || matches!(value.as_str(), "executecommand" | "command")
    });
    let user = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "ask" | "user"));
    let assistant = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "say" | "assistant" | "text"));
    let summary = discriminators
        .iter()
        .any(|value| matches!(value.as_str(), "completionresult" | "summary"));
    let notice = discriminators.iter().any(|value| value == "notice");
    let mut projection = ParsedProjection {
        rows: Vec::new(),
        outputs: Vec::new(),
        occurred_at_millis: envelope.occurred_at_millis,
    };
    if command {
        if let Some(content) = envelope
            .content
            .filter(|content| content.get().trim_start().starts_with('['))
        {
            push_explicit_result_blocks(
                raw_item,
                content,
                OutputObservationKind::Command,
                envelope,
                byte_start,
                &mut projection.outputs,
            )?;
        } else {
            push_explicit_outputs(
                raw_item,
                envelope.direct_result_body(),
                OutputCandidateContext {
                    kind: OutputObservationKind::Command,
                    base_sub_index: 0,
                    call_id: envelope.call_id.clone(),
                    outcome: envelope.outcome(),
                    occurred_at_millis: envelope.occurred_at_millis,
                    item_start: byte_start,
                    fallback_start: byte_start,
                },
                &mut projection.outputs,
            )?;
        }
        return Ok(projection);
    }
    let Some((kind, role)) = user
        .then_some((ClineEventKind::Message, ClineEventRole::User))
        .or_else(|| assistant.then_some((ClineEventKind::Message, ClineEventRole::Assistant)))
        .or_else(|| summary.then_some((ClineEventKind::Summary, ClineEventRole::Assistant)))
        .or_else(|| notice.then_some((ClineEventKind::Notice, ClineEventRole::Unknown)))
    else {
        return Ok(projection);
    };
    if let Some(body) = envelope
        .retained_body()
        .map(decode_retained_text)
        .transpose()?
        .flatten()
        .filter(|body| !body.trim().is_empty())
    {
        projection.rows.push(ClineEventRow::message(
            ClineEventContext {
                task: identity,
                component: ClineEventComponent::UiMessages,
                item: native_key,
                item_index: native_index,
                role,
                occurred_at_millis: envelope.occurred_at_millis,
            },
            0,
            kind,
            body,
        ));
    }
    Ok(projection)
}

fn decode_retained_text(
    raw: &RawValue,
) -> Result<Option<String>, (ClineItemRejectionKind, String)> {
    if !raw.get().trim_start().starts_with('"') {
        return Ok(None);
    }
    if raw.get().len() > CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES {
        return Err((
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline retained JSON string exceeds 64 KiB before unescaping".to_owned(),
        ));
    }
    serde_json::from_str::<String>(raw.get())
        .map(Some)
        .map_err(|error| {
            (
                ClineItemRejectionKind::MalformedRecord,
                format!("invalid retained Cline text: {error}"),
            )
        })
}

fn decode_output_body(raw: Option<&RawValue>) -> Result<Vec<u8>, &'static str> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.get().len() > MAX_OUTPUT_BODY_RAW_BYTES {
        return Err("Cline output body exceeds the independent 4 MiB transient bound");
    }
    if raw.get().trim_start().starts_with('"') {
        return serde_json::from_str::<String>(raw.get())
            .map(String::into_bytes)
            .map_err(|_| "Cline output body is not a valid JSON string");
    }
    if raw.get().trim() == "null" {
        return Ok(Vec::new());
    }
    let value = serde_json::from_str::<serde_json::Value>(raw.get())
        .map_err(|_| "Cline output body is not valid explicit JSON")?;
    serde_json::to_vec(&value).map_err(|_| "Cline output body could not be encoded")
}

fn decode_failure_preview(raw: Option<&RawValue>) -> Option<Box<str>> {
    let raw = raw?;
    if raw.get().trim_start().starts_with('"') {
        decode_json_string_preview(raw.get()).map(String::into_boxed_str)
    } else {
        Some(failure_preview_from_bytes(raw.get().as_bytes()))
    }
}

fn decode_json_string_preview(raw: &str) -> Option<String> {
    let bytes = raw.trim_start().as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut output = String::new();
    let mut index = 1_usize;
    let mut chars = 0_usize;
    while index < bytes.len() && chars < CLINE_NATIVE_MAX_FAILURE_PREVIEW_BYTES {
        match bytes[index] {
            b'"' => return Some(output),
            b'\\' => {
                index = index.checked_add(1)?;
                let escaped = *bytes.get(index)?;
                let decoded = match escaped {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'u' => {
                        let first = decode_hex_quad(bytes.get(index + 1..index + 5)?)?;
                        index = index.checked_add(4)?;
                        let scalar = if (0xd800..=0xdbff).contains(&first) {
                            if bytes.get(index + 1..index + 3) != Some(b"\\u") {
                                return None;
                            }
                            let second = decode_hex_quad(bytes.get(index + 3..index + 7)?)?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return None;
                            }
                            index = index.checked_add(6)?;
                            0x1_0000
                                + ((u32::from(first) - 0xd800) << 10)
                                + (u32::from(second) - 0xdc00)
                        } else {
                            u32::from(first)
                        };
                        char::from_u32(scalar)?
                    }
                    _ => return None,
                };
                output.push(decoded);
                chars = chars.saturating_add(1);
                index = index.checked_add(1)?;
            }
            _ => {
                let tail = std::str::from_utf8(bytes.get(index..)?).ok()?;
                let decoded = tail.chars().next()?;
                output.push(decoded);
                chars = chars.saturating_add(1);
                index = index.checked_add(decoded.len_utf8())?;
            }
        }
    }
    Some(output)
}

fn decode_hex_quad(bytes: &[u8]) -> Option<u16> {
    if bytes.len() != 4 {
        return None;
    }
    bytes.iter().try_fold(0_u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(*byte - b'0'),
            b'a'..=b'f' => u16::from(*byte - b'a' + 10),
            b'A'..=b'F' => u16::from(*byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    })
}

fn failure_preview_from_bytes(bytes: &[u8]) -> Box<str> {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(CLINE_NATIVE_MAX_FAILURE_PREVIEW_BYTES)
        .collect::<String>()
        .into_boxed_str()
}

fn build_output_observation(
    source: &ClineFileSourceIdentity,
    identity: &ClineTaskIdentity,
    native_index: u64,
    output: OutputCandidate<'_>,
    content: Vec<u8>,
) -> ProOutputObservation {
    let component = match source.component {
        ClineComponent::ApiHistory => "api",
        ClineComponent::UiMessages => "ui",
        ClineComponent::FallbackHistory => "fallback",
        ClineComponent::TaskMetadata => "metadata",
        ClineComponent::HistoryItem => "history_item",
        ClineComponent::TaskIndex => "task_index",
        ClineComponent::RootIndex => "root",
    };
    let unit_key = format!(
        "{}/nativepath/{}/{component}/{native_index}/{}",
        source.provider,
        identity.as_str(),
        output.sub_index
    );
    let mut locator = Vec::with_capacity(29);
    locator.push(source.component as u8);
    locator.extend_from_slice(&native_index.to_be_bytes());
    locator.extend_from_slice(&output.sub_index.to_be_bytes());
    locator.extend_from_slice(&output.byte_start.to_be_bytes());
    locator.extend_from_slice(&output.byte_end_exclusive.to_be_bytes());
    ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: unit_key.clone(),
            native_sequence: native_index,
            native_record_id: Some(unit_key),
            source_record_ordinal: Some(native_index),
            source_record_subrecord_index: Some(output.sub_index),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: output.occurred_at_millis,
        associations: OutputAssociations {
            direct_session_id: identity.as_str().to_owned(),
            root_session_id: identity.as_str().to_owned(),
            parent_session_id: None,
            provider_session_id: Some(identity.as_str().to_owned()),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id,
        command: None,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "cline_native_component_range".to_owned(),
            payload: locator,
        },
        content,
    }
}

fn native_key(native_id: Option<&str>, native_index: u64) -> ClineNativeItemKey {
    let Some(native_id) = native_id.filter(|value| valid_identity(value)) else {
        return ClineNativeItemKey::ComponentOrdinal(native_index);
    };
    ClineNativeItemKey::NativeId {
        native_id: native_id.to_owned().into_boxed_str(),
        component_ordinal: native_index,
    }
}

#[allow(clippy::too_many_arguments)]
fn rejected_item(
    component: ClineEventComponent,
    native_index: u64,
    native_id: Option<String>,
    observed_bytes: u64,
    kind: ClineItemRejectionKind,
    detail: &str,
    stats: &mut ClinePublicationStats,
) -> ParsedItem {
    let key = native_key(native_id.as_deref(), native_index);
    rejected_item_with_key(
        component,
        native_index,
        native_id,
        observed_bytes,
        kind,
        detail,
        key,
        stats,
    )
}

#[allow(clippy::too_many_arguments)]
fn rejected_item_with_key(
    component: ClineEventComponent,
    native_index: u64,
    native_id: Option<String>,
    observed_bytes: u64,
    kind: ClineItemRejectionKind,
    detail: &str,
    key: ClineNativeItemKey,
    stats: &mut ClinePublicationStats,
) -> ParsedItem {
    let rejection = item_rejection(
        component,
        native_index,
        native_id.as_deref(),
        kind,
        observed_bytes,
        detail,
    );
    let checkpoint = ClineItemCheckpoint::new(key, &[], &[], Some(&rejection));
    stats.local_rejections = stats.local_rejections.saturating_add(1);
    ParsedItem {
        checkpoint,
        rows: Vec::new(),
        outputs: Vec::new(),
        rejection: Some(rejection),
        transient_rejections: Vec::new(),
        core_bytes: 0,
    }
}

fn push_transient_rejection(
    rejections: &mut Vec<ClineItemRejection>,
    rejection: ClineItemRejection,
) {
    if rejections.len() < CLINE_NATIVE_MAX_REJECTIONS {
        rejections.push(rejection);
    }
}

fn item_rejection(
    component: ClineEventComponent,
    native_index: u64,
    native_id: Option<&str>,
    kind: ClineItemRejectionKind,
    observed_bytes: u64,
    detail: &str,
) -> ClineItemRejection {
    ClineItemRejection {
        component,
        native_index,
        native_id: native_id.map(|value| value.to_owned().into_boxed_str()),
        kind,
        observed_bytes,
        detail: bounded_detail(detail),
    }
}

fn parse_failure(
    observation: &ClineComponentObservation,
    error: &serde_json::Error,
    context: &str,
) -> super::normalize::ClineComponentFailure {
    let authority_bound = error.to_string().contains("authority bound");
    super::normalize::ClineComponentFailure {
        component: observation.component,
        path: observation.path.clone(),
        kind: if authority_bound {
            super::normalize::ClineComponentFailureKind::AuthorityBound
        } else if error.is_eof() {
            super::normalize::ClineComponentFailureKind::IncompleteJson
        } else {
            super::normalize::ClineComponentFailureKind::MalformedJson
        },
        message: bounded_detail(&format!("{context}: {error}")),
        retryable: !authority_bound && error.is_eof(),
    }
}

fn bounded_detail(detail: &str) -> Box<str> {
    detail
        .chars()
        .take(512)
        .collect::<String>()
        .into_boxed_str()
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NATIVE_ID_BYTES && !value.chars().any(char::is_control)
}

fn normalize_discriminator(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_result_discriminator(value: &str) -> bool {
    matches!(
        value,
        "tool"
            | "toolresult"
            | "tooluseresult"
            | "tooloutput"
            | "command"
            | "commandresult"
            | "commandoutput"
            | "executecommand"
            | "shell"
            | "shelloutput"
            | "terminaloutput"
            | "functionresult"
            | "functioncalloutput"
            | "customtoolcalloutput"
            | "browseractionresult"
            | "browserresult"
            | "mcpserverresponse"
            | "mcpresponse"
            | "mcpresult"
            | "mcpserverresult"
    ) || value.ends_with("output")
        || value.ends_with("response")
}

fn role_from_discriminators(values: &[String]) -> ClineEventRole {
    if values.iter().any(|value| value == "user") {
        ClineEventRole::User
    } else if values.iter().any(|value| value == "assistant") {
        ClineEventRole::Assistant
    } else if values
        .iter()
        .any(|value| matches!(value.as_str(), "system" | "developer"))
    {
        ClineEventRole::System
    } else {
        ClineEventRole::Unknown
    }
}

fn status_is_failure(value: &str) -> bool {
    matches!(
        value,
        "error" | "failed" | "failure" | "cancelled" | "canceled"
    )
}

fn status_is_success(value: &str) -> bool {
    matches!(
        value,
        "ok" | "success" | "succeeded" | "complete" | "completed"
    )
}

struct LooseBool(Option<bool>);

impl<'de> Deserialize<'de> for LooseBool {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&RawValue>::deserialize(deserializer)?;
        Ok(LooseBool(match raw.get().trim() {
            "true" | "\"true\"" => Some(true),
            "false" | "\"false\"" => Some(false),
            _ => None,
        }))
    }
}

struct LooseI32(Option<i32>);

impl<'de> Deserialize<'de> for LooseI32 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&RawValue>::deserialize(deserializer)?;
        let value = raw
            .get()
            .trim()
            .trim_matches('"')
            .parse::<i64>()
            .ok()
            .and_then(|value| i32::try_from(value).ok());
        Ok(LooseI32(value))
    }
}

struct LooseU64(Option<u64>);

impl<'de> Deserialize<'de> for LooseU64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&RawValue>::deserialize(deserializer)?;
        Ok(LooseU64(
            raw.get().trim().trim_matches('"').parse::<u64>().ok(),
        ))
    }
}

struct LooseTimestamp(Option<i64>);

impl<'de> Deserialize<'de> for LooseTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&RawValue>::deserialize(deserializer)?;
        let text = raw.get().trim();
        let parsed = if text.starts_with('"') && text.len() <= MAX_SMALL_FIELD_BYTES {
            serde_json::from_str::<String>(text).ok().and_then(|value| {
                DateTime::parse_from_rfc3339(&value)
                    .ok()
                    .map(|value| value.timestamp_millis())
                    .or_else(|| value.parse::<i64>().ok().map(normalize_integer_timestamp))
            })
        } else if let Ok(value) = text.parse::<i64>() {
            Some(normalize_integer_timestamp(value))
        } else {
            text.parse::<f64>().ok().and_then(|value| {
                let value = if value.abs() > 1_000_000_000_000.0 {
                    value
                } else {
                    value * 1000.0
                };
                value
                    .is_finite()
                    .then(|| value.round())
                    .filter(|value| *value >= i64::MIN as f64 && *value <= i64::MAX as f64)
                    .map(|value| value as i64)
            })
        };
        Ok(LooseTimestamp(parsed))
    }
}

fn normalize_integer_timestamp(value: i64) -> i64 {
    if value.unsigned_abs() < 1_000_000_000_000 {
        value.saturating_mul(1000)
    } else {
        value
    }
}

pub(super) fn parse_root_index(
    hydrated: &HydratedComponent,
    observation: &ClineComponentObservation,
    stats: &mut ClinePublicationStats,
) -> Result<Vec<ClineCatalogEntry>, super::normalize::ClineComponentFailure> {
    stats.component_parse_passes = stats.component_parse_passes.saturating_add(1);
    let mut deserializer = serde_json::Deserializer::from_slice(&hydrated.bytes);
    let mut entries = deserializer
        .deserialize_seq(RootIndexVisitor)
        .map_err(|error| parse_failure(observation, &error, "malformed Cline taskHistory"))?;
    deserializer
        .end()
        .map_err(|error| parse_failure(observation, &error, "trailing Cline taskHistory data"))?;
    entries.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    entries.dedup_by(|left, right| left.task_id == right.task_id);
    Ok(entries)
}

struct RootIndexVisitor;

impl<'de> Visitor<'de> for RootIndexVisitor {
    type Value = Vec<ClineCatalogEntry>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline taskHistory JSON array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut entries = Vec::new();
        while let Some(raw) = sequence.next_element::<RawRootEntry>()? {
            if let Some(entry) = raw.normalize() {
                if entries.len() == 4_096 {
                    return Err(serde::de::Error::custom(
                        "Cline taskHistory exceeds the 4096-entry catalog bound",
                    ));
                }
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

#[derive(Deserialize)]
struct RawRootEntry {
    #[serde(default, alias = "taskId")]
    id: Option<BoundedString<MAX_NATIVE_ID_BYTES>>,
    #[serde(default, alias = "title")]
    task: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>,
    #[serde(default, alias = "workspaceDirectory")]
    workspace_directory: Option<BoundedString<MAX_METADATA_TEXT_BYTES>>,
    #[serde(default, alias = "timestamp")]
    ts: Option<i64>,
    #[serde(default, alias = "inputTokens")]
    tokens_input: Option<u64>,
    #[serde(default, alias = "outputTokens")]
    tokens_output: Option<u64>,
}

impl RawRootEntry {
    fn normalize(self) -> Option<ClineCatalogEntry> {
        let task_id = self
            .id
            .and_then(|value| value.0)
            .filter(|value| valid_identity(value))?
            .into_boxed_str();
        Some(ClineCatalogEntry {
            task_id,
            title: bounded_metadata(self.task),
            workspace_directory: bounded_metadata(self.workspace_directory),
            timestamp_millis: self.ts.map(normalize_integer_timestamp),
            tokens_input: self.tokens_input,
            tokens_output: self.tokens_output,
        })
    }
}
