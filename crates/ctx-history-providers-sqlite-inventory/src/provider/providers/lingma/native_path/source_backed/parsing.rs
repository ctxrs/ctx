use std::collections::BTreeSet;

#[cfg(test)]
use ctx_history_core::CertifiedSourceInventory;
use ctx_history_core::{
    CaptureProvider, CertifiedSource, CoreRecord, ScannedSourceCounts, SourceKey,
};
use sha2::Digest;

use crate::{
    provider::source_backed::{
        record_sqlite_rejection, SourceBackedRecordRejectionClass,
        SourceBackedRecordRejectionDrafts,
    },
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceReadSnapshot},
    CaptureError,
};

use super::super::{
    detect_schema, hash_candidate, initial_prefix_hasher, load_candidates,
    records::{assistant_text, lingma_logical_record_sha256, native_values},
    visit_raw_rows, Candidate, RawRow, SqliteEncoding,
};
#[cfg(test)]
use super::{discovery::LingmaRootAuthorizedSource, INVENTORY_DISCOVERY_REVISION};
use super::{
    discovery::{LingmaDatabaseSourceV0, LingmaSourceInventoryV0},
    identity::{project_row, ParsedRow},
    lingma_row_projection_error, LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0,
    PARSER_REVISION,
};

const SOURCE_BACKED_PAGE_ROWS: usize = 64;
type DuplicateRequestIdentity = (Vec<u8>, Vec<u8>);

pub(crate) trait LingmaSourceBackedSinkV0 {
    fn emit(&mut self, record: CoreRecord) -> LingmaSourceBackedResultV0<()>;
}

