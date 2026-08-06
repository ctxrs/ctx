use std::io::{self, IsTerminal as _, Write};

use crate::output::MeasuredWriter;

use super::{ColorMode, Document, RenderContext, StreamKind};

type BoxedWriter = Box<dyn Write + Send>;

pub(crate) struct Ui {
    stdout: Destination,
    stderr: Destination,
}

impl Ui {
    /// Probes stdout and stderr independently and owns adaptive writers for
    /// both destinations.
    pub(crate) fn stdio(color_mode: ColorMode) -> Self {
        let stdout = io::stdout();
        let stdout_terminal = stdout.is_terminal();
        let stdout_auto_color = auto_color_enabled(&stdout);
        let stdout_context = RenderContext::detected(
            StreamKind::Stdout,
            color_mode,
            stdout_terminal,
            stream_width(StreamKind::Stdout),
            supports_unicode::on(supports_unicode::Stream::Stdout),
            stdout_auto_color,
            term_is_dumb(),
        );
        let stdout_terminal_controls = stdio_terminal_controls(&stdout, stdout_context);
        let stderr = io::stderr();
        let stderr_terminal = stderr.is_terminal();
        let stderr_auto_color = auto_color_enabled(&stderr);
        let stderr_context = RenderContext::detected(
            StreamKind::Stderr,
            color_mode,
            stderr_terminal,
            stream_width(StreamKind::Stderr),
            supports_unicode::on(supports_unicode::Stream::Stderr),
            stderr_auto_color,
            term_is_dumb(),
        );
        let stderr_terminal_controls = stdio_terminal_controls(&stderr, stderr_context);
        Self {
            stdout: Destination::adapted(stdout_context, stdout, stdout_terminal_controls),
            stderr: Destination::adapted(stderr_context, stderr, stderr_terminal_controls),
        }
    }

    /// Constructs a UI with explicit capabilities and owned writers.
    pub(crate) fn with_writers<Out, Err>(
        stdout: Out,
        stdout_context: RenderContext,
        stderr: Err,
        stderr_context: RenderContext,
    ) -> Self
    where
        Out: Write + Send + 'static,
        Err: Write + Send + 'static,
    {
        Self {
            stdout: Destination::injected(stdout_context, stdout),
            stderr: Destination::injected(stderr_context, stderr),
        }
    }

    #[cfg(test)]
    fn with_writers_and_terminal_controls<Out, Err>(
        stdout: Out,
        stdout_context: RenderContext,
        stdout_terminal_controls: bool,
        stderr: Err,
        stderr_context: RenderContext,
        stderr_terminal_controls: bool,
    ) -> Self
    where
        Out: Write + Send + 'static,
        Err: Write + Send + 'static,
    {
        Self {
            stdout: Destination::injected_with_terminal_controls(
                stdout_context,
                stdout,
                stdout_terminal_controls,
            ),
            stderr: Destination::injected_with_terminal_controls(
                stderr_context,
                stderr,
                stderr_terminal_controls,
            ),
        }
    }

    pub(crate) fn context(&self, stream: StreamKind) -> &RenderContext {
        match stream {
            StreamKind::Stdout => self.stdout.context(),
            StreamKind::Stderr => self.stderr.context(),
        }
    }

    pub(crate) fn stdout_context(&self) -> &RenderContext {
        self.stdout.context()
    }

    pub(crate) fn stderr_context(&self) -> &RenderContext {
        self.stderr.context()
    }

    pub(crate) fn write(&mut self, stream: StreamKind, document: &Document) -> io::Result<()> {
        match stream {
            StreamKind::Stdout => self.stdout.write(document),
            StreamKind::Stderr => self.stderr.write(document),
        }
    }

    pub(crate) fn write_stdout(&mut self, document: &Document) -> io::Result<()> {
        self.stdout.write(document)
    }

    pub(crate) fn write_stderr(&mut self, document: &Document) -> io::Result<()> {
        self.stderr.write(document)
    }

    pub(crate) fn stdout_live_output(&mut self) -> LiveOutput<&mut (dyn Write + Send)> {
        let context = *self.stdout.context();
        LiveOutput::new(self.stdout.writer(), context)
    }

