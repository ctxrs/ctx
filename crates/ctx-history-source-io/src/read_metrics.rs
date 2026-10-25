//! Test-only counts of bytes returned by instrumented source reads, including
//! buffered read-ahead. File identity keeps worker reads and reopened handles
//! attributable to one fixture without counting unrelated concurrent tests.

use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

type FileId = (u64, u64);
static TRACKED: Mutex<Vec<(FileId, Arc<AtomicU64>)>> = Mutex::new(Vec::new());

pub struct SourceReadGuard {
    id: FileId,
    bytes: Arc<AtomicU64>,
}

pub fn track_source_reads(path: &Path) -> SourceReadGuard {
    let metadata = path.metadata().unwrap();
    let id = (metadata.dev(), metadata.ino());
    let bytes = Arc::new(AtomicU64::new(0));
    TRACKED.lock().unwrap().push((id, Arc::clone(&bytes)));
    SourceReadGuard { id, bytes }
}

impl SourceReadGuard {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

impl Drop for SourceReadGuard {
    fn drop(&mut self) {
        TRACKED
            .lock()
            .unwrap()
            .retain(|(id, bytes)| *id != self.id || !Arc::ptr_eq(bytes, &self.bytes));
    }
}

pub fn record_source_read(file: &File, bytes: usize) {
    let tracked = TRACKED.lock().unwrap();
    if bytes == 0 || tracked.is_empty() {
        return;
    }
    let metadata = file.metadata().unwrap();
    let id = (metadata.dev(), metadata.ino());
    for (_, counter) in tracked.iter().filter(|(candidate, _)| *candidate == id) {
        counter.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

pub struct CountedFile(File);

impl CountedFile {
    pub fn new(file: File) -> Self {
        Self(file)
    }
}

impl Read for CountedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let bytes = self.0.read(buffer)?;
        record_source_read(&self.0, bytes);
        Ok(bytes)
    }
}
