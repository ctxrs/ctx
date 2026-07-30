#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRemoval {
    pub deletion: CertifiedSourceDeletion,
    pub inventory: CertifiedSourceInventory,
}

impl SourceRemoval {
    pub fn new(
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<Self, ProtocolError> {
        let removal = Self {
            deletion,
            inventory,
        };
        removal.validate()?;
        Ok(removal)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.inventory.observed_sources() > MAX_SOURCE_INVENTORY_SOURCES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source inventory witness exceeds its source count bound",
            ));
        }
        self.inventory
            .validate_contract()
            .map_err(|error| invalid_contract("source inventory witness", error))?;
        self.deletion
            .validate_contract()
            .map_err(|error| invalid_contract("source deletion witness", error))?;
        if !self.deletion.verifies(&self.inventory) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source deletion does not verify against its complete inventory witness",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub sources: Vec<CertifiedSource>,
    pub removals: Vec<SourceRemoval>,
}

impl SourceManifest {
    pub fn new(
        core_generation_id: impl Into<String>,
        mut sources: Vec<CertifiedSource>,
        mut removals: Vec<SourceRemoval>,
    ) -> Result<Self, ProtocolError> {
        sources.sort_by_key(source_identity_digest);
        removals.sort_by_key(|removal| removal.deletion.source().identity().digest());
        let manifest = Self {
            contract_version: SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: core_generation_id.into(),
            sources,
            removals,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != SOURCE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "source manifest does not match the materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        if self.sources.len() > MAX_SOURCE_MANIFEST_SOURCES
            || self.removals.len() > MAX_SOURCE_MANIFEST_REMOVALS
            || self.sources.len().saturating_add(self.removals.len()) > MAX_SOURCE_MANIFEST_SOURCES
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest exceeds its source or removal count bound",
            ));
        }
        let mut prior_source = None;
        for source in &self.sources {
            source
                .validate_contract()
                .map_err(|error| invalid_contract("certified source", error))?;
            let current = source_identity_digest(source);
            if prior_source.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source manifest sources must be sorted and unique by stable lineage",
                ));
            }
            prior_source = Some(current);
        }
        let retained = self
            .sources
            .iter()
            .map(source_identity_digest)
            .collect::<BTreeSet<_>>();
        let mut prior_removal = None;
        for removal in &self.removals {
            removal.validate()?;
            let current = removal.deletion.source().identity().digest();
            if retained.contains(&current) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source manifest cannot retain and delete the same stable lineage",
                ));
            }
            if prior_removal.is_some_and(|prior| prior >= current) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "source manifest removals must be sorted and unique by stable lineage",
                ));
            }
            prior_removal = Some(current);
        }
        Ok(())
    }

    fn validate_legacy_wire(&self) -> Result<(), ProtocolError> {
        validate_encoded_bound(
            self,
            MAX_SOURCE_MANIFEST_WIRE_BYTES,
            "source manifest exceeds its encoded byte bound",
        )
    }
}

/// Exact metadata-only identity admitted before source records are reread.
///
/// `core_generation_id` is the digest of the immutable Core generation
/// manifest. The explicit contract fields fail closed before any page is
/// accepted, while `aggregate_sha256` binds them to every ordered source and
/// removal entry without requiring a whole-manifest wire transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestHeader {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub generation_manifest_version: u32,
    pub identity_version: u16,
    pub lexical_schema_version: u32,
    pub lexical_analyzer_version: u32,
    pub policy_schema_hash: String,
    pub source_count: u32,
    pub removal_count: u32,
    pub page_count: u32,
    pub aggregate_sha256: String,
}

