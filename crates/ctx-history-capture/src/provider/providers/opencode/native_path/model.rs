use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::provider::providers::opencode) enum OpenCodeNativeSchemaFamily {
    SessionMessageSeq,
    SessionMessageSynthesizedSeq,
    SessionEntry,
    LegacyMessage,
    MessagePart,
}

impl OpenCodeNativeSchemaFamily {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::SessionMessageSeq => "session_message_seq",
            Self::SessionMessageSynthesizedSeq => "session_message_synthesized_seq",
            Self::SessionEntry => "session_entry",
            Self::LegacyMessage => "legacy_message",
            Self::MessagePart => "message_part",
        }
    }

    pub(super) const fn identity_semantics(self) -> &'static str {
        match self {
            Self::MessagePart => "opencode-native-part-id-v1",
            Self::SessionMessageSeq
            | Self::SessionMessageSynthesizedSeq
            | Self::SessionEntry
            | Self::LegacyMessage => "opencode-native-message-id-v1",
        }
    }

    pub(super) const fn ordering_semantics(self) -> &'static str {
        match self {
            Self::SessionMessageSeq => "session-id,explicit-seq,message-id",
            Self::SessionMessageSynthesizedSeq | Self::SessionEntry | Self::LegacyMessage => {
                "session-id,time-created,message-id"
            }
            Self::MessagePart => "session-id,message-time,message-id,part-time,part-id",
        }
    }

    pub(super) const fn event_table(self) -> &'static str {
        match self {
            Self::SessionMessageSeq | Self::SessionMessageSynthesizedSeq => "session_message",
            Self::SessionEntry => "session_entry",
            Self::LegacyMessage => "message",
            Self::MessagePart => "part",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OpenCodeNativeOrder {
    ExplicitSequence {
        session_id: String,
        sequence: i64,
        message_id: String,
    },
    SynthesizedSequence {
        session_id: String,
        time_created: i64,
        message_id: String,
    },
    MessagePart {
        session_id: String,
        message_time_created: i64,
        message_id: String,
        part_time_created: i64,
        part_id: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenCodeNativeEventKind {
    Message,
    Summary,
    Notice,
    ToolCall,
    ToolOutput,
    CommandOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeFileTouch {
    pub(super) path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OpenCodeNativeRejectionKind {
    MalformedJson,
    MalformedResultJson,
    UnsupportedStorageClass,
    OversizedRetainedContent,
    MissingSession,
    MissingMessage,
    SessionRelationshipMismatch,
    UnknownRecordType,
    InvalidTimestamp,
}