impl<F> LingmaSourceBackedSinkV0 for F
where
    F: FnMut(CoreRecord) -> LingmaSourceBackedResultV0<()>,
{
    fn emit(&mut self, record: CoreRecord) -> LingmaSourceBackedResultV0<()> {
        self(record)
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct LingmaDatabaseScanV0 {
    pub(super) certificate: CertifiedSource,
    pub(super) records: Vec<CoreRecord>,
}

#[cfg(test)]
impl LingmaDatabaseScanV0 {
    pub(crate) fn records(&self) -> &[CoreRecord] {
        &self.records
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedScanV0 {
    pub(super) databases: Vec<LingmaDatabaseScanV0>,
}

#[cfg(test)]
thread_local! {
    static BEFORE_DATABASE_CERTIFICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_before_database_certification(hook: Option<Box<dyn FnOnce()>>) {
    BEFORE_DATABASE_CERTIFICATION.with(|slot| {
        *slot.borrow_mut() = hook;
    });
}

#[cfg(test)]
fn before_database_certification() {
    BEFORE_DATABASE_CERTIFICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_database_certification() {}

#[cfg(test)]
impl LingmaSourceBackedScanV0 {
    pub(crate) fn databases(&self) -> &[LingmaDatabaseScanV0] {
        &self.databases
    }
}

#[cfg(test)]
pub(crate) fn scan_lingma_source_backed_v0<F>(
    data_root: &std::path::Path,
    opening_inventory: LingmaSourceInventoryV0,
    close_inventory: F,
) -> LingmaSourceBackedResultV0<LingmaSourceBackedScanV0>
where
    F: FnOnce() -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0>,
{
    reject_duplicate_paths(&opening_inventory)?;
    let mut databases = Vec::with_capacity(opening_inventory.databases.len());
    for database in &opening_inventory.databases {
        let root_authority = LingmaRootAuthorizedSource::retain(data_root, &database.path)?;
        let sqlite_snapshot = root_authority.open_snapshot()?;
        let mut records = Vec::new();
        let certificate = scan_lingma_snapshot_v0(
            database,
            sqlite_snapshot,
            &mut |record| {
                records.push(record);
                Ok(())
            },
            &mut SourceBackedRecordRejectionDrafts::default(),
        )?;
        root_authority.source_root.revalidate()?;
        databases.push(LingmaDatabaseScanV0 {
            certificate,
            records,
        });
    }

    let closing_inventory = close_inventory()?;
    if !opening_inventory.exact_inventory_eq(&closing_inventory)? {
        return Err(LingmaSourceBackedErrorV0::InventoryChangedDuringScan);
    }
    let source_keys = opening_inventory.source_keys()?;
    CertifiedSourceInventory::certify(
        opening_inventory.observation,
        closing_inventory.observation,
        INVENTORY_DISCOVERY_REVISION,
        source_keys,
    )?;
    Ok(LingmaSourceBackedScanV0 { databases })
}

pub(crate) fn reject_duplicate_paths(
    inventory: &LingmaSourceInventoryV0,
) -> LingmaSourceBackedResultV0<()> {
    let mut paths = inventory
        .databases
        .iter()
        .map(|database| database.path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(LingmaSourceBackedErrorV0::DuplicateDatabasePath);
    }
    Ok(())
}

pub(crate) fn scan_lingma_snapshot_v0(
    database: &LingmaDatabaseSourceV0,
    sqlite_snapshot: SqliteSourceReadSnapshot,
    sink: &mut impl LingmaSourceBackedSinkV0,
    rejections: &mut SourceBackedRecordRejectionDrafts,
) -> LingmaSourceBackedResultV0<CertifiedSource> {
    let scan = (|| {
        let source = database.source_key()?;
        let connection = sqlite_snapshot.connection()?;
        let encoding = detect_schema(connection)?;
        let user_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(CaptureError::from)?;
        let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
        let parsed = scan_rows(
            connection,
            encoding,
            &source,
            database.path(),
            sink,
            rejections,
        )?;
        before_database_certification();
        let schema_evidence = format!(
            "user_version={user_version}\0schema={schema_fingerprint}\0encoding={}",
            match encoding {
                SqliteEncoding::Utf8 => "utf8",
                SqliteEncoding::Utf16Le => "utf16le",
                SqliteEncoding::Utf16Be => "utf16be",
            }
        );
        SqliteLogicalSnapshot::new(
            PARSER_REVISION,
            schema_evidence.as_bytes(),
            parsed.content_digest,
            ScannedSourceCounts {
                complete_records: parsed.complete_records,
                retained_records: parsed.retained_records,
                rejected_records: parsed.rejected_records,
                ignored_records: parsed.ignored_records,
                indexed_documents: parsed.indexed_documents,
                certified_bytes: parsed.certified_bytes,
            },
        )
        .certify(source)
        .map_err(Into::into)
    })();
    match scan {
        Ok(certificate) => {
            sqlite_snapshot.finish()?;
            Ok(certificate)
        }
        Err(primary) => match sqlite_snapshot.abort() {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(LingmaSourceBackedErrorV0::SnapshotCleanup {
                primary: Box::new(primary),
                cleanup,
            }),
        },
    }
}

struct ParsedScan {
    complete_records: u64,
    retained_records: u64,
    rejected_records: u64,
    ignored_records: u64,
    indexed_documents: u64,
    certified_bytes: u64,
    content_digest: [u8; 32],
}

fn scan_rows(
    connection: &rusqlite::Connection,
    encoding: SqliteEncoding,
    source: &SourceKey,
    source_path: &std::path::Path,
    sink: &mut impl LingmaSourceBackedSinkV0,
    rejections: &mut SourceBackedRecordRejectionDrafts,
) -> LingmaSourceBackedResultV0<ParsedScan> {
    let mut after_rowid = None;
    let mut physical_ordinal = 0_u64;
    let mut certified_bytes = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut indexed_documents = 0_u64;
    let mut hasher = initial_prefix_hasher();
    let duplicate_requests = duplicate_request_identities(connection)?;
    let mut page = Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS);

    loop {
        let candidates = load_candidates(connection, encoding, after_rowid, None)?;
        if candidates.is_empty() {
            break;
        }
        let rowids = candidates
            .iter()
            .filter(|candidate| candidate.can_decode())
            .map(|candidate| candidate.rowid)
            .collect::<Vec<_>>();
        let mut candidate_index = 0;
        visit_raw_rows(connection, &rowids, |raw| {
            while candidates
                .get(candidate_index)
                .is_some_and(|candidate| !candidate.can_decode())
            {
                process_candidate(
                    &candidates[candidate_index],
                    None,
                    encoding,
                    source,
                    source_path,
                    &duplicate_requests,
                    sink,
                    rejections,
                    &mut page,
                    &mut hasher,
                    &mut physical_ordinal,
                    &mut certified_bytes,
                    &mut retained_records,
                    &mut rejected_records,
                    &mut ignored_records,
                    &mut indexed_documents,
                )?;
                candidate_index += 1;
            }
            let candidate = candidates
                .get(candidate_index)
                .ok_or(CaptureError::SourceChangedDuringCapture)?;
            if candidate.rowid != raw.rowid {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            candidate_index += 1;
            process_candidate(
                candidate,
                Some(raw),
                encoding,
                source,
                source_path,
                &duplicate_requests,
                sink,
                rejections,
                &mut page,
                &mut hasher,
                &mut physical_ordinal,
                &mut certified_bytes,
                &mut retained_records,
                &mut rejected_records,
                &mut ignored_records,
                &mut indexed_documents,
            )
        })?;
        while candidates
            .get(candidate_index)
            .is_some_and(|candidate| !candidate.can_decode())
        {
            process_candidate(
                &candidates[candidate_index],
                None,
                encoding,
                source,
                source_path,
                &duplicate_requests,
                sink,
                rejections,
                &mut page,
                &mut hasher,
                &mut physical_ordinal,
                &mut certified_bytes,
                &mut retained_records,
                &mut rejected_records,
                &mut ignored_records,
                &mut indexed_documents,
            )?;
            candidate_index += 1;
        }
        if candidate_index != candidates.len() {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        after_rowid = candidates.last().map(|candidate| candidate.rowid);
    }

    let complete_records = retained_records
        .checked_add(rejected_records)
        .and_then(|count| count.checked_add(ignored_records))
        .ok_or(LingmaSourceBackedErrorV0::CountOverflow)?;
    emit_page(sink, &mut page)?;
    Ok(ParsedScan {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents,
        certified_bytes,
        content_digest: hasher.finalize().into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn process_candidate(
    candidate: &Candidate,
    raw: Option<RawRow>,
    encoding: SqliteEncoding,
    source: &SourceKey,
    source_path: &std::path::Path,
    duplicate_requests: &BTreeSet<DuplicateRequestIdentity>,
    sink: &mut impl LingmaSourceBackedSinkV0,
    rejections: &mut SourceBackedRecordRejectionDrafts,
    page: &mut Vec<CoreRecord>,
    hasher: &mut sha2::Sha256,
    physical_ordinal: &mut u64,
    certified_bytes: &mut u64,
    retained_records: &mut u64,
    rejected_records: &mut u64,
    ignored_records: &mut u64,
    indexed_documents: &mut u64,
) -> LingmaSourceBackedResultV0<()> {
    *certified_bytes = certified_bytes
        .checked_add(u64::try_from(candidate.encoded_bytes).map_err(|_| {
            CaptureError::SystemInvariant("Lingma certified byte count exceeds u64")
        })?)
        .ok_or(CaptureError::SystemInvariant(
            "Lingma certified byte count exhausted",
        ))?;
    hash_candidate(hasher, candidate, raw.as_ref());
    let parsed = if candidate.can_decode() {
        raw.and_then(|raw| super::super::decode_raw_row(raw, encoding).ok())
    } else {
        None
    };
    match parsed {
        Some(row) if row.chat_prompt.trim().is_empty() => {
            *ignored_records = ignored_records.saturating_add(1);
        }
        Some(row) => {
            let logical_records = 1_u64 + u64::from(assistant_text(&row).is_some());
            let record_digest = lingma_logical_record_sha256(&native_values(&row));
            let request_identity_unique = row
                .request_id
                .as_ref()
                .filter(|request_id| !request_id.trim().is_empty())
                .is_some_and(|request_id| {
                    !duplicate_requests.contains(&(
                        row.session_id.as_bytes().to_vec(),
                        request_id.as_bytes().to_vec(),
                    ))
                });
            let mut projected = Vec::with_capacity(2);
            let projection = project_row(
                source,
                ParsedRow {
                    ordinal: *physical_ordinal,
                    row,
                    record_digest,
                    request_identity_unique,
                },
                &mut projected,
            );
            match projection {
                Ok(()) => {
                    *retained_records = retained_records
                        .checked_add(logical_records)
                        .ok_or(LingmaSourceBackedErrorV0::CountOverflow)?;
                    *indexed_documents = indexed_documents
                        .checked_add(
                            u64::try_from(projected.len())
                                .map_err(|_| LingmaSourceBackedErrorV0::CountOverflow)?,
                        )
                        .ok_or(LingmaSourceBackedErrorV0::CountOverflow)?;
                    if page.len().saturating_add(projected.len()) > SOURCE_BACKED_PAGE_ROWS {
                        emit_page(sink, page)?;
                    }
                    page.extend(projected);
                }
                Err(error) if lingma_row_projection_error(&error) => {
                    *rejected_records = rejected_records.saturating_add(1);
                    record_sqlite_rejection(
                        rejections,
                        source,
                        CaptureProvider::Lingma,
                        source_path,
                        u64::try_from(candidate.rowid)
                            .unwrap_or(physical_ordinal.saturating_add(1)),
                        SourceBackedRecordRejectionClass::UnsupportedRecord,
                        error.to_string(),
                    );
                }
                Err(error) => return Err(error),
            }
        }
        None => {
            *rejected_records = rejected_records.saturating_add(1);
            record_sqlite_rejection(
                rejections,
                source,
                CaptureProvider::Lingma,
                source_path,
                u64::try_from(candidate.rowid).unwrap_or(physical_ordinal.saturating_add(1)),
                if candidate.can_decode() {
                    SourceBackedRecordRejectionClass::MalformedRecord
                } else {
                    SourceBackedRecordRejectionClass::UnsupportedRecord
                },
                if candidate.can_decode() {
                    "Lingma SQLite row could not be decoded"
                } else {
                    "Lingma SQLite row exceeds the supported shape or size bound"
                },
            );
        }
    }
    *physical_ordinal = physical_ordinal.saturating_add(1);
    Ok(())
}

fn duplicate_request_identities(
    connection: &rusqlite::Connection,
) -> Result<BTreeSet<DuplicateRequestIdentity>, CaptureError> {
    let mut statement = connection.prepare(
        "select cast(session_id as blob), cast(request_id as blob)
           from chat_record
          where request_id is not null and length(cast(request_id as blob)) > 0
          group by cast(session_id as blob), cast(request_id as blob)
         having count(*) > 1",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect::<Result<_, _>>().map_err(CaptureError::from)
}

fn emit_page(
    sink: &mut impl LingmaSourceBackedSinkV0,
    page: &mut Vec<CoreRecord>,
) -> LingmaSourceBackedResultV0<()> {
    for record in page.drain(..) {
        sink.emit(record)?;
    }
    Ok(())
}
