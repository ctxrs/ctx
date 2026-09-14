use std::{
    io::{self, Write as _},
    sync::{Arc, Mutex},
};

use unicode_width::UnicodeWidthStr as _;

use crate::ui::{Document, RenderContext};

pub(crate) fn assert_fits(document: &Document, context: &RenderContext) {
    let width = context.content_width().unwrap_or(1);
    for line in document.render_plain().lines() {
        assert!(line.width() <= width, "{line:?} exceeded {width} columns");
    }
}

pub(crate) fn strip_ansi(rendered: &str) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered.as_bytes()).unwrap();
    String::from_utf8(stream.into_inner()).unwrap()
}

#[derive(Clone, Default)]
pub(crate) struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.bytes()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
