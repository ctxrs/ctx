use super::*;

impl SqlToken<'_> {
    pub(super) fn is(&self, word: &str) -> bool {
        self.kind == b'w' && self.text.eq_ignore_ascii_case(word)
    }
}
