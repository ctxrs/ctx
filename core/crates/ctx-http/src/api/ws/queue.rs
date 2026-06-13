use std::time::Duration;

mod buffers;
mod partials;
mod stream;

#[cfg(test)]
pub(super) use buffers::BACKGROUND_HEAD_BATCH_CHUNK_LIMIT;
pub(super) use buffers::{
    HeadBatchBuffer, HeadBatchLane, NextWorkspaceStreamItem, SummaryBatchBuffer,
    HEAD_BATCH_TOTAL_LIMIT,
};
pub(super) use stream::{
    log_head_batch_push_error, log_summary_batch_push_error, push_stream_message,
    take_next_workspace_stream_item, workspace_stream_is_idle, StreamQueue,
};

#[cfg(test)]
mod tests;
