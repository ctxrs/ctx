mod complete_content;
mod message;
pub(crate) mod native_path;
mod source;

pub(crate) use complete_content::{
    decode_deepagents_content_address, resolve_deepagents_content,
    validate_deepagents_content_schema, DeepAgentsContentAddress, DEEPAGENTS_CONTENT_LOCATOR_KIND,
};

#[cfg(test)]
#[path = "deepagents/tests.rs"]
mod tests;
