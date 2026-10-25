use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
};

use ctx_history_capture_runtime::{
    SourceBackedRouteByteReservation, SourceBackedRouteResourceKind, SourceBackedRouteResources,
};

use super::{JsonlFamilyError, JsonlPhysicalDigest, JsonlRecordFraming, JsonlResult};

/// Hard physical bound for one provider-owned standard Zstandard JSONL stream.
pub const MAX_STANDARD_ZSTD_COMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Hard logical bound for one decoded standard Zstandard JSONL stream.
pub const MAX_STANDARD_ZSTD_DECOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Compressed snapshot plus decoded spool retained by one standard stream.
pub const MAX_STANDARD_ZSTD_TEMP_BYTES_PER_LEAF: u64 = 256 * 1024 * 1024;
/// The production route scratch budget admits this many maximum-size leaves.
pub const MAX_STANDARD_ZSTD_PARALLEL_STREAMS: usize = 4;
const MAX_STANDARD_ZSTD_EXPANSION_RATIO: u64 = 256;
const STANDARD_ZSTD_EXPANSION_SLACK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_STANDARD_ZSTD_WINDOW_LOG: u32 = 27;
const STANDARD_ZSTD_COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(super) struct StandardZstdBacking {
    pub(super) physical_reader: BufReader<File>,
    pub(super) logical_length: u64,
    pub(super) total_records: u64,
    pub(super) _scratch_reservations: Vec<SourceBackedRouteByteReservation>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct StandardZstdLimits {
    compressed_bytes: u64,
    decompressed_bytes: u64,
    expansion_ratio: u64,
    expansion_slack_bytes: u64,
}

impl StandardZstdLimits {
    pub(super) const PRODUCTION: Self = Self {
        compressed_bytes: MAX_STANDARD_ZSTD_COMPRESSED_BYTES,
        decompressed_bytes: MAX_STANDARD_ZSTD_DECOMPRESSED_BYTES,
        expansion_ratio: MAX_STANDARD_ZSTD_EXPANSION_RATIO,
        expansion_slack_bytes: STANDARD_ZSTD_EXPANSION_SLACK_BYTES,
    };
}

pub(super) struct DecodedStandardZstd {
    pub(super) plaintext: File,
    pub(super) physical_reader: BufReader<File>,
    pub(super) logical_length: u64,
    pub(super) total_records: u64,
    pub(super) scratch_reservations: Vec<SourceBackedRouteByteReservation>,
}

pub(super) fn decode_standard_zstd_jsonl<E: JsonlFamilyError>(
    file: File,
    compressed_length: u64,
    framing: JsonlRecordFraming,
    limits: StandardZstdLimits,
    route_resources: Option<&SourceBackedRouteResources>,
) -> JsonlResult<DecodedStandardZstd, E> {
    if compressed_length == 0 {
        return Err(invalid_standard_zstd::<E>("compressed stream is empty"));
    }
    if compressed_length > limits.compressed_bytes {
        return Err(invalid_standard_zstd::<E>(
            &format!("compressed stream exceeds the bounded physical limit (compressed_bytes={compressed_length}, limit_bytes={})", limits.compressed_bytes),
        ));
    }
    if compressed_length > MAX_STANDARD_ZSTD_TEMP_BYTES_PER_LEAF {
        return Err(invalid_standard_zstd::<E>(
            &format!("compressed snapshot exceeds the per-leaf temporary-volume limit (compressed_bytes={compressed_length}, limit_bytes={MAX_STANDARD_ZSTD_TEMP_BYTES_PER_LEAF})"),
        ));
    }
    let ratio_bound = compressed_length
        .saturating_mul(limits.expansion_ratio)
        .saturating_add(limits.expansion_slack_bytes);
    let decoded_bound = limits.decompressed_bytes.min(ratio_bound);
    let compressed_reservation =
        reserve_standard_zstd_scratch::<E>(route_resources, compressed_length)?;
    let snapshot = snapshot_standard_zstd_prefix::<E>(file, compressed_length)?;
    #[cfg(any(test, feature = "test-support"))]
    run_after_standard_zstd_snapshot_hook();
    let decoder_source = snapshot.try_clone()?;
    let mut decoder = zstd::stream::read::Decoder::new(decoder_source).map_err(|error| {
        E::invalid_payload(format!(
            "invalid standard Zstandard JSONL stream header: {error}"
        ))
    })?;
    decoder.window_log_max(MAX_STANDARD_ZSTD_WINDOW_LOG)?;
    let mut plaintext = tempfile::tempfile()?;
    let mut buffer = [0_u8; STANDARD_ZSTD_COPY_BUFFER_BYTES];
    let mut logical_length = 0_u64;
    let mut total_records = 0_u64;
    let mut trailing_bytes = 0_u64;
    let mut trailing_bytes_are_nul = true;
    let mut scratch_reservations = Vec::new();
    if let Some(reservation) = compressed_reservation {
        scratch_reservations.push(reservation);
    }
    loop {
        let read = decoder.read(&mut buffer).map_err(|error| {
            E::invalid_payload(format!(
                "corrupt or truncated Zstandard JSONL stream: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        let read_u64 = u64::try_from(read)
            .map_err(|_| E::system_invariant("decoded Zstandard chunk exceeds u64"))?;
        logical_length = logical_length.checked_add(read_u64).ok_or_else(|| {
            E::invalid_payload("decoded Zstandard JSONL length overflowed".to_owned())
        })?;
        if logical_length > decoded_bound {
            return Err(invalid_standard_zstd::<E>(
                &format!("decoded stream exceeds the bounded decompression limit (compressed_bytes={compressed_length}, decoded_bytes_at_least={logical_length}, limit_bytes={decoded_bound})"),
            ));
        }
        if compressed_length.saturating_add(logical_length) > MAX_STANDARD_ZSTD_TEMP_BYTES_PER_LEAF
        {
            return Err(invalid_standard_zstd::<E>(
                &format!("compressed snapshot and decoded stream exceed the per-leaf temporary-volume limit (compressed_bytes={compressed_length}, decoded_bytes_at_least={logical_length}, limit_bytes={MAX_STANDARD_ZSTD_TEMP_BYTES_PER_LEAF})"),
            ));
        }
        if let Some(reservation) = reserve_standard_zstd_scratch::<E>(route_resources, read_u64)? {
            scratch_reservations.push(reservation);
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                total_records = total_records.checked_add(1).ok_or_else(|| {
                    E::invalid_payload("decoded Zstandard JSONL record count overflowed".to_owned())
                })?;
                trailing_bytes = 0;
                trailing_bytes_are_nul = true;
            } else {
                trailing_bytes = trailing_bytes.saturating_add(1);
                trailing_bytes_are_nul &= *byte == 0;
            }
        }
        plaintext.write_all(&buffer[..read])?;
    }
    drop(decoder.finish());
    if logical_length == 0 {
        return Err(invalid_standard_zstd::<E>(
            "decoded stream contains no JSONL records",
        ));
    }
    if trailing_bytes != 0 {
        if framing.allows_terminal_nul_padding() && trailing_bytes_are_nul {
            total_records = total_records.checked_add(1).ok_or_else(|| {
                E::invalid_payload("decoded Zstandard JSONL record count overflowed".to_owned())
            })?;
        } else {
            return Err(invalid_standard_zstd::<E>(
                "decoded stream has a non-terminated JSONL tail",
            ));
        }
    }
    if total_records == 0 {
        return Err(invalid_standard_zstd::<E>(
            "decoded stream contains no complete JSONL records",
        ));
    }
    plaintext.flush()?;
    plaintext.seek(SeekFrom::Start(0))?;
    let mut physical_reader = BufReader::new(snapshot);
    physical_reader.seek(SeekFrom::Start(0))?;
    Ok(DecodedStandardZstd {
        plaintext,
        physical_reader,
        logical_length,
        total_records,
        scratch_reservations,
    })
}

fn snapshot_standard_zstd_prefix<E: JsonlFamilyError>(
    mut source: File,
    compressed_length: u64,
) -> JsonlResult<File, E> {
    source.seek(SeekFrom::Start(0))?;
    let mut snapshot = tempfile::tempfile()?;
    let mut remaining = compressed_length;
    let mut buffer = [0_u8; STANDARD_ZSTD_COPY_BUFFER_BYTES];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| E::system_invariant("Zstandard snapshot chunk exceeds usize"))?;
        let read = source.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(E::source_changed());
        }
        snapshot.write_all(&buffer[..read])?;
        remaining = remaining.saturating_sub(read as u64);
    }
    snapshot.flush()?;
    snapshot.seek(SeekFrom::Start(0))?;
    Ok(snapshot)
}

fn reserve_standard_zstd_scratch<E: JsonlFamilyError>(
    route_resources: Option<&SourceBackedRouteResources>,
    bytes: u64,
) -> JsonlResult<Option<SourceBackedRouteByteReservation>, E> {
    let Some(route_resources) = route_resources else {
        return Ok(None);
    };
    let bytes = usize::try_from(bytes).map_err(|_| {
        E::invalid_payload("standard Zstandard scratch size exceeds usize".to_owned())
    })?;
    route_resources
        .reserve(SourceBackedRouteResourceKind::LogicalSourceScratch, bytes)
        .map(Some)
        .map_err(|error| {
            E::invalid_payload(format!(
                "standard Zstandard JSONL temporary-volume budget unavailable: {error}"
            ))
        })
}

#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    static AFTER_STANDARD_ZSTD_SNAPSHOT_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_after_standard_zstd_snapshot_hook(hook: impl FnOnce() + 'static) {
    AFTER_STANDARD_ZSTD_SNAPSHOT_HOOK.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "standard Zstandard snapshot hook is already installed"
        );
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_standard_zstd_snapshot_hook() {
    let hook = AFTER_STANDARD_ZSTD_SNAPSHOT_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn standard_zstd_physical_end(
    current: u64,
    physical_length: u64,
    logical_end: u64,
    logical_length: u64,
    next_ordinal: u64,
    total_records: u64,
) -> u64 {
    if logical_end >= logical_length || next_ordinal >= total_records {
        return physical_length;
    }
    let proportional =
        (u128::from(logical_end) * u128::from(physical_length) / u128::from(logical_length)) as u64;
    current
        .max(proportional)
        .min(physical_length.saturating_sub(1))
}

pub(super) fn advance_standard_zstd_digest<E: JsonlFamilyError>(
    reader: &mut BufReader<File>,
    digest: &mut JsonlPhysicalDigest,
    start: u64,
    end: u64,
    complete: bool,
) -> JsonlResult<(), E> {
    if end < start {
        return Err(E::system_invariant(
            "standard Zstandard JSONL physical progress regressed",
        ));
    }
    let mut remaining = end - start;
    let mut buffer = [0_u8; STANDARD_ZSTD_COPY_BUFFER_BYTES];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| E::system_invariant("Zstandard digest chunk exceeds usize"))?;
        reader.read_exact(&mut buffer[..take])?;
        digest.update_physical_unit(&buffer[..take], complete);
        remaining = remaining.saturating_sub(take as u64);
    }
    Ok(())
}

fn invalid_standard_zstd<E: JsonlFamilyError>(detail: &str) -> E {
    E::invalid_payload(format!("invalid standard Zstandard JSONL: {detail}"))
}

#[cfg(test)]
mod tests;
