//! Kiro's provider-owned NativePath ingestion leaf.
//!
//! The provider entry point below routes solely to its NativePath driver.
//! There is no alternate producer, projector, coordinator, or runtime fallback.

mod event;
mod history;
pub(crate) mod native_path;

pub(crate) use history::{
    decode_kiro_conversation_for_complete, kiro_history_events, kiro_provider_session_id,
    kiro_session_started_at,
};
