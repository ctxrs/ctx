use ctx_history_core::CaptureProvider;

use crate::captured_batch::{NativeLocator, NativePosition};
use crate::{CaptureError, Result, CLINE_TASK_JSON_SOURCE_FORMAT, ROO_TASK_JSON_SOURCE_FORMAT};

pub(super) const TASK_JSON_CAPTURE_REVISION: u32 = 4;
pub(super) const TASK_JSON_POLICY_REVISION: u32 = 5;
pub(super) const TASK_JSON_RECORD_KIND: &str = "task-json-native-item-v1";
pub(super) const TASK_JSON_POSITION_KIND: &str = "task-json-stream-v1";
pub(super) const TASK_JSON_LOCATOR_KIND: &str = "task-json-source-item-v1";
pub(super) const TASK_JSON_POSITION_VERSION: u8 = 1;
pub(super) const TASK_JSON_LOCATOR_VERSION: u8 = 1;
pub(super) const TASK_JSON_POSITION_BYTES: usize = 27;
pub(super) const TASK_JSON_LOCATOR_BYTES: usize = 19;
pub(super) const TASK_JSON_TERMINAL_PHASE: u8 = 3;
pub(super) const TASK_JSON_DONE_PHASE: u8 = 4;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TaskJsonProviderSpec {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) display_name: &'static str,
    pub(crate) api_file: &'static str,
    pub(crate) ui_file: &'static str,
    pub(crate) metadata_file: &'static str,
    pub(crate) history_item_file: Option<&'static str>,
    pub(crate) index_file: Option<&'static str>,
    pub(crate) fallback_api_file: Option<&'static str>,
}

