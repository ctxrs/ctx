use std::io::{self, IsTerminal as _, Write};

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
        );
        let stdout_writer: BoxedWriter = Box::new(anstream::AutoStream::new(
            stdout,
            stdout_context.adapter_choice(),
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
        );
        let stderr_writer: BoxedWriter = Box::new(anstream::AutoStream::new(
            stderr,
            stderr_context.adapter_choice(),
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

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.stderr.flush()
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
        Self::new(context, adapted)
    }

    const fn context(&self) -> &RenderContext {
        &self.context
    }

    fn write(&mut self, document: &Document) -> io::Result<()> {
        self.writer
            .write_all(document.render(&self.context).as_bytes())
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
