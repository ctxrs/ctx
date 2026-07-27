use std::{
    collections::BTreeMap,
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

mod item;
mod projection;
mod scanner;

use item::*;
use projection::*;
pub(super) use scanner::*;

fn native_key(
    native_id: Option<&str>,
    native_index: u64,
    occurrences: Option<&mut BTreeMap<String, u64>>,
) -> ClineNativeItemKey {
    let Some(native_id) = native_id.filter(|value| valid_identity(value)) else {
        return ClineNativeItemKey::ComponentOrdinal(native_index);
    };
    let occurrence = occurrences.map_or(0, |occurrences| {
        let occurrence = occurrences.get(native_id).copied().unwrap_or_default();
        occurrences.insert(native_id.to_owned(), occurrence.saturating_add(1));
        occurrence
    });
    ClineNativeItemKey::NativeId {
        native_id: native_id.to_owned().into_boxed_str(),
        occurrence,
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
    let key = native_key(native_id.as_deref(), native_index, None);
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
