//! Deep Agents source-backed capture namespace.

pub(crate) mod source_backed;

// The complete-content compatibility bridge still consumes provider message
// identity and classification through this historical module path.
pub(crate) use super::message::DeepAgentsNativeEvent;
pub(super) use super::message::{
    deepagents_message_identity, deepagents_native_event, DeepAgentsParsedMessage,
};
