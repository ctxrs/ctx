pub const IMPORT_INVENTORY_CHECKPOINT_FORMAT_VERSION: u32 = 1;
pub const IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES: usize = 1024 * 1024;
pub const IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS: usize = 1024;
pub const IMPORT_INVENTORY_CHECKPOINT_MAX_KEYSET_BYTES: usize = 4096;
pub const IMPORT_INVENTORY_SELECTION_FORMAT_VERSION: u32 = 1;
pub const IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION: u32 = 1;

const IMPORT_INVENTORY_SELECTION_MEMBER_DOMAIN: &[u8] = b"ctx-import-inventory-selection-member-v1";
const IMPORT_INVENTORY_SELECTION_PREFIX_DOMAIN: &[u8] = b"ctx-import-inventory-selection-prefix-v1";
const IMPORT_INVENTORY_SELECTION_COMMITMENT_DOMAIN: &[u8] =
    b"ctx-import-inventory-selection-commitment-v1";
const IMPORT_INVENTORY_EFFECT_PAYLOAD_DOMAIN: &[u8] = b"ctx-import-inventory-effect-payload-v1";

const CHECKPOINT_WRITE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportInventoryNativePathIdentity<'a> {
    pub platform_tag: &'a str,
    pub encoding_tag: &'a str,
    pub opaque_hash: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryOwnedPathIdentity {
    pub platform_tag: String,
    pub encoding_tag: String,
    pub opaque_hash: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportInventoryScratchOwner<'a> {
    pub owner_epoch: u64,
    pub owner_token: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportInventoryScratchState<'a> {
    Trusted {
        identity: &'a [u8],
        integrity: &'a [u8],
        lock_identity: &'a [u8],
        database_identity: &'a [u8],
        owner: Option<ImportInventoryScratchOwner<'a>>,
    },
    Missing,
    Corrupt,
    Tampered,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryCheckpointTrust<'a> {
    pub run_id: &'a [u8],
    pub inventory_family: ProviderFileInventoryFamily,
    pub provider: CaptureProvider,
    pub source_format: &'a str,
    pub source_root: &'a str,
    pub source_identity: &'a [u8],
    pub source_fingerprint: &'a [u8],
    pub root_path: ImportInventoryNativePathIdentity<'a>,
    pub inventory_generation: u64,
    pub checkpoint_format_version: u32,
    pub producer_build_id: &'a [u8],
    pub store_schema_version: u32,
    pub scratch_identity: &'a [u8],
    pub scratch_lock_identity: &'a [u8],
    pub scratch_database_identity: &'a [u8],
    pub publication_state_marker: &'a str,
    pub publication_owner: Option<&'a ProviderFilePublicationInventoryOwner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportInventoryFrozenSelectionCommitment {
    pub format_version: u32,
    pub algorithm_version: u32,
    pub total_count: u64,
    pub final_keyset: Option<[u8; 32]>,
    pub final_prefix: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportInventoryEffectMembership {
    pub commitment_identity: [u8; 32],
    pub ordinal: u64,
    pub prior_keyset: Option<[u8; 32]>,
    pub resulting_keyset: [u8; 32],
    pub prior_prefix: [u8; 32],
    pub resulting_prefix: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventorySelectionCanonicalizationRequest<'a> {
    pub format_version: u32,
    pub algorithm_version: u32,
    pub ordinal: u64,
    pub capture_journal_identity: &'a [u8; 32],
    pub native_path: ImportInventoryNativePathIdentity<'a>,
    pub inventory_family: ProviderFileInventoryFamily,
    pub provider: CaptureProvider,
    pub source_format: &'a str,
    pub source_root: &'a str,
    pub prior_keyset: Option<&'a [u8; 32]>,
    pub resulting_keyset: &'a [u8; 32],
    pub prior_prefix: &'a [u8; 32],
    pub accounted_bytes: u64,
    pub effect: ImportInventoryCanonicalEffect<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportInventorySelectionCanonicalization {
    pub payload_fingerprint: [u8; 32],
    pub member_digest: [u8; 32],
    pub resulting_prefix: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryActiveDirectoryProof<'a> {
    pub path: ImportInventoryNativePathIdentity<'a>,
    pub directory_identity: &'a [u8],
    pub directory_fingerprint: &'a [u8],
    pub scratch_identity: &'a [u8],
    pub scratch_integrity: &'a [u8],
    pub scratch_lock_identity: &'a [u8],
    pub scratch_database_identity: &'a [u8],
    pub attempt_count: u64,
    pub replay_count: u64,
    pub observed_entries: u64,
    pub next_retry_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryCaptureCheckpoint<'a> {
    pub scratch: ImportInventoryScratchState<'a>,
    pub active_directory: Option<ImportInventoryActiveDirectoryProof<'a>>,
    pub discovery_complete: bool,
    pub effects_complete: bool,
    pub directory_queue_empty: bool,
    pub directory_count: u64,
    pub completed_directory_count: u64,
    pub discovered_path_count: u64,
    pub planned_path_count: u64,
    pub selection_keyset: Option<&'a [u8]>,
    pub selection_eof: bool,
    pub selection_complete: bool,
    pub selection_commitment: Option<ImportInventoryFrozenSelectionCommitment>,
    pub replay_count: u64,
    pub next_retry_at_ms: Option<i64>,
    pub last_error: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryCheckpointLease {
    pub run_id: Vec<u8>,
    pub inventory_family: ProviderFileInventoryFamily,
    pub provider: CaptureProvider,
    pub source_root: String,
    pub inventory_generation: u64,
    pub owner_id: String,
    pub owner_epoch: u64,
    pub owner_token: Vec<u8>,
    pub lease_expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryCheckpointAcquisition {
    pub lease: ImportInventoryCheckpointLease,
    pub requires_scratch_adoption: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryCheckpointRecovery {
    pub run_id: Vec<u8>,
    pub inventory_family: ProviderFileInventoryFamily,
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_identity: Vec<u8>,
    pub source_fingerprint: Vec<u8>,
    pub root_path: ImportInventoryOwnedPathIdentity,
    pub inventory_generation: u64,
    pub checkpoint_format_version: u32,
    pub producer_build_id: Vec<u8>,
    pub store_schema_version: u32,
    pub scratch_identity: Vec<u8>,
    pub scratch_integrity: Vec<u8>,
    pub scratch_lock_identity: Vec<u8>,
    pub scratch_database_identity: Vec<u8>,
    pub publication_state_marker: String,
    pub publication_owner: Option<ProviderFilePublicationInventoryOwner>,
}

impl ImportInventoryCheckpointRecovery {
    pub fn trust(&self) -> ImportInventoryCheckpointTrust<'_> {
        ImportInventoryCheckpointTrust {
            run_id: &self.run_id,
            inventory_family: self.inventory_family,
            provider: self.provider,
            source_format: &self.source_format,
            source_root: &self.source_root,
            source_identity: &self.source_identity,
            source_fingerprint: &self.source_fingerprint,
            root_path: ImportInventoryNativePathIdentity {
                platform_tag: &self.root_path.platform_tag,
                encoding_tag: &self.root_path.encoding_tag,
                opaque_hash: &self.root_path.opaque_hash,
            },
            inventory_generation: self.inventory_generation,
            checkpoint_format_version: self.checkpoint_format_version,
            producer_build_id: &self.producer_build_id,
            store_schema_version: self.store_schema_version,
            scratch_identity: &self.scratch_identity,
            scratch_lock_identity: &self.scratch_lock_identity,
            scratch_database_identity: &self.scratch_database_identity,
            publication_state_marker: &self.publication_state_marker,
            publication_owner: self.publication_owner.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryPathEffectRequest<'a> {
    pub scratch: ImportInventoryScratchState<'a>,
    pub capture_journal_identity: &'a [u8; 32],
    pub native_path: ImportInventoryNativePathIdentity<'a>,
    pub membership: ImportInventoryEffectMembership,
    pub accounted_bytes: u64,
    pub effect: ImportInventoryCanonicalEffect<'a>,
}

#[derive(Debug, Clone, Copy)]
pub enum ImportInventoryCanonicalEffect<'a> {
    CatalogUpsert(&'a CatalogSession),
    SourceImportUpsert(&'a SourceImportFile),
    CatalogStale {
        source_path: &'a str,
        observed_at_ms: i64,
    },
    SourceImportStale {
        source_path: &'a str,
        observed_at_ms: i64,
    },
    CatalogRescan {
        source_path: &'a str,
    },
    SourceImportRescan {
        source_path: &'a str,
    },
    CatalogObservationRejected {
        source_path: &'a str,
    },
    SourceImportObservationRejected {
        source_path: &'a str,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportInventoryEffectCounters {
    pub affected_rows: u64,
    pub affected_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImportInventoryPathEffectOutcome {
    Applied(ImportInventoryEffectCounters),
    AlreadyApplied(ImportInventoryEffectCounters),
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryCheckpointCompletionProof<'a> {
    pub capture: ImportInventoryCaptureCheckpoint<'a>,
    pub applied_path_count: u64,
    pub applied_row_count: u64,
    pub applied_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryCheckpointCleanupProof {
    pub checkpoint_format_version: u32,
    pub producer_build_id: Vec<u8>,
    pub store_schema_version: u32,
    pub run_id: Vec<u8>,
    pub inventory_family: ProviderFileInventoryFamily,
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_identity: Vec<u8>,
    pub source_fingerprint: Vec<u8>,
    pub root_path: ImportInventoryOwnedPathIdentity,
    pub inventory_generation: u64,
    pub scratch_identity: Vec<u8>,
    pub scratch_integrity: Vec<u8>,
    pub scratch_lock_identity: Vec<u8>,
    pub scratch_database_identity: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportInventoryCleanupDisposition {
    Pending,
    Complete,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryCleanupAdvance<'a> {
    pub expected_cleanup_keyset: Option<&'a [u8]>,
    pub cleanup_keyset: Option<&'a [u8]>,
    pub visited_rows_delta: u64,
    pub cleaned_rows_delta: u64,
    pub cleaned_bytes_delta: u64,
    pub disposition: ImportInventoryCleanupDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryCleanupProgress {
    pub disposition: ImportInventoryCleanupDisposition,
    pub cleanup_keyset: Option<Vec<u8>>,
    pub visited_rows: u64,
    pub cleaned_rows: u64,
    pub cleaned_bytes: u64,
    pub attempt_count: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ImportInventoryStoreReconciliationBudget {
    pub max_rows: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryStoreReconciliationProgress {
    pub complete: bool,
    pub keyset: Option<i64>,
    pub visited_rows: u64,
    pub stale_rows: u64,
    pub visited_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryActiveDirectoryStatus {
    pub path: ImportInventoryOwnedPathIdentity,
    pub directory_identity: Vec<u8>,
    pub directory_fingerprint: Vec<u8>,
    pub attempt_count: u64,
    pub replay_count: u64,
    pub observed_entries: u64,
    pub next_retry_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInventoryCheckpointStatus {
    pub status: String,
    pub phase: String,
    pub owner_state: String,
    pub owner_epoch: u64,
    pub lease_owner_id: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub active_directory: Option<ImportInventoryActiveDirectoryStatus>,
    pub application_keyset: Option<Vec<u8>>,
    pub application_ordinal: u64,
    pub application_prefix: [u8; 32],
    pub discovery_complete: bool,
    pub effects_complete: bool,
    pub directory_queue_empty: bool,
    pub directory_count: u64,
    pub completed_directory_count: u64,
    pub discovered_path_count: u64,
    pub planned_path_count: u64,
    pub selection_keyset: Option<Vec<u8>>,
    pub selection_eof: bool,
    pub selection_complete: bool,
    pub selection_commitment: Option<ImportInventoryFrozenSelectionCommitment>,
    pub selection_commitment_identity: Option<[u8; 32]>,
    pub applied_path_count: u64,
    pub applied_row_count: u64,
    pub applied_bytes: u64,
    pub attempt_count: u64,
    pub replay_count: u64,
    pub next_retry_at_ms: Option<i64>,
    pub last_error: Option<String>,
    pub abandon_reason: Option<String>,
    pub cleanup_status: String,
    pub cleanup_keyset: Option<Vec<u8>>,
    pub cleanup_visited_row_count: u64,
    pub cleanup_row_count: u64,
    pub cleanup_bytes: u64,
    pub cleanup_attempt_count: u64,
    pub store_reconciliation_complete: bool,
    pub store_reconciliation_keyset: Option<i64>,
    pub store_reconciliation_visited_rows: u64,
    pub store_reconciliation_stale_rows: u64,
    pub store_reconciliation_visited_bytes: u64,
    pub scratch_identity: Vec<u8>,
    pub scratch_integrity: Vec<u8>,
    pub scratch_lock_identity: Vec<u8>,
    pub scratch_database_identity: Vec<u8>,
}

enum CheckpointCommit<T> {
    Value(T),
    Failure(StoreError),
}

struct TrustedScratch<'a> {
    identity: &'a [u8],
    integrity: &'a [u8],
    lock_identity: &'a [u8],
    database_identity: &'a [u8],
    owner: Option<ImportInventoryScratchOwner<'a>>,
}

struct CheckpointRow {
    source_format: String,
    source_identity: Vec<u8>,
    source_fingerprint: Vec<u8>,
    root_path: ImportInventoryOwnedPathIdentity,
    inventory_generation: u64,
    scratch_identity: Vec<u8>,
    scratch_lock_identity: Vec<u8>,
    scratch_database_identity: Vec<u8>,
    status: String,
    discovery_complete: bool,
    effects_complete: bool,
    directory_queue_empty: bool,
    owner_epoch: u64,
    owner_token: Option<Vec<u8>>,
    owner_state: String,
    scratch_owner_epoch: Option<u64>,
    scratch_owner_token: Option<Vec<u8>>,
    lease_owner_id: Option<String>,
    lease_expires_at_ms: Option<i64>,
    active_directory: Option<ImportInventoryActiveDirectoryStatus>,
    directory_count: u64,
    completed_directory_count: u64,
    discovered_path_count: u64,
    planned_path_count: u64,
    applied_path_count: u64,
    applied_row_count: u64,
    applied_bytes: u64,
    attempt_count: u64,
    replay_count: u64,
    selection_keyset: Option<Vec<u8>>,
    selection_eof: bool,
    selection_complete: bool,
    selection_commitment: Option<ImportInventoryFrozenSelectionCommitment>,
    selection_commitment_identity: Option<[u8; 32]>,
    application_ordinal: u64,
    application_keyset: Option<Vec<u8>>,
    application_prefix: [u8; 32],
    run_checkpoint_format_version: u32,
    run_producer_build_id: Vec<u8>,
    run_store_schema_version: u32,
    run_publication_state_marker: String,
    run_publication_owner: Option<ProviderFilePublicationInventoryOwner>,
    run_status: String,
    current_generation: Option<u64>,
    store_reconciliation_complete: bool,
    store_reconciliation_keyset: i64,
    store_reconciliation_visited_rows: u64,
    store_reconciliation_stale_rows: u64,
    store_reconciliation_visited_bytes: u64,
}

pub fn import_inventory_selection_initial_prefix(
    format_version: u32,
    algorithm_version: u32,
) -> Result<[u8; 32]> {
    validate_import_inventory_selection_versions(format_version, algorithm_version)?;
    let mut digest = Sha256::new();
    hash_inventory_field(&mut digest, IMPORT_INVENTORY_SELECTION_PREFIX_DOMAIN);
    hash_inventory_field(&mut digest, &format_version.to_be_bytes());
    hash_inventory_field(&mut digest, &algorithm_version.to_be_bytes());
    Ok(digest.finalize().into())
}

pub fn import_inventory_selection_commitment_identity(
    commitment: ImportInventoryFrozenSelectionCommitment,
) -> Result<[u8; 32]> {
    validate_import_inventory_selection_commitment(commitment)?;
    let mut digest = Sha256::new();
    hash_inventory_field(&mut digest, IMPORT_INVENTORY_SELECTION_COMMITMENT_DOMAIN);
    hash_inventory_field(&mut digest, &commitment.format_version.to_be_bytes());
    hash_inventory_field(&mut digest, &commitment.algorithm_version.to_be_bytes());
    hash_inventory_field(&mut digest, &commitment.total_count.to_be_bytes());
    hash_inventory_optional_field(
        &mut digest,
        commitment.final_keyset.as_ref().map(<[u8; 32]>::as_slice),
    );
    hash_inventory_field(&mut digest, &commitment.final_prefix);
    Ok(digest.finalize().into())
}

pub fn canonical_import_inventory_selection_step(
    request: ImportInventorySelectionCanonicalizationRequest<'_>,
) -> Result<ImportInventorySelectionCanonicalization> {
    validate_import_inventory_selection_versions(
        request.format_version,
        request.algorithm_version,
    )?;
    validate_native_path(request.native_path)?;
    if request.native_path.opaque_hash.len() != 32 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection native path hash must be SHA-256",
        ));
    }
    if request.capture_journal_identity != request.resulting_keyset {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection resulting keyset must equal the capture journal identity",
        ));
    }
    if (request.ordinal == 0) != request.prior_keyset.is_none() {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection prior keyset does not match its ordinal",
        ));
    }
    if request.ordinal == 0
        && *request.prior_prefix
            != import_inventory_selection_initial_prefix(
                request.format_version,
                request.algorithm_version,
            )?
    {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection initial prefix is invalid",
        ));
    }
    if request.source_format.is_empty()
        || request.source_format.len() > 256
        || request.source_root.is_empty()
        || request.source_root.len() > 32768
    {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection canonical source scope is invalid",
        ));
    }
    if request.accounted_bytes > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64 {
        return Err(StoreError::ImportInventoryCheckpointPageTooLarge {
            max_bytes: IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES,
        });
    }
    validate_canonical_effect_payload_size(request.effect)?;
    validate_canonical_effect_checkpoint_scope(
        request.inventory_family,
        request.provider,
        request.source_format,
        request.source_root,
        request.effect,
    )?;
    let (effect_kind, source_path) = canonical_effect_identity(request.effect);
    if source_path.is_empty() || source_path.len() > 32768 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "canonical source path length is invalid",
        ));
    }
    let payload_fingerprint = canonical_import_inventory_effect_fingerprint(request.effect);
    let mut member = Sha256::new();
    hash_inventory_field(&mut member, IMPORT_INVENTORY_SELECTION_MEMBER_DOMAIN);
    hash_inventory_field(&mut member, &request.format_version.to_be_bytes());
    hash_inventory_field(&mut member, &request.algorithm_version.to_be_bytes());
    hash_inventory_field(&mut member, &request.ordinal.to_be_bytes());
    hash_inventory_field(&mut member, request.capture_journal_identity);
    hash_inventory_field(&mut member, request.native_path.platform_tag.as_bytes());
    hash_inventory_field(&mut member, request.native_path.encoding_tag.as_bytes());
    hash_inventory_field(&mut member, request.native_path.opaque_hash);
    hash_inventory_field(
        &mut member,
        checkpoint_inventory_family_str(request.inventory_family).as_bytes(),
    );
    hash_inventory_field(&mut member, request.provider.as_str().as_bytes());
    hash_inventory_field(&mut member, request.source_format.as_bytes());
    hash_inventory_field(&mut member, request.source_root.as_bytes());
    hash_inventory_field(&mut member, source_path.as_bytes());
    hash_inventory_field(&mut member, effect_kind.as_bytes());
    hash_inventory_field(&mut member, &payload_fingerprint);
    hash_inventory_field(&mut member, &request.accounted_bytes.to_be_bytes());
    hash_inventory_optional_field(&mut member, request.prior_keyset.map(<[u8; 32]>::as_slice));
    hash_inventory_field(&mut member, request.resulting_keyset);
    let member_digest: [u8; 32] = member.finalize().into();

    let mut prefix = Sha256::new();
    hash_inventory_field(&mut prefix, IMPORT_INVENTORY_SELECTION_PREFIX_DOMAIN);
    hash_inventory_field(&mut prefix, &request.format_version.to_be_bytes());
    hash_inventory_field(&mut prefix, &request.algorithm_version.to_be_bytes());
    hash_inventory_field(&mut prefix, request.prior_prefix);
    hash_inventory_field(&mut prefix, &member_digest);
    Ok(ImportInventorySelectionCanonicalization {
        payload_fingerprint,
        member_digest,
        resulting_prefix: prefix.finalize().into(),
    })
}

impl Store {
    pub fn start_import_inventory_checkpoint(
        &self,
        trust: ImportInventoryCheckpointTrust<'_>,
        capture: ImportInventoryCaptureCheckpoint<'_>,
        owner_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<ImportInventoryCheckpointAcquisition> {
        validate_checkpoint_trust_input(&trust, owner_id, now_ms, lease_expires_at_ms)?;
        validate_capture_checkpoint(capture)?;
        validate_new_capture_checkpoint(capture)?;
        let scratch = trusted_scratch(capture.scratch)?;
        validate_stable_scratch(&trust, &scratch)?;
        if scratch.owner.is_some() {
            return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
                field: "new scratch owner",
            });
        }
        let owner_token = new_checkpoint_owner_token();
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            self.validate_current_import_inventory_publication_snapshot(
                trust.publication_state_marker,
                trust.publication_owner,
            )?;
            if self.current_import_inventory_generation_for_checkpoint(
                trust.provider,
                trust.source_root,
                trust.inventory_family,
            )? != Some(trust.inventory_generation)
            {
                return Err(StoreError::ImportInventoryCheckpointGenerationMismatch);
            }
            let source_checkpoint_exists = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM import_inventory_checkpoints \
                 WHERE inventory_family = ?1 AND provider = ?2 AND source_root = ?3 \
                   AND inventory_generation = ?4)",
                params![
                    checkpoint_inventory_family_str(trust.inventory_family),
                    trust.provider.as_str(),
                    trust.source_root,
                    checkpoint_i64(trust.inventory_generation)?,
                ],
                |row| row.get::<_, bool>(0),
            )?;
            if source_checkpoint_exists {
                return Err(StoreError::ImportInventoryCheckpointInvariant(
                    "source generation already has a durable checkpoint",
                ));
            }
            let run = self
                .conn
                .query_row(
                    "SELECT checkpoint_format_version, producer_build_id, \
                            store_schema_version, status, publication_state_marker, \
                            publication_owner_present, publication_provider, \
                            publication_inventory_family, publication_source_format, \
                            publication_source_root, publication_source_path, \
                            publication_inventory_generation, publication_file_size_bytes, \
                            publication_file_modified_at_ms, publication_import_revision, \
                            publication_metadata_json \
                     FROM import_inventory_runs WHERE run_id = ?1",
                    [trust.run_id],
                    |row| {
                        Ok((
                            nonnegative_i64_to_u32(row.get(0)?)?,
                            row.get::<_, Vec<u8>>(1)?,
                            nonnegative_i64_to_u32(row.get(2)?)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            decode_import_inventory_publication_owner(row, 5)?,
                        ))
                    },
                )
                .optional()?;
            match run {
                Some((format, build, schema, status, publication_marker, publication_owner)) => {
                    if format != trust.checkpoint_format_version
                        || build != trust.producer_build_id
                        || schema != trust.store_schema_version
                    {
                        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
                            field: "run format or build",
                        });
                    }
                    if publication_marker != trust.publication_state_marker
                        || publication_owner.as_ref() != trust.publication_owner
                    {
                        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
                            field: "run publication inventory snapshot",
                        });
                    }
                    if status != "active" {
                        return Err(StoreError::ImportInventoryCheckpointInvariant(
                            "inventory run is not active",
                        ));
                    }
                }
                None => {
                    self.conn.execute(
                        "INSERT INTO import_inventory_runs (\
                           run_id, checkpoint_format_version, producer_build_id, \
                           store_schema_version, publication_state_marker, \
                           publication_owner_present, publication_provider, \
                           publication_inventory_family, publication_source_format, \
                           publication_source_root, publication_source_path, \
                           publication_inventory_generation, publication_file_size_bytes, \
                           publication_file_modified_at_ms, publication_import_revision, \
                           publication_metadata_json, created_at_ms, updated_at_ms\
                         ) VALUES (\
                           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, \
                           ?12, ?13, ?14, ?15, ?16, ?17, ?17\
                         )",
                        params![
                            trust.run_id,
                            i64::from(trust.checkpoint_format_version),
                            trust.producer_build_id,
                            i64::from(trust.store_schema_version),
                            trust.publication_state_marker,
                            trust.publication_owner.is_some(),
                            trust.publication_owner.map(|owner| owner.provider.as_str()),
                            trust.publication_owner.map(|owner| {
                                checkpoint_inventory_family_str(owner.inventory_family)
                            }),
                            trust
                                .publication_owner
                                .map(|owner| owner.source_format.as_str()),
                            trust
                                .publication_owner
                                .map(|owner| owner.source_root.as_str()),
                            trust
                                .publication_owner
                                .map(|owner| owner.source_path.as_str()),
                            trust
                                .publication_owner
                                .map(|owner| checkpoint_i64(owner.inventory_generation))
                                .transpose()?,
                            trust
                                .publication_owner
                                .map(|owner| checkpoint_i64(owner.file_size_bytes))
                                .transpose()?,
                            trust
                                .publication_owner
                                .map(|owner| owner.file_modified_at_ms),
                            trust
                                .publication_owner
                                .map(|owner| i64::from(owner.import_revision)),
                            trust
                                .publication_owner
                                .and_then(|owner| owner.metadata_json.as_deref()),
                            now_ms,
                        ],
                    )?;
                }
            }
            let active = capture.active_directory;
            let changed = self.conn.execute(
                r#"
                INSERT INTO import_inventory_checkpoints (
                  run_id, inventory_family, provider, source_format, source_root,
                  source_identity, source_fingerprint,
                  root_platform_tag, root_encoding_tag, root_path_hash,
                  inventory_generation, scratch_identity, scratch_integrity,
                  scratch_lock_identity, phase, discovery_complete,
                  application_complete, directory_queue_empty,
                  owner_epoch, owner_token, owner_state, lease_owner_id,
                  lease_expires_at_ms, active_directory_platform_tag,
                  active_directory_encoding_tag, active_directory_path_hash,
                  active_directory_identity, active_directory_fingerprint,
                  active_directory_attempt_count, active_directory_replay_count,
                  active_directory_next_retry_at_ms, directory_count,
                  completed_directory_count, planned_path_count, replay_count, next_retry_at_ms,
                  last_error, active_directory_observed_entries, discovered_path_count,
                  attempt_count, scratch_database_identity, selection_keyset,
                  selection_eof, selection_complete, application_prefix,
                  selection_format_version, selection_algorithm_version,
                  selection_total_count, selection_final_keyset,
                  selection_final_prefix, selection_commitment_identity,
                  created_at_ms, updated_at_ms
                ) VALUES (
                  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                  ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                  1, ?19, 'awaiting_scratch_adoption', ?20, ?21,
                  ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29,
                  ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38,
                  ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, ?50, ?50
                )
                "#,
                params![
                    trust.run_id,
                    checkpoint_inventory_family_str(trust.inventory_family),
                    trust.provider.as_str(),
                    trust.source_format,
                    trust.source_root,
                    trust.source_identity,
                    trust.source_fingerprint,
                    trust.root_path.platform_tag,
                    trust.root_path.encoding_tag,
                    trust.root_path.opaque_hash,
                    checkpoint_i64(trust.inventory_generation)?,
                    scratch.identity,
                    scratch.integrity,
                    scratch.lock_identity,
                    capture_phase(capture),
                    capture.discovery_complete,
                    capture.effects_complete,
                    capture.directory_queue_empty,
                    &owner_token,
                    owner_id,
                    lease_expires_at_ms,
                    active.map(|value| value.path.platform_tag),
                    active.map(|value| value.path.encoding_tag),
                    active.map(|value| value.path.opaque_hash),
                    active.map(|value| value.directory_identity),
                    active.map(|value| value.directory_fingerprint),
                    active
                        .map(|value| checkpoint_i64(value.attempt_count))
                        .transpose()?,
                    active
                        .map(|value| checkpoint_i64(value.replay_count))
                        .transpose()?,
                    active.and_then(|value| value.next_retry_at_ms),
                    checkpoint_i64(capture.directory_count)?,
                    checkpoint_i64(capture.completed_directory_count)?,
                    checkpoint_i64(capture.planned_path_count)?,
                    checkpoint_i64(capture.replay_count)?,
                    capture.next_retry_at_ms,
                    capture.last_error,
                    active
                        .map(|value| checkpoint_i64(value.observed_entries))
                        .transpose()?,
                    checkpoint_i64(capture.discovered_path_count)?,
                    active
                        .map(|value| checkpoint_i64(value.attempt_count))
                        .transpose()?
                        .unwrap_or(0),
                    scratch.database_identity,
                    capture.selection_keyset,
                    capture.selection_eof,
                    capture.selection_complete,
                    import_inventory_selection_initial_prefix(
                        IMPORT_INVENTORY_SELECTION_FORMAT_VERSION,
                        IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION,
                    )?,
                    capture
                        .selection_commitment
                        .map(|commitment| i64::from(commitment.format_version)),
                    capture
                        .selection_commitment
                        .map(|commitment| i64::from(commitment.algorithm_version)),
                    capture
                        .selection_commitment
                        .map(|commitment| checkpoint_i64(commitment.total_count))
                        .transpose()?,
                    capture
                        .selection_commitment
                        .and_then(|commitment| commitment.final_keyset),
                    capture
                        .selection_commitment
                        .map(|commitment| commitment.final_prefix),
                    capture
                        .selection_commitment
                        .map(import_inventory_selection_commitment_identity)
                        .transpose()?,
                    now_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointInvariant(
                    "checkpoint already exists",
                ));
            }
            let changed = self.conn.execute(
                "UPDATE import_inventory_runs \
                 SET source_count = source_count + 1, updated_at_ms = ?2 \
                 WHERE run_id = ?1 AND status = 'active'",
                params![trust.run_id, now_ms],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointInvariant(
                    "inventory run stopped during checkpoint creation",
                ));
            }
            Ok(ImportInventoryCheckpointAcquisition {
                lease: lease_from_trust(
                    trust,
                    owner_id,
                    1,
                    owner_token.clone(),
                    lease_expires_at_ms,
                ),
                requires_scratch_adoption: true,
            })
        })
    }

    pub fn acquire_import_inventory_checkpoint(
        &self,
        trust: ImportInventoryCheckpointTrust<'_>,
        capture: ImportInventoryCaptureCheckpoint<'_>,
        owner_id: &str,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<ImportInventoryCheckpointAcquisition> {
        validate_checkpoint_trust_input(&trust, owner_id, now_ms, lease_expires_at_ms)?;
        let next_token = new_checkpoint_owner_token();
        let committed = self.with_inventory_checkpoint_transaction(
            CHECKPOINT_WRITE_TIMEOUT,
            || -> Result<CheckpointCommit<ImportInventoryCheckpointAcquisition>> {
                let Some(row) = self.load_import_inventory_checkpoint(&trust)? else {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointNotFound,
                    ));
                };
                if row.status != "active" || row.run_status != "active" {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointInvariant("checkpoint is not active"),
                    ));
                }
                if let (Some(current_owner), Some(expires_at)) =
                    (row.lease_owner_id.as_deref(), row.lease_expires_at_ms)
                {
                    if current_owner != owner_id && expires_at > now_ms {
                        return Ok(CheckpointCommit::Failure(
                            StoreError::ImportInventoryCheckpointBusy {
                                owner_id: current_owner.to_owned(),
                            },
                        ));
                    }
                }
                if let Some(error) = checkpoint_trust_error(&row, &trust, true) {
                    self.abandon_import_inventory_checkpoint_inner(
                        &trust,
                        now_ms,
                        &error.to_string(),
                        false,
                    )?;
                    return Ok(CheckpointCommit::Failure(error));
                }
                if let Err(error) = self.validate_current_import_inventory_publication_snapshot(
                    &row.run_publication_state_marker,
                    row.run_publication_owner.as_ref(),
                ) {
                    self.abandon_import_inventory_checkpoint_inner(
                        &trust,
                        now_ms,
                        &error.to_string(),
                        false,
                    )?;
                    return Ok(CheckpointCommit::Failure(error));
                }
                if let Err(error) = validate_capture_checkpoint_shape(capture) {
                    self.abandon_import_inventory_checkpoint_inner(
                        &trust,
                        now_ms,
                        &error.to_string(),
                        true,
                    )?;
                    return Ok(CheckpointCommit::Failure(error));
                }
                let scratch = match trusted_scratch(capture.scratch) {
                    Ok(scratch) => scratch,
                    Err(error) => {
                        self.abandon_import_inventory_checkpoint_inner(
                            &trust,
                            now_ms,
                            &error.to_string(),
                            true,
                        )?;
                        return Ok(CheckpointCommit::Failure(error));
                    }
                };
                if let Err(error) = validate_scratch_for_acquisition(&row, &scratch) {
                    self.abandon_import_inventory_checkpoint_inner(
                        &trust,
                        now_ms,
                        &error.to_string(),
                        true,
                    )?;
                    return Ok(CheckpointCommit::Failure(error));
                }
                if let Err(error) = validate_active_directory_scratch(capture, &scratch) {
                    self.abandon_import_inventory_checkpoint_inner(
                        &trust,
                        now_ms,
                        &error.to_string(),
                        true,
                    )?;
                    return Ok(CheckpointCommit::Failure(error));
                }
                if let Err(error) = validate_capture_progress(&row, capture) {
                    self.abandon_import_inventory_checkpoint_inner(
                        &trust,
                        now_ms,
                        &error.to_string(),
                        true,
                    )?;
                    return Ok(CheckpointCommit::Failure(error));
                }
                let next_epoch = row.owner_epoch.checked_add(1).ok_or(
                    StoreError::InvalidImportInventoryCheckpoint("owner epoch overflow"),
                )?;
                let active = capture.active_directory;
                let attempt_count = match advanced_import_inventory_attempt_count(&row, capture) {
                    Ok(attempt_count) => attempt_count,
                    Err(error) => {
                        self.abandon_import_inventory_checkpoint_inner(
                            &trust,
                            now_ms,
                            &error.to_string(),
                            true,
                        )?;
                        return Ok(CheckpointCommit::Failure(error));
                    }
                };
                let scratch_owner = scratch.owner;
                let changed = self.conn.execute(
                    "UPDATE import_inventory_checkpoints SET \
                       scratch_integrity = ?5, scratch_owner_epoch = ?6, \
                       scratch_owner_token = ?7, owner_epoch = ?8, owner_token = ?9, \
                       owner_state = 'awaiting_scratch_adoption', lease_owner_id = ?10, \
                       lease_expires_at_ms = ?11, phase = ?12, discovery_complete = ?13, \
                       application_complete = ?14, directory_queue_empty = ?15, \
                       active_directory_platform_tag = ?16, \
                       active_directory_encoding_tag = ?17, active_directory_path_hash = ?18, \
                       active_directory_identity = ?19, active_directory_fingerprint = ?20, \
                       active_directory_attempt_count = ?21, \
                       active_directory_replay_count = ?22, \
                       active_directory_next_retry_at_ms = ?23, directory_count = ?24, \
                       completed_directory_count = ?25, planned_path_count = ?26, \
                       replay_count = ?27, next_retry_at_ms = ?28, last_error = ?29, \
                       active_directory_observed_entries = ?30, \
                       discovered_path_count = ?31, attempt_count = ?32, \
                       scratch_database_identity = ?33, selection_keyset = ?34, \
                       selection_eof = ?35, selection_complete = ?36, \
                       selection_format_version = ?37, selection_algorithm_version = ?38, \
                       selection_total_count = ?39, selection_final_keyset = ?40, \
                       selection_final_prefix = ?41, selection_commitment_identity = ?42, \
                       updated_at_ms = ?43 \
                     WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                       AND source_root = ?4 AND owner_epoch = ?44 AND owner_token IS ?45 \
                       AND status = 'active'",
                    params![
                        trust.run_id,
                        checkpoint_inventory_family_str(trust.inventory_family),
                        trust.provider.as_str(),
                        trust.source_root,
                        scratch.integrity,
                        scratch_owner
                            .map(|owner| checkpoint_i64(owner.owner_epoch))
                            .transpose()?,
                        scratch_owner.map(|owner| owner.owner_token),
                        checkpoint_i64(next_epoch)?,
                        &next_token,
                        owner_id,
                        lease_expires_at_ms,
                        capture_phase(capture),
                        capture.discovery_complete,
                        capture.effects_complete,
                        capture.directory_queue_empty,
                        active.map(|value| value.path.platform_tag),
                        active.map(|value| value.path.encoding_tag),
                        active.map(|value| value.path.opaque_hash),
                        active.map(|value| value.directory_identity),
                        active.map(|value| value.directory_fingerprint),
                        active
                            .map(|value| checkpoint_i64(value.attempt_count))
                            .transpose()?,
                        active
                            .map(|value| checkpoint_i64(value.replay_count))
                            .transpose()?,
                        active.and_then(|value| value.next_retry_at_ms),
                        checkpoint_i64(capture.directory_count)?,
                        checkpoint_i64(capture.completed_directory_count)?,
                        checkpoint_i64(capture.planned_path_count)?,
                        checkpoint_i64(capture.replay_count)?,
                        capture.next_retry_at_ms,
                        capture.last_error,
                        active
                            .map(|value| checkpoint_i64(value.observed_entries))
                            .transpose()?,
                        checkpoint_i64(capture.discovered_path_count)?,
                        checkpoint_i64(attempt_count)?,
                        scratch.database_identity,
                        capture.selection_keyset,
                        capture.selection_eof,
                        capture.selection_complete,
                        capture
                            .selection_commitment
                            .map(|commitment| i64::from(commitment.format_version)),
                        capture
                            .selection_commitment
                            .map(|commitment| i64::from(commitment.algorithm_version)),
                        capture
                            .selection_commitment
                            .map(|commitment| checkpoint_i64(commitment.total_count))
                            .transpose()?,
                        capture
                            .selection_commitment
                            .and_then(|commitment| commitment.final_keyset),
                        capture
                            .selection_commitment
                            .map(|commitment| commitment.final_prefix),
                        capture
                            .selection_commitment
                            .map(import_inventory_selection_commitment_identity)
                            .transpose()?,
                        now_ms,
                        checkpoint_i64(row.owner_epoch)?,
                        row.owner_token.as_deref(),
                    ],
                )?;
                if changed != 1 {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointStaleAuthority,
                    ));
                }
                Ok(CheckpointCommit::Value(
                    ImportInventoryCheckpointAcquisition {
                        lease: lease_from_trust(
                            trust,
                            owner_id,
                            next_epoch,
                            next_token.clone(),
                            lease_expires_at_ms,
                        ),
                        requires_scratch_adoption: true,
                    },
                ))
            },
        )?;
        finish_checkpoint_commit(committed)
    }

    pub fn confirm_import_inventory_checkpoint_scratch_adoption(
        &self,
        lease: &ImportInventoryCheckpointLease,
        capture: ImportInventoryCaptureCheckpoint<'_>,
        now_ms: i64,
    ) -> Result<()> {
        validate_capture_checkpoint(capture)?;
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let row = self.validate_import_inventory_lease(lease, now_ms, "active")?;
            if row.owner_state != "awaiting_scratch_adoption" {
                return Err(StoreError::ImportInventoryCheckpointInvariant(
                    "scratch adoption is not pending",
                ));
            }
            let scratch = trusted_scratch(capture.scratch)?;
            validate_scratch_owned_by_lease(&row, lease, &scratch)?;
            validate_capture_progress(&row, capture)?;
            self.update_capture_checkpoint_summary(lease, &row, capture, "active", now_ms)
        })
    }

    pub fn record_import_inventory_capture_checkpoint(
        &self,
        lease: &ImportInventoryCheckpointLease,
        capture: ImportInventoryCaptureCheckpoint<'_>,
        now_ms: i64,
    ) -> Result<()> {
        validate_capture_checkpoint(capture)?;
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let row = self.validate_import_inventory_active_authority(
                lease,
                capture.scratch,
                now_ms,
                true,
            )?;
            validate_capture_progress(&row, capture)?;
            self.update_capture_checkpoint_summary(lease, &row, capture, "active", now_ms)
        })
    }

    pub fn apply_import_inventory_path_effect(
        &self,
        lease: &ImportInventoryCheckpointLease,
        request: ImportInventoryPathEffectRequest<'_>,
        now_ms: i64,
    ) -> Result<ImportInventoryPathEffectOutcome> {
        validate_native_path(request.native_path)?;
        if request.native_path.opaque_hash.len() != 32 {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "selection native path hash must be SHA-256",
            ));
        }
        if request.accounted_bytes > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64 {
            return Err(StoreError::ImportInventoryCheckpointPageTooLarge {
                max_bytes: IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES,
            });
        }
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let row = self.validate_import_inventory_active_authority(
                lease,
                request.scratch,
                now_ms,
                true,
            )?;
            self.validate_current_import_inventory_publication_snapshot(
                &row.run_publication_state_marker,
                row.run_publication_owner.as_ref(),
            )?;
            if !row.discovery_complete || !row.selection_eof || !row.selection_complete {
                return Err(StoreError::ImportInventoryCheckpointIncomplete(
                    "capture journal membership is not complete",
                ));
            }
            let commitment =
                row.selection_commitment
                    .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                        "frozen selection commitment is missing",
                    ))?;
            let commitment_identity = row.selection_commitment_identity.ok_or(
                StoreError::ImportInventoryCheckpointInvariant(
                    "frozen selection commitment identity is missing",
                ),
            )?;
            if request.membership.commitment_identity != commitment_identity {
                return Err(StoreError::ImportInventoryCheckpointIdempotenceConflict);
            }
            let canonical = canonical_import_inventory_selection_step(
                ImportInventorySelectionCanonicalizationRequest {
                    format_version: commitment.format_version,
                    algorithm_version: commitment.algorithm_version,
                    ordinal: request.membership.ordinal,
                    capture_journal_identity: request.capture_journal_identity,
                    native_path: request.native_path,
                    inventory_family: lease.inventory_family,
                    provider: lease.provider,
                    source_format: &row.source_format,
                    source_root: &lease.source_root,
                    prior_keyset: request.membership.prior_keyset.as_ref(),
                    resulting_keyset: &request.membership.resulting_keyset,
                    prior_prefix: &request.membership.prior_prefix,
                    accounted_bytes: request.accounted_bytes,
                    effect: request.effect,
                },
            )?;
            if canonical.resulting_prefix != request.membership.resulting_prefix {
                return Err(StoreError::ImportInventoryCheckpointIdempotenceConflict);
            }
            let (effect_kind, source_path) = canonical_effect_identity(request.effect);
            let scratch = trusted_scratch(request.scratch)?;
            let existing = self
                .conn
                .query_row(
                    "SELECT capture_journal_identity, source_path, effect_kind, \
                            selection_commitment_identity, selection_ordinal, \
                            prior_application_keyset, resulting_application_keyset, \
                            prior_application_prefix, resulting_application_prefix, \
                            payload_fingerprint, member_digest, affected_row_count, \
                            affected_bytes, prior_applied_row_count, \
                            resulting_applied_row_count, prior_applied_bytes, \
                            resulting_applied_bytes \
                     FROM import_inventory_path_effects \
                     WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                       AND source_root = ?4 AND inventory_generation = ?5 \
                       AND path_platform_tag = ?6 AND path_encoding_tag = ?7 \
                       AND native_path_hash = ?8",
                    params![
                        &lease.run_id,
                        checkpoint_inventory_family_str(lease.inventory_family),
                        lease.provider.as_str(),
                        &lease.source_root,
                        checkpoint_i64(lease.inventory_generation)?,
                        request.native_path.platform_tag,
                        request.native_path.encoding_tag,
                        request.native_path.opaque_hash,
                    ],
                    |result| {
                        Ok((
                            result.get::<_, Vec<u8>>(0)?,
                            result.get::<_, String>(1)?,
                            result.get::<_, String>(2)?,
                            result.get::<_, Vec<u8>>(3)?,
                            nonnegative_i64_to_u64(result.get(4)?)?,
                            result.get::<_, Option<Vec<u8>>>(5)?,
                            result.get::<_, Vec<u8>>(6)?,
                            result.get::<_, Vec<u8>>(7)?,
                            result.get::<_, Vec<u8>>(8)?,
                            result.get::<_, Vec<u8>>(9)?,
                            result.get::<_, Vec<u8>>(10)?,
                            nonnegative_i64_to_u64(result.get(11)?)?,
                            nonnegative_i64_to_u64(result.get(12)?)?,
                            nonnegative_i64_to_u64(result.get(13)?)?,
                            nonnegative_i64_to_u64(result.get(14)?)?,
                            nonnegative_i64_to_u64(result.get(15)?)?,
                            nonnegative_i64_to_u64(result.get(16)?)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((
                journal,
                stored_path,
                stored_kind,
                stored_commitment,
                stored_ordinal,
                stored_prior_keyset,
                stored_resulting_keyset,
                stored_prior_prefix,
                stored_resulting_prefix,
                stored_payload_fingerprint,
                stored_member_digest,
                affected_rows,
                affected_bytes,
                prior_rows,
                resulting_rows,
                prior_bytes,
                resulting_bytes,
            )) = existing
            {
                if journal != request.capture_journal_identity
                    || stored_path != source_path
                    || stored_kind != effect_kind
                    || stored_commitment != commitment_identity
                    || stored_ordinal != request.membership.ordinal
                    || stored_prior_keyset.as_deref()
                        != request
                            .membership
                            .prior_keyset
                            .as_ref()
                            .map(<[u8; 32]>::as_slice)
                    || stored_resulting_keyset != request.membership.resulting_keyset
                    || stored_prior_prefix != request.membership.prior_prefix
                    || stored_resulting_prefix != request.membership.resulting_prefix
                    || stored_payload_fingerprint != canonical.payload_fingerprint
                    || stored_member_digest != canonical.member_digest
                    || affected_bytes != request.accounted_bytes
                    || resulting_rows
                        != prior_rows.checked_add(affected_rows).ok_or(
                            StoreError::ImportInventoryCheckpointInvariant(
                                "stored effect row counter overflow",
                            ),
                        )?
                    || resulting_bytes
                        != prior_bytes.checked_add(affected_bytes).ok_or(
                            StoreError::ImportInventoryCheckpointInvariant(
                                "stored effect byte counter overflow",
                            ),
                        )?
                    || row.application_ordinal <= stored_ordinal
                    || row.applied_row_count < resulting_rows
                    || row.applied_bytes < resulting_bytes
                {
                    return Err(StoreError::ImportInventoryCheckpointIdempotenceConflict);
                }
                self.update_checkpoint_after_duplicate_effect(lease, scratch.integrity, now_ms)?;
                return Ok(ImportInventoryPathEffectOutcome::AlreadyApplied(
                    ImportInventoryEffectCounters {
                        affected_rows,
                        affected_bytes,
                    },
                ));
            }
            let journal_path = self
                .conn
                .query_row(
                    "SELECT path_platform_tag, path_encoding_tag, native_path_hash \
                     FROM import_inventory_path_effects \
                     WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                       AND source_root = ?4 AND inventory_generation = ?5 \
                       AND (capture_journal_identity = ?6 OR selection_ordinal = ?7)",
                    params![
                        &lease.run_id,
                        checkpoint_inventory_family_str(lease.inventory_family),
                        lease.provider.as_str(),
                        &lease.source_root,
                        checkpoint_i64(lease.inventory_generation)?,
                        request.capture_journal_identity,
                        checkpoint_i64(request.membership.ordinal)?,
                    ],
                    |result| {
                        Ok((
                            result.get::<_, String>(0)?,
                            result.get::<_, String>(1)?,
                            result.get::<_, Vec<u8>>(2)?,
                        ))
                    },
                )
                .optional()?;
            if journal_path.is_some() {
                return Err(StoreError::ImportInventoryCheckpointIdempotenceConflict);
            }
            if row.store_reconciliation_complete {
                return Err(StoreError::ImportInventoryCheckpointIdempotenceConflict);
            }
            if row.application_ordinal != request.membership.ordinal
                || row.applied_path_count != request.membership.ordinal
                || row.application_keyset.as_deref()
                    != request
                        .membership
                        .prior_keyset
                        .as_ref()
                        .map(<[u8; 32]>::as_slice)
                || row.application_prefix != request.membership.prior_prefix
            {
                return Err(StoreError::ImportInventoryCheckpointIdempotenceConflict);
            }
            if row.applied_path_count >= row.planned_path_count
                || row.application_ordinal >= commitment.total_count
            {
                return Err(StoreError::ImportInventoryCheckpointIncomplete(
                    "path effect exceeds capture's committed journal membership",
                ));
            }
            let affected_rows =
                self.apply_import_inventory_canonical_effect(lease, &row, request.effect, now_ms)?;
            let resulting_rows = row.applied_row_count.checked_add(affected_rows).ok_or(
                StoreError::ImportInventoryCheckpointInvariant(
                    "checkpoint applied row counter overflow",
                ),
            )?;
            let resulting_bytes = row
                .applied_bytes
                .checked_add(request.accounted_bytes)
                .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                    "checkpoint applied byte counter overflow",
                ))?;
            self.conn.execute(
                r#"
                INSERT INTO import_inventory_path_effects (
                  run_id, inventory_family, provider, source_root, inventory_generation,
                  capture_journal_identity, path_platform_tag, path_encoding_tag,
                  native_path_hash, source_path, effect_kind,
                  selection_commitment_identity, selection_ordinal,
                  prior_application_keyset, resulting_application_keyset,
                  prior_application_prefix, resulting_application_prefix,
                  payload_fingerprint, member_digest, owner_epoch,
                  prior_applied_row_count, resulting_applied_row_count,
                  prior_applied_bytes, resulting_applied_bytes,
                  affected_row_count, affected_bytes, applied_at_ms
                ) VALUES (
                  ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                  ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                  ?22, ?23, ?24, ?25, ?26, ?27
                )
                "#,
                params![
                    &lease.run_id,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_i64(lease.inventory_generation)?,
                    request.capture_journal_identity,
                    request.native_path.platform_tag,
                    request.native_path.encoding_tag,
                    request.native_path.opaque_hash,
                    source_path,
                    effect_kind,
                    commitment_identity,
                    checkpoint_i64(request.membership.ordinal)?,
                    request.membership.prior_keyset,
                    request.membership.resulting_keyset,
                    request.membership.prior_prefix,
                    request.membership.resulting_prefix,
                    canonical.payload_fingerprint,
                    canonical.member_digest,
                    checkpoint_i64(lease.owner_epoch)?,
                    checkpoint_i64(row.applied_row_count)?,
                    checkpoint_i64(resulting_rows)?,
                    checkpoint_i64(row.applied_bytes)?,
                    checkpoint_i64(resulting_bytes)?,
                    checkpoint_i64(affected_rows)?,
                    checkpoint_i64(request.accounted_bytes)?,
                    now_ms,
                ],
            )?;
            let changed = self.conn.execute(
                "UPDATE import_inventory_checkpoints SET scratch_integrity = ?8, \
                     application_ordinal = ?9, application_keyset = ?10, \
                     application_prefix = ?11, applied_path_count = ?9, \
                     applied_row_count = ?12, applied_bytes = ?13, updated_at_ms = ?14 \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
                   AND owner_state = 'active' AND lease_owner_id = ?7 \
                   AND selection_commitment_identity = ?15 \
                   AND application_ordinal = ?16 AND application_keyset IS ?17 \
                   AND application_prefix = ?18 AND applied_path_count = ?16 \
                   AND applied_row_count = ?19 AND applied_bytes = ?20 \
                   AND applied_path_count < planned_path_count",
                params![
                    &lease.run_id,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_i64(lease.owner_epoch)?,
                    &lease.owner_token,
                    &lease.owner_id,
                    scratch.integrity,
                    checkpoint_i64(request.membership.ordinal.checked_add(1).ok_or(
                        StoreError::ImportInventoryCheckpointInvariant(
                            "selection ordinal overflow",
                        ),
                    )?)?,
                    request.membership.resulting_keyset,
                    request.membership.resulting_prefix,
                    checkpoint_i64(resulting_rows)?,
                    checkpoint_i64(resulting_bytes)?,
                    now_ms,
                    commitment_identity,
                    checkpoint_i64(request.membership.ordinal)?,
                    request.membership.prior_keyset,
                    request.membership.prior_prefix,
                    checkpoint_i64(row.applied_row_count)?,
                    checkpoint_i64(row.applied_bytes)?,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
            }
            Ok(ImportInventoryPathEffectOutcome::Applied(
                ImportInventoryEffectCounters {
                    affected_rows,
                    affected_bytes: request.accounted_bytes,
                },
            ))
        })
    }

    pub fn reconcile_import_inventory_store_rows_page(
        &self,
        lease: &ImportInventoryCheckpointLease,
        scratch: ImportInventoryScratchState<'_>,
        budget: ImportInventoryStoreReconciliationBudget,
        now_ms: i64,
    ) -> Result<ImportInventoryStoreReconciliationProgress> {
        if budget.max_rows == 0
            || budget.max_rows > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS
            || budget.max_bytes == 0
            || budget.max_bytes > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES
        {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "store reconciliation budget is invalid",
            ));
        }
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let row =
                self.validate_import_inventory_active_authority(lease, scratch, now_ms, true)?;
            self.validate_current_import_inventory_publication_snapshot(
                &row.run_publication_state_marker,
                row.run_publication_owner.as_ref(),
            )?;
            let commitment =
                row.selection_commitment
                    .ok_or(StoreError::ImportInventoryCheckpointIncomplete(
                        "frozen selection commitment is missing",
                    ))?;
            if !row.discovery_complete
                || !row.selection_eof
                || !row.selection_complete
                || !row.effects_complete
                || row.application_ordinal != commitment.total_count
                || row.application_keyset.as_deref()
                    != commitment.final_keyset.as_ref().map(<[u8; 32]>::as_slice)
                || row.application_prefix != commitment.final_prefix
            {
                return Err(StoreError::ImportInventoryCheckpointIncomplete(
                    "capture effects must converge before store reconciliation",
                ));
            }
            if row.store_reconciliation_complete {
                return Ok(ImportInventoryStoreReconciliationProgress {
                    complete: true,
                    keyset: (row.store_reconciliation_keyset != 0)
                        .then_some(row.store_reconciliation_keyset),
                    visited_rows: row.store_reconciliation_visited_rows,
                    stale_rows: row.store_reconciliation_stale_rows,
                    visited_bytes: row.store_reconciliation_visited_bytes,
                });
            }
            let scan_sql = match lease.inventory_family {
                ProviderFileInventoryFamily::Catalog => {
                    "SELECT rowid, length(CAST(source_path AS BLOB)) + \
                            length(CAST(source_format AS BLOB)) + 64, source_format = ?4 \
                     FROM catalog_sessions \
                       INDEXED BY idx_catalog_sessions_provider_source_root_stale \
                     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 \
                       AND rowid > ?3 ORDER BY rowid LIMIT ?5"
                }
                ProviderFileInventoryFamily::SourceImport => {
                    "SELECT rowid, length(CAST(source_path AS BLOB)) + \
                            length(CAST(source_format AS BLOB)) + 64, source_format = ?4 \
                     FROM source_import_files \
                       INDEXED BY idx_source_import_files_provider_source_root_stale \
                     WHERE provider = ?1 AND source_root = ?2 AND is_stale = 0 \
                       AND rowid > ?3 ORDER BY rowid LIMIT ?5"
                }
            };
            let candidates = self
                .conn
                .prepare(scan_sql)?
                .query_map(
                    params![
                        lease.provider.as_str(),
                        &lease.source_root,
                        row.store_reconciliation_keyset,
                        &row.source_format,
                        checkpoint_i64(budget.max_rows as u64)?,
                    ],
                    |candidate| {
                        Ok((
                            candidate.get::<_, i64>(0)?,
                            candidate.get::<_, i64>(1)?,
                            candidate.get::<_, bool>(2)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut page_rows = 0u64;
            let mut page_bytes = 0u64;
            let mut page_stale_rows = 0u64;
            let mut next_keyset = row.store_reconciliation_keyset;
            for (candidate_rowid, candidate_bytes, matching_format) in &candidates {
                let candidate_bytes = nonnegative_i64_to_u64(*candidate_bytes)?;
                if page_bytes.saturating_add(candidate_bytes) > budget.max_bytes as u64 {
                    if page_rows == 0 {
                        return Err(StoreError::ImportInventoryCheckpointNoProgress);
                    }
                    break;
                }
                let update_sql = match lease.inventory_family {
                    ProviderFileInventoryFamily::Catalog => {
                        "UPDATE catalog_sessions AS inventory \
                         SET is_stale = 1, cataloged_at_ms = ?5 \
                         WHERE inventory.rowid = ?1 AND inventory.provider = ?2 \
                           AND inventory.source_format = ?3 AND inventory.source_root = ?4 \
                           AND inventory.is_stale = 0 AND NOT EXISTS (\
                             SELECT 1 FROM import_inventory_path_effects AS effect \
                             WHERE effect.run_id = ?6 AND effect.inventory_family = ?7 \
                               AND effect.provider = ?2 AND effect.source_root = ?4 \
                               AND effect.inventory_generation = ?8 \
                               AND effect.source_path = inventory.source_path\
                           )"
                    }
                    ProviderFileInventoryFamily::SourceImport => {
                        "UPDATE source_import_files AS inventory \
                         SET is_stale = 1, observed_at_ms = ?5 \
                         WHERE inventory.rowid = ?1 AND inventory.provider = ?2 \
                           AND inventory.source_format = ?3 AND inventory.source_root = ?4 \
                           AND inventory.is_stale = 0 AND NOT EXISTS (\
                             SELECT 1 FROM import_inventory_path_effects AS effect \
                             WHERE effect.run_id = ?6 AND effect.inventory_family = ?7 \
                               AND effect.provider = ?2 AND effect.source_root = ?4 \
                               AND effect.inventory_generation = ?8 \
                               AND effect.source_path = inventory.source_path\
                           )"
                    }
                };
                if *matching_format {
                    page_stale_rows = page_stale_rows
                        .checked_add(self.conn.execute(
                            update_sql,
                            params![
                                candidate_rowid,
                                lease.provider.as_str(),
                                &row.source_format,
                                &lease.source_root,
                                now_ms,
                                &lease.run_id,
                                checkpoint_inventory_family_str(lease.inventory_family),
                                checkpoint_i64(lease.inventory_generation)?,
                            ],
                        )? as u64)
                        .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                            "store reconciliation stale-row counter overflow",
                        ))?;
                }
                page_rows = page_rows.checked_add(1).ok_or(
                    StoreError::ImportInventoryCheckpointInvariant(
                        "store reconciliation visited-row counter overflow",
                    ),
                )?;
                page_bytes = page_bytes.checked_add(candidate_bytes).ok_or(
                    StoreError::ImportInventoryCheckpointInvariant(
                        "store reconciliation byte counter overflow",
                    ),
                )?;
                next_keyset = *candidate_rowid;
            }
            let complete =
                page_rows == candidates.len() as u64 && candidates.len() < budget.max_rows;
            let visited_rows = row
                .store_reconciliation_visited_rows
                .checked_add(page_rows)
                .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                    "store reconciliation visited-row counter overflow",
                ))?;
            let stale_rows = row
                .store_reconciliation_stale_rows
                .checked_add(page_stale_rows)
                .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                    "store reconciliation stale-row counter overflow",
                ))?;
            let visited_bytes = row
                .store_reconciliation_visited_bytes
                .checked_add(page_bytes)
                .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                    "store reconciliation byte counter overflow",
                ))?;
            let trusted = trusted_scratch(scratch)?;
            let changed = self.conn.execute(
                "UPDATE import_inventory_checkpoints SET scratch_integrity = ?8, \
                     store_reconciliation_keyset = ?9, \
                     store_reconciliation_complete = ?10, \
                     store_reconciliation_visited_rows = ?11, \
                     store_reconciliation_stale_rows = ?12, \
                     store_reconciliation_visited_bytes = ?13, \
                     phase = CASE WHEN ?10 THEN 'finalization' ELSE 'application' END, \
                     updated_at_ms = ?14 \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
                   AND lease_owner_id = ?7 AND owner_state = 'active' \
                   AND store_reconciliation_complete = 0 \
                   AND store_reconciliation_keyset = ?15",
                params![
                    &lease.run_id,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_i64(lease.owner_epoch)?,
                    &lease.owner_token,
                    &lease.owner_id,
                    trusted.integrity,
                    next_keyset,
                    complete,
                    checkpoint_i64(visited_rows)?,
                    checkpoint_i64(stale_rows)?,
                    checkpoint_i64(visited_bytes)?,
                    now_ms,
                    row.store_reconciliation_keyset,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
            }
            Ok(ImportInventoryStoreReconciliationProgress {
                complete,
                keyset: (next_keyset != 0).then_some(next_keyset),
                visited_rows,
                stale_rows,
                visited_bytes,
            })
        })
    }

    pub fn finalize_import_inventory_checkpoint(
        &self,
        lease: &ImportInventoryCheckpointLease,
        trust: ImportInventoryCheckpointTrust<'_>,
        proof: ImportInventoryCheckpointCompletionProof<'_>,
        now_ms: i64,
    ) -> Result<ImportInventoryCheckpointCleanupProof> {
        validate_checkpoint_trust(&trust)?;
        validate_capture_checkpoint(proof.capture)?;
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let row = self.validate_import_inventory_active_authority(
                lease,
                proof.capture.scratch,
                now_ms,
                true,
            )?;
            if let Some(error) = checkpoint_trust_error(&row, &trust, true) {
                return Err(error);
            }
            self.validate_current_import_inventory_publication_snapshot(
                &row.run_publication_state_marker,
                row.run_publication_owner.as_ref(),
            )?;
            validate_capture_progress(&row, proof.capture)?;
            let commitment =
                row.selection_commitment
                    .ok_or(StoreError::ImportInventoryCheckpointIncomplete(
                        "frozen selection commitment is missing",
                    ))?;
            if !proof.capture.discovery_complete
                || !proof.capture.selection_eof
                || !proof.capture.selection_complete
                || !proof.capture.effects_complete
                || !proof.capture.directory_queue_empty
                || proof.capture.active_directory.is_some()
                || proof.capture.completed_directory_count != proof.capture.directory_count
                || row.applied_path_count != proof.capture.planned_path_count
                || row.applied_path_count != proof.applied_path_count
                || row.applied_row_count != proof.applied_row_count
                || row.applied_bytes != proof.applied_bytes
                || row.application_ordinal != commitment.total_count
                || row.application_keyset.as_deref()
                    != commitment.final_keyset.as_ref().map(<[u8; 32]>::as_slice)
                || row.application_prefix != commitment.final_prefix
                || !row.store_reconciliation_complete
            {
                return Err(StoreError::ImportInventoryCheckpointIncomplete(
                    "capture completion proof or main-store counters are incomplete",
                ));
            }
            self.update_capture_checkpoint_summary(lease, &row, proof.capture, "active", now_ms)?;
            let changed = self.conn.execute(
                "UPDATE import_inventory_generations SET completed_generation = ?4 \
                 WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3 \
                   AND current_generation = ?4 AND completed_generation <= ?4",
                params![
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    checkpoint_i64(lease.inventory_generation)?,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointGenerationMismatch);
            }
            let changed = self.conn.execute(
                "UPDATE import_inventory_checkpoints SET status = 'completed', \
                     phase = 'complete', owner_token = NULL, owner_state = 'inactive', \
                     lease_owner_id = NULL, lease_expires_at_ms = NULL, updated_at_ms = ?8 \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
                   AND lease_owner_id = ?7 AND owner_state = 'active' \
                   AND active_directory_path_hash IS NULL AND discovery_complete = 1 \
                   AND selection_eof = 1 AND selection_complete = 1 \
                   AND application_complete = 1 AND directory_queue_empty = 1 \
                   AND application_ordinal = ?9 AND application_keyset IS ?10 \
                   AND application_prefix = ?11 AND selection_commitment_identity = ?12 \
                   AND store_reconciliation_complete = 1",
                params![
                    &lease.run_id,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_i64(lease.owner_epoch)?,
                    &lease.owner_token,
                    &lease.owner_id,
                    now_ms,
                    checkpoint_i64(commitment.total_count)?,
                    commitment.final_keyset,
                    commitment.final_prefix,
                    import_inventory_selection_commitment_identity(commitment)?,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
            }
            let changed = self.conn.execute(
                "UPDATE import_inventory_runs \
                 SET completed_source_count = completed_source_count + 1, \
                     status = CASE WHEN completed_source_count + 1 = source_count \
                       AND abandoned_source_count = 0 THEN 'completed' ELSE status END, \
                     updated_at_ms = ?2 WHERE run_id = ?1 AND status = 'active'",
                params![&lease.run_id, now_ms],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointInvariant(
                    "inventory run stopped before publication",
                ));
            }
            self.load_import_inventory_checkpoint_cleanup_proof_by_key(
                &lease.run_id,
                lease.inventory_family,
                lease.provider,
                &lease.source_root,
            )?
            .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                "completed checkpoint cleanup proof is unavailable",
            ))
        })
    }

    pub fn ensure_import_inventory_checkpoint_authority(
        &self,
        lease: &ImportInventoryCheckpointLease,
        scratch: ImportInventoryScratchState<'_>,
        now_ms: i64,
    ) -> Result<()> {
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            self.validate_import_inventory_active_authority(lease, scratch, now_ms, true)?;
            Ok(())
        })
    }

    pub fn renew_import_inventory_checkpoint_lease(
        &self,
        lease: &ImportInventoryCheckpointLease,
        scratch: ImportInventoryScratchState<'_>,
        now_ms: i64,
        lease_expires_at_ms: i64,
    ) -> Result<ImportInventoryCheckpointLease> {
        if lease_expires_at_ms <= now_ms {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "lease expiry must be in the future",
            ));
        }
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let trusted = trusted_scratch(scratch)?;
            self.validate_import_inventory_active_authority(lease, scratch, now_ms, true)?;
            let changed = self.conn.execute(
                "UPDATE import_inventory_checkpoints SET scratch_integrity = ?8, \
                     lease_expires_at_ms = ?9, updated_at_ms = ?10 \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
                   AND lease_owner_id = ?7 AND owner_state = 'active'",
                params![
                    &lease.run_id,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_i64(lease.owner_epoch)?,
                    &lease.owner_token,
                    &lease.owner_id,
                    trusted.integrity,
                    lease_expires_at_ms,
                    now_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
            }
            let mut renewed = lease.clone();
            renewed.lease_expires_at_ms = lease_expires_at_ms;
            Ok(renewed)
        })
    }

    pub fn abandon_import_inventory_checkpoint(
        &self,
        lease: &ImportInventoryCheckpointLease,
        scratch: ImportInventoryScratchState<'_>,
        reason: &str,
        now_ms: i64,
    ) -> Result<ImportInventoryCheckpointCleanupProof> {
        if reason.is_empty() || reason.len() > 4096 {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "abandon reason length is invalid",
            ));
        }
        self.with_inventory_checkpoint_transaction(CHECKPOINT_WRITE_TIMEOUT, || {
            let row = self.validate_import_inventory_lease(lease, now_ms, "active")?;
            if row.owner_state != "active" || row.run_status != "active" {
                return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
            }
            let (scratch_integrity, cleanup_blocked) = match trusted_scratch(scratch) {
                Ok(trusted) => {
                    validate_scratch_owned_by_lease(&row, lease, &trusted)?;
                    (Some(trusted.integrity), false)
                }
                Err(StoreError::ImportInventoryCheckpointScratchMissing) => (None, false),
                Err(
                    StoreError::ImportInventoryCheckpointScratchCorrupt
                    | StoreError::ImportInventoryCheckpointScratchTampered,
                ) => (None, true),
                Err(error) => return Err(error),
            };
            let changed = self.conn.execute(
                "UPDATE import_inventory_checkpoints SET status = 'abandoned', \
                     phase = 'abandoned', \
                     scratch_integrity = COALESCE(?8, scratch_integrity), owner_token = NULL, \
                     owner_state = 'inactive', lease_owner_id = NULL, \
                     lease_expires_at_ms = NULL, abandon_reason = ?9, last_error = ?9, \
                     cleanup_status = CASE WHEN ?10 THEN 'blocked' ELSE 'pending' END, \
                     updated_at_ms = ?11 \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
                   AND lease_owner_id = ?7 AND owner_state = 'active' AND status = 'active'",
                params![
                    &lease.run_id,
                    checkpoint_inventory_family_str(lease.inventory_family),
                    lease.provider.as_str(),
                    &lease.source_root,
                    checkpoint_i64(lease.owner_epoch)?,
                    &lease.owner_token,
                    &lease.owner_id,
                    scratch_integrity,
                    reason,
                    cleanup_blocked,
                    now_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
            }
            self.record_import_inventory_run_abandonment(&lease.run_id, reason, now_ms)?;
            self.load_import_inventory_checkpoint_cleanup_proof_by_key(
                &lease.run_id,
                lease.inventory_family,
                lease.provider,
                &lease.source_root,
            )?
            .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                "abandoned checkpoint cleanup proof is unavailable",
            ))
        })
    }

    pub fn advance_import_inventory_checkpoint_cleanup(
        &self,
        proof: &ImportInventoryCheckpointCleanupProof,
        advance: ImportInventoryCleanupAdvance<'_>,
        now_ms: i64,
    ) -> Result<ImportInventoryCleanupProgress> {
        validate_import_inventory_cleanup_proof(proof)?;
        if let Some(expected_keyset) = advance.expected_cleanup_keyset {
            validate_keyset(expected_keyset)?;
        }
        if let Some(cleanup_keyset) = advance.cleanup_keyset {
            validate_keyset(cleanup_keyset)?;
        }
        if advance.visited_rows_delta > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS as u64
            || advance.cleaned_rows_delta > advance.visited_rows_delta
        {
            return Err(StoreError::ImportInventoryCheckpointPageTooManyRows {
                max_rows: IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_ROWS,
            });
        }
        if advance.cleaned_bytes_delta > IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64 {
            return Err(StoreError::ImportInventoryCheckpointPageTooLarge {
                max_bytes: IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES,
            });
        }
        let committed = self.with_inventory_checkpoint_transaction(
            CHECKPOINT_WRITE_TIMEOUT,
            || -> Result<CheckpointCommit<ImportInventoryCleanupProgress>> {
                let Some(current_proof) = self
                    .load_import_inventory_checkpoint_cleanup_proof_by_key(
                        &proof.run_id,
                        proof.inventory_family,
                        proof.provider,
                        &proof.source_root,
                    )?
                else {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointNotFound,
                    ));
                };
                if current_proof != *proof {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointCleanupBlocked,
                    ));
                }
                let cleanup_state = self.conn.query_row(
                    "SELECT status, owner_state, cleanup_keyset, cleanup_status, \
                            cleanup_visited_row_count, cleanup_row_count, cleanup_bytes, \
                            cleanup_attempt_count \
                     FROM import_inventory_checkpoints \
                     WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                       AND source_root = ?4",
                    params![
                        &proof.run_id,
                        checkpoint_inventory_family_str(proof.inventory_family),
                        proof.provider.as_str(),
                        &proof.source_root,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                            row.get::<_, String>(3)?,
                            nonnegative_i64_to_u64(row.get(4)?)?,
                            nonnegative_i64_to_u64(row.get(5)?)?,
                            nonnegative_i64_to_u64(row.get(6)?)?,
                            nonnegative_i64_to_u64(row.get(7)?)?,
                        ))
                    },
                )?;
                if !matches!(cleanup_state.0.as_str(), "abandoned" | "completed")
                    || cleanup_state.1 != "inactive"
                {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointCleanupBlocked,
                    ));
                }
                if matches!(cleanup_state.3.as_str(), "blocked" | "complete") {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointCleanupBlocked,
                    ));
                }
                if cleanup_state.2.as_deref() != advance.expected_cleanup_keyset {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointStaleAuthority,
                    ));
                }
                let visited_rows = cleanup_state
                    .4
                    .checked_add(advance.visited_rows_delta)
                    .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                        "cleanup visited-row counter overflow",
                    ))?;
                let cleaned_rows = cleanup_state
                    .5
                    .checked_add(advance.cleaned_rows_delta)
                    .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                        "cleanup row counter overflow",
                    ))?;
                let cleaned_bytes = cleanup_state
                    .6
                    .checked_add(advance.cleaned_bytes_delta)
                    .ok_or(StoreError::ImportInventoryCheckpointInvariant(
                        "cleanup byte counter overflow",
                    ))?;
                let attempt_count = cleanup_state.7.checked_add(1).ok_or(
                    StoreError::ImportInventoryCheckpointInvariant(
                        "cleanup attempt counter overflow",
                    ),
                )?;
                let complete = advance.disposition == ImportInventoryCleanupDisposition::Complete;
                let changed = self.conn.execute(
                    "UPDATE import_inventory_checkpoints SET \
                     cleanup_keyset = ?9, cleanup_visited_row_count = ?10, \
                     cleanup_row_count = ?11, cleanup_bytes = ?12, \
                     cleanup_attempt_count = ?13, \
                     cleanup_status = CASE WHEN ?14 THEN 'complete' ELSE 'running' END, \
                     phase = CASE WHEN ?14 THEN 'complete' ELSE 'cleanup' END, \
                     updated_at_ms = ?15 \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4 AND scratch_identity = ?5 \
                   AND scratch_integrity = ?6 AND scratch_lock_identity = ?7 \
                   AND scratch_database_identity = ?8 AND cleanup_keyset IS ?16 \
                   AND owner_state = 'inactive' AND status IN ('abandoned', 'completed') \
                   AND cleanup_status IN ('pending', 'running')",
                    params![
                        &proof.run_id,
                        checkpoint_inventory_family_str(proof.inventory_family),
                        proof.provider.as_str(),
                        &proof.source_root,
                        &proof.scratch_identity,
                        &proof.scratch_integrity,
                        &proof.scratch_lock_identity,
                        &proof.scratch_database_identity,
                        advance.cleanup_keyset,
                        checkpoint_i64(visited_rows)?,
                        checkpoint_i64(cleaned_rows)?,
                        checkpoint_i64(cleaned_bytes)?,
                        checkpoint_i64(attempt_count)?,
                        complete,
                        now_ms,
                        advance.expected_cleanup_keyset,
                    ],
                )?;
                if changed != 1 {
                    return Ok(CheckpointCommit::Failure(
                        StoreError::ImportInventoryCheckpointStaleAuthority,
                    ));
                }
                Ok(CheckpointCommit::Value(ImportInventoryCleanupProgress {
                    disposition: advance.disposition,
                    cleanup_keyset: advance.cleanup_keyset.map(<[u8]>::to_vec),
                    visited_rows,
                    cleaned_rows,
                    cleaned_bytes,
                    attempt_count,
                }))
            },
        )?;
        finish_checkpoint_commit(committed)
    }

    pub fn import_inventory_checkpoint_status(
        &self,
        run_id: &[u8],
        inventory_family: ProviderFileInventoryFamily,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<ImportInventoryCheckpointStatus>> {
        self.conn
            .query_row(
                "SELECT status, phase, owner_state, owner_epoch, lease_owner_id, \
                        lease_expires_at_ms, active_directory_platform_tag, \
                        active_directory_encoding_tag, active_directory_path_hash, \
                        active_directory_identity, active_directory_fingerprint, \
                        active_directory_attempt_count, active_directory_replay_count, \
                        active_directory_observed_entries, \
                        active_directory_next_retry_at_ms, application_keyset, discovery_complete, \
                        application_complete, directory_queue_empty, directory_count, \
                        completed_directory_count, discovered_path_count, planned_path_count, \
                        applied_path_count, \
                        applied_row_count, applied_bytes, attempt_count, replay_count, \
                        next_retry_at_ms, \
                        last_error, abandon_reason, \
                        cleanup_status, cleanup_keyset, cleanup_row_count, cleanup_bytes, \
                        scratch_identity, scratch_integrity, scratch_lock_identity, \
                        scratch_database_identity, selection_keyset, selection_eof, \
                        selection_complete, application_ordinal, application_prefix, \
                        selection_format_version, selection_algorithm_version, \
                        selection_total_count, selection_final_keyset, \
                        selection_final_prefix, selection_commitment_identity, \
                        cleanup_visited_row_count, cleanup_attempt_count, \
                        store_reconciliation_complete, store_reconciliation_keyset, \
                        store_reconciliation_visited_rows, store_reconciliation_stale_rows, \
                        store_reconciliation_visited_bytes \
                 FROM import_inventory_checkpoints \
                 WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
                   AND source_root = ?4",
                params![
                    run_id,
                    checkpoint_inventory_family_str(inventory_family),
                    provider.as_str(),
                    source_root,
                ],
                |row| {
                    let active_directory = decode_active_directory(row, 6)?;
                    Ok(ImportInventoryCheckpointStatus {
                        status: row.get(0)?,
                        phase: row.get(1)?,
                        owner_state: row.get(2)?,
                        owner_epoch: nonnegative_i64_to_u64(row.get(3)?)?,
                        lease_owner_id: row.get(4)?,
                        lease_expires_at_ms: row.get(5)?,
                        active_directory,
                        application_keyset: row.get(15)?,
                        application_ordinal: nonnegative_i64_to_u64(row.get(42)?)?,
                        application_prefix: decode_import_inventory_hash(row, 43)?,
                        discovery_complete: row.get(16)?,
                        effects_complete: row.get(17)?,
                        directory_queue_empty: row.get(18)?,
                        directory_count: nonnegative_i64_to_u64(row.get(19)?)?,
                        completed_directory_count: nonnegative_i64_to_u64(row.get(20)?)?,
                        discovered_path_count: nonnegative_i64_to_u64(row.get(21)?)?,
                        planned_path_count: nonnegative_i64_to_u64(row.get(22)?)?,
                        applied_path_count: nonnegative_i64_to_u64(row.get(23)?)?,
                        applied_row_count: nonnegative_i64_to_u64(row.get(24)?)?,
                        applied_bytes: nonnegative_i64_to_u64(row.get(25)?)?,
                        attempt_count: nonnegative_i64_to_u64(row.get(26)?)?,
                        replay_count: nonnegative_i64_to_u64(row.get(27)?)?,
                        next_retry_at_ms: row.get(28)?,
                        last_error: row.get(29)?,
                        abandon_reason: row.get(30)?,
                        cleanup_status: row.get(31)?,
                        cleanup_keyset: row.get(32)?,
                        cleanup_visited_row_count: nonnegative_i64_to_u64(row.get(50)?)?,
                        cleanup_row_count: nonnegative_i64_to_u64(row.get(33)?)?,
                        cleanup_bytes: nonnegative_i64_to_u64(row.get(34)?)?,
                        cleanup_attempt_count: nonnegative_i64_to_u64(row.get(51)?)?,
                        store_reconciliation_complete: row.get(52)?,
                        store_reconciliation_keyset: match row.get::<_, i64>(53)? {
                            0 => None,
                            keyset => Some(keyset),
                        },
                        store_reconciliation_visited_rows: nonnegative_i64_to_u64(row.get(54)?)?,
                        store_reconciliation_stale_rows: nonnegative_i64_to_u64(row.get(55)?)?,
                        store_reconciliation_visited_bytes: nonnegative_i64_to_u64(row.get(56)?)?,
                        scratch_identity: row.get(35)?,
                        scratch_integrity: row.get(36)?,
                        scratch_lock_identity: row.get(37)?,
                        scratch_database_identity: row.get(38)?,
                        selection_keyset: row.get(39)?,
                        selection_eof: row.get(40)?,
                        selection_complete: row.get(41)?,
                        selection_commitment: decode_import_inventory_selection_commitment(
                            row, 44,
                        )?,
                        selection_commitment_identity: decode_optional_import_inventory_hash(
                            row, 49,
                        )?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn recoverable_import_inventory_checkpoint(
        &self,
        inventory_family: ProviderFileInventoryFamily,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<ImportInventoryCheckpointRecovery>> {
        self.conn
            .query_row(
                "SELECT checkpoint.run_id, checkpoint.source_format, \
                        checkpoint.source_identity, checkpoint.source_fingerprint, \
                        checkpoint.root_platform_tag, checkpoint.root_encoding_tag, \
                        checkpoint.root_path_hash, checkpoint.inventory_generation, \
                        run.checkpoint_format_version, run.producer_build_id, \
                        run.store_schema_version, checkpoint.scratch_identity, \
                        checkpoint.scratch_integrity, checkpoint.scratch_lock_identity, \
                        checkpoint.scratch_database_identity, run.publication_state_marker, \
                        run.publication_owner_present, run.publication_provider, \
                        run.publication_inventory_family, run.publication_source_format, \
                        run.publication_source_root, run.publication_source_path, \
                        run.publication_inventory_generation, run.publication_file_size_bytes, \
                        run.publication_file_modified_at_ms, run.publication_import_revision, \
                        run.publication_metadata_json \
                 FROM import_inventory_generations AS generation \
                 JOIN import_inventory_checkpoints AS checkpoint \
                   ON checkpoint.inventory_family = generation.inventory_family \
                  AND checkpoint.provider = generation.provider \
                  AND checkpoint.source_root = generation.source_root \
                  AND checkpoint.inventory_generation = generation.current_generation \
                 JOIN import_inventory_runs AS run ON run.run_id = checkpoint.run_id \
                 WHERE generation.inventory_family = ?1 AND generation.provider = ?2 \
                   AND generation.source_root = ?3 \
                   AND checkpoint.status IN ('active', 'abandoned', 'cleaning')",
                params![
                    checkpoint_inventory_family_str(inventory_family),
                    provider.as_str(),
                    source_root,
                ],
                |row| {
                    Ok(ImportInventoryCheckpointRecovery {
                        run_id: row.get(0)?,
                        inventory_family,
                        provider,
                        source_format: row.get(1)?,
                        source_root: source_root.to_owned(),
                        source_identity: row.get(2)?,
                        source_fingerprint: row.get(3)?,
                        root_path: ImportInventoryOwnedPathIdentity {
                            platform_tag: row.get(4)?,
                            encoding_tag: row.get(5)?,
                            opaque_hash: row.get(6)?,
                        },
                        inventory_generation: nonnegative_i64_to_u64(row.get(7)?)?,
                        checkpoint_format_version: nonnegative_i64_to_u32(row.get(8)?)?,
                        producer_build_id: row.get(9)?,
                        store_schema_version: nonnegative_i64_to_u32(row.get(10)?)?,
                        scratch_identity: row.get(11)?,
                        scratch_integrity: row.get(12)?,
                        scratch_lock_identity: row.get(13)?,
                        scratch_database_identity: row.get(14)?,
                        publication_state_marker: row.get(15)?,
                        publication_owner: decode_import_inventory_publication_owner(row, 16)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn import_inventory_checkpoint_cleanup_proof(
        &self,
        run_id: &[u8],
        inventory_family: ProviderFileInventoryFamily,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<ImportInventoryCheckpointCleanupProof>> {
        self.load_import_inventory_checkpoint_cleanup_proof_by_key(
            run_id,
            inventory_family,
            provider,
            source_root,
        )
    }

    fn update_capture_checkpoint_summary(
        &self,
        lease: &ImportInventoryCheckpointLease,
        row: &CheckpointRow,
        capture: ImportInventoryCaptureCheckpoint<'_>,
        owner_state: &str,
        now_ms: i64,
    ) -> Result<()> {
        let scratch = trusted_scratch(capture.scratch)?;
        let owner = scratch
            .owner
            .ok_or(StoreError::ImportInventoryCheckpointStaleAuthority)?;
        let active = capture.active_directory;
        let attempt_count = advanced_import_inventory_attempt_count(row, capture)?;
        let changed = self.conn.execute(
            "UPDATE import_inventory_checkpoints SET scratch_integrity = ?8, \
                 scratch_owner_epoch = ?5, scratch_owner_token = ?6, owner_state = ?9, \
                 phase = ?10, discovery_complete = ?11, application_complete = ?12, \
                 directory_queue_empty = ?13, active_directory_platform_tag = ?14, \
                 active_directory_encoding_tag = ?15, active_directory_path_hash = ?16, \
                 active_directory_identity = ?17, active_directory_fingerprint = ?18, \
                 active_directory_attempt_count = ?19, \
                 active_directory_replay_count = ?20, \
                 active_directory_next_retry_at_ms = ?21, directory_count = ?22, \
                 completed_directory_count = ?23, planned_path_count = ?24, \
                 replay_count = ?25, next_retry_at_ms = ?26, last_error = ?27, \
                 active_directory_observed_entries = ?28, \
                 discovered_path_count = ?29, attempt_count = ?30, \
                 scratch_database_identity = ?31, selection_keyset = ?32, \
                 selection_eof = ?33, selection_complete = ?34, \
                 selection_format_version = ?35, selection_algorithm_version = ?36, \
                 selection_total_count = ?37, selection_final_keyset = ?38, \
                 selection_final_prefix = ?39, selection_commitment_identity = ?40, \
                 updated_at_ms = ?41 \
             WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
               AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
               AND lease_owner_id = ?7 AND status IN ('active', 'cleaning')",
            params![
                &lease.run_id,
                checkpoint_inventory_family_str(lease.inventory_family),
                lease.provider.as_str(),
                &lease.source_root,
                checkpoint_i64(owner.owner_epoch)?,
                owner.owner_token,
                &lease.owner_id,
                scratch.integrity,
                owner_state,
                capture_phase(capture),
                capture.discovery_complete,
                capture.effects_complete,
                capture.directory_queue_empty,
                active.map(|value| value.path.platform_tag),
                active.map(|value| value.path.encoding_tag),
                active.map(|value| value.path.opaque_hash),
                active.map(|value| value.directory_identity),
                active.map(|value| value.directory_fingerprint),
                active
                    .map(|value| checkpoint_i64(value.attempt_count))
                    .transpose()?,
                active
                    .map(|value| checkpoint_i64(value.replay_count))
                    .transpose()?,
                active.and_then(|value| value.next_retry_at_ms),
                checkpoint_i64(capture.directory_count)?,
                checkpoint_i64(capture.completed_directory_count)?,
                checkpoint_i64(capture.planned_path_count)?,
                checkpoint_i64(capture.replay_count)?,
                capture.next_retry_at_ms,
                capture.last_error,
                active
                    .map(|value| checkpoint_i64(value.observed_entries))
                    .transpose()?,
                checkpoint_i64(capture.discovered_path_count)?,
                checkpoint_i64(attempt_count)?,
                scratch.database_identity,
                capture.selection_keyset,
                capture.selection_eof,
                capture.selection_complete,
                capture
                    .selection_commitment
                    .map(|commitment| i64::from(commitment.format_version)),
                capture
                    .selection_commitment
                    .map(|commitment| i64::from(commitment.algorithm_version)),
                capture
                    .selection_commitment
                    .map(|commitment| checkpoint_i64(commitment.total_count))
                    .transpose()?,
                capture
                    .selection_commitment
                    .and_then(|commitment| commitment.final_keyset),
                capture
                    .selection_commitment
                    .map(|commitment| commitment.final_prefix),
                capture
                    .selection_commitment
                    .map(import_inventory_selection_commitment_identity)
                    .transpose()?,
                now_ms,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
        }
        Ok(())
    }

    fn update_checkpoint_after_duplicate_effect(
        &self,
        lease: &ImportInventoryCheckpointLease,
        scratch_integrity: &[u8],
        now_ms: i64,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE import_inventory_checkpoints SET scratch_integrity = ?8, \
                 updated_at_ms = ?9 \
             WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
               AND source_root = ?4 AND owner_epoch = ?5 AND owner_token = ?6 \
               AND lease_owner_id = ?7 AND owner_state = 'active' AND status = 'active'",
            params![
                &lease.run_id,
                checkpoint_inventory_family_str(lease.inventory_family),
                lease.provider.as_str(),
                &lease.source_root,
                checkpoint_i64(lease.owner_epoch)?,
                &lease.owner_token,
                &lease.owner_id,
                scratch_integrity,
                now_ms,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
        }
        Ok(())
    }

    fn validate_import_inventory_active_authority(
        &self,
        lease: &ImportInventoryCheckpointLease,
        scratch: ImportInventoryScratchState<'_>,
        now_ms: i64,
        require_current_generation: bool,
    ) -> Result<CheckpointRow> {
        let row = self.validate_import_inventory_lease(lease, now_ms, "active")?;
        if row.owner_state != "active" {
            return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
        }
        if row.run_status != "active" {
            return Err(StoreError::ImportInventoryCheckpointInvariant(
                "inventory run is not active",
            ));
        }
        let trusted = trusted_scratch(scratch)?;
        validate_scratch_owned_by_lease(&row, lease, &trusted)?;
        if require_current_generation && row.current_generation != Some(lease.inventory_generation)
        {
            return Err(StoreError::ImportInventoryCheckpointGenerationMismatch);
        }
        Ok(row)
    }

    fn validate_import_inventory_lease(
        &self,
        lease: &ImportInventoryCheckpointLease,
        now_ms: i64,
        required_status: &str,
    ) -> Result<CheckpointRow> {
        let row = self
            .load_import_inventory_checkpoint_by_key(
                &lease.run_id,
                lease.inventory_family,
                lease.provider,
                &lease.source_root,
            )?
            .ok_or(StoreError::ImportInventoryCheckpointNotFound)?;
        if row.status != required_status
            || row.owner_epoch != lease.owner_epoch
            || row.owner_token.as_deref() != Some(lease.owner_token.as_slice())
            || row.lease_owner_id.as_deref() != Some(lease.owner_id.as_str())
            || row
                .lease_expires_at_ms
                .is_none_or(|expiry| expiry <= now_ms)
            || row.inventory_generation != lease.inventory_generation
        {
            return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
        }
        Ok(row)
    }

    fn load_import_inventory_checkpoint(
        &self,
        trust: &ImportInventoryCheckpointTrust<'_>,
    ) -> Result<Option<CheckpointRow>> {
        self.load_import_inventory_checkpoint_by_key(
            trust.run_id,
            trust.inventory_family,
            trust.provider,
            trust.source_root,
        )
    }

    fn load_import_inventory_checkpoint_by_key(
        &self,
        run_id: &[u8],
        inventory_family: ProviderFileInventoryFamily,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<CheckpointRow>> {
        self.conn
            .query_row(
                r#"
                SELECT checkpoint.source_format, checkpoint.source_identity,
                       checkpoint.source_fingerprint, checkpoint.root_platform_tag,
                       checkpoint.root_encoding_tag, checkpoint.root_path_hash,
                       checkpoint.inventory_generation, checkpoint.scratch_identity,
                       checkpoint.scratch_lock_identity, checkpoint.status,
                       checkpoint.discovery_complete, checkpoint.application_complete,
                       checkpoint.directory_queue_empty,
                       checkpoint.owner_epoch, checkpoint.owner_token, checkpoint.owner_state,
                       checkpoint.scratch_owner_epoch, checkpoint.scratch_owner_token,
                       checkpoint.lease_owner_id, checkpoint.lease_expires_at_ms,
                       checkpoint.active_directory_platform_tag,
                       checkpoint.active_directory_encoding_tag,
                       checkpoint.active_directory_path_hash,
                       checkpoint.active_directory_identity,
                       checkpoint.active_directory_fingerprint,
                       checkpoint.active_directory_attempt_count,
                       checkpoint.active_directory_replay_count,
                       checkpoint.active_directory_observed_entries,
                       checkpoint.active_directory_next_retry_at_ms,
                       checkpoint.directory_count, checkpoint.completed_directory_count,
                       checkpoint.discovered_path_count, checkpoint.planned_path_count,
                       checkpoint.applied_path_count,
                       checkpoint.applied_row_count, checkpoint.applied_bytes,
                       checkpoint.attempt_count, checkpoint.replay_count,
                       run.checkpoint_format_version, run.producer_build_id,
                       run.store_schema_version, run.status, generation.current_generation,
                       checkpoint.scratch_database_identity, checkpoint.selection_keyset,
                       checkpoint.selection_eof, checkpoint.selection_complete,
                       checkpoint.selection_format_version,
                       checkpoint.selection_algorithm_version,
                       checkpoint.selection_total_count, checkpoint.selection_final_keyset,
                       checkpoint.selection_final_prefix,
                       checkpoint.selection_commitment_identity,
                       checkpoint.application_ordinal, checkpoint.application_keyset,
                       checkpoint.application_prefix, run.publication_state_marker,
                       run.publication_owner_present, run.publication_provider,
                       run.publication_inventory_family, run.publication_source_format,
                       run.publication_source_root, run.publication_source_path,
                       run.publication_inventory_generation,
                       run.publication_file_size_bytes,
                       run.publication_file_modified_at_ms,
                       run.publication_import_revision, run.publication_metadata_json,
                       checkpoint.store_reconciliation_complete,
                       checkpoint.store_reconciliation_keyset,
                       checkpoint.store_reconciliation_visited_rows,
                       checkpoint.store_reconciliation_stale_rows,
                       checkpoint.store_reconciliation_visited_bytes
                FROM import_inventory_checkpoints AS checkpoint
                JOIN import_inventory_runs AS run ON run.run_id = checkpoint.run_id
                LEFT JOIN import_inventory_generations AS generation
                  ON generation.provider = checkpoint.provider
                 AND generation.source_root = checkpoint.source_root
                 AND generation.inventory_family = checkpoint.inventory_family
                WHERE checkpoint.run_id = ?1 AND checkpoint.inventory_family = ?2
                  AND checkpoint.provider = ?3 AND checkpoint.source_root = ?4
                "#,
                params![
                    run_id,
                    checkpoint_inventory_family_str(inventory_family),
                    provider.as_str(),
                    source_root,
                ],
                |row| {
                    Ok(CheckpointRow {
                        source_format: row.get(0)?,
                        source_identity: row.get(1)?,
                        source_fingerprint: row.get(2)?,
                        root_path: ImportInventoryOwnedPathIdentity {
                            platform_tag: row.get(3)?,
                            encoding_tag: row.get(4)?,
                            opaque_hash: row.get(5)?,
                        },
                        inventory_generation: nonnegative_i64_to_u64(row.get(6)?)?,
                        scratch_identity: row.get(7)?,
                        scratch_lock_identity: row.get(8)?,
                        status: row.get(9)?,
                        discovery_complete: row.get(10)?,
                        effects_complete: row.get(11)?,
                        directory_queue_empty: row.get(12)?,
                        owner_epoch: nonnegative_i64_to_u64(row.get(13)?)?,
                        owner_token: row.get(14)?,
                        owner_state: row.get(15)?,
                        scratch_owner_epoch: row
                            .get::<_, Option<i64>>(16)?
                            .map(nonnegative_i64_to_u64)
                            .transpose()?,
                        scratch_owner_token: row.get(17)?,
                        lease_owner_id: row.get(18)?,
                        lease_expires_at_ms: row.get(19)?,
                        active_directory: decode_active_directory(row, 20)?,
                        directory_count: nonnegative_i64_to_u64(row.get(29)?)?,
                        completed_directory_count: nonnegative_i64_to_u64(row.get(30)?)?,
                        discovered_path_count: nonnegative_i64_to_u64(row.get(31)?)?,
                        planned_path_count: nonnegative_i64_to_u64(row.get(32)?)?,
                        applied_path_count: nonnegative_i64_to_u64(row.get(33)?)?,
                        applied_row_count: nonnegative_i64_to_u64(row.get(34)?)?,
                        applied_bytes: nonnegative_i64_to_u64(row.get(35)?)?,
                        attempt_count: nonnegative_i64_to_u64(row.get(36)?)?,
                        replay_count: nonnegative_i64_to_u64(row.get(37)?)?,
                        run_checkpoint_format_version: nonnegative_i64_to_u32(row.get(38)?)?,
                        run_producer_build_id: row.get(39)?,
                        run_store_schema_version: nonnegative_i64_to_u32(row.get(40)?)?,
                        run_status: row.get(41)?,
                        current_generation: row
                            .get::<_, Option<i64>>(42)?
                            .map(nonnegative_i64_to_u64)
                            .transpose()?,
                        scratch_database_identity: row.get(43)?,
                        selection_keyset: row.get(44)?,
                        selection_eof: row.get(45)?,
                        selection_complete: row.get(46)?,
                        selection_commitment: decode_import_inventory_selection_commitment(
                            row, 47,
                        )?,
                        selection_commitment_identity: decode_optional_import_inventory_hash(
                            row, 52,
                        )?,
                        application_ordinal: nonnegative_i64_to_u64(row.get(53)?)?,
                        application_keyset: row.get(54)?,
                        application_prefix: decode_import_inventory_hash(row, 55)?,
                        run_publication_state_marker: row.get(56)?,
                        run_publication_owner: decode_import_inventory_publication_owner(row, 57)?,
                        store_reconciliation_complete: row.get(68)?,
                        store_reconciliation_keyset: row.get(69)?,
                        store_reconciliation_visited_rows: nonnegative_i64_to_u64(row.get(70)?)?,
                        store_reconciliation_stale_rows: nonnegative_i64_to_u64(row.get(71)?)?,
                        store_reconciliation_visited_bytes: nonnegative_i64_to_u64(row.get(72)?)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn load_import_inventory_checkpoint_cleanup_proof_by_key(
        &self,
        run_id: &[u8],
        inventory_family: ProviderFileInventoryFamily,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Option<ImportInventoryCheckpointCleanupProof>> {
        self.conn
            .query_row(
                "SELECT run.checkpoint_format_version, run.producer_build_id, \
                        run.store_schema_version, checkpoint.run_id, \
                        checkpoint.source_format, checkpoint.source_identity, \
                        checkpoint.source_fingerprint, checkpoint.root_platform_tag, \
                        checkpoint.root_encoding_tag, checkpoint.root_path_hash, \
                        checkpoint.inventory_generation, checkpoint.scratch_identity, \
                        checkpoint.scratch_integrity, checkpoint.scratch_lock_identity, \
                        checkpoint.scratch_database_identity \
                 FROM import_inventory_checkpoints AS checkpoint \
                 JOIN import_inventory_runs AS run ON run.run_id = checkpoint.run_id \
                 WHERE checkpoint.run_id = ?1 AND checkpoint.inventory_family = ?2 \
                   AND checkpoint.provider = ?3 AND checkpoint.source_root = ?4 \
                   AND checkpoint.status IN ('abandoned', 'completed') \
                   AND checkpoint.owner_state = 'inactive'",
                params![
                    run_id,
                    checkpoint_inventory_family_str(inventory_family),
                    provider.as_str(),
                    source_root,
                ],
                |row| {
                    Ok(ImportInventoryCheckpointCleanupProof {
                        checkpoint_format_version: nonnegative_i64_to_u32(row.get(0)?)?,
                        producer_build_id: row.get(1)?,
                        store_schema_version: nonnegative_i64_to_u32(row.get(2)?)?,
                        run_id: row.get(3)?,
                        inventory_family,
                        provider,
                        source_format: row.get(4)?,
                        source_root: source_root.to_owned(),
                        source_identity: row.get(5)?,
                        source_fingerprint: row.get(6)?,
                        root_path: ImportInventoryOwnedPathIdentity {
                            platform_tag: row.get(7)?,
                            encoding_tag: row.get(8)?,
                            opaque_hash: row.get(9)?,
                        },
                        inventory_generation: nonnegative_i64_to_u64(row.get(10)?)?,
                        scratch_identity: row.get(11)?,
                        scratch_integrity: row.get(12)?,
                        scratch_lock_identity: row.get(13)?,
                        scratch_database_identity: row.get(14)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn current_import_inventory_generation_for_checkpoint(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        inventory_family: ProviderFileInventoryFamily,
    ) -> Result<Option<u64>> {
        self.conn
            .query_row(
                "SELECT current_generation FROM import_inventory_generations \
                 WHERE provider = ?1 AND source_root = ?2 AND inventory_family = ?3",
                params![
                    provider.as_str(),
                    source_root,
                    checkpoint_inventory_family_str(inventory_family),
                ],
                |row| nonnegative_i64_to_u64(row.get(0)?),
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn validate_current_import_inventory_publication_snapshot(
        &self,
        expected_state_marker: &str,
        expected_owner: Option<&ProviderFilePublicationInventoryOwner>,
    ) -> Result<()> {
        let (current_state_marker, current_owner) =
            self.effective_provider_file_publication_inventory_snapshot()?;
        if current_state_marker != expected_state_marker || current_owner.as_ref() != expected_owner
        {
            return Err(StoreError::ImportInventoryCheckpointPublicationTransition {
                expected_state_marker: expected_state_marker.to_owned(),
                current_state_marker,
            });
        }
        Ok(())
    }

    fn apply_import_inventory_canonical_effect(
        &self,
        lease: &ImportInventoryCheckpointLease,
        row: &CheckpointRow,
        effect: ImportInventoryCanonicalEffect<'_>,
        now_ms: i64,
    ) -> Result<u64> {
        let changed = match effect {
            ImportInventoryCanonicalEffect::CatalogUpsert(session) => self
                .upsert_catalog_sessions(
                    lease.inventory_generation,
                    std::slice::from_ref(session),
                )?,
            ImportInventoryCanonicalEffect::SourceImportUpsert(file) => self
                .upsert_source_import_files(
                    lease.inventory_generation,
                    std::slice::from_ref(file),
                )?,
            ImportInventoryCanonicalEffect::CatalogStale {
                source_path,
                observed_at_ms,
            } => self.conn.execute(
                "UPDATE catalog_sessions SET is_stale = 1, cataloged_at_ms = ?6 \
                 WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3 \
                   AND source_path = ?4 AND EXISTS (\
                     SELECT 1 FROM import_inventory_generations \
                     WHERE provider = ?1 AND source_root = ?3 \
                       AND inventory_family = 'catalog_sessions' \
                       AND current_generation = ?5\
                   )",
                params![
                    lease.provider.as_str(),
                    &row.source_format,
                    &lease.source_root,
                    source_path,
                    checkpoint_i64(lease.inventory_generation)?,
                    observed_at_ms,
                ],
            )?,
            ImportInventoryCanonicalEffect::SourceImportStale {
                source_path,
                observed_at_ms,
            } => self.conn.execute(
                "UPDATE source_import_files SET is_stale = 1, observed_at_ms = ?6 \
                 WHERE provider = ?1 AND source_format = ?2 AND source_root = ?3 \
                   AND source_path = ?4 AND EXISTS (\
                     SELECT 1 FROM import_inventory_generations \
                     WHERE provider = ?1 AND source_root = ?3 \
                       AND inventory_family = 'source_import_files' \
                       AND current_generation = ?5\
                   )",
                params![
                    lease.provider.as_str(),
                    &row.source_format,
                    &lease.source_root,
                    source_path,
                    checkpoint_i64(lease.inventory_generation)?,
                    observed_at_ms,
                ],
            )?,
            ImportInventoryCanonicalEffect::CatalogRescan { source_path } => self.conn.execute(
                "UPDATE catalog_sessions SET pending_reason = CASE \
                   WHEN indexed_status IN ('pending', 'failed') \
                     THEN COALESCE(pending_reason, 'legacy') \
                   ELSE COALESCE(pending_reason, 'explicit_rescan') END, cataloged_at_ms = ?5 \
                 WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3 \
                   AND source_format = ?6 \
                   AND is_stale = 0 AND EXISTS (SELECT 1 FROM import_inventory_generations \
                     WHERE provider = ?1 AND source_root = ?2 \
                       AND inventory_family = 'catalog_sessions' AND current_generation = ?4)",
                params![
                    lease.provider.as_str(),
                    &lease.source_root,
                    source_path,
                    checkpoint_i64(lease.inventory_generation)?,
                    now_ms,
                    &row.source_format,
                ],
            )?,
            ImportInventoryCanonicalEffect::SourceImportRescan { source_path } => {
                self.conn.execute(
                    "UPDATE source_import_files SET pending_reason = CASE \
                   WHEN indexed_status IN ('pending', 'failed') \
                     THEN COALESCE(pending_reason, 'legacy') \
                   ELSE COALESCE(pending_reason, 'explicit_rescan') END, observed_at_ms = ?5 \
                 WHERE provider = ?1 AND source_root = ?2 AND source_path = ?3 \
                   AND source_format = ?6 \
                   AND is_stale = 0 AND EXISTS (SELECT 1 FROM import_inventory_generations \
                     WHERE provider = ?1 AND source_root = ?2 \
                       AND inventory_family = 'source_import_files' AND current_generation = ?4)",
                    params![
                        lease.provider.as_str(),
                        &lease.source_root,
                        source_path,
                        checkpoint_i64(lease.inventory_generation)?,
                        now_ms,
                        &row.source_format,
                    ],
                )?
            }
            ImportInventoryCanonicalEffect::CatalogObservationRejected { .. }
            | ImportInventoryCanonicalEffect::SourceImportObservationRejected { .. } => 0,
        };
        u64::try_from(changed).map_err(|_| {
            StoreError::InvalidImportInventoryCheckpoint("affected row count overflow")
        })
    }

    fn abandon_import_inventory_checkpoint_inner(
        &self,
        trust: &ImportInventoryCheckpointTrust<'_>,
        now_ms: i64,
        reason: &str,
        cleanup_blocked: bool,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE import_inventory_checkpoints SET status = 'abandoned', \
                 phase = 'abandoned', owner_token = NULL, owner_state = 'inactive', \
                 lease_owner_id = NULL, lease_expires_at_ms = NULL, abandon_reason = ?5, \
                 last_error = ?5, cleanup_status = CASE WHEN ?6 THEN 'blocked' ELSE 'pending' END, \
                 updated_at_ms = ?7 \
             WHERE run_id = ?1 AND inventory_family = ?2 AND provider = ?3 \
               AND source_root = ?4 AND status = 'active'",
            params![
                trust.run_id,
                checkpoint_inventory_family_str(trust.inventory_family),
                trust.provider.as_str(),
                trust.source_root,
                reason,
                cleanup_blocked,
                now_ms,
            ],
        )?;
        if changed == 1 {
            self.record_import_inventory_run_abandonment(trust.run_id, reason, now_ms)?;
        }
        Ok(())
    }

    fn record_import_inventory_run_abandonment(
        &self,
        run_id: &[u8],
        reason: &str,
        now_ms: i64,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE import_inventory_runs \
             SET abandoned_source_count = abandoned_source_count + 1, status = 'abandoned', \
                 last_error = ?2, updated_at_ms = ?3 \
             WHERE run_id = ?1 AND status IN ('active', 'abandoned') \
               AND abandoned_source_count < source_count",
            params![run_id, reason, now_ms],
        )?;
        if changed != 1 {
            return Err(StoreError::ImportInventoryCheckpointInvariant(
                "inventory run could not record abandonment",
            ));
        }
        Ok(())
    }

    fn with_inventory_checkpoint_transaction<T>(
        &self,
        timeout: Duration,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let timeout = timeout.max(Duration::from_millis(1));
        let started = std::time::Instant::now();
        self.conn.busy_timeout(Duration::ZERO)?;
        self.conn
            .progress_handler(1_000, Some(move || started.elapsed() >= timeout));
        let begin = self.conn.execute_batch("BEGIN IMMEDIATE");
        if let Err(error) = begin {
            self.conn.progress_handler(0, None::<fn() -> bool>);
            self.conn.busy_timeout(self.busy_timeout)?;
            if inventory_checkpoint_sqlite_busy(&error) {
                return Err(StoreError::ImportInventoryCheckpointWriterBusy);
            }
            return Err(error.into());
        }
        let result = match action() {
            Ok(value) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(value),
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(StoreError::from(error))
                }
            },
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        };
        self.conn.progress_handler(0, None::<fn() -> bool>);
        self.conn.busy_timeout(self.busy_timeout)?;
        match result {
            Err(error) if inventory_checkpoint_interrupted(&error) => {
                Err(StoreError::ImportInventoryCheckpointNoProgress)
            }
            Err(StoreError::Sql(error)) if inventory_checkpoint_sqlite_busy(&error) => {
                Err(StoreError::ImportInventoryCheckpointWriterBusy)
            }
            other => other,
        }
    }
}

fn validate_checkpoint_trust_input(
    trust: &ImportInventoryCheckpointTrust<'_>,
    owner_id: &str,
    now_ms: i64,
    lease_expires_at_ms: i64,
) -> Result<()> {
    validate_checkpoint_trust(trust)?;
    if owner_id.is_empty() || owner_id.len() > 256 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "owner id length is invalid",
        ));
    }
    if lease_expires_at_ms <= now_ms {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "lease expiry must be in the future",
        ));
    }
    Ok(())
}

fn validate_checkpoint_trust(trust: &ImportInventoryCheckpointTrust<'_>) -> Result<()> {
    if trust.run_id.is_empty() || trust.run_id.len() > 1024 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "run id length is invalid",
        ));
    }
    if trust.source_format.is_empty() || trust.source_format.len() > 256 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "source format length is invalid",
        ));
    }
    if trust.source_root.is_empty() || trust.source_root.len() > 32768 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "source root length is invalid",
        ));
    }
    if trust.inventory_generation == 0 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "inventory generation must be positive",
        ));
    }
    if trust.checkpoint_format_version != IMPORT_INVENTORY_CHECKPOINT_FORMAT_VERSION {
        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "checkpoint format version",
        });
    }
    if trust.store_schema_version != crate::current_history_store_schema_version() {
        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "store schema version",
        });
    }
    if trust.producer_build_id.is_empty() || trust.producer_build_id.len() > 1024 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "producer build id length is invalid",
        ));
    }
    validate_opaque_identity(trust.source_identity, "source identity")?;
    validate_opaque_identity(trust.source_fingerprint, "source fingerprint")?;
    validate_native_path(trust.root_path)?;
    validate_opaque_identity(trust.scratch_identity, "scratch identity")?;
    validate_opaque_identity(trust.scratch_lock_identity, "scratch lock identity")?;
    validate_opaque_identity(trust.scratch_database_identity, "scratch database identity")?;
    if trust.publication_state_marker.len() != 64
        || !trust
            .publication_state_marker
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "publication state marker is invalid",
        ));
    }
    if let Some(owner) = trust.publication_owner {
        if owner.source_format.is_empty()
            || owner.source_format.len() > 256
            || owner.source_root.is_empty()
            || owner.source_root.len() > 32768
            || owner.source_path.is_empty()
            || owner.source_path.len() > 32768
            || owner.inventory_generation == 0
        {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "publication inventory owner is invalid",
            ));
        }
    }
    Ok(())
}

