use std::fmt;

use thiserror::Error;

#[cfg(test)]
use super::CapturedRecordPayload;
use super::{
    CapturedBatch, CapturedBatchBuilder, CapturedBatchError, CapturedRecord, CapturedSqliteValue,
    NativeLocator, NativePosition, ProviderRecordKind, SourceObservation, StructuralRejectionKind,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
    CAPTURE_BATCH_MAX_RECORDS,
};

const SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES: usize =
    CAPTURE_BATCH_MAX_PAYLOAD_BYTES + CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES;

pub(crate) struct SqliteLogicalRow {
    next_position: NativePosition,
    record: CapturedRecord,
}

impl SqliteLogicalRow {
    pub(crate) fn native_content(
        next_position: NativePosition,
        ordinal: u64,
        locator: NativeLocator,
        record_kind: ProviderRecordKind,
        content: Vec<u8>,
    ) -> Result<Self, SqliteLogicalRowError> {
        Ok(Self {
            next_position,
            record: CapturedRecord::content(ordinal, locator, record_kind, content)?,
        })
    }

    pub(crate) fn values(
        next_position: NativePosition,
        ordinal: u64,
        locator: NativeLocator,
        record_kind: ProviderRecordKind,
        values: Vec<CapturedSqliteValue>,
    ) -> Result<Self, SqliteLogicalRowError> {
        Ok(Self {
            next_position,
            record: CapturedRecord::sqlite_logical(ordinal, locator, record_kind, values)?,
        })
    }

    pub(crate) fn oversize(
        next_position: NativePosition,
        ordinal: u64,
        locator: NativeLocator,
        record_kind: ProviderRecordKind,
        observed_bytes: u64,
    ) -> Result<Self, SqliteLogicalRowError> {
        let maximum = u64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
            .map_err(|_| SqliteLogicalRowError::LengthOverflow)?;
        if observed_bytes <= maximum {
            return Err(SqliteLogicalRowError::InvalidOversizeMarker {
                observed_bytes,
                maximum: CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
            });
        }
        Ok(Self {
            next_position,
            record: CapturedRecord::structural_rejection(
                ordinal,
                locator,
                record_kind,
                StructuralRejectionKind::OversizeRecord,
                observed_bytes,
            ),
        })
    }

    pub(crate) fn next_position(&self) -> &NativePosition {
        &self.next_position
    }

    pub(crate) fn ordinal(&self) -> u64 {
        self.record.ordinal()
    }

    #[cfg(test)]
    pub(crate) fn record(&self) -> &CapturedRecord {
        &self.record
    }
}

impl fmt::Debug for SqliteLogicalRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqliteLogicalRow")
            .field("next_position", &self.next_position)
            .field("ordinal", &self.record.ordinal())
            .field("locator", &self.record.locator())
            .field("record_kind", &self.record.record_kind())
            .field("payload", &self.record.payload())
            .finish()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum SqliteLogicalRowError {
    #[error("SQLite logical-row capture length overflow")]
    LengthOverflow,
    #[error(
        "SQLite logical-row oversize marker is {observed_bytes} bytes, but markers must exceed the {maximum}-byte representable record limit"
    )]
    InvalidOversizeMarker { observed_bytes: u64, maximum: usize },
    #[error(transparent)]
    Batch(#[from] CapturedBatchError),
}

pub(crate) struct SqliteLogicalRowBatchProducer<F> {
    source: SourceObservation,
    current_position: NativePosition,
    last_ordinal: Option<u64>,
    fetch_next: F,
    lookahead: Option<SqliteLogicalRow>,
    exhausted: bool,
}

impl<F> SqliteLogicalRowBatchProducer<F> {
    pub(crate) fn new(
        source: SourceObservation,
        start_position: NativePosition,
        fetch_next: F,
    ) -> Self {
        Self {
            source,
            current_position: start_position,
            last_ordinal: None,
            fetch_next,
            lookahead: None,
            exhausted: false,
        }
    }

    pub(crate) fn current_position(&self) -> &NativePosition {
        &self.current_position
    }

