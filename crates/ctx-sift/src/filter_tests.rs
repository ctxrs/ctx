// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use crate::filter;
use std::io::{self, Cursor, Read};
use std::time::{Duration, Instant};

#[test]
fn complete_filter_is_reversible_and_reports_delivered_counts() {
    let input = "task worker completed an unchanged checkpoint\r\n".repeat(100);
    let mut output = Vec::new();
    let measurement =
        filter::filter(Cursor::new(input.clone().into_bytes()), &mut output, true).unwrap();
    let result = measurement.compacted.unwrap();
    assert_eq!(sift::restore(result.encoding, &result.text).unwrap(), input);
    assert_eq!(output, result.text.as_bytes());
    assert_eq!(measurement.input_bytes, input.len() as u64);
    assert_eq!(measurement.output_bytes, output.len() as u64);
}

struct Delayed {
    first: bool,
}
impl Read for Delayed {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.first {
            self.first = false;
            bytes[..6].copy_from_slice(b"ready\n");
            Ok(6)
        } else {
            std::thread::sleep(Duration::from_millis(450));
            Ok(0)
        }
    }
}
struct Timed<'a> {
    started: Instant,
    first: &'a mut Option<Duration>,
    bytes: &'a mut Vec<u8>,
}
impl io::Write for Timed<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !bytes.is_empty() {
            self.first.get_or_insert(self.started.elapsed());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn default_filter_releases_progress_before_eof() {
    let mut first = None;
    let mut bytes = Vec::new();
    let output = Timed {
        started: Instant::now(),
        first: &mut first,
        bytes: &mut bytes,
    };
    let result = filter::filter(Delayed { first: true }, output, false).unwrap();
    assert!(first.unwrap() < Duration::from_millis(400));
    assert_eq!(bytes, b"ready\n");
    assert!(result.original.is_none());
    assert!(result.compacted.is_none());
}

#[test]
fn overflow_binary_and_short_input_stay_exact() {
    for bytes in [
        vec![0xff, 0, 10],
        b"short\r\n".to_vec(),
        vec![b'x'; 8 * 1024 * 1024 + 1],
    ] {
        let mut output = Vec::new();
        let result = filter::filter(Cursor::new(bytes.clone()), &mut output, true).unwrap();
        assert_eq!(bytes, output);
        assert!(result.compacted.is_none());
        assert_eq!(result.output_bytes, bytes.len() as u64);
    }
}

#[test]
fn closed_consumer_is_an_error_not_a_successful_measurement() {
    struct Closed;
    impl io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        filter::filter(Cursor::new(b"data"), Closed, true)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn read_failure_preserves_partial_bytes_without_compacting() {
    struct Failing(bool);
    impl Read for Failing {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                self.0 = false;
                bytes[..1024].fill(b'x');
                Ok(1024)
            } else {
                Err(io::Error::from(io::ErrorKind::ConnectionReset))
            }
        }
    }
    for complete in [false, true] {
        let mut output = Vec::new();
        let error = filter::filter(Failing(true), &mut output, complete)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        assert_eq!(output, vec![b'x'; 1024]);
    }
}
