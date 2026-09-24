use super::*;

impl RobotSpan {
    pub(super) fn range(&self) -> Range<usize> {
        self.start..self.end
    }
    pub(super) fn valid(&self, source: &str) -> bool {
        self.start <= self.end
            && self.end <= source.len()
            && source.is_char_boundary(self.start)
            && source.is_char_boundary(self.end)
    }
}