fn validate_import_inventory_cleanup_proof(
    proof: &ImportInventoryCheckpointCleanupProof,
) -> Result<()> {
    if proof.checkpoint_format_version != IMPORT_INVENTORY_CHECKPOINT_FORMAT_VERSION {
        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "cleanup checkpoint format version",
        });
    }
    if proof.store_schema_version != crate::current_history_store_schema_version() {
        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "cleanup store schema version",
        });
    }
    if proof.run_id.is_empty() || proof.run_id.len() > 1024 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "cleanup run id length is invalid",
        ));
    }
    if proof.producer_build_id.is_empty() || proof.producer_build_id.len() > 1024 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "cleanup producer build id length is invalid",
        ));
    }
    if proof.source_format.is_empty() || proof.source_format.len() > 256 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "cleanup source format length is invalid",
        ));
    }
    if proof.source_root.is_empty() || proof.source_root.len() > 32768 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "cleanup source root length is invalid",
        ));
    }
    if proof.inventory_generation == 0 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "cleanup inventory generation must be positive",
        ));
    }
    validate_opaque_identity(&proof.source_identity, "cleanup source identity")?;
    validate_opaque_identity(&proof.source_fingerprint, "cleanup source fingerprint")?;
    validate_native_path(ImportInventoryNativePathIdentity {
        platform_tag: &proof.root_path.platform_tag,
        encoding_tag: &proof.root_path.encoding_tag,
        opaque_hash: &proof.root_path.opaque_hash,
    })?;
    validate_opaque_identity(&proof.scratch_identity, "cleanup scratch identity")?;
    validate_integrity(&proof.scratch_integrity, "cleanup scratch integrity")?;
    validate_opaque_identity(
        &proof.scratch_lock_identity,
        "cleanup scratch lock identity",
    )?;
    validate_opaque_identity(
        &proof.scratch_database_identity,
        "cleanup scratch database identity",
    )
}