    pub(crate) fn stderr_live_output(&mut self) -> LiveOutput<&mut (dyn Write + Send)> {
        let context = *self.stderr.context();
        LiveOutput::new(self.stderr.writer(), context)
    }

    pub(crate) fn stdout_writer(&mut self) -> &mut (dyn Write + Send) {
        self.stdout.writer()
    }

    pub(crate) fn stderr_writer(&mut self) -> &mut (dyn Write + Send) {
        self.stderr.writer()
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.stderr.flush()
    }
}

/// Owns all cursor motion used to replace a rendered terminal frame. Dynamic
/// content is rendered separately and is never part of a control sequence.
pub(crate) struct LiveOutput<W> {
    writer: W,
    context: RenderContext,
    rendered_lines: usize,
}

impl<W: Write> LiveOutput<W> {
    pub(crate) fn new(writer: W, context: RenderContext) -> Self {
        Self {
            writer,
            context,
            rendered_lines: 0,
        }
    }

    pub(crate) const fn context(&self) -> &RenderContext {
        &self.context
    }

    #[cfg(test)]
    pub(crate) fn into_inner(self) -> W {
        self.writer
    }

    #[cfg(test)]
    pub(crate) const fn inner(&self) -> &W {
        &self.writer
    }

    pub(crate) fn write_document(&mut self, document: &Document) -> io::Result<()> {
        self.writer
            .write_all(document.render(&self.context).as_bytes())?;
        self.writer.flush()
    }

    pub(crate) fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }

    pub(crate) fn write_frame(&mut self, document: &Document, final_frame: bool) -> io::Result<()> {
        let frame = document.render(&self.context);
        if !self.context.live_output_capable() {
            self.writer.write_all(frame.as_bytes())?;
            self.writer.write_all(b"\n")?;
            return self.writer.flush();
        }

        let lines = frame
            .strip_suffix('\n')
            .unwrap_or(&frame)
            .split('\n')
            .collect::<Vec<_>>();
        if self.rendered_lines == 0 {
            self.writer.write_all(frame.as_bytes())?;
            self.rendered_lines = if final_frame { 0 } else { lines.len() };
            return self.writer.flush();
        }

        write!(self.writer, "\u{1b}[{}A", self.rendered_lines)?;
        let previous_lines = self.rendered_lines;
        let height = previous_lines.max(lines.len());
        for row in 0..height {
            self.writer.write_all(b"\r\x1b[2K")?;
            if let Some(line) = lines.get(row) {
                self.writer.write_all(line.as_bytes())?;
            }
            self.writer.write_all(b"\n")?;
        }
        if previous_lines > lines.len() {
            write!(
                self.writer,
                "\u{1b}[{}A",
                previous_lines.saturating_sub(lines.len())
            )?;
        }
        self.rendered_lines = if final_frame { 0 } else { lines.len() };
        self.writer.flush()
    }
}

struct Destination {
    context: RenderContext,
    writer: BoxedWriter,
}

impl Destination {
    fn new(context: RenderContext, writer: BoxedWriter) -> Self {
        Self { context, writer }
    }

    fn injected<W>(context: RenderContext, writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self::injected_with_terminal_controls(context, writer, context.live_output_capable())
    }

    fn injected_with_terminal_controls<W>(
        context: RenderContext,
        writer: W,
        terminal_controls: bool,
    ) -> Self
    where
        W: Write + Send + 'static,
    {
        let writer: BoxedWriter = Box::new(writer);
        Self::adapted(context, writer, terminal_controls)
    }

    /// Keeps platform terminal adaptation at the final shared writer boundary.
    /// Measurement remains outside the adapter so every caller follows the
    /// same stdout/stderr accounting path.
    fn adapted<W>(context: RenderContext, writer: W, terminal_controls: bool) -> Self
    where
        W: anstream::stream::RawStream + anstream::stream::AsLockedWrite + Send + 'static,
    {
        let context = context.with_terminal_control_support(terminal_controls);
        let adapted = terminal_adapter(writer, context);
        let measured: BoxedWriter = Box::new(MeasuredWriter::current(adapted, context.stream()));
        Self::new(context, measured)
    }

