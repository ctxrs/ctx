#![allow(unused_imports)]
use std::{env, fmt, path::PathBuf, str::FromStr, sync::OnceLock, time::SystemTime};

use chrono::{DateTime, Utc};

use directories::BaseDirs;

use regex::Regex;

use serde::{Deserialize, Serialize};

use thiserror::Error;

use uuid::Uuid;

macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
        default $default:ident
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn variants() -> &'static [&'static str] {
                &[$($value),+]
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(CoreError::InvalidEnumValue {
                        enum_name: stringify!($name),
                        value: value.to_owned(),
                    }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

mod history_jsonl;

mod provider;

pub use history_jsonl::*;

pub use provider::*;

#[path = "core/error.rs"]
mod core_error;
pub(crate) use core_error::*;
pub use core_error::{CoreError, Result};

#[path = "core/time.rs"]
mod core_time;
pub(crate) use core_time::*;
pub use core_time::{utc_now, CaptureSource, ContextCitation, EntityTimestamps};

#[path = "core/visibility.rs"]
mod core_visibility;
pub use core_visibility::Visibility;
pub(crate) use core_visibility::*;

#[path = "core/summary.rs"]
mod core_summary;
pub use core_summary::Fidelity;
pub(crate) use core_summary::*;

#[path = "core/sync.rs"]
mod core_sync;
pub(crate) use core_sync::*;
pub use core_sync::{SyncAlias, SyncDirection, SyncMetadata, SyncOutboxItem, SyncState};

#[path = "core/confidence.rs"]
mod core_confidence;
pub use core_confidence::Confidence;
pub(crate) use core_confidence::*;

#[path = "core/text.rs"]
mod core_text;
pub use core_text::RedactionState;
pub(crate) use core_text::*;

#[path = "core/search.rs"]
mod core_search;
pub(crate) use core_search::*;

#[path = "core/import.rs"]
mod core_import;
pub(crate) use core_import::*;
pub use core_import::{CaptureSourceKind, RunType};

#[path = "core/antigravity.rs"]
mod core_antigravity;
pub use core_antigravity::CaptureProvider;
pub(crate) use core_antigravity::*;

#[path = "core/record.rs"]
mod core_record;
pub(crate) use core_record::*;
pub use core_record::{
    CitationReference, FileTouched, HistoryRecord, HistoryRecordLink, HistoryRecordLinkType,
    HistoryRecordMetadata, HistoryRecordStatus, HistoryRecordTag, RecordEdge, RecordEdgeType, Run,
    Summary,
};

#[path = "core/agent.rs"]
mod core_agent;
pub use core_agent::AgentType;
pub(crate) use core_agent::*;

#[path = "core/session.rs"]
mod core_session;
pub(crate) use core_session::*;
pub use core_session::{Session, SessionEdge, SessionEdgeType, SessionStatus};

#[path = "core/status.rs"]
mod core_status;
pub(crate) use core_status::*;
pub use core_status::{RunStatus, SyncBatch, SyncBatchStatus};

#[path = "core/event.rs"]
mod core_event;
pub(crate) use core_event::*;
pub use core_event::{CaptureEnvelope, Event, EventRole, EventType};

#[path = "core/vcs.rs"]
mod core_vcs;
pub(crate) use core_vcs::*;
pub use core_vcs::{VcsChange, VcsChangeKind, VcsHost, VcsKind};

#[path = "core/artifact.rs"]
mod core_artifact;
pub(crate) use core_artifact::*;
pub use core_artifact::{Artifact, ArtifactKind, ContextCitationType, HistoryRecordLinkTargetType};

#[path = "core/provider.rs"]
mod core_provider;
pub(crate) use core_provider::*;
pub use core_provider::{CaptureSourceDescriptor, SummaryKind};

#[path = "core/file.rs"]
mod core_file;
pub use core_file::FileChangeKind;
pub(crate) use core_file::*;

#[path = "core/tag.rs"]
mod core_tag;
pub use core_tag::TagKind;
pub(crate) use core_tag::*;

#[path = "core/blob.rs"]
mod core_blob;
pub use core_blob::SyncOutboxOperation;
pub(crate) use core_blob::*;

#[path = "core/audit.rs"]
mod core_audit;
pub(crate) use core_audit::*;
pub use core_audit::{AuditActorKind, AuditLogEntry};

#[path = "core/archive.rs"]
mod core_archive;
pub use core_archive::SessionHistoryArchive;
pub(crate) use core_archive::*;

#[path = "core/path.rs"]
mod core_path;
pub(crate) use core_path::*;
pub use core_path::{
    blob_dir, config_path, default_data_root, device_path, history_dir, logs_dir, object_dir,
    VcsWorkspace,
};

#[path = "core/json.rs"]
mod core_json;
pub use core_json::Tag;
pub(crate) use core_json::*;

#[path = "core/cursor.rs"]
mod core_cursor;
pub(crate) use core_cursor::*;
pub use core_cursor::{ContextPagination, SyncCursor};

#[path = "core/context.rs"]
mod core_context;
pub(crate) use core_context::*;
pub use core_context::{ContextLinks, ContextTruncation};

#[path = "core/default.rs"]
mod core_default;
pub(crate) use core_default::*;

#[path = "core/new.rs"]
mod core_new;
pub use core_new::new_id;
pub(crate) use core_new::*;

#[path = "core/sqlite.rs"]
mod core_sqlite;
pub use core_sqlite::database_path;
pub(crate) use core_sqlite::*;

#[path = "core/spool.rs"]
mod core_spool;
pub(crate) use core_spool::*;
pub use core_spool::{inbox_dir, spool_dir};

#[path = "core/redact.rs"]
mod core_redact;
pub(crate) use core_redact::*;
pub use core_redact::{
    redact_preview, redact_secret_markers, redact_share_safe_markers, redact_share_safe_preview,
};

#[path = "core/secret.rs"]
mod core_secret;
pub(crate) use core_secret::*;

#[path = "core/credentialed.rs"]
mod core_credentialed;
pub(crate) use core_credentialed::*;

#[path = "core/database.rs"]
mod core_database;
pub(crate) use core_database::*;

#[path = "core/email.rs"]
mod core_email;
pub(crate) use core_email::*;

#[path = "core/bearer.rs"]
mod core_bearer;
pub(crate) use core_bearer::*;

#[path = "core/authorization.rs"]
mod core_authorization;
pub(crate) use core_authorization::*;

#[path = "core/password.rs"]
mod core_password;
pub(crate) use core_password::*;

#[path = "core/standalone.rs"]
mod core_standalone;
pub(crate) use core_standalone::*;

#[cfg(test)]
#[path = "core_tests/tests.rs"]
mod tests;