fn validate_capture_checkpoint(capture: ImportInventoryCaptureCheckpoint<'_>) -> Result<()> {
    let scratch = trusted_scratch(capture.scratch)?;
    validate_opaque_identity(scratch.identity, "scratch identity")?;
    validate_integrity(scratch.integrity, "scratch integrity")?;
    validate_opaque_identity(scratch.lock_identity, "scratch lock identity")?;
    validate_opaque_identity(scratch.database_identity, "scratch database identity")?;
    if let Some(owner) = scratch.owner {
        if owner.owner_epoch == 0 || !(16..=64).contains(&owner.owner_token.len()) {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "scratch owner authority is invalid",
            ));
        }
    }
    validate_active_directory_scratch(capture, &scratch)?;
    validate_capture_checkpoint_shape(capture)
}

fn validate_capture_checkpoint_shape(capture: ImportInventoryCaptureCheckpoint<'_>) -> Result<()> {
    if let Some(active) = capture.active_directory {
        validate_native_path(active.path)?;
        validate_opaque_identity(active.directory_identity, "active directory identity")?;
        validate_opaque_identity(active.directory_fingerprint, "active directory fingerprint")?;
        validate_opaque_identity(active.scratch_identity, "active directory scratch identity")?;
        validate_integrity(
            active.scratch_integrity,
            "active directory scratch integrity",
        )?;
        validate_opaque_identity(
            active.scratch_lock_identity,
            "active directory scratch lock identity",
        )?;
        validate_opaque_identity(
            active.scratch_database_identity,
            "active directory scratch database identity",
        )?;
        if active.attempt_count == 0 || active.replay_count > active.attempt_count {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "active directory retry counters are invalid",
            ));
        }
    }
    if let Some(selection_keyset) = capture.selection_keyset {
        validate_keyset(selection_keyset)?;
    }
    if capture.selection_complete != capture.selection_commitment.is_some() {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection commitment does not match completion state",
        ));
    }
    if let Some(commitment) = capture.selection_commitment {
        validate_import_inventory_selection_commitment(commitment)?;
        if commitment.total_count != capture.planned_path_count {
            return Err(StoreError::InvalidImportInventoryCheckpoint(
                "selection commitment count does not match planned paths",
            ));
        }
    }
    if capture.completed_directory_count > capture.directory_count
        || capture.planned_path_count > capture.discovered_path_count
        || (capture.selection_eof && !capture.discovery_complete)
        || (capture.selection_complete && !capture.selection_eof)
        || (capture.effects_complete && !capture.selection_complete)
        || (!capture.discovery_complete && capture.effects_complete)
        || (!capture.directory_queue_empty && capture.effects_complete)
        || (capture.active_directory.is_some() && capture.effects_complete)
        || (capture.effects_complete
            && capture.completed_directory_count != capture.directory_count)
    {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "capture checkpoint counters or completion state are invalid",
        ));
    }
    if capture.last_error.is_some_and(|error| error.len() > 4096) {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "capture checkpoint error is too large",
        ));
    }
    Ok(())
}