    const fn context(&self) -> &RenderContext {
        &self.context
    }

    fn write(&mut self, document: &Document) -> io::Result<()> {
        self.writer
            .write_all(document.render(&self.context).as_bytes())
    }

    fn writer(&mut self) -> &mut (dyn Write + Send) {
        self.writer.as_mut()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn terminal_adapter<W>(writer: W, context: RenderContext) -> anstream::AutoStream<W>
where
    W: anstream::stream::RawStream,
{
    anstream::AutoStream::new(writer, terminal_adapter_choice(context))
}

const fn terminal_adapter_choice(context: RenderContext) -> anstream::ColorChoice {
    if context.live_output_capable() {
        // The actual destination handle has already enabled VT processing.
        // Bypass anstream's combined stdout/stderr capability probe.
        anstream::ColorChoice::AlwaysAnsi
    } else if context.color_enabled() {
        // Keep anstream's Wincon styling fallback when VT is unavailable.
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    }
}

fn resolve_terminal_controls(
    context: RenderContext,
    enable_for_destination: impl FnOnce() -> bool,
) -> bool {
    context.live_output_capable() && enable_for_destination()
}

#[cfg(not(windows))]
fn stdio_terminal_controls<W>(_writer: &W, context: RenderContext) -> bool {
    resolve_terminal_controls(context, || true)
}

#[cfg(windows)]
fn stdio_terminal_controls<W>(writer: &W, context: RenderContext) -> bool
where
    W: std::os::windows::io::AsRawHandle,
{
    resolve_terminal_controls(context, || {
        enable_windows_terminal_controls(writer.as_raw_handle())
    })
}

#[cfg(windows)]
fn enable_windows_terminal_controls(handle: std::os::windows::io::RawHandle) -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };

    let handle = handle as HANDLE;
    if handle.is_null() {
        return false;
    }

