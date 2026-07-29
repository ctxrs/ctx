#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginSourceManifestRequest {
    pub manifest: SourceManifest,
}

impl BeginSourceManifestRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.manifest.validate()?;
        self.manifest.validate_legacy_wire()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "begin source manifest request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestBegan {
    pub core_generation_id: String,
    pub materializer_revision: String,
    pub progress: Vec<SourceProgress>,
    pub replayed: bool,
}

impl SourceManifestBegan {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_progress_set(
            &self.progress,
            None,
            false,
            "source manifest begin progress",
        )?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest begin response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginSourceManifestAdmissionRequest {
    pub header: SourceManifestHeader,
}

impl BeginSourceManifestAdmissionRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.header.validate()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "begin source manifest admission request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmissionBegan {
    pub cursor: SourceManifestAdmissionCursor,
    pub replayed: bool,
}

impl SourceManifestAdmissionBegan {
    pub fn validate_for(&self, header: &SourceManifestHeader) -> Result<(), ProtocolError> {
        self.cursor.validate_for(header)?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest admission begin response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmitSourceManifestPageRequest {
    pub page: SourceManifestPage,
}

impl AdmitSourceManifestPageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.page.validate()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES,
            "source manifest page admission request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestPageAdmitted {
    pub cursor: SourceManifestAdmissionCursor,
    pub replayed: bool,
}

impl SourceManifestPageAdmitted {
    pub fn validate_for(&self, header: &SourceManifestHeader) -> Result<(), ProtocolError> {
        self.cursor.validate_for(header)?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest page admission response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishSourceManifestAdmissionRequest {
    pub header: SourceManifestHeader,
}

impl FinishSourceManifestAdmissionRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.header.validate()?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "finish source manifest admission request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmitted {
    pub receipt: SourceManifestAdmissionReceipt,
    pub materializer_revision: String,
    pub progress: Vec<SourceProgress>,
    pub replayed: bool,
}

impl SourceManifestAdmitted {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.receipt.validate()?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_progress_set(
            &self.progress,
            None,
            false,
            "admitted source manifest progress",
        )?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest admission response exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareSourceRequest {
    pub core_generation_id: String,
    pub source: SourceKey,
    pub certified_revision_sha256: String,
    pub materializer_revision: String,
    pub disposition: SourceDisposition,
    pub expected_prior: Option<SourceProgress>,
}

impl PrepareSourceRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("prepared source identity", error))?;
        validate_sha256(&self.certified_revision_sha256, "certified source revision")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        if let Some(prior) = &self.expected_prior {
            prior.validate()?;
            if prior.source.identity() != self.source.identity() {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "source preparation prior progress belongs to another lineage",
                ));
            }
        }
        match (self.disposition, self.expected_prior.as_ref()) {
            (SourceDisposition::NewSource, None) => Ok(()),
            (SourceDisposition::Resume, Some(prior))
                if prior.source.exact_descriptor_eq(&self.source)
                    && prior.certified_revision_sha256 == self.certified_revision_sha256
                    && prior.materializer_revision == self.materializer_revision =>
            {
                Ok(())
            }
            (SourceDisposition::Rewrite, Some(prior)) if prior.source_epoch < u64::MAX => Ok(()),
            (SourceDisposition::Rewrite, Some(_)) => Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source rewrite cannot advance an exhausted source epoch",
            )),
            _ => Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source disposition and expected prior progress disagree",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePrepared {
    pub core_generation_id: String,
    pub progress: SourceProgress,
    pub replayed: bool,
}

