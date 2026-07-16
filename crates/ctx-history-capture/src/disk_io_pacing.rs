use std::{
    cell::RefCell,
    fmt,
    io::{self, Read, Seek, SeekFrom},
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const MAX_SLEEP: Duration = Duration::from_millis(25);

type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;
type Sleeper = Arc<dyn Fn(Duration) + Send + Sync>;

#[derive(Clone)]
pub struct DiskIoPacer {
    bytes_per_second: u64,
    burst_bytes: u64,
    burst_nanos: u128,
    state: Arc<Mutex<PacerState>>,
    clock: Clock,
    sleeper: Sleeper,
}

struct PacerState {
    last_nanos: u128,
    available_nanos: u128,
    charged_bytes: u64,
}

impl fmt::Debug for DiskIoPacer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiskIoPacer")
            .field("bytes_per_second", &self.bytes_per_second)
            .field("burst_bytes", &self.burst_bytes)
            .finish_non_exhaustive()
    }
}

impl DiskIoPacer {
    pub fn new(bytes_per_second: u64, burst_bytes: u64) -> Self {
        let started = Instant::now();
        Self::with_runtime(
            bytes_per_second,
            burst_bytes,
            Arc::new(move || started.elapsed()),
            Arc::new(std::thread::sleep),
        )
    }

    fn with_runtime(
        bytes_per_second: u64,
        burst_bytes: u64,
        clock: Clock,
        sleeper: Sleeper,
    ) -> Self {
        let bytes_per_second = bytes_per_second.max(1);
        let burst_bytes = burst_bytes.max(1);
        let burst_nanos = byte_cost_nanos(burst_bytes, bytes_per_second);
        let now = clock().as_nanos();
        Self {
            bytes_per_second,
            burst_bytes,
            burst_nanos,
            state: Arc::new(Mutex::new(PacerState {
                last_nanos: now,
                available_nanos: burst_nanos,
                charged_bytes: 0,
            })),
            clock,
            sleeper,
        }
    }

    pub fn bytes_per_second(&self) -> u64 {
        self.bytes_per_second
    }

    pub fn burst_bytes(&self) -> u64 {
        self.burst_bytes
    }