    let mut mode: CONSOLE_MODE = 0;
    unsafe {
        if GetConsoleMode(handle, &mut mode) == 0 {
            // `IsTerminal` also recognizes MSYS/Cygwin pseudo-terminals,
            // whose pipe handles do not expose console modes. Ordinary pipes
            // never reach this probe because the render context gates them.
            return std::env::var_os("TERM").is_some_and(|term| term != "dumb" && term != "cygwin");
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            return true;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

fn auto_color_enabled<S>(stream: &S) -> bool
where
    S: anstream::stream::RawStream,
{
    matches!(
        anstream::AutoStream::choice(stream),
        anstream::ColorChoice::Always | anstream::ColorChoice::AlwaysAnsi
    )
}

fn term_is_dumb() -> bool {
    std::env::var_os("TERM").is_some_and(|term| term == "dumb")
}

fn stream_width(stream: StreamKind) -> Option<usize> {
    #[cfg(any(unix, windows))]
    {
        let size = match stream {
            StreamKind::Stdout => terminal_size::terminal_size_of(io::stdout()),
            StreamKind::Stderr => terminal_size::terminal_size_of(io::stderr()),
        };
        size.map(|(terminal_size::Width(width), _)| usize::from(width))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = stream;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{Line, Span, TestContext, Token};
    use std::{
        cell::Cell,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn document(lines: &[&str]) -> Document {
        let mut document = Document::new();
        for line in lines {
            document.push_line(Line::new().with(Span::new(*line, Token::Heading)));
        }
        document
    }

    #[test]
    fn live_controller_bytes_cover_first_grow_shrink_and_final_frames() {
        let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
        let mut output = LiveOutput::new(Vec::new(), context);
        output.write_frame(&document(&["one"]), false).unwrap();
        output
            .write_frame(&document(&["one", "two"]), false)
            .unwrap();
        output.write_frame(&document(&["short"]), false).unwrap();
        output.write_frame(&document(&["done"]), true).unwrap();
        output.write_frame(&document(&["after"]), false).unwrap();

        let rendered = String::from_utf8(output.into_inner()).unwrap();
        assert_eq!(
            rendered,
            concat!(
                "one\n",
                "\x1b[1A\r\x1b[2Kone\n\r\x1b[2Ktwo\n",
                "\x1b[2A\r\x1b[2Kshort\n\r\x1b[2K\n\x1b[1A",
                "\x1b[1A\r\x1b[2Kdone\n",
                "after\n",
            )
        );
    }

    #[test]
    fn ui_adapter_preserves_live_controls_when_tty_styling_is_disabled() {
        let contexts = [
            TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
            TestContext::tty(StreamKind::Stdout, 80)
                .color(ColorMode::Auto)
                .no_color(true),
            TestContext::tty(StreamKind::Stdout, 80)
                .color(ColorMode::Auto)
                .auto_color(false),
        ];
        for test_context in contexts {
            let stdout = SharedWriter::default();
            let capture = stdout.clone();
            let mut ui = Ui::with_writers(
                stdout,
                RenderContext::for_test(test_context),
                Vec::new(),
                RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
            );
            let mut output = ui.stdout_live_output();
            output
                .write_frame(&document(&["long first row", "stale second row"]), false)
                .unwrap();
            output
                .write_frame(&document(&["short replacement"]), false)
                .unwrap();
            output.write_frame(&document(&["done"]), true).unwrap();

            let rendered = capture.text();
            assert_eq!(
                rendered,
                concat!(
                    "long first row\nstale second row\n",
                    "\x1b[2A\r\x1b[2Kshort replacement\n\r\x1b[2K\n\x1b[1A",
                    "\x1b[1A\r\x1b[2Kdone\n",
                )
            );
            assert!(
                !rendered.contains("\x1b[1m"),
                "styling must remain disabled"
            );
        }
    }

    #[test]
    fn terminal_adapter_capability_is_independent_of_styling() {
        let live_without_style = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
        );
        assert!(!live_without_style.color_enabled());
        assert!(live_without_style.live_output_capable());
        assert_eq!(
            terminal_adapter_choice(live_without_style),
            anstream::ColorChoice::AlwaysAnsi
        );
        let adapted = terminal_adapter(Vec::new(), live_without_style);
        assert_eq!(adapted.current_choice(), anstream::ColorChoice::AlwaysAnsi);
        let destination = Destination::adapted(live_without_style, Vec::new(), true);
        assert!(destination.context().live_output_capable());

        let styled_pipe =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always));
        assert!(!styled_pipe.live_output_capable());
        assert_eq!(
            terminal_adapter_choice(styled_pipe),
            anstream::ColorChoice::Always
        );

        for unadapted in [
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            RenderContext::for_test(
                TestContext::tty(StreamKind::Stdout, 80)
                    .term_dumb(true)
                    .color(ColorMode::Never),
            ),
        ] {
            assert!(!unadapted.live_output_capable());
            assert!(!unadapted.color_enabled());
            assert_eq!(
                terminal_adapter_choice(unadapted),
                anstream::ColorChoice::Never
            );
            let adapted = terminal_adapter(Vec::new(), unadapted);
            assert_eq!(adapted.current_choice(), anstream::ColorChoice::Never);
        }
    }

    #[test]
    fn per_destination_terminal_control_resolution_gates_live_output() {
        let live = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
        );
        assert!(resolve_terminal_controls(live, || true));
        assert!(!resolve_terminal_controls(live, || false));

        let pipe =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always));
        assert!(pipe.color_enabled());
        let probed = Cell::new(false);
        assert!(!resolve_terminal_controls(pipe, || {
            probed.set(true);
            true
        }));
        assert!(!probed.get(), "redirected streams must not be probed");

        let unsupported = live.with_terminal_control_support(false);
        assert!(!unsupported.live_output_capable());
        assert_eq!(
            terminal_adapter_choice(unsupported),
            anstream::ColorChoice::Never
        );