    pub(crate) fn next_batch<E>(
        &mut self,
    ) -> Result<Option<CapturedBatch>, SqliteLogicalRowsBatchError<E>>
    where
        F: FnMut(NativePosition) -> Result<Option<SqliteLogicalRow>, E>,
    {
        if self.exhausted {
            return Ok(None);
        }

        let mut range_end = self.current_position.clone();
        let mut last_ordinal = self.last_ordinal;
        let mut builder =
            CapturedBatchBuilder::new(self.source.clone(), self.current_position.clone());
        let mut reached_end = false;

        // A singleton-sized row ends the batch before another callback can hydrate lookahead.
        while builder.record_count() < CAPTURE_BATCH_MAX_RECORDS
            && (builder.is_empty()
                || builder.retained_payload_bytes() < CAPTURE_BATCH_MAX_PAYLOAD_BYTES)
        {
            let row = match self.lookahead.take() {
                Some(row) => row,
                None => {
                    let Some(row) = (self.fetch_next)(range_end.clone())
                        .map_err(SqliteLogicalRowsBatchError::Callback)?
                    else {
                        reached_end = true;
                        break;
                    };
                    row
                }
            };

            validate_next_position(&range_end, row.next_position())?;
            if last_ordinal.is_some_and(|ordinal| ordinal >= row.ordinal()) {
                return Err(SqliteLogicalRowsBatchError::NonIncreasingOrdinal);
            }

            let SqliteLogicalRow {
                next_position,
                record,
            } = row;
            let in_flight_payload_bytes = builder
                .retained_payload_bytes()
                .checked_add(record.retained_bytes())
                .ok_or(CapturedBatchError::LengthOverflow)?;
            if in_flight_payload_bytes > SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES {
                return Err(SqliteLogicalRowsBatchError::InFlightPayloadTooLarge {
                    actual: in_flight_payload_bytes,
                    maximum: SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES,
                });
            }
            if !builder.can_accept(&record) {
                if builder.is_empty() {
                    return Err(CapturedBatchError::BatchFull.into());
                }
                self.lookahead = Some(SqliteLogicalRow {
                    next_position,
                    record,
                });
                break;
            }

            last_ordinal = Some(record.ordinal());
            range_end = next_position;
            builder.push(record)?;
        }

        if builder.is_empty() {
            self.exhausted = reached_end;
            return Ok(None);
        }

        // A normal batch plus one representable logical-row lookahead stays within the named
        // 24 MiB in-flight ceiling. Probe here so EOF is part of the returned batch delivery and
        // the importer never has to release a final batch merely to discover exhaustion. An
        // oversize singleton cannot safely overlap another maximum-size row, so the importer
        // publishes that singleton before requesting again.
        if !reached_end
            && self.lookahead.is_none()
            && builder.retained_payload_bytes() <= CAPTURE_BATCH_MAX_PAYLOAD_BYTES
        {
            match (self.fetch_next)(range_end.clone())
                .map_err(SqliteLogicalRowsBatchError::Callback)?
            {
                Some(row) => {
                    validate_next_position(&range_end, row.next_position())?;
                    if last_ordinal.is_some_and(|ordinal| ordinal >= row.ordinal()) {
                        return Err(SqliteLogicalRowsBatchError::NonIncreasingOrdinal);
                    }
                    let in_flight_payload_bytes = builder
                        .retained_payload_bytes()
                        .checked_add(row.record.retained_bytes())
                        .ok_or(CapturedBatchError::LengthOverflow)?;
                    if in_flight_payload_bytes > SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES {
                        return Err(SqliteLogicalRowsBatchError::InFlightPayloadTooLarge {
                            actual: in_flight_payload_bytes,
                            maximum: SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES,
                        });
                    }
                    self.lookahead = Some(row);
                }
                None => reached_end = true,
            }
        }

        if reached_end {
            builder.mark_source_exhausted();
        }

        let batch = builder.finish(range_end.clone())?;
        self.current_position = range_end;
        self.last_ordinal = last_ordinal;
        self.exhausted = reached_end;
        Ok(Some(batch))
    }
}

fn validate_next_position<E>(
    current: &NativePosition,
    next: &NativePosition,
) -> Result<(), SqliteLogicalRowsBatchError<E>> {
    if current.kind() != next.kind() {
        return Err(SqliteLogicalRowsBatchError::PositionKindChanged);
    }
    // Providers encode tuple keysets so their SQL order is preserved by byte ordering here.
    if next.value() <= current.value() {
        return Err(SqliteLogicalRowsBatchError::NonIncreasingNativePosition);
    }
    Ok(())
}

#[derive(Error)]
pub(crate) enum SqliteLogicalRowsBatchError<E> {
    #[error("SQLite logical-row callback failed")]
    Callback(E),
    #[error("SQLite logical-row native position kind changed")]
    PositionKindChanged,
    #[error("SQLite logical-row native positions must be strictly increasing")]
    NonIncreasingNativePosition,
    #[error("SQLite logical-row ordinals must be strictly increasing")]
    NonIncreasingOrdinal,
    #[error(
        "SQLite logical-row producer retained {actual} payload bytes in flight, maximum {maximum}"
    )]
    InFlightPayloadTooLarge { actual: usize, maximum: usize },
    #[error(transparent)]
    Batch(#[from] CapturedBatchError),
}

