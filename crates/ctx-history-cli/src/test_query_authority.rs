use std::path::Path;

use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};

pub(crate) fn publish_empty_generation(data_root: &Path) -> String {
    let index_root = data_root.join("search/lexical");
    GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap()
        .generation_id
}

pub(crate) fn active_generation_id(data_root: &Path) -> String {
    VerifiedIndex::open_pinned(data_root.join("search/lexical"))
        .unwrap()
        .generation_id()
        .to_owned()
}
