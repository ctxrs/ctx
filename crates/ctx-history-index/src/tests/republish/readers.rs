use super::*;

#[test]
fn subprocess_continuous_readers_query_across_atomic_predecessor_swap() {
    let predecessor = GoldenPredecessor::copy();
    let held_reader = VerifiedIndex::open(predecessor.root()).unwrap();
    let reader_root = predecessor.root().to_path_buf();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let published = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let before_swap_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let after_swap_reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader_stop = std::sync::Arc::clone(&stop);
    let reader_published = std::sync::Arc::clone(&published);
    let reader_before_swap_reads = std::sync::Arc::clone(&before_swap_reads);
    let reader_after_swap_reads = std::sync::Arc::clone(&after_swap_reads);
    let reader = thread::spawn(move || {
        while !reader_stop.load(std::sync::atomic::Ordering::Acquire) {
            let index = match VerifiedIndex::open(&reader_root) {
                Ok(index) => index,
                // The active pointer may change between the verified reader's
                // bounded observations. This typed race is retryable; every
                // other open failure remains a test failure.
                Err(IndexError::ConcurrentGenerationChange) => continue,
                Err(error) => panic!("continuous reader failed to open: {error:?}"),
            };
            assert_eq!(index.count_term("evidence").unwrap(), 3);
            if reader_published.load(std::sync::atomic::Ordering::Acquire) {
                reader_after_swap_reads.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            } else {
                reader_before_swap_reads.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
        }
    });

    let initial_deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    while before_swap_reads.load(std::sync::atomic::Ordering::Acquire) < 4 {
        assert!(
            Instant::now() < initial_deadline,
            "reader did not query predecessor"
        );
        thread::yield_now();
    }
    let (marker, continue_path, result) = subprocess_paths(predecessor.root());
    let mut child = spawn_republish_subprocess(
        predecessor.root(),
        "pause-republish:BeforePointerPublication",
    );
    wait_for_subprocess_marker(&mut child, &marker);
    let before_continue = before_swap_reads.load(std::sync::atomic::Ordering::Acquire);
    let during_deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    while before_swap_reads.load(std::sync::atomic::Ordering::Acquire) < before_continue + 4 {
        assert!(
            Instant::now() < during_deadline,
            "reader stalled before pointer swap"
        );
        thread::yield_now();
    }
    fs::write(continue_path, b"continue").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(fs::read_to_string(result)
        .unwrap()
        .starts_with("COMMITTED "));
    published.store(true, std::sync::atomic::Ordering::Release);
    let successor_deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    while after_swap_reads.load(std::sync::atomic::Ordering::Acquire) < 4 {
        assert!(
            Instant::now() < successor_deadline,
            "reader did not query successor"
        );
        thread::yield_now();
    }
    stop.store(true, std::sync::atomic::Ordering::Release);
    reader.join().unwrap();
    assert_eq!(held_reader.count_term("evidence").unwrap(), 3);
}