impl<E> fmt::Debug for SqliteLogicalRowsBatchError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Callback(_) => formatter.write_str("Callback(<redacted>)"),
            Self::PositionKindChanged => formatter.write_str("PositionKindChanged"),
            Self::NonIncreasingNativePosition => formatter.write_str("NonIncreasingNativePosition"),
            Self::NonIncreasingOrdinal => formatter.write_str("NonIncreasingOrdinal"),
            Self::InFlightPayloadTooLarge { actual, maximum } => formatter
                .debug_struct("InFlightPayloadTooLarge")
                .field("actual", actual)
                .field("maximum", maximum)
                .finish(),
            Self::Batch(error) => formatter.debug_tuple("Batch").field(error).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, convert::Infallible, rc::Rc};

    use ctx_history_core::CaptureProvider;

    use super::*;
    use crate::captured_batch::{CAPTURE_BATCH_MAX_PAYLOAD_BYTES, MAX_SQLITE_VALUES_PER_RECORD};

    fn observation() -> SourceObservation {
        SourceObservation::new(
            CaptureProvider::Codex,
            "codex_state_sqlite",
            "state-db:test",
            "snapshot:test",
            "provider:codex:codex_state_sqlite:source:test",
            1,
            1,
            None,
        )
        .unwrap()
    }

    fn position(value: u64) -> NativePosition {
        NativePosition::new("sqlite-keyset-v1", value.to_be_bytes().to_vec()).unwrap()
    }

    fn position_value(value: &NativePosition) -> u64 {
        u64::from_be_bytes(value.value().try_into().unwrap())
    }

    fn locator(value: u64) -> NativeLocator {
        NativeLocator::new("sqlite-logical-row-v1", value.to_be_bytes().to_vec()).unwrap()
    }

    fn record_kind() -> ProviderRecordKind {
        ProviderRecordKind::new("codex-thread-v1").unwrap()
    }

    fn values_row(
        next_position: u64,
        ordinal: u64,
        values: Vec<CapturedSqliteValue>,
    ) -> SqliteLogicalRow {
        SqliteLogicalRow::values(
            position(next_position),
            ordinal,
            locator(ordinal),
            record_kind(),
            values,
        )
        .unwrap()
    }

    fn oversize_row(next_position: u64, ordinal: u64, observed_bytes: u64) -> SqliteLogicalRow {
        SqliteLogicalRow::oversize(
            position(next_position),
            ordinal,
            locator(ordinal),
            record_kind(),
            observed_bytes,
        )
        .unwrap()
    }

    #[test]
    fn native_content_retains_exact_bytes_position_and_locator() {
        let content = vec![0, 255, b'\n', 17];
        let row = SqliteLogicalRow::native_content(
            position(7),
            6,
            locator(6),
            record_kind(),
            content.clone(),
        )
        .unwrap();

        assert_eq!(row.next_position(), &position(7));
        assert_eq!(row.ordinal(), 6);
        assert_eq!(row.record().locator(), &locator(6));
        assert_eq!(row.record().record_kind(), &record_kind());
        assert!(matches!(
            row.record().payload(),
            CapturedRecordPayload::NativeBytes(actual) if actual == &content
        ));

        let error = SqliteLogicalRow::native_content(
            position(1),
            0,
            locator(0),
            record_kind(),
            vec![0; CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + 1],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            SqliteLogicalRowError::Batch(CapturedBatchError::RecordPayloadTooLarge { .. })
        ));
    }

    #[test]
    fn sixty_five_rows_split_at_the_record_limit() {
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        let mut producer =
            SqliteLogicalRowBatchProducer::new(observation(), position(0), move |after| {
                callback_calls.set(callback_calls.get() + 1);
                let next = position_value(&after) + 1;
                Ok::<_, Infallible>(
                    (next <= 65)
                        .then(|| values_row(next, next - 1, vec![CapturedSqliteValue::Null])),
                )
            });

        let first = producer.next_batch().unwrap().unwrap();
        let second = producer.next_batch().unwrap().unwrap();

        assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
        assert_eq!(first.records()[0].ordinal(), 0);
        assert_eq!(first.records()[63].ordinal(), 63);
        assert_eq!(first.range_end(), second.range_before());
        assert!(!first.source_exhausted());
        assert_eq!(second.records().len(), 1);
        assert_eq!(second.records()[0].ordinal(), 64);
        assert_eq!(second.range_end(), &position(65));
        assert!(second.source_exhausted());
        assert_eq!(calls.get(), 66);
        assert!(producer.next_batch().unwrap().is_none());
        assert_eq!(calls.get(), 66);
    }

    #[test]
    fn byte_boundary_retains_one_bounded_row_without_refetching_it() {
        let calls = Rc::new(Cell::new(0));
        let second_row_fetches = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        let callback_second_row_fetches = Rc::clone(&second_row_fetches);
        let mut producer =
            SqliteLogicalRowBatchProducer::new(observation(), position(0), move |after| {
                callback_calls.set(callback_calls.get() + 1);
                Ok::<_, Infallible>(match position_value(&after) {
                    0 => Some(values_row(
                        1,
                        0,
                        vec![CapturedSqliteValue::Text(
                            "x".repeat(CAPTURE_BATCH_MAX_PAYLOAD_BYTES - 6),
                        )],
                    )),
                    1 => {
                        callback_second_row_fetches.set(callback_second_row_fetches.get() + 1);
                        Some(values_row(
                            2,
                            1,
                            vec![CapturedSqliteValue::Blob(vec![
                                0;
                                CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES
                                    - 5
                            ])],
                        ))
                    }
                    _ => None,
                })
            });

        let first = producer.next_batch().unwrap().unwrap();
        assert_eq!(first.records().len(), 1);
        assert_eq!(first.range_end(), &position(1));
        assert_eq!(
            first.retained_payload_bytes(),
            CAPTURE_BATCH_MAX_PAYLOAD_BYTES - 1
        );
        assert_eq!(
            SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES,
            24 * 1024 * 1024
        );
        let lookahead_payload_bytes = producer
            .lookahead
            .as_ref()
            .map(|row| row.record.retained_bytes())
            .unwrap();
        assert_eq!(
            lookahead_payload_bytes,
            CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES
        );
        assert!(
            first.retained_payload_bytes() + lookahead_payload_bytes
                <= SQLITE_LOGICAL_ROWS_MAX_IN_FLIGHT_PAYLOAD_BYTES
        );
        assert_eq!(second_row_fetches.get(), 1);
        assert_eq!(calls.get(), 2);
        drop(first);

        let second = producer.next_batch().unwrap().unwrap();
        assert_eq!(second.range_before(), &position(1));
        assert_eq!(second.range_end(), &position(2));
        assert_eq!(second.records().len(), 1);
        assert_eq!(
            second.retained_payload_bytes(),
            CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES
        );
        assert!(producer.lookahead.is_none());
        assert_eq!(second_row_fetches.get(), 1);
        assert_eq!(calls.get(), 2);
        drop(second);
        assert!(producer.next_batch().unwrap().is_none());
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn preserves_all_sqlite_value_tags_without_debug_payloads() {
        let real_bits = 0x7ff8_0000_0000_0042;
        let blob = vec![0, 255, 17, 34];
        let row = values_row(
            1,
            0,
            vec![
                CapturedSqliteValue::Null,
                CapturedSqliteValue::Integer(-9),
                CapturedSqliteValue::RealBits(real_bits),
                CapturedSqliteValue::Text("transcript-secret".to_owned()),
                CapturedSqliteValue::Blob(blob.clone()),
            ],
        );
        let debug = format!("{row:?}");
        assert!(!debug.contains("transcript-secret"));
        assert!(!debug.contains("255"));

        let mut row = Some(row);
        let mut producer =
            SqliteLogicalRowBatchProducer::new(observation(), position(0), move |_| {
                Ok::<_, Infallible>(row.take())
            });
        let batch = producer.next_batch().unwrap().unwrap();
        let CapturedRecordPayload::SqliteValues(values) = batch.records()[0].payload() else {
            panic!("expected SQLite logical values");
        };

        assert!(matches!(&values[0], CapturedSqliteValue::Null));
        assert!(matches!(
            &values[1],
            CapturedSqliteValue::Integer(value) if *value == -9
        ));
        assert!(matches!(
            &values[2],
            CapturedSqliteValue::RealBits(bits) if *bits == real_bits
        ));
        assert!(matches!(
            &values[3],
            CapturedSqliteValue::Text(value) if value == "transcript-secret"
        ));
        assert!(matches!(
            &values[4],
            CapturedSqliteValue::Blob(value) if value == &blob
        ));
    }

    #[test]
    fn representable_large_row_is_a_singleton() {
        let retained_bytes = CAPTURE_BATCH_MAX_PAYLOAD_BYTES + 5;
        let mut row = Some(values_row(
            1,
            0,
            vec![CapturedSqliteValue::Blob(vec![
                0;
                CAPTURE_BATCH_MAX_PAYLOAD_BYTES
            ])],
        ));
        let mut producer =
            SqliteLogicalRowBatchProducer::new(observation(), position(0), move |_| {
                Ok::<_, Infallible>(row.take())
            });

        let batch = producer.next_batch().unwrap().unwrap();
        assert_eq!(batch.records().len(), 1);
        assert_eq!(batch.retained_payload_bytes(), retained_bytes);
        assert!(batch.retained_payload_bytes() > CAPTURE_BATCH_MAX_PAYLOAD_BYTES);
    }

    #[test]
    fn structural_oversize_advances_and_continues_from_the_exact_position() {
        let observed_bytes = CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64 + 1;
        let seen_positions = Rc::new(std::cell::RefCell::new(Vec::new()));
        let callback_positions = Rc::clone(&seen_positions);
        let mut producer =
            SqliteLogicalRowBatchProducer::new(observation(), position(0), move |after| {
                let after = position_value(&after);
                callback_positions.borrow_mut().push(after);
                Ok::<_, Infallible>(match after {
                    0 => Some(values_row(1, 10, vec![CapturedSqliteValue::Integer(1)])),
                    1 => Some(oversize_row(2, 11, observed_bytes)),
                    2 => Some(values_row(3, 12, vec![CapturedSqliteValue::Integer(3)])),
                    _ => None,
                })
            });

        let batch = producer.next_batch().unwrap().unwrap();
        assert_eq!(batch.records().len(), 3);
        assert!(matches!(
            batch.records()[1].payload(),
            CapturedRecordPayload::StructuralRejection {
                kind: StructuralRejectionKind::OversizeRecord,
                observed_bytes: actual,
            } if *actual == observed_bytes
        ));
        assert_eq!(batch.range_end(), &position(3));
        assert_eq!(&*seen_positions.borrow(), &[0, 1, 2, 3]);
    }

    #[test]
    fn duplicate_and_backward_positions_are_rejected_without_advancing() {
        for next_position in [2, 1] {
            let mut producer =
                SqliteLogicalRowBatchProducer::new(observation(), position(2), move |_| {
                    Ok::<_, Infallible>(Some(values_row(
                        next_position,
                        0,
                        vec![CapturedSqliteValue::Null],
                    )))
                });

            assert!(matches!(
                producer.next_batch(),
                Err(SqliteLogicalRowsBatchError::NonIncreasingNativePosition)
            ));
            assert_eq!(producer.current_position(), &position(2));
        }
    }

    #[test]
    fn duplicate_and_backward_ordinals_are_rejected() {
        for next_ordinal in [5, 4] {
            let mut producer =
                SqliteLogicalRowBatchProducer::new(observation(), position(0), move |after| {
                    Ok::<_, Infallible>(match position_value(&after) {
                        0 => Some(values_row(1, 5, vec![CapturedSqliteValue::Null])),
                        1 => Some(values_row(2, next_ordinal, vec![CapturedSqliteValue::Null])),
                        _ => None,
                    })
                });

            assert!(matches!(
                producer.next_batch(),
                Err(SqliteLogicalRowsBatchError::NonIncreasingOrdinal)
            ));
            assert_eq!(producer.current_position(), &position(0));
        }
    }

    #[test]
    fn row_construction_uses_existing_value_and_oversize_bounds() {
        let too_many_values = (0..=MAX_SQLITE_VALUES_PER_RECORD)
            .map(|_| CapturedSqliteValue::Null)
            .collect();
        assert!(matches!(
            SqliteLogicalRow::values(position(1), 0, locator(0), record_kind(), too_many_values),
            Err(SqliteLogicalRowError::Batch(
                CapturedBatchError::TooManySqliteValues { .. }
            ))
        ));
        assert!(matches!(
            SqliteLogicalRow::oversize(
                position(1),
                0,
                locator(0),
                record_kind(),
                CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64,
            ),
            Err(SqliteLogicalRowError::InvalidOversizeMarker { .. })
        ));
    }

    #[test]
    fn empty_stream_stays_empty_without_repolling() {
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        let mut producer =
            SqliteLogicalRowBatchProducer::new(observation(), position(0), move |_| {
                callback_calls.set(callback_calls.get() + 1);
                Ok::<_, Infallible>(None)
            });

        assert!(producer.next_batch().unwrap().is_none());
        assert!(producer.next_batch().unwrap().is_none());
        assert_eq!(calls.get(), 1);
        assert_eq!(producer.current_position(), &position(0));
    }
}