fn validate_new_capture_checkpoint(capture: ImportInventoryCaptureCheckpoint<'_>) -> Result<()> {
    if capture.active_directory.is_some()
        || capture.completed_directory_count != 0
        || capture.discovered_path_count != 0
        || capture.planned_path_count != 0
        || capture.replay_count != 0
        || capture.selection_keyset.is_some()
        || capture.selection_eof
        || capture.selection_complete
        || capture.selection_commitment.is_some()
        || capture.discovery_complete
        || capture.effects_complete
    {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "new scratch progressed before store owner acquisition",
        ));
    }
    Ok(())
}

fn validate_active_directory_scratch(
    capture: ImportInventoryCaptureCheckpoint<'_>,
    scratch: &TrustedScratch<'_>,
) -> Result<()> {
    if let Some(active) = capture.active_directory {
        if active.scratch_identity != scratch.identity
            || active.scratch_lock_identity != scratch.lock_identity
            || active.scratch_database_identity != scratch.database_identity
        {
            return Err(StoreError::ImportInventoryCheckpointScratchTampered);
        }
        if active.scratch_integrity != scratch.integrity {
            return Err(StoreError::ImportInventoryCheckpointScratchCorrupt);
        }
    }
    Ok(())
}

fn validate_capture_progress(
    row: &CheckpointRow,
    capture: ImportInventoryCaptureCheckpoint<'_>,
) -> Result<()> {
    if capture.directory_count < row.directory_count
        || capture.completed_directory_count < row.completed_directory_count
        || capture.discovered_path_count < row.discovered_path_count
        || capture.planned_path_count < row.planned_path_count
        || capture.replay_count < row.replay_count
        || (row.discovery_complete && !capture.discovery_complete)
        || (row.selection_eof && !capture.selection_eof)
        || (row.selection_complete && !capture.selection_complete)
        || (row.effects_complete && !capture.effects_complete)
    {
        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "capture checkpoint regressed",
        });
    }
    if let Some(stored) = row.selection_commitment {
        if capture.selection_commitment != Some(stored) {
            return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
                field: "frozen selection commitment changed",
            });
        }
    }
    if let Some(owned) = row.active_directory.as_ref() {
        if capture
            .active_directory
            .is_none_or(|proof| !active_directory_matches(owned, proof))
            && capture.completed_directory_count == row.completed_directory_count
        {
            return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
                field: "active directory changed or cleared without completion",
            });
        }
    }
    if let (Some(owned), Some(proof)) = (row.active_directory.as_ref(), capture.active_directory) {
        if active_directory_matches(owned, proof)
            && (proof.attempt_count < owned.attempt_count
                || proof.replay_count < owned.replay_count
                || proof.observed_entries < owned.observed_entries)
        {
            return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
                field: "active directory counters regressed",
            });
        }
    }
    Ok(())
}

