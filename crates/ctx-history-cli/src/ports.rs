use ctx_terminal::{Document, RenderContext, StreamKind, Ui};
use std::{io, time::Duration};

/// Domain-owned terminal facts from one search attempt. Exact work remains in
/// the read-application receipt; presentation adapters choose how to serialize it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchExecutionObservation {
    pub refresh_duration: Option<Duration>,
    pub refresh_status: Option<SearchRefreshStatus>,
    pub refresh_source_count: Option<u64>,
    pub query_duration: Option<Duration>,
    pub render_duration: Option<Duration>,
    pub output_duration: Option<Duration>,
    pub backend_requested: Option<ctx_history_read_application::SearchBackend>,
    pub backend_effective: Option<ctx_history_read_application::SearchBackend>,
    pub result_count: Option<u64>,
    pub citation_count: Option<u64>,
    pub zero_result: Option<bool>,
    pub has_indexed_content_after: Option<bool>,
    pub work: ctx_history_read_application::SearchWorkReceipt,
    pub final_candidate_pool: Option<u64>,
    pub candidate_pool_truncated: Option<bool>,
    pub concentration: Option<ctx_history_read_application::SearchConcentrationReceipt>,
    pub diversification: Option<ctx_history_read_application::SearchDiversificationDecision>,
    pub stop_reason: Option<ctx_history_read_application::SearchStopReason>,
    pub failure_phase: Option<SearchFailurePhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRefreshStatus {
    ExistingGeneration,
    DaemonBackground,
    DaemonUnavailable,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFailurePhase {
    Preparation,
    Refresh,
    GenerationOpen,
    QueryPreparation,
    SemanticRetrieval,
    IndexQueryDecode,
    ResultProjection,
    Render,
    Output,
}

/// The output channel selected by a command body. The final adapter decides
/// how terminal styling and stream handles are implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

pub trait TerminalPort {
    fn context(&self, stream: OutputStream) -> &RenderContext;

    fn write_document(&mut self, stream: OutputStream, document: &Document) -> io::Result<()>;

    fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()>;

    fn flush(&mut self) -> io::Result<()>;
}

impl TerminalPort for Ui {
    fn context(&self, stream: OutputStream) -> &RenderContext {
        self.context(match stream {
            OutputStream::Stdout => StreamKind::Stdout,
            OutputStream::Stderr => StreamKind::Stderr,
        })
    }

    fn write_document(&mut self, stream: OutputStream, document: &Document) -> io::Result<()> {
        self.write(
            match stream {
                OutputStream::Stdout => StreamKind::Stdout,
                OutputStream::Stderr => StreamKind::Stderr,
            },
            document,
        )
    }

    fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()> {
        match stream {
            OutputStream::Stdout => self.stdout_writer().write_all(bytes),
            OutputStream::Stderr => self.stderr_writer().write_all(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ui::flush(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputStream, TerminalPort};
    use ctx_terminal::{Document, RenderContext, StreamKind, TestContext};
    use std::io;

    struct RecordingPort {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: RenderContext,
    }

    impl TerminalPort for RecordingPort {
        fn context(&self, _stream: OutputStream) -> &RenderContext {
            &self.context
        }

        fn write_document(
            &mut self,
            _stream: OutputStream,
            _document: &Document,
        ) -> io::Result<()> {
            Ok(())
        }

        fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()> {
            match stream {
                OutputStream::Stdout => self.stdout.extend_from_slice(bytes),
                OutputStream::Stderr => self.stderr.extend_from_slice(bytes),
            }
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn terminal_port_preserves_selected_stream_bytes() {
        let mut port = RecordingPort {
            stdout: Vec::new(),
            stderr: Vec::new(),
            context: RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        };
        {
            let terminal: &mut dyn TerminalPort = &mut port;
            terminal.write(OutputStream::Stdout, b"out").unwrap();
            terminal.write(OutputStream::Stderr, b"err").unwrap();
        }
        assert_eq!(port.stdout, b"out");
        assert_eq!(port.stderr, b"err");
    }
}
