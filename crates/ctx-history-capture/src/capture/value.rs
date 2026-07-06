#[allow(unused_imports)]
use super::*;

pub(crate) fn value_looks_like_file_content(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.contains('\n')
            || text.len() > 120
            || text.contains("*** Begin Patch")
            || text.contains("@@")
    })
}