fn validate_native_path(path: ImportInventoryNativePathIdentity<'_>) -> Result<()> {
    if path.platform_tag.is_empty() || path.platform_tag.len() > 32 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "native path platform tag length is invalid",
        ));
    }
    if path.encoding_tag.is_empty() || path.encoding_tag.len() > 32 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "native path encoding tag length is invalid",
        ));
    }
    validate_hash(path.opaque_hash, "native path hash")
}

fn validate_hash(value: &[u8], field: &'static str) -> Result<()> {
    if !(16..=128).contains(&value.len()) {
        return Err(StoreError::InvalidImportInventoryCheckpoint(field));
    }
    Ok(())
}

fn validate_opaque_identity(value: &[u8], field: &'static str) -> Result<()> {
    if value.is_empty() || value.len() > 1024 {
        return Err(StoreError::InvalidImportInventoryCheckpoint(field));
    }
    Ok(())
}

fn validate_integrity(value: &[u8], field: &'static str) -> Result<()> {
    if !(16..=256).contains(&value.len()) {
        return Err(StoreError::InvalidImportInventoryCheckpoint(field));
    }
    Ok(())
}

fn validate_keyset(value: &[u8]) -> Result<()> {
    if value.len() > IMPORT_INVENTORY_CHECKPOINT_MAX_KEYSET_BYTES {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "checkpoint keyset is too large",
        ));
    }
    Ok(())
}

