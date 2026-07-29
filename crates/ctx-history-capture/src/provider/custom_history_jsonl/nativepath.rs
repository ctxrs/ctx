use std::{io::BufRead, path::Path};

use ctx_history_store::Store;

use crate::{CaptureError, CustomHistoryJsonlV1ImportOptions, ProviderImportSummary, Result};

mod reader;
mod source_backed;

pub(crate) use source_backed::{
    observe_custom_history_source_backed_explicit, revalidate_custom_history_source_backed,
    scan_custom_history_source_backed_explicit, CustomHistoryReplacementEvidence,
    CustomHistoryReplacementReason, CustomHistorySourceBackedDisposition,
    CustomHistorySourceBackedError, CustomHistorySourceBackedInput,
    CustomHistorySourceBackedInventory, CustomHistorySourceBackedOutcome,
    CustomHistorySourceBackedPage, CustomHistorySourceBackedReceipt,
    CustomHistorySourceBackedResolver, CustomHistorySourceBackedResult,
    CustomHistorySourceBackedRoute,
};

pub(crate) fn import_custom_history_nativepath(
    _path: &Path,
    _store: &mut Store,
    _options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Custom History Store ingestion was removed; use an explicitly configured source-backed route"
            .to_owned(),
    ))
}

pub(crate) fn import_custom_history_nativepath_reader(
    _reader: impl BufRead,
    _store: &mut Store,
    _options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Custom History reader Store ingestion was removed; use an explicitly configured source-backed route"
            .to_owned(),
    ))
}

pub(crate) use reader::{
    validate_custom_history_nativepath, validate_custom_history_nativepath_reader,
};

#[cfg(test)]
mod source_backed_tests;
