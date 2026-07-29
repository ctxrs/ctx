use std::collections::BTreeMap;

use ctx_history_core::{
    CertifiedSource, CertifiedSourceInventory, ScannedSourceCounts, SourceKey, TypedKey,
};
use sha2::Digest;

use crate::{provider::sqlite::sqlite_schema_fingerprint, CaptureError};

use super::super::{
    detect_schema, hash_candidate, initial_prefix_hasher, load_candidates, load_raw_row,
    records::{assistant_text, lingma_logical_record_sha256, native_values},
    SqliteEncoding,
};
use super::{
    discovery::{
        source_observation, source_revision_digest, LingmaDatabaseSourceV0,
        LingmaRootAuthorizedSource, LingmaSourceInventoryV0,
    },
    identity::{project_row, LingmaSourceBackedRecordV0, ParsedRow},
    LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0, INVENTORY_DISCOVERY_REVISION,
    PARSER_REVISION,
};

#[derive(Debug, Clone)]
pub(crate) struct LingmaDatabaseScanV0 {
    pub(super) certificate: CertifiedSource,
    pub(super) records: Vec<LingmaSourceBackedRecordV0>,
}

impl LingmaDatabaseScanV0 {
    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }

    pub(crate) fn records(&self) -> &[LingmaSourceBackedRecordV0] {
        &self.records
    }
}

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

impl LingmaSourceBackedScanV0 {
    pub(crate) fn databases(&self) -> &[LingmaDatabaseScanV0] {
        &self.databases
    }
}

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
        databases.push(scan_database(database)?);
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

fn reject_duplicate_paths(inventory: &LingmaSourceInventoryV0) -> LingmaSourceBackedResultV0<()> {
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

fn scan_database(
    database: &LingmaDatabaseSourceV0,
) -> LingmaSourceBackedResultV0<LingmaDatabaseScanV0> {
    let source = database.source_key()?;
    let root_authority = LingmaRootAuthorizedSource::retain(&database.path)?;
    let sqlite_snapshot = root_authority.open_snapshot()?;
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let connection = sqlite_snapshot.connection()?;
    let encoding = detect_schema(connection)?;
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
    let opening = source_observation(
        source.clone(),
        &opening_evidence,
        user_version,
        &schema_fingerprint,
        encoding,
    )?;
    let source_revision_digest = source_revision_digest(&opening);
    let revision_scope = TypedKey::bytes(opening.revision().to_vec())?;
    let source_path = database.path.display().to_string();
    let parsed = scan_rows(
        connection,
        encoding,
        &source,
        &source_revision_digest,
        &revision_scope,
        &source_path,
    )?;
    sqlite_snapshot.finish()?;
    root_authority.source_root.revalidate()?;

    let closing_sqlite_snapshot = root_authority.open_snapshot()?;
    let closing_evidence = closing_sqlite_snapshot.evidence().clone();
    let closing_connection = closing_sqlite_snapshot.connection()?;
    let closing_encoding = detect_schema(closing_connection)?;
    let closing_user_version = closing_connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let closing_schema_fingerprint = sqlite_schema_fingerprint(closing_connection)?;
    closing_sqlite_snapshot.finish()?;
    before_database_certification();
    if !root_authority.evidence_is_current(&closing_evidence)? {
        return Err(LingmaSourceBackedErrorV0::SourceChangedDuringScan);
    }
    let closing = source_observation(
        source,
        &closing_evidence,
        closing_user_version,
        &closing_schema_fingerprint,
        closing_encoding,
    )?;
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        PARSER_REVISION,
        parsed.content_digest,
        ScannedSourceCounts {
            complete_records: parsed.complete_records,
            retained_records: parsed.retained_records,
            rejected_records: parsed.rejected_records,
            ignored_records: parsed.ignored_records,
            indexed_documents: u64::try_from(parsed.records.len())
                .map_err(|_| LingmaSourceBackedErrorV0::CountOverflow)?,
            certified_bytes: parsed.certified_bytes,
        },
    )?;
    Ok(LingmaDatabaseScanV0 {
        certificate,
        records: parsed.records,
    })
}

struct ParsedScan {
    records: Vec<LingmaSourceBackedRecordV0>,
    complete_records: u64,
    retained_records: u64,
    rejected_records: u64,
    ignored_records: u64,
    certified_bytes: u64,
    content_digest: [u8; 32],
}

fn scan_rows(
    connection: &rusqlite::Connection,
    encoding: SqliteEncoding,
    source: &SourceKey,
    source_revision_digest: &[u8; 32],
    revision_scope: &TypedKey,
    source_path: &str,
) -> Result<ParsedScan, CaptureError> {
    let mut after_rowid = None;
    let mut physical_ordinal = 0_u64;
    let mut certified_bytes = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut hasher = initial_prefix_hasher();
    let mut rows = Vec::new();

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
                    retained_records = retained_records.saturating_add(logical_records);
                    let record_digest = lingma_logical_record_sha256(&native_values(&row));
                    rows.push(ParsedRow {
                        ordinal: physical_ordinal,
                        row,
                        record_digest,
                    });
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
        .ok_or(CaptureError::SystemInvariant(
            "Lingma logical record count exhausted",
        ))?;
    let request_counts = request_identity_counts(&rows);
    let mut records = Vec::with_capacity(usize::try_from(retained_records).unwrap_or(usize::MAX));
    for parsed in rows {
        project_row(
            source,
            source_revision_digest,
            revision_scope,
            source_path,
            &request_counts,
            parsed,
            &mut records,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    }
    Ok(ParsedScan {
        records,
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        certified_bytes,
        content_digest: hasher.finalize().into(),
    })
}

fn request_identity_counts(rows: &[ParsedRow]) -> BTreeMap<(String, String), usize> {
    let mut counts = BTreeMap::new();
    for parsed in rows {
        let Some(request_id) = parsed
            .row
            .request_id
            .as_ref()
            .filter(|request_id| !request_id.trim().is_empty())
        else {
            continue;
        };
        *counts
            .entry((parsed.row.session_id.clone(), request_id.clone()))
            .or_default() += 1;
    }
    counts
}