fn trusted_scratch(scratch: ImportInventoryScratchState<'_>) -> Result<TrustedScratch<'_>> {
    match scratch {
        ImportInventoryScratchState::Trusted {
            identity,
            integrity,
            lock_identity,
            database_identity,
            owner,
        } => {
            validate_opaque_identity(identity, "scratch identity")?;
            validate_integrity(integrity, "scratch integrity")?;
            validate_opaque_identity(lock_identity, "scratch lock identity")?;
            validate_opaque_identity(database_identity, "scratch database identity")?;
            if let Some(owner) = owner {
                if owner.owner_epoch == 0 || !(16..=64).contains(&owner.owner_token.len()) {
                    return Err(StoreError::InvalidImportInventoryCheckpoint(
                        "scratch owner authority is invalid",
                    ));
                }
            }
            Ok(TrustedScratch {
                identity,
                integrity,
                lock_identity,
                database_identity,
                owner,
            })
        }
        ImportInventoryScratchState::Missing => {
            Err(StoreError::ImportInventoryCheckpointScratchMissing)
        }
        ImportInventoryScratchState::Corrupt => {
            Err(StoreError::ImportInventoryCheckpointScratchCorrupt)
        }
        ImportInventoryScratchState::Tampered => {
            Err(StoreError::ImportInventoryCheckpointScratchTampered)
        }
    }
}

fn validate_stable_scratch(
    trust: &ImportInventoryCheckpointTrust<'_>,
    scratch: &TrustedScratch<'_>,
) -> Result<()> {
    if scratch.identity != trust.scratch_identity
        || scratch.lock_identity != trust.scratch_lock_identity
        || scratch.database_identity != trust.scratch_database_identity
    {
        return Err(StoreError::ImportInventoryCheckpointScratchTampered);
    }
    Ok(())
}

fn validate_scratch_for_acquisition(
    row: &CheckpointRow,
    scratch: &TrustedScratch<'_>,
) -> Result<()> {
    if scratch.identity != row.scratch_identity
        || scratch.lock_identity != row.scratch_lock_identity
        || scratch.database_identity != row.scratch_database_identity
    {
        return Err(StoreError::ImportInventoryCheckpointScratchTampered);
    }
    let owner_matches = match row.owner_state.as_str() {
        "awaiting_scratch_adoption" => {
            scratch_owner_matches_parts(
                scratch.owner,
                row.scratch_owner_epoch,
                row.scratch_owner_token.as_deref(),
            ) || scratch_owner_matches_parts(
                scratch.owner,
                Some(row.owner_epoch),
                row.owner_token.as_deref(),
            )
        }
        "active" => scratch_owner_matches_parts(
            scratch.owner,
            Some(row.owner_epoch),
            row.owner_token.as_deref(),
        ),
        _ => false,
    };
    if !owner_matches {
        return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
    }
    Ok(())
}

fn validate_scratch_owned_by_lease(
    row: &CheckpointRow,
    lease: &ImportInventoryCheckpointLease,
    scratch: &TrustedScratch<'_>,
) -> Result<()> {
    if scratch.identity != row.scratch_identity
        || scratch.lock_identity != row.scratch_lock_identity
        || scratch.database_identity != row.scratch_database_identity
    {
        return Err(StoreError::ImportInventoryCheckpointScratchTampered);
    }
    if !scratch_owner_matches_parts(
        scratch.owner,
        Some(lease.owner_epoch),
        Some(lease.owner_token.as_slice()),
    ) {
        return Err(StoreError::ImportInventoryCheckpointStaleAuthority);
    }
    Ok(())
}

fn scratch_owner_matches_parts(
    owner: Option<ImportInventoryScratchOwner<'_>>,
    expected_epoch: Option<u64>,
    expected_token: Option<&[u8]>,
) -> bool {
    match (owner, expected_epoch, expected_token) {
        (None, None, None) => true,
        (Some(owner), Some(epoch), Some(token)) => {
            owner.owner_epoch == epoch && owner.owner_token == token
        }
        _ => false,
    }
}

fn checkpoint_trust_error(
    row: &CheckpointRow,
    trust: &ImportInventoryCheckpointTrust<'_>,
    require_current_generation: bool,
) -> Option<StoreError> {
    let mismatch = if row.run_checkpoint_format_version != trust.checkpoint_format_version {
        Some("checkpoint format version")
    } else if row.run_producer_build_id != trust.producer_build_id {
        Some("producer build")
    } else if row.run_store_schema_version != trust.store_schema_version {
        Some("store schema version")
    } else if row.run_publication_state_marker != trust.publication_state_marker
        || row.run_publication_owner.as_ref() != trust.publication_owner
    {
        Some("publication inventory snapshot")
    } else if row.source_format != trust.source_format {
        Some("source format")
    } else if row.source_identity != trust.source_identity {
        Some("source identity")
    } else if row.source_fingerprint != trust.source_fingerprint {
        Some("source fingerprint")
    } else if !owned_path_matches(&row.root_path, trust.root_path) {
        Some("source root identity")
    } else if row.inventory_generation != trust.inventory_generation {
        Some("inventory generation")
    } else if row.scratch_identity != trust.scratch_identity {
        Some("scratch identity")
    } else if row.scratch_lock_identity != trust.scratch_lock_identity {
        Some("scratch lock identity")
    } else if row.scratch_database_identity != trust.scratch_database_identity {
        Some("scratch database identity")
    } else {
        None
    };
    if let Some(field) = mismatch {
        return Some(StoreError::ImportInventoryCheckpointTrustMismatch { field });
    }
    if require_current_generation && row.current_generation != Some(trust.inventory_generation) {
        return Some(StoreError::ImportInventoryCheckpointGenerationMismatch);
    }
    None
}

fn capture_phase(capture: ImportInventoryCaptureCheckpoint<'_>) -> &'static str {
    if !capture.discovery_complete {
        "discovery"
    } else if !capture.selection_complete {
        "selection"
    } else if !capture.effects_complete {
        "application"
    } else {
        "finalization"
    }
}

fn active_directory_matches(
    owned: &ImportInventoryActiveDirectoryStatus,
    proof: ImportInventoryActiveDirectoryProof<'_>,
) -> bool {
    owned_path_matches(&owned.path, proof.path)
        && owned.directory_identity == proof.directory_identity
        && owned.directory_fingerprint == proof.directory_fingerprint
}

fn advanced_import_inventory_attempt_count(
    row: &CheckpointRow,
    capture: ImportInventoryCaptureCheckpoint<'_>,
) -> Result<u64> {
    let delta = match (row.active_directory.as_ref(), capture.active_directory) {
        (Some(owned), Some(proof)) if active_directory_matches(owned, proof) => proof
            .attempt_count
            .checked_sub(owned.attempt_count)
            .ok_or(StoreError::ImportInventoryCheckpointTrustMismatch {
                field: "active directory attempt count regressed",
            })?,
        (_, Some(proof)) => proof.attempt_count,
        (_, None) => 0,
    };
    let next = row.attempt_count.checked_add(delta).ok_or(
        StoreError::InvalidImportInventoryCheckpoint("directory attempt count overflow"),
    )?;
    if capture.replay_count > next {
        return Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "replay count exceeds persisted attempts",
        });
    }
    Ok(next)
}

