use std::{
    io::{self, BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use tantivy::directory::{
    error::OpenWriteError, AntiCallToken, Directory, MmapDirectory, TerminatingWrite, WritePtr,
};

/// One candidate's first output error, shared by its writer/merge threads.
/// Reopening a directory creates a fresh observation; no state is persisted.
#[derive(Clone, Default)]
pub(super) struct WriteFailure(Arc<Mutex<Option<io::Error>>>);

fn copy_error(error: &io::Error) -> io::Error {
    error.raw_os_error().map_or_else(
        || io::Error::new(error.kind(), error.to_string()),
        io::Error::from_raw_os_error,
    )
}

impl WriteFailure {
    pub(super) fn record(&self, error: &io::Error) {
        let mut first = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if first.is_none() {
            *first = Some(copy_error(error));
        }
    }

    pub(super) fn check(&self) -> io::Result<()> {
        let first = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match first.as_ref() {
            Some(error) => Err(copy_error(error)),
            None => Ok(()),
        }
    }

    pub(super) fn open_write(
        &self,
        directory: &MmapDirectory,
        path: &Path,
    ) -> Result<WritePtr, OpenWriteError> {
        let write = directory.open_write(path).inspect_err(|error| {
            if let OpenWriteError::IoError { io_error, .. } = error {
                self.record(io_error);
            }
        })?;
        let capacity = write.capacity();
        let inner = write.into_inner().map_err(|error| {
            OpenWriteError::wrap_io_error(error.into_error(), path.to_path_buf())
        })?;
        Ok(BufWriter::with_capacity(
            capacity,
            Box::new(ObservedWriter {
                inner,
                failure: self.clone(),
            }),
        ))
    }
}

struct ObservedWriter {
    inner: Box<dyn TerminatingWrite + Send + Sync>,
    failure: WriteFailure,
}

impl Write for ObservedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner
            .write(bytes)
            .inspect_err(|error| self.failure.record(error))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner
            .flush()
            .inspect_err(|error| self.failure.record(error))
    }
}

impl TerminatingWrite for ObservedWriter {
    fn terminate_ref(&mut self, token: AntiCallToken) -> io::Result<()> {
        self.inner
            .terminate_ref(token)
            .inspect_err(|error| self.failure.record(error))
    }
}
