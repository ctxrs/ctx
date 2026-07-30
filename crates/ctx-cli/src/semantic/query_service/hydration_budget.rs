use std::{
    io::{self, Write},
    sync::{Condvar, Mutex, MutexGuard},
};

use ctx_history_capture::complete_content::COMPLETE_CONTENT_MAX_BODY_BYTES;
use ctx_history_core::{EventHydrationRequest, NativeRecordCoordinate};
use serde::Serialize;

pub(super) const SOURCE_HYDRATION_MAX_BYTES: usize = 64 * 1024 * 1024;

// Every source-backed provider already rejects one complete body above 16 MiB.
// Unknown-size native/SQLite coordinates reserve that full ceiling in bounded
// waves before the provider is entered. JSONL coordinates reserve their exact
// certified byte ranges request-wide, including record framing.
pub(super) const SOURCE_HYDRATION_MAX_ITEM_BYTES: usize = COMPLETE_CONTENT_MAX_BODY_BYTES;
const TRANSIENT_ITEM_OVERHEAD_BYTES: usize = 512;
const RETAINED_ITEM_OVERHEAD_BYTES: usize = 512;
const RESPONSE_ENVELOPE_OVERHEAD_BYTES: usize = 512;

#[derive(Default)]
struct SerializedByteCounter(usize);