fn decode_active_directory(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<ImportInventoryActiveDirectoryStatus>> {
    let platform = row.get::<_, Option<String>>(offset)?;
    let encoding = row.get::<_, Option<String>>(offset + 1)?;
    let hash = row.get::<_, Option<Vec<u8>>>(offset + 2)?;
    let identity = row.get::<_, Option<Vec<u8>>>(offset + 3)?;
    let fingerprint = row.get::<_, Option<Vec<u8>>>(offset + 4)?;
    let attempt_count = row.get::<_, Option<i64>>(offset + 5)?;
    let replay_count = row.get::<_, Option<i64>>(offset + 6)?;
    let observed_entries = row.get::<_, Option<i64>>(offset + 7)?;
    let next_retry_at_ms = row.get::<_, Option<i64>>(offset + 8)?;
    match (
        platform,
        encoding,
        hash,
        identity,
        fingerprint,
        attempt_count,
        replay_count,
        observed_entries,
    ) {
        (
            Some(platform_tag),
            Some(encoding_tag),
            Some(opaque_hash),
            Some(identity),
            Some(fingerprint),
            Some(attempt_count),
            Some(replay_count),
            Some(observed_entries),
        ) => Ok(Some(ImportInventoryActiveDirectoryStatus {
            path: ImportInventoryOwnedPathIdentity {
                platform_tag,
                encoding_tag,
                opaque_hash,
            },
            directory_identity: identity,
            directory_fingerprint: fingerprint,
            attempt_count: nonnegative_i64_to_u64(attempt_count)?,
            replay_count: nonnegative_i64_to_u64(replay_count)?,
            observed_entries: nonnegative_i64_to_u64(observed_entries)?,
            next_retry_at_ms,
        })),
        (None, None, None, None, None, None, None, None) if next_retry_at_ms.is_none() => Ok(None),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn decode_import_inventory_hash(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<[u8; 32]> {
    row.get::<_, Vec<u8>>(index)?
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn decode_optional_import_inventory_hash(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<[u8; 32]>> {
    row.get::<_, Option<Vec<u8>>>(index)?
        .map(|value| value.try_into().map_err(|_| rusqlite::Error::InvalidQuery))
        .transpose()
}

fn decode_import_inventory_selection_commitment(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<ImportInventoryFrozenSelectionCommitment>> {
    let format_version = row.get::<_, Option<i64>>(offset)?;
    let algorithm_version = row.get::<_, Option<i64>>(offset + 1)?;
    let total_count = row.get::<_, Option<i64>>(offset + 2)?;
    let final_keyset = decode_optional_import_inventory_hash(row, offset + 3)?;
    let final_prefix = decode_optional_import_inventory_hash(row, offset + 4)?;
    match (format_version, algorithm_version, total_count, final_prefix) {
        (Some(format_version), Some(algorithm_version), Some(total_count), Some(final_prefix)) => {
            let commitment = ImportInventoryFrozenSelectionCommitment {
                format_version: nonnegative_i64_to_u32(format_version)?,
                algorithm_version: nonnegative_i64_to_u32(algorithm_version)?,
                total_count: nonnegative_i64_to_u64(total_count)?,
                final_keyset,
                final_prefix,
            };
            validate_import_inventory_selection_commitment(commitment)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(Some(commitment))
        }
        (None, None, None, None) if final_keyset.is_none() => Ok(None),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn decode_import_inventory_publication_owner(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<ProviderFilePublicationInventoryOwner>> {
    let present = row.get::<_, bool>(offset)?;
    let provider = row.get::<_, Option<String>>(offset + 1)?;
    let inventory_family = row.get::<_, Option<String>>(offset + 2)?;
    let source_format = row.get::<_, Option<String>>(offset + 3)?;
    let source_root = row.get::<_, Option<String>>(offset + 4)?;
    let source_path = row.get::<_, Option<String>>(offset + 5)?;
    let inventory_generation = row.get::<_, Option<i64>>(offset + 6)?;
    let file_size_bytes = row.get::<_, Option<i64>>(offset + 7)?;
    let file_modified_at_ms = row.get::<_, Option<i64>>(offset + 8)?;
    let import_revision = row.get::<_, Option<i64>>(offset + 9)?;
    let metadata_json = row.get::<_, Option<String>>(offset + 10)?;
    if !present {
        if provider.is_none()
            && inventory_family.is_none()
            && source_format.is_none()
            && source_root.is_none()
            && source_path.is_none()
            && inventory_generation.is_none()
            && file_size_bytes.is_none()
            && file_modified_at_ms.is_none()
            && import_revision.is_none()
            && metadata_json.is_none()
        {
            return Ok(None);
        }
        return Err(rusqlite::Error::InvalidQuery);
    }
    let provider =
        CaptureProvider::from_str(provider.as_deref().ok_or(rusqlite::Error::InvalidQuery)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let inventory_family = match inventory_family.as_deref() {
        Some("catalog_sessions") => ProviderFileInventoryFamily::Catalog,
        Some("source_import_files") => ProviderFileInventoryFamily::SourceImport,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(Some(ProviderFilePublicationInventoryOwner {
        provider,
        inventory_family,
        source_format: source_format.ok_or(rusqlite::Error::InvalidQuery)?,
        source_root: source_root.ok_or(rusqlite::Error::InvalidQuery)?,
        source_path: source_path.ok_or(rusqlite::Error::InvalidQuery)?,
        inventory_generation: nonnegative_i64_to_u64(
            inventory_generation.ok_or(rusqlite::Error::InvalidQuery)?,
        )?,
        file_size_bytes: nonnegative_i64_to_u64(
            file_size_bytes.ok_or(rusqlite::Error::InvalidQuery)?,
        )?,
        file_modified_at_ms: file_modified_at_ms.ok_or(rusqlite::Error::InvalidQuery)?,
        import_revision: nonnegative_i64_to_u32(
            import_revision.ok_or(rusqlite::Error::InvalidQuery)?,
        )?,
        metadata_json,
    }))
}

fn validate_canonical_effect_checkpoint_scope(
    inventory_family: ProviderFileInventoryFamily,
    provider: CaptureProvider,
    source_format: &str,
    source_root: &str,
    effect: ImportInventoryCanonicalEffect<'_>,
) -> Result<()> {
    let valid = match effect {
        ImportInventoryCanonicalEffect::CatalogUpsert(session) => {
            inventory_family == ProviderFileInventoryFamily::Catalog
                && session.provider == provider
                && session.source_format == source_format
                && session.source_root == source_root
        }
        ImportInventoryCanonicalEffect::SourceImportUpsert(file) => {
            inventory_family == ProviderFileInventoryFamily::SourceImport
                && file.provider == provider
                && file.source_format == source_format
                && file.source_root == source_root
        }
        ImportInventoryCanonicalEffect::CatalogStale { .. }
        | ImportInventoryCanonicalEffect::CatalogRescan { .. }
        | ImportInventoryCanonicalEffect::CatalogObservationRejected { .. } => {
            inventory_family == ProviderFileInventoryFamily::Catalog
        }
        ImportInventoryCanonicalEffect::SourceImportStale { .. }
        | ImportInventoryCanonicalEffect::SourceImportRescan { .. }
        | ImportInventoryCanonicalEffect::SourceImportObservationRejected { .. } => {
            inventory_family == ProviderFileInventoryFamily::SourceImport
        }
    };
    if valid {
        Ok(())
    } else {
        Err(StoreError::ImportInventoryCheckpointTrustMismatch {
            field: "canonical effect scope",
        })
    }
}

fn validate_import_inventory_selection_versions(
    format_version: u32,
    algorithm_version: u32,
) -> Result<()> {
    if format_version != IMPORT_INVENTORY_SELECTION_FORMAT_VERSION {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection commitment format version is unsupported",
        ));
    }
    if algorithm_version != IMPORT_INVENTORY_SELECTION_ALGORITHM_VERSION {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection commitment algorithm version is unsupported",
        ));
    }
    Ok(())
}

fn validate_import_inventory_selection_commitment(
    commitment: ImportInventoryFrozenSelectionCommitment,
) -> Result<()> {
    validate_import_inventory_selection_versions(
        commitment.format_version,
        commitment.algorithm_version,
    )?;
    if (commitment.total_count == 0) != commitment.final_keyset.is_none() {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "selection final keyset does not match total count",
        ));
    }
    if commitment.total_count == 0
        && commitment.final_prefix
            != import_inventory_selection_initial_prefix(
                commitment.format_version,
                commitment.algorithm_version,
            )?
    {
        return Err(StoreError::InvalidImportInventoryCheckpoint(
            "empty selection commitment prefix is invalid",
        ));
    }
    Ok(())
}

fn hash_inventory_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn hash_inventory_optional_field(digest: &mut Sha256, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hash_inventory_field(digest, &[1]);
            hash_inventory_field(digest, value);
        }
        None => hash_inventory_field(digest, &[0]),
    }
}

fn hash_inventory_optional_text(digest: &mut Sha256, value: Option<&str>) {
    hash_inventory_optional_field(digest, value.map(str::as_bytes));
}

fn canonical_import_inventory_effect_fingerprint(
    effect: ImportInventoryCanonicalEffect<'_>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    hash_inventory_field(&mut digest, IMPORT_INVENTORY_EFFECT_PAYLOAD_DOMAIN);
    hash_inventory_field(&mut digest, canonical_effect_identity(effect).0.as_bytes());
    match effect {
        ImportInventoryCanonicalEffect::CatalogUpsert(session) => {
            hash_inventory_field(&mut digest, session.provider.as_str().as_bytes());
            hash_inventory_field(&mut digest, session.source_format.as_bytes());
            hash_inventory_field(&mut digest, session.source_root.as_bytes());
            hash_inventory_field(&mut digest, session.source_path.as_bytes());
            hash_inventory_optional_text(&mut digest, session.external_session_id.as_deref());
            hash_inventory_optional_text(
                &mut digest,
                session.parent_external_session_id.as_deref(),
            );
            hash_inventory_field(&mut digest, session.agent_type.as_str().as_bytes());
            hash_inventory_optional_text(&mut digest, session.role_hint.as_deref());
            hash_inventory_optional_text(&mut digest, session.external_agent_id.as_deref());
            hash_inventory_optional_text(&mut digest, session.cwd.as_deref());
            hash_inventory_optional_field(
                &mut digest,
                session
                    .session_started_at_ms
                    .as_ref()
                    .map(|value| value.to_be_bytes())
                    .as_ref()
                    .map(<[u8; 8]>::as_slice),
            );
            hash_inventory_field(&mut digest, &session.file_size_bytes.to_be_bytes());
            hash_inventory_field(&mut digest, &session.file_modified_at_ms.to_be_bytes());
            hash_inventory_field(&mut digest, &session.import_revision.to_be_bytes());
            hash_inventory_field(&mut digest, &session.cataloged_at_ms.to_be_bytes());
            hash_inventory_json(&mut digest, &session.metadata);
        }
        ImportInventoryCanonicalEffect::SourceImportUpsert(file) => {
            hash_inventory_field(&mut digest, file.provider.as_str().as_bytes());
            hash_inventory_field(&mut digest, file.source_format.as_bytes());
            hash_inventory_field(&mut digest, file.source_root.as_bytes());
            hash_inventory_field(&mut digest, file.source_path.as_bytes());
            hash_inventory_field(&mut digest, &file.file_size_bytes.to_be_bytes());
            hash_inventory_field(&mut digest, &file.file_modified_at_ms.to_be_bytes());
            hash_inventory_field(&mut digest, &file.import_revision.to_be_bytes());
            hash_inventory_field(&mut digest, &file.observed_at_ms.to_be_bytes());
            hash_inventory_json(&mut digest, &file.metadata);
        }
        ImportInventoryCanonicalEffect::CatalogStale {
            source_path,
            observed_at_ms,
        }
        | ImportInventoryCanonicalEffect::SourceImportStale {
            source_path,
            observed_at_ms,
        } => {
            hash_inventory_field(&mut digest, source_path.as_bytes());
            hash_inventory_field(&mut digest, &observed_at_ms.to_be_bytes());
        }
        ImportInventoryCanonicalEffect::CatalogRescan { source_path }
        | ImportInventoryCanonicalEffect::SourceImportRescan { source_path }
        | ImportInventoryCanonicalEffect::CatalogObservationRejected { source_path }
        | ImportInventoryCanonicalEffect::SourceImportObservationRejected { source_path } => {
            hash_inventory_field(&mut digest, source_path.as_bytes());
        }
    }
    digest.finalize().into()
}

fn hash_inventory_json(digest: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hash_inventory_field(digest, b"null"),
        Value::Bool(value) => {
            hash_inventory_field(digest, b"bool");
            hash_inventory_field(digest, &[u8::from(*value)]);
        }
        Value::Number(value) => {
            hash_inventory_field(digest, b"number");
            hash_inventory_field(digest, value.to_string().as_bytes());
        }
        Value::String(value) => {
            hash_inventory_field(digest, b"string");
            hash_inventory_field(digest, value.as_bytes());
        }
        Value::Array(values) => {
            hash_inventory_field(digest, b"array");
            hash_inventory_field(digest, &(values.len() as u64).to_be_bytes());
            for value in values {
                hash_inventory_json(digest, value);
            }
        }
        Value::Object(values) => {
            hash_inventory_field(digest, b"object");
            hash_inventory_field(digest, &(values.len() as u64).to_be_bytes());
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                hash_inventory_field(digest, key.as_bytes());
                hash_inventory_json(digest, &values[key]);
            }
        }
    }
}

fn validate_canonical_effect_payload_size(
    effect: ImportInventoryCanonicalEffect<'_>,
) -> Result<()> {
    let mut counter = InventoryPayloadByteCounter::new(IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES);
    match effect {
        ImportInventoryCanonicalEffect::CatalogUpsert(session) => {
            counter.add(session.source_format.len())?;
            counter.add(session.source_root.len())?;
            counter.add(session.source_path.len())?;
            for value in [
                session.external_session_id.as_deref(),
                session.parent_external_session_id.as_deref(),
                session.role_hint.as_deref(),
                session.external_agent_id.as_deref(),
                session.cwd.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                counter.add(value.len())?;
            }
            counter.add_json(&session.metadata)?;
        }
        ImportInventoryCanonicalEffect::SourceImportUpsert(file) => {
            counter.add(file.source_format.len())?;
            counter.add(file.source_root.len())?;
            counter.add(file.source_path.len())?;
            counter.add_json(&file.metadata)?;
        }
        ImportInventoryCanonicalEffect::CatalogStale { source_path, .. }
        | ImportInventoryCanonicalEffect::SourceImportStale { source_path, .. }
        | ImportInventoryCanonicalEffect::CatalogRescan { source_path }
        | ImportInventoryCanonicalEffect::SourceImportRescan { source_path }
        | ImportInventoryCanonicalEffect::CatalogObservationRejected { source_path }
        | ImportInventoryCanonicalEffect::SourceImportObservationRejected { source_path } => {
            counter.add(source_path.len())?;
        }
    }
    Ok(())
}

struct InventoryPayloadByteCounter {
    bytes: usize,
    max_bytes: usize,
    exceeded: bool,
}

impl InventoryPayloadByteCounter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: 0,
            max_bytes,
            exceeded: false,
        }
    }

    fn add(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.bytes.checked_add(bytes).ok_or(
            StoreError::ImportInventoryCheckpointPageTooLarge {
                max_bytes: self.max_bytes,
            },
        )?;
        if self.bytes > self.max_bytes {
            self.exceeded = true;
            return Err(StoreError::ImportInventoryCheckpointPageTooLarge {
                max_bytes: self.max_bytes,
            });
        }
        Ok(())
    }

    fn add_json(&mut self, value: &serde_json::Value) -> Result<()> {
        match serde_json::to_writer(&mut *self, value) {
            Ok(()) => Ok(()),
            Err(_) if self.exceeded => Err(StoreError::ImportInventoryCheckpointPageTooLarge {
                max_bytes: self.max_bytes,
            }),
            Err(error) => Err(StoreError::Json(error)),
        }
    }
}