pub(crate) fn task_json_provider(provider: CaptureProvider) -> TaskJsonProviderSpec {
    match provider {
        CaptureProvider::RooCode => TaskJsonProviderSpec {
            provider,
            source_format: ROO_TASK_JSON_SOURCE_FORMAT,
            display_name: "Roo Code",
            api_file: "api_conversation_history.json",
            ui_file: "ui_messages.json",
            metadata_file: "task_metadata.json",
            history_item_file: Some("history_item.json"),
            index_file: Some("_index.json"),
            fallback_api_file: Some("claude_messages.json"),
        },
        _ => TaskJsonProviderSpec {
            provider: CaptureProvider::Cline,
            source_format: CLINE_TASK_JSON_SOURCE_FORMAT,
            display_name: "Cline",
            api_file: "api_conversation_history.json",
            ui_file: "ui_messages.json",
            metadata_file: "task_metadata.json",
            history_item_file: None,
            index_file: None,
            fallback_api_file: None,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum TaskJsonMessagePhase {
    Api = 0,
    Ui = 1,
    Fallback = 2,
}

impl TaskJsonMessagePhase {
    pub(super) fn decode(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Api),
            1 => Ok(Self::Ui),
            2 => Ok(Self::Fallback),
            _ => Err(CaptureError::InvalidPayload(
                "task JSON cursor contains an invalid message phase".to_owned(),
            )),
        }
    }

    pub(super) fn source(self) -> &'static str {
        match self {
            Self::Api => "api_conversation_history",
            Self::Ui => "ui_messages",
            Self::Fallback => "claude_messages",
        }
    }

    pub(super) fn file_name(self, spec: TaskJsonProviderSpec) -> Option<&'static str> {
        match self {
            Self::Api => Some(spec.api_file),
            Self::Ui => Some(spec.ui_file),
            Self::Fallback => spec.fallback_api_file,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum TaskJsonArrayContainer {
    Unknown = 0,
    Direct = 1,
    Wrapped = 2,
}

impl TaskJsonArrayContainer {
    pub(super) fn decode(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Direct),
            2 => Ok(Self::Wrapped),
            _ => Err(CaptureError::InvalidPayload(
                "task JSON cursor contains an invalid array container".to_owned(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TaskJsonStreamPosition {
    pub(super) phase: u8,
    pub(super) container: TaskJsonArrayContainer,
    pub(super) offset: u64,
    pub(super) native_index: u64,
    pub(super) ordinal: u64,
}

impl TaskJsonStreamPosition {
    pub(super) fn initial() -> Self {
        Self {
            phase: TaskJsonMessagePhase::Api as u8,
            container: TaskJsonArrayContainer::Unknown,
            offset: 0,
            native_index: 0,
            ordinal: 0,
        }
    }

    pub(super) fn terminal(ordinal: u64) -> Self {
        Self {
            phase: TASK_JSON_TERMINAL_PHASE,
            container: TaskJsonArrayContainer::Unknown,
            offset: 0,
            native_index: 0,
            ordinal,
        }
    }

    pub(super) fn done(ordinal: u64) -> Self {
        Self {
            phase: TASK_JSON_DONE_PHASE,
            container: TaskJsonArrayContainer::Unknown,
            offset: 0,
            native_index: 0,
            ordinal,
        }
    }
}

pub(super) fn task_json_native_position(
    position: TaskJsonStreamPosition,
) -> Result<NativePosition> {
    let mut value = Vec::with_capacity(TASK_JSON_POSITION_BYTES);
    value.push(TASK_JSON_POSITION_VERSION);
    value.push(position.phase);
    value.push(position.container as u8);
    value.extend_from_slice(&position.offset.to_be_bytes());
    value.extend_from_slice(&position.native_index.to_be_bytes());
    value.extend_from_slice(&position.ordinal.to_be_bytes());
    NativePosition::new(TASK_JSON_POSITION_KIND, value).map_err(task_json_captured_batch_error)
}

pub(super) fn task_json_decode_position(
    position: &NativePosition,
) -> Result<TaskJsonStreamPosition> {
    if position.kind() != TASK_JSON_POSITION_KIND
        || position.value().len() != TASK_JSON_POSITION_BYTES
        || position.value()[0] != TASK_JSON_POSITION_VERSION
    {
        return Err(CaptureError::InvalidPayload(
            "invalid task JSON native position".to_owned(),
        ));
    }
    let phase = position.value()[1];
    if phase > TASK_JSON_DONE_PHASE {
        return Err(CaptureError::InvalidPayload(
            "task JSON native position phase is out of range".to_owned(),
        ));
    }
    let container = TaskJsonArrayContainer::decode(position.value()[2])?;
    let offset = task_json_decode_u64(&position.value()[3..11])?;
    let native_index = task_json_decode_u64(&position.value()[11..19])?;
    let ordinal = task_json_decode_u64(&position.value()[19..27])?;
    if phase >= TASK_JSON_TERMINAL_PHASE
        && (container != TaskJsonArrayContainer::Unknown || offset != 0 || native_index != 0)
    {
        return Err(CaptureError::InvalidPayload(
            "task JSON terminal position contains array state".to_owned(),
        ));
    }
    if phase < TASK_JSON_TERMINAL_PHASE && offset == 0 {
        if container != TaskJsonArrayContainer::Unknown || native_index != 0 {
            return Err(CaptureError::InvalidPayload(
                "task JSON zero-offset position contains array state".to_owned(),
            ));
        }
    } else if phase < TASK_JSON_TERMINAL_PHASE && container == TaskJsonArrayContainer::Unknown {
        return Err(CaptureError::InvalidPayload(
            "task JSON resumed array position is missing its container".to_owned(),
        ));
    }
    Ok(TaskJsonStreamPosition {
        phase,
        container,
        offset,
        native_index,
        ordinal,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum TaskJsonRecordClass {
    Event = 0,
    FileError = 1,
    Terminal = 2,
}

pub(super) fn task_json_locator(
    phase: u8,
    class: TaskJsonRecordClass,
    native_index: u64,
    offset: u64,
) -> Result<NativeLocator> {
    let mut value = Vec::with_capacity(TASK_JSON_LOCATOR_BYTES);
    value.push(TASK_JSON_LOCATOR_VERSION);
    value.push(phase);
    value.push(class as u8);
    value.extend_from_slice(&native_index.to_be_bytes());
    value.extend_from_slice(&offset.to_be_bytes());
    NativeLocator::new(TASK_JSON_LOCATOR_KIND, value).map_err(task_json_captured_batch_error)
}

pub(super) fn task_json_decode_locator(
    locator: &NativeLocator,
) -> Result<(u8, TaskJsonRecordClass, u64, u64)> {
    if locator.kind() != TASK_JSON_LOCATOR_KIND
        || locator.value().len() != TASK_JSON_LOCATOR_BYTES
        || locator.value()[0] != TASK_JSON_LOCATOR_VERSION
    {
        return Err(CaptureError::InvalidPayload(
            "invalid task JSON native locator".to_owned(),
        ));
    }
    let class = match locator.value()[2] {
        0 => TaskJsonRecordClass::Event,
        1 => TaskJsonRecordClass::FileError,
        2 => TaskJsonRecordClass::Terminal,
        _ => {
            return Err(CaptureError::InvalidPayload(
                "task JSON native locator class is out of range".to_owned(),
            ));
        }
    };
    Ok((
        locator.value()[1],
        class,
        task_json_decode_u64(&locator.value()[3..11])?,
        task_json_decode_u64(&locator.value()[11..19])?,
    ))
}

fn task_json_decode_u64(bytes: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| CaptureError::InvalidPayload("invalid task JSON cursor integer".to_owned()))?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn task_json_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("invalid task JSON captured batch: {error}"))
}
