#[allow(unused_imports)]
use super::*;

pub(crate) fn local_preview(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