    pub fn pace(&self, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let mut remaining_nanos = byte_cost_nanos(bytes, self.bytes_per_second);
        {
            let mut state = self.lock_state();
            state.charged_bytes = state.charged_bytes.saturating_add(bytes);
        }
        while remaining_nanos > 0 {
            let sleep = {
                let mut state = self.lock_state();
                let now = (self.clock)().as_nanos();
                let replenished = state
                    .available_nanos
                    .saturating_add(now.saturating_sub(state.last_nanos));
                state.available_nanos = replenished.min(self.burst_nanos);
                state.last_nanos = now;
                let paid = remaining_nanos.min(state.available_nanos);
                state.available_nanos -= paid;
                remaining_nanos -= paid;
                duration_from_nanos(
                    remaining_nanos
                        .min(MAX_SLEEP.as_nanos())
                        .min(self.burst_nanos),
                )
            };
            if !sleep.is_zero() {
                (self.sleeper)(sleep);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn charged_bytes(&self) -> u64 {
        self.lock_state().charged_bytes
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, PacerState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn byte_cost_nanos(bytes: u64, bytes_per_second: u64) -> u128 {
    u128::from(bytes)
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(u128::from(bytes_per_second) - 1)
        / u128::from(bytes_per_second)
}

fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

thread_local! {
    static CURRENT_PACER: RefCell<Option<DiskIoPacer>> = const { RefCell::new(None) };
}

pub struct DiskIoPacingGuard {
    previous: Option<DiskIoPacer>,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for DiskIoPacingGuard {
    fn drop(&mut self) {
        CURRENT_PACER.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

pub fn install_disk_io_pacer(pacer: DiskIoPacer) -> DiskIoPacingGuard {
    let previous = CURRENT_PACER.with(|slot| slot.borrow_mut().replace(pacer));
    DiskIoPacingGuard {
        previous,
        _not_send: PhantomData,
    }
}

pub fn pace_current_disk_io(bytes: u64) {
    let pacer = CURRENT_PACER.with(|slot| slot.borrow().clone());
    if let Some(pacer) = pacer {
        pacer.pace(bytes);
    }
}

pub(crate) fn current_disk_io_pacer() -> Option<DiskIoPacer> {
    CURRENT_PACER.with(|slot| slot.borrow().clone())
}

pub(crate) fn current_disk_io_burst_bytes() -> Option<u64> {
    current_disk_io_pacer().map(|pacer| pacer.burst_bytes())
}

#[derive(Debug)]
pub(crate) struct PacedReader<R> {
    inner: R,
}

impl<R> PacedReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner }
    }

    pub(crate) fn get_ref(&self) -> &R {
        &self.inner
    }

    pub(crate) fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }
}

impl<R: Read> Read for PacedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        pace_current_disk_io(buffer.len() as u64);
        self.inner.read(buffer)
    }
}

impl<R: Seek> Seek for PacedReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    struct FakeTime {
        now: Mutex<Duration>,
        sleeps: Mutex<Vec<Duration>>,
    }

    fn fake_pacer(rate: u64, burst: u64) -> (DiskIoPacer, Arc<FakeTime>) {
        let time = Arc::new(FakeTime::default());
        let clock_time = Arc::clone(&time);
        let sleep_time = Arc::clone(&time);
        let pacer = DiskIoPacer::with_runtime(
            rate,
            burst,
            Arc::new(move || *clock_time.now.lock().unwrap()),
            Arc::new(move |duration| {
                sleep_time.sleeps.lock().unwrap().push(duration);
                *sleep_time.now.lock().unwrap() += duration;
            }),
        );
        (pacer, time)
    }

    #[test]
    fn oversized_charge_progresses_with_small_sleeps() {
        let (pacer, time) = fake_pacer(1_000, 100);
        pacer.pace(1_100);

        let sleeps = time.sleeps.lock().unwrap();
        assert_eq!(
            sleeps.iter().copied().sum::<Duration>(),
            Duration::from_secs(1)
        );
        assert!(sleeps.iter().all(|sleep| *sleep <= MAX_SLEEP));
        assert_eq!(pacer.charged_bytes(), 1_100);
    }

    #[test]
    fn idle_credit_is_capped_at_the_burst() {
        let (pacer, time) = fake_pacer(1_000, 100);
        pacer.pace(100);
        *time.now.lock().unwrap() += Duration::from_secs(10);
        pacer.pace(150);

        assert_eq!(
            time.sleeps
                .lock()
                .unwrap()
                .iter()
                .copied()
                .sum::<Duration>(),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn sleep_quantum_does_not_discard_a_small_burst() {
        let (pacer, time) = fake_pacer(1_000, 1);
        pacer.pace(101);

        let sleeps = time.sleeps.lock().unwrap();
        assert_eq!(
            sleeps.iter().copied().sum::<Duration>(),
            Duration::from_millis(100)
        );
        assert!(sleeps
            .iter()
            .all(|sleep| *sleep <= Duration::from_millis(1)));
    }

    #[test]
    fn scoped_pacer_restores_the_previous_accountant() {
        let (outer, _) = fake_pacer(1_000, 1_000);
        let (inner, _) = fake_pacer(1_000, 1_000);
        let _outer_scope = install_disk_io_pacer(outer.clone());
        pace_current_disk_io(10);
        {
            let _inner_scope = install_disk_io_pacer(inner.clone());
            pace_current_disk_io(20);
        }
        pace_current_disk_io(30);

        assert_eq!(outer.charged_bytes(), 40);
        assert_eq!(inner.charged_bytes(), 20);
    }

    #[test]
    fn paced_reader_reserves_budget_before_physical_read() {
        struct ObservedRead {
            time: Arc<FakeTime>,
            observed_nanos: Arc<AtomicU64>,
        }

        impl Read for ObservedRead {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let now = self.time.now.lock().unwrap().as_nanos() as u64;
                self.observed_nanos.store(now, Ordering::SeqCst);
                buffer.fill(b'x');
                Ok(buffer.len())
            }
        }

        let (pacer, time) = fake_pacer(1_000, 1);
        let _pacing = install_disk_io_pacer(pacer);
        let observed_nanos = Arc::new(AtomicU64::new(0));
        let mut reader = PacedReader::new(ObservedRead {
            time,
            observed_nanos: Arc::clone(&observed_nanos),
        });
        reader.read_exact(&mut [0_u8; 101]).unwrap();

        assert_eq!(observed_nanos.load(Ordering::SeqCst), 100_000_000);
    }

    #[test]
    fn inherited_worker_pacer_shares_the_aggregate_budget() {
        let (pacer, time) = fake_pacer(1_000, 100);
        let _pacing = install_disk_io_pacer(pacer.clone());
        pace_current_disk_io(100);
        let inherited = current_disk_io_pacer().unwrap();
        std::thread::spawn(move || {
            let _pacing = install_disk_io_pacer(inherited);
            pace_current_disk_io(100);
        })
        .join()
        .unwrap();

        assert_eq!(pacer.charged_bytes(), 200);
        assert_eq!(*time.now.lock().unwrap(), Duration::from_millis(100));
    }
}
