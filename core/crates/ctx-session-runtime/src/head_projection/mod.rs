mod activity;
mod deltas;
mod events;
mod messages;
mod metadata;
mod turns;

pub use activity::{activity_from_turn, derive_summary_activity};
pub use deltas::{
    build_session_summary_delta, resolve_projection_rev_for_stream_delta,
    should_include_session_metadata_in_head_delta,
};
pub use events::{event_context_window, is_session_gap_notice};
pub use messages::{derive_message_preview, message_from_event};
pub use metadata::session_metadata_from_session;
pub use turns::{
    patch_turn_from_event, recompute_turn_tool_counts, should_refresh_turn_from_store,
    turn_from_event,
};

#[cfg(test)]
mod tests;