        let styled_unsupported = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Always),
        )
        .with_terminal_control_support(false);
        assert!(!styled_unsupported.live_output_capable());
        assert!(styled_unsupported.color_enabled());
        assert_eq!(
            terminal_adapter_choice(styled_unsupported),
            anstream::ColorChoice::Always
        );
    }

    #[test]
    fn split_stdout_stderr_terminal_controls_are_independent() {
        let cases = [
            (
                RenderContext::for_test(
                    TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
                ),
                true,
                RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
                false,
            ),
            (
                RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
                false,
                RenderContext::for_test(
                    TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Never),
                ),
                true,
            ),
        ];

        for (stdout_context, stdout_controls, stderr_context, stderr_controls) in cases {
            let stdout = SharedWriter::default();
            let stdout_capture = stdout.clone();
            let stderr = SharedWriter::default();
            let stderr_capture = stderr.clone();
            let mut ui = Ui::with_writers_and_terminal_controls(
                stdout,
                stdout_context,
                stdout_controls,
                stderr,
                stderr_context,
                stderr_controls,
            );

            assert_eq!(ui.stdout_context().live_output_capable(), stdout_controls);
            assert_eq!(ui.stderr_context().live_output_capable(), stderr_controls);

            {
                let mut output = ui.stdout_live_output();
                output
                    .write_frame(&document(&["stdout first", "stdout stale"]), false)
                    .unwrap();
                output
                    .write_frame(&document(&["stdout replacement"]), true)
                    .unwrap();
            }
            {
                let mut output = ui.stderr_live_output();
                output
                    .write_frame(&document(&["stderr first", "stderr stale"]), false)
                    .unwrap();
                output
                    .write_frame(&document(&["stderr replacement"]), true)
                    .unwrap();
            }

            let stdout = stdout_capture.text();
            let stderr = stderr_capture.text();
            assert_eq!(stdout.contains("\x1b[2A"), stdout_controls);
            assert_eq!(stdout.contains("\x1b[2K"), stdout_controls);
            assert_eq!(stderr.contains("\x1b[2A"), stderr_controls);
            assert_eq!(stderr.contains("\x1b[2K"), stderr_controls);
            assert!(!stdout.contains("\x1b[1m"));
            assert!(!stderr.contains("\x1b[1m"));
        }
    }

    #[test]
    fn append_controller_writes_documents_and_lines_exactly() {
        let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
        let mut output = LiveOutput::new(Vec::new(), context);
        output.write_document(&document(&["plain"])).unwrap();
        output.write_line(r#"{"type":"ctx_progress"}"#).unwrap();
        assert_eq!(
            String::from_utf8(output.into_inner()).unwrap(),
            "plain\n{\"type\":\"ctx_progress\"}\n"
        );
    }

    #[test]
    fn pipe_and_term_dumb_append_without_cursor_motion() {
        for context in [
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).term_dumb(true)),
        ] {
            let mut output = LiveOutput::new(Vec::new(), context);
            output.write_frame(&document(&["one"]), false).unwrap();
            output.write_frame(&document(&["two"]), false).unwrap();
            let rendered = String::from_utf8(output.into_inner()).unwrap();
            assert_eq!(rendered, "one\n\ntwo\n\n");
            assert!(!rendered.contains('\u{1b}'));
        }
    }

    #[test]
    fn forced_color_on_a_pipe_never_enables_cursor_motion() {
        let context =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always));
        assert!(context.color_enabled());
        assert!(!context.live_output_capable());
        let mut output = LiveOutput::new(Vec::new(), context);
        output.write_frame(&document(&["one"]), false).unwrap();
        output.write_frame(&document(&["two"]), false).unwrap();
        let rendered = String::from_utf8(output.into_inner()).unwrap();
        assert!(rendered.contains("\x1b[1m"));
        assert!(!rendered.contains("\x1b[1A"));
        assert!(!rendered.contains("\x1b[2K"));
    }

    #[test]
    fn dynamic_text_is_neutralized_before_live_control_bytes() {
        let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
        let mut output = LiveOutput::new(Vec::new(), context);
        output
            .write_frame(&document(&["source\x1b[999A\rname"]), false)
            .unwrap();
        let rendered = String::from_utf8(output.into_inner()).unwrap();
        assert_eq!(rendered, "source\\x1b[999A\\rname\n");
    }
}
