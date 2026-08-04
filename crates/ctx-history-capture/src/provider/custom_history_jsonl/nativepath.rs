mod reader;
mod source_backed;

pub(crate) use source_backed::{
    custom_history_jsonl_family_adapter, CustomHistorySourceBackedInput,
};

#[cfg(test)]
mod source_backed_tests;
