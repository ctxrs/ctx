use clap::ValueEnum;

pub(crate) const DEFAULT_TERMINAL_WIDTH: usize = 80;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub(super) const fn from_cli_value(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"auto" => Some(Self::Auto),
            b"always" => Some(Self::Always),
            b"never" => Some(Self::Never),
            _ => None,
        }
    }

    pub(super) const fn as_anstream(self) -> anstream::ColorChoice {
        match self {
            Self::Auto => anstream::ColorChoice::Auto,
            Self::Always => anstream::ColorChoice::Always,
            Self::Never => anstream::ColorChoice::Never,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderContext {
    stream: StreamKind,
    color_mode: ColorMode,
    is_terminal: bool,
    color_enabled: bool,
    live_output_capable: bool,
    terminal_width: Option<usize>,
    unicode: bool,
}

impl RenderContext {
    /// Stable, unbounded plain-text context for deterministic command-local
    /// estimates and output-limit decisions. Dispatch separately accounts for
    /// the actual terminal-adapted bytes delivered at runtime.
    pub(crate) const fn canonical_human_measurement() -> Self {
        Self {
            stream: StreamKind::Stdout,
            color_mode: ColorMode::Never,
            is_terminal: false,
            color_enabled: false,
            live_output_capable: false,
            terminal_width: None,
            unicode: true,
        }
    }

    pub(crate) fn for_test(test: TestContext) -> Self {
        let auto_color_enabled = test
            .auto_color_enabled
            .unwrap_or(test.is_terminal && !test.no_color && !test.term_dumb);
        Self::from_capabilities(
            test.stream,
            test.color_mode,
            test.is_terminal,
            test.terminal_width,
            test.unicode,
            auto_color_enabled,
            test.term_dumb,
        )
    }

    pub(super) fn detected(
        stream: StreamKind,
        color_mode: ColorMode,
        is_terminal: bool,
        terminal_width: Option<usize>,
        unicode: bool,
        auto_color_enabled: bool,
        term_dumb: bool,
    ) -> Self {
        Self::from_capabilities(
            stream,
            color_mode,
            is_terminal,
            terminal_width,
            unicode,
            auto_color_enabled,
            term_dumb,
        )
    }

    fn from_capabilities(
        stream: StreamKind,
        color_mode: ColorMode,
        is_terminal: bool,
        terminal_width: Option<usize>,
        unicode: bool,
        auto_color_enabled: bool,
        term_dumb: bool,
    ) -> Self {
        let color_enabled = match color_mode {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => auto_color_enabled,
        };
        let terminal_width = is_terminal
            .then_some(terminal_width.unwrap_or(DEFAULT_TERMINAL_WIDTH))
            .filter(|width| *width > 0);

        Self {
            stream,
            color_mode,
            is_terminal,
            color_enabled,
            live_output_capable: is_terminal && !term_dumb,
            terminal_width,
            unicode,
        }
    }

    pub(crate) const fn stream(self) -> StreamKind {
        self.stream
    }

    pub(crate) const fn color_mode(self) -> ColorMode {
        self.color_mode
    }

    pub(crate) const fn is_terminal(self) -> bool {
        self.is_terminal
    }

    pub(crate) const fn color_enabled(self) -> bool {
        self.color_enabled
    }

    pub(crate) const fn live_output_capable(self) -> bool {
        self.live_output_capable
    }

    pub(super) const fn with_terminal_control_support(mut self, supported: bool) -> Self {
        self.live_output_capable = self.live_output_capable && supported;
        self
    }

    pub(crate) const fn terminal_width(self) -> Option<usize> {
        self.terminal_width
    }

    /// Width available to components after reserving the terminal's final
    /// column. Redirected human output is intentionally unbounded.
    pub(crate) fn content_width(self) -> Option<usize> {
        self.terminal_width
            .map(|width| width.saturating_sub(1).max(1))
    }

    pub(crate) const fn unicode(self) -> bool {
        self.unicode
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TestContext {
    stream: StreamKind,
    color_mode: ColorMode,
    is_terminal: bool,
    terminal_width: Option<usize>,
    unicode: bool,
    no_color: bool,
    term_dumb: bool,
    auto_color_enabled: Option<bool>,
}

impl TestContext {
    pub(crate) const fn tty(stream: StreamKind, width: usize) -> Self {
        Self {
            stream,
            color_mode: ColorMode::Never,
            is_terminal: true,
            terminal_width: Some(width),
            unicode: true,
            no_color: false,
            term_dumb: false,
            auto_color_enabled: None,
        }
    }

    pub(crate) const fn pipe(stream: StreamKind) -> Self {
        Self {
            stream,
            color_mode: ColorMode::Auto,
            is_terminal: false,
            terminal_width: None,
            unicode: true,
            no_color: false,
            term_dumb: false,
            auto_color_enabled: None,
        }
    }

    pub(crate) const fn color(mut self, color_mode: ColorMode) -> Self {
        self.color_mode = color_mode;
        self
    }

    pub(crate) const fn unicode(mut self, unicode: bool) -> Self {
        self.unicode = unicode;
        self
    }

    pub(crate) const fn no_color(mut self, no_color: bool) -> Self {
        self.no_color = no_color;
        self
    }

    pub(crate) const fn term_dumb(mut self, term_dumb: bool) -> Self {
        self.term_dumb = term_dumb;
        self
    }

    pub(crate) const fn auto_color(mut self, enabled: bool) -> Self {
        self.auto_color_enabled = Some(enabled);
        self
    }

    pub(crate) const fn unknown_width(mut self) -> Self {
        self.terminal_width = None;
        self
    }
}
