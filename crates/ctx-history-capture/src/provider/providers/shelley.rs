use std::path::Path;

use ctx_history_store::Store;

use crate::{ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result};

mod native_path;
mod normalization;
mod relationships;
mod source;

pub(crate) use relationships::{
    decode_shelley_conversation, decode_shelley_message, shelley_conversation_values,
    shelley_message_complete_text, shelley_message_values, shelley_verified_record_values,
    ShelleyConversationRow, ShelleyMessageRow,
};
#[cfg(test)]
pub(crate) use relationships::{shelley_event_index, shelley_value_text};
pub(crate) use source::{
    shelley_conversation_columns, shelley_conversation_select_expressions, shelley_message_columns,
    shelley_message_select_expressions,
};

pub(crate) use normalization::shelley_complete_event;

const SHELLEY_CAPTURE_REVISION: u32 = 10;
const SHELLEY_POLICY_REVISION: u32 = 6;
const SHELLEY_MESSAGE_VALUE_COUNT: usize = 15;
const SHELLEY_CONVERSATION_VALUE_COUNT: usize = 17;

pub(crate) fn import_shelley_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_shelley_native_path(path, store, context, import_options)
}
