mod scanner;
mod source_backed;

pub(crate) use source_backed::{
    hydrate_trae_source_backed_locator_v0, scan_trae_source_backed_explicit_v0,
    TraeSourceBackedErrorV0,
};
