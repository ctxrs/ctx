mod reader;
mod source_backed;

pub(crate) use source_backed::{
    observe_custom_history_source_backed_explicit, revalidate_custom_history_source_backed,
    scan_custom_history_source_backed_explicit, CustomHistorySourceBackedDisposition,
    CustomHistorySourceBackedInput, CustomHistorySourceBackedOutcome,
    CustomHistorySourceBackedResolver,
};

#[cfg(test)]
mod source_backed_tests;
