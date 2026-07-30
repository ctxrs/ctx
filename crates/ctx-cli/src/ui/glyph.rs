use super::RenderContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Glyph {
    Success,
    Failure,
    Warning,
    Progress,
    Rule,
    Ellipsis,
}

impl Glyph {
    pub(super) const fn render(self, context: &RenderContext) -> &'static str {
        match (self, context.unicode()) {
            (Self::Success, true) => "✓",
            (Self::Success, false) => "OK",
            (Self::Failure, true) => "✗",
            (Self::Failure, false) => "X",
            (Self::Warning, _) => "!",
            (Self::Progress, true) => "━",
            (Self::Progress, false) => "=",
            (Self::Rule, true) => "─",
            (Self::Rule, false) => "-",
            (Self::Ellipsis, true) => "…",
            (Self::Ellipsis, false) => "...",
        }
    }
}
