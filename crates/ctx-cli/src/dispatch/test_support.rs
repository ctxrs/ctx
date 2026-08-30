use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use crate::ui::{ColorMode, RenderContext, StreamKind, TestContext, Ui};

#[derive(Clone, Default)]
pub(super) struct SharedBytes(Arc<Mutex<Vec<u8>>>);

impl SharedBytes {
    pub(super) fn bytes(&self) -> Vec<u8> {
        self.0.lock().map(|bytes| bytes.clone()).unwrap_or_default()
    }
}

impl Write for SharedBytes {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("shared test writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn pipe_ui(color: ColorMode) -> (Ui, SharedBytes, SharedBytes) {
    let stdout = SharedBytes::default();
    let stderr = SharedBytes::default();
    let ui = Ui::with_writers(
        stdout.clone(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(color)),
        stderr.clone(),
        RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(color)),
    );
    (ui, stdout, stderr)
}
