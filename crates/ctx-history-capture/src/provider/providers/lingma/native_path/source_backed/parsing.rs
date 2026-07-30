use std::collections::BTreeSet;

#[cfg(test)]
use ctx_history_core::CertifiedSourceInventory;
use ctx_history_core::{CertifiedSource, ScannedSourceCounts, SourceKey};
use ctx_history_index::LexicalDocument;
use sha2::Digest;

use crate::{
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceReadSnapshot},
    CaptureError,
};

use super::super::{
    detect_schema, hash_candidate, initial_prefix_hasher, load_candidates, load_raw_row,
    records::{assistant_text, lingma_logical_record_sha256, native_values},
    SqliteEncoding,
};
#[cfg(test)]
use super::{
    discovery::LingmaRootAuthorizedSource, identity::LingmaSourceBackedRecordV0,
    INVENTORY_DISCOVERY_REVISION,
};
use super::{
    discovery::{LingmaDatabaseSourceV0, LingmaSourceInventoryV0},
    identity::{project_row, ParsedRow},
    LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0, PARSER_REVISION,
};

const SOURCE_BACKED_PAGE_ROWS: usize = 64;
type DuplicateRequestIdentity = (Vec<u8>, Vec<u8>);

pub(crate) trait LingmaSourceBackedSinkV0 {
    fn emit(&mut self, document: LexicalDocument) -> LingmaSourceBackedResultV0<()>;
}

impl<F> LingmaSourceBackedSinkV0 for F
where
    F: FnMut(LexicalDocument) -> LingmaSourceBackedResultV0<()>,
{
    fn emit(&mut self, document: LexicalDocument) -> LingmaSourceBackedResultV0<()> {
        self(document)
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct LingmaDatabaseScanV0 {
    pub(super) certificate: CertifiedSource,
    pub(super) records: Vec<LingmaSourceBackedRecordV0>,
}

#[cfg(test)]
impl LingmaDatabaseScanV0 {
    pub(crate) fn records(&self) -> &[LingmaSourceBackedRecordV0] {
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
    opening_inventory: LingmaSourceInventoryV0,
    close_inventory: F,
) -> LingmaSourceBackedResultV0<LingmaSourceBackedScanV0>
where
    F: FnOnce() -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0>,
{
    reject_duplicate_paths(&opening_inventory)?;
    let mut databases = Vec::with_capacity(opening_inventory.databases.len());
    for database in &opening_inventory.databases {
        let root_authority = LingmaRootAuthorizedSource::retain(&database.path)?;
        let sqlite_snapshot = root_authority.open_snapshot()?;
        let mut records = Vec::new();
        let certificate = scan_lingma_snapshot_v0(database, sqlite_snapshot, &mut |document| {
            records.push(LingmaSourceBackedRecordV0 { document });
            Ok(())
        })?;
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
) -> LingmaSourceBackedResultV0<CertifiedSource> {
    let source = database.source_key()?;
    let connection = sqlite_snapshot.connection()?;
    let encoding = detect_schema(connection)?;
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
    let source_path = database.path.display().to_string();
    let parsed = scan_rows(connection, encoding, &source, &source_path, sink)?;
    before_database_certification();
    sqlite_snapshot.finish()?;
    let schema_evidence = format!(
        "user_version={user_version}\0schema={schema_fingerprint}\0encoding={}",
        match encoding {
            SqliteEncoding::Utf8 => "utf8",
            SqliteEncoding::Utf16Le => "utf16le",
            SqliteEncoding::Utf16Be => "utf16be",
        }
    );
    Ok(SqliteLogicalSnapshot::new(
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
    .certify(source)?)
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
    source_path: &str,
    sink: &mut impl LingmaSourceBackedSinkV0,
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
        for candidate in &candidates {
            certified_bytes = certified_bytes
                .checked_add(u64::try_from(candidate.encoded_bytes).map_err(|_| {
                    CaptureError::SystemInvariant("Lingma certified byte count exceeds u64")
                })?)
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma certified byte count exhausted",
                ))?;
            let raw = candidate
                .can_hydrate()
                .then(|| load_raw_row(connection, candidate.rowid))
                .transpose()?;
            hash_candidate(&mut hasher, candidate, raw.as_ref());
            let parsed = if !candidate.required_fields_present() || !candidate.can_hydrate() {
                None
            } else {
                raw.and_then(|raw| super::super::decode_raw_row(raw, encoding).ok())
            };
            match parsed {
                Some(row) if row.chat_prompt.trim().is_empty() => {
                    ignored_records = ignored_records.saturating_add(1);
                }
                Some(row) => {
                    let logical_records = 1_u64 + u64::from(assistant_text(&row).is_some());
                    retained_records = retained_records
                        .checked_add(logical_records)
                        .ok_or(LingmaSourceBackedErrorV0::CountOverflow)?;
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
                    project_row(
                        source,
                        source_path,
                        ParsedRow {
                            ordinal: physical_ordinal,
                            row,
                            record_digest,
                            request_identity_unique,
                        },
                        &mut projected,
                    )?;
                    indexed_documents = indexed_documents
                        .checked_add(
                            u64::try_from(projected.len())
                                .map_err(|_| LingmaSourceBackedErrorV0::CountOverflow)?,
                        )
                        .ok_or(LingmaSourceBackedErrorV0::CountOverflow)?;
                    if page.len().saturating_add(projected.len()) > SOURCE_BACKED_PAGE_ROWS {
                        emit_page(sink, &mut page)?;
                    }
                    page.extend(projected.into_iter().map(|record| record.document));
                }
                None => {
                    rejected_records = rejected_records.saturating_add(1);
                }
            }
            physical_ordinal = physical_ordinal.saturating_add(1);
            after_rowid = Some(candidate.rowid);
        }
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
    page: &mut Vec<LexicalDocument>,
) -> LingmaSourceBackedResultV0<()> {
    for document in page.drain(..) {
        sink.emit(document)?;
    }
    Ok(())
}