impl Write for SerializedByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized byte count overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn serialized_json_bytes<T: Serialize + ?Sized>(
    value: &T,
) -> Result<usize, serde_json::Error> {
    let mut counter = SerializedByteCounter::default();
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum HydrationBudgetError {
    Cancelled,
    Exhausted,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(super) struct HydrationBudgetSnapshot {
    pub(super) limit_bytes: usize,
    pub(super) retained_bytes: usize,
    pub(super) in_flight_bytes: usize,
    pub(super) peak_bytes: usize,
    pub(super) committed_items: usize,
    pub(super) reservations: usize,
    pub(super) cancelled: bool,
    pub(super) exhausted: bool,
}

#[derive(Debug, Default)]
struct HydrationBudgetState {
    retained_bytes: usize,
    in_flight_bytes: usize,
    peak_bytes: usize,
    committed_items: usize,
    reservations: usize,
    cancelled: bool,
    exhausted: bool,
}

#[derive(Debug)]
pub(super) struct HydrationByteBudget {
    limit_bytes: usize,
    state: Mutex<HydrationBudgetState>,
    changed: Condvar,
}

impl HydrationByteBudget {
    pub(super) fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            state: Mutex::new(HydrationBudgetState::default()),
            changed: Condvar::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn charge_retained(&self, bytes: usize) -> Result<(), HydrationBudgetError> {
        let mut state = self.lock_state();
        let Some(next) = state.retained_bytes.checked_add(bytes) else {
            return Err(self.exhaust(&mut state));
        };
        if next
            .checked_add(state.in_flight_bytes)
            .is_none_or(|total| total > self.limit_bytes)
        {
            return Err(self.exhaust(&mut state));
        }
        state.retained_bytes = next;
        update_peak(&mut state);
        Ok(())
    }

    pub(super) fn reserve(
        &self,
        bytes: usize,
    ) -> Result<HydrationReservation<'_>, HydrationBudgetError> {
        let mut state = self.lock_state();
        loop {
            if state.cancelled {
                return Err(if state.exhausted {
                    HydrationBudgetError::Exhausted
                } else {
                    HydrationBudgetError::Cancelled
                });
            }
            let retained_remaining = self.limit_bytes.saturating_sub(state.retained_bytes);
            if bytes > retained_remaining {
                return Err(self.exhaust(&mut state));
            }
            let available = retained_remaining.saturating_sub(state.in_flight_bytes);
            if bytes <= available {
                state.in_flight_bytes += bytes;
                state.reservations = state.reservations.saturating_add(1);
                update_peak(&mut state);
                return Ok(HydrationReservation {
                    budget: self,
                    reserved_bytes: bytes,
                    active: true,
                });
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    pub(super) fn cancel(&self) {
        let mut state = self.lock_state();
        state.cancelled = true;
        self.changed.notify_all();
    }

    pub(super) fn cancel_exhausted(&self) {
        let mut state = self.lock_state();
        self.exhaust(&mut state);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.lock_state().cancelled
    }

    pub(super) fn available_when_idle(&self) -> usize {
        let state = self.lock_state();
        debug_assert_eq!(state.in_flight_bytes, 0);
        self.limit_bytes.saturating_sub(state.retained_bytes)
    }

    pub(super) fn snapshot(&self) -> HydrationBudgetSnapshot {
        let state = self.lock_state();
        HydrationBudgetSnapshot {
            limit_bytes: self.limit_bytes,
            retained_bytes: state.retained_bytes,
            in_flight_bytes: state.in_flight_bytes,
            peak_bytes: state.peak_bytes,
            committed_items: state.committed_items,
            reservations: state.reservations,
            cancelled: state.cancelled,
            exhausted: state.exhausted,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, HydrationBudgetState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn exhaust(&self, state: &mut HydrationBudgetState) -> HydrationBudgetError {
        state.cancelled = true;
        state.exhausted = true;
        self.changed.notify_all();
        HydrationBudgetError::Exhausted
    }
}

pub(super) struct HydrationReservation<'a> {
    budget: &'a HydrationByteBudget,
    reserved_bytes: usize,
    active: bool,
}

impl HydrationReservation<'_> {
    pub(super) fn commit(
        mut self,
        retained_bytes: usize,
        items: usize,
    ) -> Result<(), HydrationBudgetError> {
        let mut state = self.budget.lock_state();
        state.in_flight_bytes = state.in_flight_bytes.saturating_sub(self.reserved_bytes);
        self.active = false;
        if state.cancelled {
            self.budget.changed.notify_all();
            return Err(if state.exhausted {
                HydrationBudgetError::Exhausted
            } else {
                HydrationBudgetError::Cancelled
            });
        }
        let Some(next_retained) = state.retained_bytes.checked_add(retained_bytes) else {
            return Err(self.budget.exhaust(&mut state));
        };
        if next_retained
            .checked_add(state.in_flight_bytes)
            .is_none_or(|total| total > self.budget.limit_bytes)
        {
            return Err(self.budget.exhaust(&mut state));
        }
        state.retained_bytes = next_retained;
        state.committed_items = state.committed_items.saturating_add(items);
        update_peak(&mut state);
        self.budget.changed.notify_all();
        Ok(())
    }
}

impl Drop for HydrationReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.budget.lock_state();
        state.in_flight_bytes = state.in_flight_bytes.saturating_sub(self.reserved_bytes);
        self.active = false;
        self.budget.changed.notify_all();
    }
}

pub(super) fn provider_read_reservation_bytes(
    request: &EventHydrationRequest,
    display_copy_bytes: usize,
) -> Result<usize, HydrationBudgetError> {
    provider_read_bytes(request)?
        .checked_add(display_copy_bytes)
        .and_then(|bytes| bytes.checked_add(TRANSIENT_ITEM_OVERHEAD_BYTES))
        .ok_or(HydrationBudgetError::Exhausted)
}

pub(super) fn unknown_provider_read_reservation_bytes(
    display_copy_bytes: usize,
) -> Result<usize, HydrationBudgetError> {
    SOURCE_HYDRATION_MAX_ITEM_BYTES
        .checked_add(display_copy_bytes)
        .and_then(|bytes| bytes.checked_add(TRANSIENT_ITEM_OVERHEAD_BYTES))
        .ok_or(HydrationBudgetError::Exhausted)
}

pub(super) fn provider_read_size_is_exact(request: &EventHydrationRequest) -> bool {
    matches!(
        request.locator().coordinate(),
        NativeRecordCoordinate::Jsonl { .. }
    )
}

pub(super) fn retained_response_items_metadata_charge(
    items: usize,
) -> Result<usize, HydrationBudgetError> {
    items
        .checked_mul(RETAINED_ITEM_OVERHEAD_BYTES)
        .ok_or(HydrationBudgetError::Exhausted)
}

pub(super) fn retained_response_item_content_charge(
    text: &String,
) -> Result<usize, HydrationBudgetError> {
    Ok(escaped_json_string_bytes(text).max(text.capacity()))
}

#[cfg(test)]
pub(super) fn retained_response_item_charge(text: &String) -> Result<usize, HydrationBudgetError> {
    retained_response_item_content_charge(text)?
        .checked_add(RETAINED_ITEM_OVERHEAD_BYTES)
        .ok_or(HydrationBudgetError::Exhausted)
}

fn provider_read_bytes(request: &EventHydrationRequest) -> Result<usize, HydrationBudgetError> {
    match request.locator().coordinate() {
        // The certified range includes JSONL delimiters/framing and may be a
        // few bytes larger than the provider's decoded complete-body ceiling.
        NativeRecordCoordinate::Jsonl { byte_length, .. } => {
            usize::try_from(*byte_length).map_err(|_| HydrationBudgetError::Exhausted)
        }
        _ => Ok(SOURCE_HYDRATION_MAX_ITEM_BYTES),
    }
}

pub(super) fn provider_body_is_admitted(
    request: &EventHydrationRequest,
    length: usize,
    capacity: usize,
) -> bool {
    if length > SOURCE_HYDRATION_MAX_ITEM_BYTES {
        return false;
    }
    provider_read_bytes(request)
        .ok()
        .map(|read_bytes| read_bytes.min(SOURCE_HYDRATION_MAX_ITEM_BYTES))
        .and_then(|body_bytes| body_bytes.checked_add(TRANSIENT_ITEM_OVERHEAD_BYTES))
        .is_some_and(|maximum| capacity <= maximum)
}

pub(super) fn successful_response_envelope_charge(
    generation_id: &str,
) -> Result<usize, HydrationBudgetError> {
    RESPONSE_ENVELOPE_OVERHEAD_BYTES
        .checked_add(generation_id.len())
        .ok_or(HydrationBudgetError::Exhausted)
}

fn escaped_json_string_bytes(value: &str) -> usize {
    value.bytes().fold(0_usize, |total, byte| {
        total.saturating_add(match byte {
            b'"' | b'\\' | b'\x08' | b'\x09' | b'\x0a' | b'\x0c' | b'\x0d' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        })
    })
}

fn update_peak(state: &mut HydrationBudgetState) {
    state.peak_bytes = state
        .peak_bytes
        .max(state.retained_bytes.saturating_add(state.in_flight_bytes));
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Barrier,
        },
        time::Duration,
    };

    use super::*;

    #[test]
    fn exact_boundary_commits_and_next_byte_exhausts() {
        let budget = HydrationByteBudget::new(4_096);
        budget.charge_retained(128).unwrap();
        budget.reserve(1_024).unwrap().commit(3_968, 1).unwrap();

        let exact = budget.snapshot();
        assert_eq!(exact.retained_bytes, exact.limit_bytes);
        assert_eq!(exact.peak_bytes, exact.limit_bytes);
        assert!(!exact.cancelled);
        assert_eq!(
            budget.reserve(1).err(),
            Some(HydrationBudgetError::Exhausted)
        );
        assert!(budget.snapshot().exhausted);
    }

    #[test]
    fn pathological_concurrency_never_crosses_one_aggregate_limit() {
        const THREADS: usize = 128;
        const RESERVATION_BYTES: usize = 1_024;
        const LIMIT_BYTES: usize = 64 * 1_024;

        let budget = Arc::new(HydrationByteBudget::new(LIMIT_BYTES));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                let budget = Arc::clone(&budget);
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                scope.spawn(move || {
                    let reservation = budget.reserve(RESERVATION_BYTES).unwrap();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                    active.fetch_sub(1, Ordering::SeqCst);
                    reservation.commit(1, 1).unwrap();
                });
            }
        });

        let snapshot = budget.snapshot();
        assert!(
            max_active.load(Ordering::SeqCst) <= LIMIT_BYTES / RESERVATION_BYTES,
            "{snapshot:?}"
        );
        assert!(snapshot.peak_bytes <= snapshot.limit_bytes);
        assert_eq!(snapshot.committed_items, THREADS);
        assert_eq!(snapshot.retained_bytes, THREADS);
        assert_eq!(snapshot.reservations, THREADS);
        assert_eq!(snapshot.in_flight_bytes, 0);
        assert!(!snapshot.cancelled);
        assert!(!snapshot.exhausted);
    }

    #[test]
    fn cancellation_wakes_waiter_without_late_reservation() {
        let budget = Arc::new(HydrationByteBudget::new(1_024));
        let held = budget.reserve(900).unwrap();
        let started = Arc::new(Barrier::new(2));
        let outcome = std::thread::scope(|scope| {
            let waiting_budget = Arc::clone(&budget);
            let waiting_started = Arc::clone(&started);
            let waiter = scope.spawn(move || {
                waiting_started.wait();
                waiting_budget.reserve(200).err()
            });
            started.wait();
            std::thread::sleep(Duration::from_millis(2));
            budget.cancel();
            waiter
                .join()
                .unwrap_or(Some(HydrationBudgetError::Cancelled))
        });
        drop(held);

        assert_eq!(outcome, Some(HydrationBudgetError::Cancelled));
        let snapshot = budget.snapshot();
        assert!(snapshot.cancelled);
        assert_eq!(snapshot.reservations, 1);
        assert_eq!(snapshot.in_flight_bytes, 0);
    }

    #[test]
    fn retained_charge_covers_json_escaping_capacity_and_metadata() {
        let mut text = String::with_capacity(1_024);
        text.push_str("plain\n\"quoted\"\u{0001}λ");
        let charge = retained_response_item_charge(&text).unwrap();
        let item = serde_json::json!({
            "event_id": "00000000-0000-0000-0000-000000000000",
            "text": text,
        });
        let encoded_item = serde_json::to_vec(&item).unwrap();
        let generation_id = "a".repeat(64);
        let encoded_response = serde_json::to_vec(&serde_json::json!({
            "ok": true,
            "schema_version": 1,
            "generation_id": &generation_id,
            "items": [item],
        }))
        .unwrap();

        assert!(charge >= encoded_item.len());
        assert!(charge >= 1_024);
        assert!(
            charge + successful_response_envelope_charge(&generation_id).unwrap()
                >= encoded_response.len()
        );
    }
}
