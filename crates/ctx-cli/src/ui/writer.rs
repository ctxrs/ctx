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
        let stdout_writer: BoxedWriter = Box::new(MeasuredWriter::current(
            anstream::AutoStream::new(stdout, stdout_context.adapter_choice()),
            StreamKind::Stdout,
        ));

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
        let stderr_writer: BoxedWriter = Box::new(MeasuredWriter::current(
            anstream::AutoStream::new(stderr, stderr_context.adapter_choice()),
            StreamKind::Stderr,
        ));

        Self {
            stdout: Destination::new(stdout_context, stdout_writer),
            stderr: Destination::new(stderr_context, stderr_writer),
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
        let writer: BoxedWriter = Box::new(writer);
        let adapted: BoxedWriter =
            Box::new(anstream::AutoStream::new(writer, context.adapter_choice()));
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