impl SourceManifestHeader {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core_generation_id: impl Into<String>,
        generation_manifest_version: u32,
        identity_version: u16,
        lexical_schema_version: u32,
        lexical_analyzer_version: u32,
        policy_schema_hash: impl Into<String>,
        page_count: u32,
        sources: &[CertifiedSource],
        removals: &[SourceRemoval],
    ) -> Result<Self, ProtocolError> {
        validate_manifest_entries(sources, removals)?;
        let source_count = u32::try_from(sources.len()).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest source count overflowed",
            )
        })?;
        let removal_count = u32::try_from(removals.len()).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest removal count overflowed",
            )
        })?;
        let mut header = Self {
            contract_version: SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: core_generation_id.into(),
            generation_manifest_version,
            identity_version,
            lexical_schema_version,
            lexical_analyzer_version,
            policy_schema_hash: policy_schema_hash.into(),
            source_count,
            removal_count,
            page_count,
            aggregate_sha256: String::new(),
        };
        header.aggregate_sha256 = source_manifest_aggregate_sha256(&header, sources, removals)?;
        header.validate()?;
        Ok(header)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != SOURCE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "source manifest header does not match the materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.policy_schema_hash, "source generation policy")?;
        validate_sha256(&self.aggregate_sha256, "source manifest aggregate")?;
        let sources = usize::try_from(self.source_count).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest source count overflowed",
            )
        })?;
        let removals = usize::try_from(self.removal_count).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest removal count overflowed",
            )
        })?;
        if sources > MAX_SOURCE_MANIFEST_SOURCES
            || removals > MAX_SOURCE_MANIFEST_REMOVALS
            || sources.saturating_add(removals) > MAX_SOURCE_MANIFEST_SOURCES
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest header exceeds its source or removal count bound",
            ));
        }
        let minimum_page_count = sources
            .div_ceil(MAX_SOURCE_MANIFEST_PAGE_ITEMS)
            .saturating_add(removals.div_ceil(MAX_SOURCE_MANIFEST_PAGE_ITEMS));
        let total_entries = sources.saturating_add(removals);
        if usize::try_from(self.page_count)
            .ok()
            .is_none_or(|page_count| page_count < minimum_page_count || page_count > total_entries)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest header page count is outside its bounded entry topology",
            ));
        }
        Ok(())
    }

    pub fn validate_contents(
        &self,
        sources: &[CertifiedSource],
        removals: &[SourceRemoval],
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        validate_manifest_entries(sources, removals)?;
        if usize::try_from(self.source_count).ok() != Some(sources.len())
            || usize::try_from(self.removal_count).ok() != Some(removals.len())
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest aggregate counts do not match admitted entries",
            ));
        }
        if source_manifest_aggregate_sha256(self, sources, removals)? != self.aggregate_sha256 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest aggregate digest does not match admitted entries",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "entries",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SourceManifestPageEntries {
    Sources(Vec<CertifiedSource>),
    Removals(Vec<SourceRemoval>),
}

