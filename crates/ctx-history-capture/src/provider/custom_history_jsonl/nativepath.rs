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

pub(crate) use reader::{
    validate_custom_history_nativepath, validate_custom_history_nativepath_reader,
};

#[cfg(test)]
mod source_backed_tests;
