#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub indexer_threads: usize,
    pub memory_bytes: usize,
}

impl Default for WriterOptions {
    fn default() -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self {
            indexer_threads: parallelism.clamp(1, 8),
            memory_bytes: 512 * 1024 * 1024,
        }
    }
}