impl SourceManifestPageEntries {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Sources(entries) => entries.len(),
            Self::Removals(entries) => entries.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One deterministic metadata-only page in a source-manifest admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestPage {
    pub contract_version: u16,
    pub core_generation_id: String,
    pub aggregate_sha256: String,
    pub previous_page_sha256: String,
    pub page_index: u32,
    pub item_index: u32,
    pub entries: SourceManifestPageEntries,
    pub page_sha256: String,
}

impl SourceManifestPage {
    pub fn new(
        header: &SourceManifestHeader,
        previous_page_sha256: impl Into<String>,
        page_index: u32,
        item_index: u32,
        entries: SourceManifestPageEntries,
    ) -> Result<Self, ProtocolError> {
        header.validate()?;
        if page_index >= header.page_count {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest page index exceeds its declared topology",
            ));
        }
        let mut page = Self {
            contract_version: SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            core_generation_id: header.core_generation_id.clone(),
            aggregate_sha256: header.aggregate_sha256.clone(),
            previous_page_sha256: previous_page_sha256.into(),
            page_index,
            item_index,
            entries,
            page_sha256: String::new(),
        };
        page.page_sha256 = source_manifest_page_sha256(&page)?;
        page.validate()?;
        Ok(page)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contract_version != SOURCE_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "source manifest page does not match the materialization contract",
            ));
        }
        validate_sha256(&self.core_generation_id, "Core generation ID")?;
        validate_sha256(&self.aggregate_sha256, "source manifest aggregate")?;
        validate_sha256(&self.previous_page_sha256, "source manifest previous page")?;
        validate_sha256(&self.page_sha256, "source manifest page")?;
        if self.entries.is_empty() || self.entries.len() > MAX_SOURCE_MANIFEST_PAGE_ITEMS {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "source manifest page exceeds its item count bound",
            ));
        }
        match &self.entries {
            SourceManifestPageEntries::Sources(sources) => {
                validate_manifest_entries(sources, &[])?;
            }
            SourceManifestPageEntries::Removals(removals) => {
                validate_manifest_entries(&[], removals)?;
            }
        }
        if source_manifest_page_sha256(self)? != self.page_sha256 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest page digest does not match its entries",
            ));
        }
        validate_encoded_bound(
            self,
            MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES,
            "source manifest page exceeds its encoded byte bound",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmissionCursor {
    pub core_generation_id: String,
    pub aggregate_sha256: String,
    pub next_page_previous_sha256: String,
    pub next_page_index: u32,
    pub next_source_index: u32,
    pub next_removal_index: u32,
}

impl SourceManifestAdmissionCursor {
    #[must_use]
    pub fn initial(header: &SourceManifestHeader) -> Self {
        Self {
            core_generation_id: header.core_generation_id.clone(),
            aggregate_sha256: header.aggregate_sha256.clone(),
            next_page_previous_sha256: source_manifest_initial_chain_sha256(header),
            next_page_index: 0,
            next_source_index: 0,
            next_removal_index: 0,
        }
    }

    pub fn validate_for(&self, header: &SourceManifestHeader) -> Result<(), ProtocolError> {
        header.validate()?;
        validate_sha256(
            &self.next_page_previous_sha256,
            "source manifest admission chain",
        )?;
        if self.core_generation_id != header.core_generation_id
            || self.aggregate_sha256 != header.aggregate_sha256
            || self.next_page_index > header.page_count
            || self.next_source_index > header.source_count
            || self.next_removal_index > header.removal_count
            || (self.next_source_index < header.source_count && self.next_removal_index != 0)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest admission cursor is outside its exact manifest",
            ));
        }
        let admitted_entries = self
            .next_source_index
            .checked_add(self.next_removal_index)
            .ok_or_else(|| {
                ProtocolError::new(
                    ErrorClass::Bounds,
                    "source manifest admission cursor count overflowed",
                )
            })?;
        if (self.next_page_index == 0) != (admitted_entries == 0)
            || self.next_page_index > admitted_entries
            || admitted_entries
                > self.next_page_index.saturating_mul(
                    u32::try_from(MAX_SOURCE_MANIFEST_PAGE_ITEMS).unwrap_or(u32::MAX),
                )
            || (self.next_page_index == header.page_count)
                != (self.next_source_index == header.source_count
                    && self.next_removal_index == header.removal_count)
            || (self.next_page_index == 0
                && self.next_page_previous_sha256 != source_manifest_initial_chain_sha256(header))
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest admission cursor has an invalid page topology",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn is_complete_for(&self, header: &SourceManifestHeader) -> bool {
        self.validate_for(header).is_ok()
            && self.next_source_index == header.source_count
            && self.next_removal_index == header.removal_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestAdmissionReceipt {
    pub header: SourceManifestHeader,
    pub page_count: u32,
    pub terminal_chain_sha256: String,
}

impl SourceManifestAdmissionReceipt {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.header.validate()?;
        validate_sha256(
            &self.terminal_chain_sha256,
            "source manifest terminal chain",
        )?;
        if self.page_count != self.header.page_count
            || (self.page_count == 0
                && self.terminal_chain_sha256 != source_manifest_initial_chain_sha256(&self.header))
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                "source manifest receipt does not match its exact page topology",
            ));
        }
        Ok(())
    }
}
