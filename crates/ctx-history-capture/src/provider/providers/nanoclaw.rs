pub(crate) mod native_path;
mod position;
mod project;
mod projection;
mod rows;
mod source;

mod complete_content;
pub(crate) use complete_content::{selected_component_addresses, NanoClawCompleteProject};
pub(crate) use position::decode_nanoclaw_message_locator;

#[cfg(test)]
#[path = "nanoclaw/tests.rs"]
mod tests;

// These revisions remain the released NanoClaw semantic contract.
const NANOCLAW_CAPTURE_REVISION: u32 = 2;
const NANOCLAW_POLICY_REVISION: u32 = 4;
pub(crate) const NANOCLAW_MESSAGE_LOCATOR_KIND: &str = "nanoclaw-project-message-v1";
