use std::path::Path;

use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

pub(crate) mod native_path;
mod normalization;
mod relationships;
mod source;

pub(crate) use relationships::{
    decode_shelley_conversation, decode_shelley_message, shelley_conversation_values,
    shelley_message_complete_text, shelley_message_values, shelley_native_record_id,
    shelley_verified_record_values, ShelleyConversationRow,
};
pub(crate) use source::{
    shelley_conversation_columns, shelley_conversation_select_expressions, shelley_message_columns,
    shelley_message_select_expressions,
};

pub(crate) use normalization::shelley_complete_event;

const SHELLEY_CAPTURE_REVISION: u32 = 11;
const SHELLEY_POLICY_REVISION: u32 = 7;
const SHELLEY_MESSAGE_VALUE_COUNT: usize = 15;
const SHELLEY_CONVERSATION_VALUE_COUNT: usize = 17;

pub(crate) fn import_shelley_nativepath(
    _path: &Path,
    _store: &mut ctx_history_store::Store,
    _context: ProviderAdapterContext,
    _import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Shelley Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}