impl SourcePrepared {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.progress.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializeSourcePageRequest {
    pub core_generation_id: String,
    pub expected_prior: SourceProgress,
    pub next_frontier: Option<SourceFrontier>,
    pub terminal: bool,
    pub records: Vec<SourceRecord>,
}

impl MaterializeSourcePageRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.expected_prior.validate()?;
        if self.expected_prior.terminal {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "terminal source progress cannot accept another materialization page",
            ));
        }
        if let Some(frontier) = &self.next_frontier {
            frontier
                .validate_contract()
                .map_err(|error| invalid_contract("next source frontier", error))?;
        }
        if !self.terminal
            && (self.records.is_empty()
                || self.next_frontier.is_none()
                || self.next_frontier == self.expected_prior.frontier)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "nonterminal source page must contain records and advance its frontier",
            ));
        }
        if self.records.len() > MAX_SOURCE_RECORDS_PER_PAGE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source page exceeds its record count bound",
            ));
        }
        let mut prior_order = None;
        let mut event_ids = BTreeSet::new();
        let mut content_bytes = 0_usize;
        for record in &self.records {
            if !record
                .locator
                .source()
                .exact_descriptor_eq(&self.expected_prior.source)
            {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source page record belongs to another source descriptor",
                ));
            }
            let current = (record.metadata.event_sequence, record.event_id.digest());
            if prior_order.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "source page records must be in strict stable event order",
                ));
            }
            if !event_ids.insert(record.event_id.digest()) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source page contains a duplicate stable event ID",
                ));
            }
            content_bytes = content_bytes
                .checked_add(record.validate_and_count_bytes()?)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "source page transient-content byte total overflowed",
                    )
                })?;
            if content_bytes > MAX_SOURCE_CONTENT_BYTES_PER_PAGE {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "source page exceeds its transient-content byte bound",
                ));
            }
            prior_order = Some(current);
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_PAGE_WIRE_BYTES,
            "source page exceeds its encoded byte bound",
        )
    }

    #[must_use]
    pub fn next_progress(&self) -> SourceProgress {
        SourceProgress {
            source: self.expected_prior.source.clone(),
            source_epoch: self.expected_prior.source_epoch,
            certified_revision_sha256: self.expected_prior.certified_revision_sha256.clone(),
            frontier: self.next_frontier.clone(),
            materializer_revision: self.expected_prior.materializer_revision.clone(),
            terminal: self.terminal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePageMaterialized {
    pub core_generation_id: String,
    pub progress: SourceProgress,
    pub accepted_records: u32,
    pub materialized_facts: u32,
    pub replayed: bool,
}

impl SourcePageMaterialized {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.progress.validate()?;
        if self.accepted_records as usize > MAX_SOURCE_RECORDS_PER_PAGE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source page acknowledgement exceeds its record count bound",
            ));
        }
        if self.materialized_facts as usize
            > MAX_SOURCE_RECORDS_PER_PAGE.saturating_mul(MAX_SOURCE_FACTS_PER_RECORD)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source page acknowledgement exceeds its detector-fact count bound",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteSourceRequest {
    pub core_generation_id: String,
    pub removal: SourceRemoval,
    pub expected_prior: SourceProgress,
}

impl DeleteSourceRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.removal.validate()?;
        self.expected_prior.validate()?;
        if !self
            .removal
            .deletion
            .source()
            .exact_descriptor_eq(&self.expected_prior.source)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source deletion witness does not match the expected progress descriptor",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "delete source request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDeleted {
    pub core_generation_id: String,
    pub source: SourceKey,
    pub removed_source_epoch: u64,
    pub replayed: bool,
}

impl SourceDeleted {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        self.source
            .validate_contract()
            .map_err(|error| invalid_contract("deleted source identity", error))?;
        if self.removed_source_epoch == 0 {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "deleted source epoch must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishSourceManifestRequest {
    pub manifest: SourceManifest,
    pub expected_progress: Vec<SourceProgress>,
}

impl FinishSourceManifestRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.manifest.validate()?;
        self.manifest.validate_legacy_wire()?;
        validate_progress_set(
            &self.expected_progress,
            None,
            true,
            "finished source progress",
        )?;
        if self.expected_progress.len() != self.manifest.sources.len() {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "finish source manifest progress does not cover every retained source",
            ));
        }
        let mut materializer_revision = None;
        for (progress, source) in self.expected_progress.iter().zip(&self.manifest.sources) {
            if !progress
                .source
                .exact_descriptor_eq(source.observation().source())
                || progress.certified_revision_sha256 != certified_source_revision_sha256(source)?
                || progress.frontier.as_ref() != source.frontier()
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "finish source manifest progress does not match its certified source",
                ));
            }
            if materializer_revision
                .is_some_and(|revision: &str| revision != progress.materializer_revision)
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "finish source manifest progress mixes materializer revisions",
                ));
            }
            materializer_revision = Some(progress.materializer_revision.as_str());
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "finish source manifest request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishAdmittedSourceManifestRequest {
    pub admission: SourceManifestAdmissionReceipt,
    pub expected_progress: Vec<SourceProgress>,
}

impl FinishAdmittedSourceManifestRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.admission.validate()?;
        validate_progress_set(
            &self.expected_progress,
            None,
            true,
            "finished admitted source progress",
        )?;
        if self.expected_progress.len()
            != usize::try_from(self.admission.header.source_count).map_err(|_| {
                ProtocolError::new(
                    ErrorClass::Bounds,
                    "admitted source manifest source count overflowed",
                )
            })?
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "finish admitted source manifest progress does not cover every retained source",
            ));
        }
        let mut materializer_revision = None;
        for progress in &self.expected_progress {
            if materializer_revision
                .is_some_and(|revision: &str| revision != progress.materializer_revision)
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "finish admitted source manifest progress mixes materializer revisions",
                ));
            }
            materializer_revision = Some(progress.materializer_revision.as_str());
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "finish admitted source manifest request exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestReceipt {
    pub core_generation_id: String,
    pub manifest_aggregate_sha256: String,
    pub materializer_revision: String,
    pub progress: Vec<SourceProgress>,
}

impl SourceManifestReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.manifest_aggregate_sha256, "source manifest aggregate")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_progress_set(
            &self.progress,
            Some(&self.materializer_revision),
            true,
            "source manifest receipt progress",
        )?;
        validate_encoded_bound(
            self,
            MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source manifest receipt exceeds its encoded byte bound",
        )
    }
}

/// Compact, exact identity of one completed source-materialization receipt.
///
/// Status returns the complete receipt so the public host can validate it.
/// Queries carry this identity instead of repeating the receipt's potentially
/// large per-source progress set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestReceiptIdentity {
    pub core_generation_id: String,
    pub materializer_revision: String,
    pub receipt_sha256: String,
}

impl SourceManifestReceiptIdentity {
    pub fn from_receipt(receipt: &SourceManifestReceipt) -> Result<Self, ProtocolError> {
        receipt.validate()?;
        Ok(Self {
            core_generation_id: receipt.core_generation_id.clone(),
            materializer_revision: receipt.materializer_revision.clone(),
            receipt_sha256: source_manifest_receipt_sha256(receipt)?,
        })
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_identity(&self.materializer_revision, "source materializer revision")?;
        validate_sha256(&self.receipt_sha256, "source manifest receipt")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestFinished {
    pub receipt: SourceManifestReceipt,
    pub replayed: bool,
}

impl SourceManifestFinished {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.receipt.validate()
    }
}

pub fn certified_source_revision_sha256(source: &CertifiedSource) -> Result<String, ProtocolError> {
    source
        .validate_contract()
        .map_err(|error| invalid_contract("certified source", error))?;
    let bytes = serde_json::to_vec(source).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "certified source revision encoding failed",
        )
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn legacy_source_manifest_sha256(manifest: &SourceManifest) -> Result<String, ProtocolError> {
    manifest.validate()?;
    manifest.validate_legacy_wire()?;
    let bytes = serde_json::to_vec(manifest).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "legacy source manifest encoding failed",
        )
    })?;
    Ok(sha256_hex(&bytes))
}

pub fn source_manifest_receipt_sha256(
    receipt: &SourceManifestReceipt,
) -> Result<String, ProtocolError> {
    receipt.validate()?;
    let bytes = serde_json::to_vec(receipt).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "source manifest receipt encoding failed",
        )
    })?;
    Ok(sha256_hex(&bytes))
}