impl std::io::Write for InventoryPayloadByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.add(buffer.len()).is_err() {
            return Err(std::io::Error::other(
                "durable inventory payload byte bound exceeded",
            ));
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn canonical_effect_identity<'a>(
    effect: ImportInventoryCanonicalEffect<'a>,
) -> (&'static str, &'a str) {
    match effect {
        ImportInventoryCanonicalEffect::CatalogUpsert(session) => {
            ("catalog_upsert", &session.source_path)
        }
        ImportInventoryCanonicalEffect::SourceImportUpsert(file) => {
            ("source_upsert", &file.source_path)
        }
        ImportInventoryCanonicalEffect::CatalogStale { source_path, .. } => {
            ("catalog_stale", source_path)
        }
        ImportInventoryCanonicalEffect::SourceImportStale { source_path, .. } => {
            ("source_stale", source_path)
        }
        ImportInventoryCanonicalEffect::CatalogRescan { source_path } => {
            ("catalog_rescan", source_path)
        }
        ImportInventoryCanonicalEffect::SourceImportRescan { source_path } => {
            ("source_rescan", source_path)
        }
        ImportInventoryCanonicalEffect::CatalogObservationRejected { source_path } => {
            ("catalog_rejected", source_path)
        }
        ImportInventoryCanonicalEffect::SourceImportObservationRejected { source_path } => {
            ("source_rejected", source_path)
        }
    }
}

fn checkpoint_inventory_family_str(family: ProviderFileInventoryFamily) -> &'static str {
    match family {
        ProviderFileInventoryFamily::Catalog => "catalog_sessions",
        ProviderFileInventoryFamily::SourceImport => "source_import_files",
    }
}

fn checkpoint_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidImportInventoryCheckpoint("checkpoint counter exceeds SQLite range")
    })
}

fn new_checkpoint_owner_token() -> Vec<u8> {
    Uuid::new_v4().as_bytes().to_vec()
}

fn lease_from_trust(
    trust: ImportInventoryCheckpointTrust<'_>,
    owner_id: &str,
    owner_epoch: u64,
    owner_token: Vec<u8>,
    lease_expires_at_ms: i64,
) -> ImportInventoryCheckpointLease {
    ImportInventoryCheckpointLease {
        run_id: trust.run_id.to_vec(),
        inventory_family: trust.inventory_family,
        provider: trust.provider,
        source_root: trust.source_root.to_owned(),
        inventory_generation: trust.inventory_generation,
        owner_id: owner_id.to_owned(),
        owner_epoch,
        owner_token,
        lease_expires_at_ms,
    }
}

fn owned_path_matches(
    owned: &ImportInventoryOwnedPathIdentity,
    borrowed: ImportInventoryNativePathIdentity<'_>,
) -> bool {
    owned.platform_tag == borrowed.platform_tag
        && owned.encoding_tag == borrowed.encoding_tag
        && owned.opaque_hash == borrowed.opaque_hash
}

fn finish_checkpoint_commit<T>(commit: CheckpointCommit<T>) -> Result<T> {
    match commit {
        CheckpointCommit::Value(value) => Ok(value),
        CheckpointCommit::Failure(error) => Err(error),
    }
}

fn inventory_checkpoint_interrupted(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Sql(rusqlite::Error::SqliteFailure(sqlite_error, _))
            if sqlite_error.code == rusqlite::ErrorCode::OperationInterrupted
    )
}

fn inventory_checkpoint_sqlite_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(sqlite_error, _)
            if matches!(
                sqlite_error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}
