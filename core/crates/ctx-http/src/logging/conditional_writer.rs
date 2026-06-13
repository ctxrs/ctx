use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::Metadata;
use tracing_subscriber::fmt::writer::MakeWriter;

pub(super) struct ConditionalWriter<W> {
    inner: W,
    blocked: Arc<AtomicBool>,
}

impl<W: io::Write> io::Write for ConditionalWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.blocked.load(Ordering::Relaxed) {
            Ok(buf.len())
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.blocked.load(Ordering::Relaxed) {
            Ok(())
        } else {
            self.inner.flush()
        }
    }
}

pub(super) struct ConditionalMakeWriter<W> {
    pub(super) inner: W,
    pub(super) blocked: Arc<AtomicBool>,
}

impl<'a, W> MakeWriter<'a> for ConditionalMakeWriter<W>
where
    W: MakeWriter<'a>,
{
    type Writer = ConditionalWriter<W::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        ConditionalWriter {
            inner: self.inner.make_writer(),
            blocked: Arc::clone(&self.blocked),
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        ConditionalWriter {
            inner: self.inner.make_writer_for(meta),
            blocked: Arc::clone(&self.blocked),
        }
    }
}
