pub(crate) mod native_path;
#[cfg(test)]
pub(crate) use native_path::{
    scan_lingma_source_backed_v0, LingmaDatabaseSourceV0, LingmaSourceInventoryV0,
};

#[cfg(test)]
#[path = "lingma/tests.rs"]
mod tests;
