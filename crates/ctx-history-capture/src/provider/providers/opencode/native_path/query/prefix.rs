use super::*;

pub(super) struct OrderedPrefixBuild {
    pub(super) evidence: OpenCodeNativeOrderedPrefixEvidence,
    pub(super) comparison: Option<OpenCodeNativeRestartPrefixComparison>,
    pub(super) session_rows_read: u64,
    pub(super) event_rows_read: u64,
    pub(super) pro_rows_read: u64,
}

struct SequencePrefixBuild {
    evidence: OpenCodeNativeSequencePrefixEvidence,
    prior_prefix_matches: bool,
    rows_read: u64,
}

struct SequencePrefixAccumulator<'prior> {
    hasher: Sha256,
    empty_max_key_digest: String,
    count: u64,
    max_key_digest: String,
    prior: Option<&'prior OpenCodeNativeSequencePrefixEvidence>,
    observed_prior_prefix: Option<OpenCodeNativeSequencePrefixEvidence>,
}

impl<'prior> SequencePrefixAccumulator<'prior> {
    fn new(
        domain: &'static [u8],
        prior: Option<&'prior OpenCodeNativeSequencePrefixEvidence>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        let mut empty_max = Sha256::new();
        empty_max.update(domain);
        empty_max.update(b"empty-max-key");
        let empty_max_key_digest = super::super::schema::hex_digest(empty_max.finalize().into());
        let observed_prior_prefix = prior.filter(|evidence| evidence.count == 0).map(|_| {
            OpenCodeNativeSequencePrefixEvidence {
                count: 0,
                max_key_digest: empty_max_key_digest.clone(),
                rolling_digest: super::super::schema::hex_digest(hasher.clone().finalize().into()),
            }
        });
        Self {
            hasher,
            max_key_digest: empty_max_key_digest.clone(),
            empty_max_key_digest,
            count: 0,
            prior,
            observed_prior_prefix,
        }
    }

    fn observe(&mut self, key_digest: [u8; 32], unit_digest: [u8; 32]) -> Result<()> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode prefix evidence count overflowed",
            ))?;
        self.hasher.update(key_digest);
        self.hasher.update(unit_digest);
        self.max_key_digest = super::super::schema::hex_digest(key_digest);
        if self
            .prior
            .is_some_and(|evidence| evidence.count == self.count)
        {
            self.observed_prior_prefix = Some(OpenCodeNativeSequencePrefixEvidence {
                count: self.count,
                max_key_digest: self.max_key_digest.clone(),
                rolling_digest: super::super::schema::hex_digest(
                    self.hasher.clone().finalize().into(),
                ),
            });
        }
        Ok(())
    }

    fn finish(self, rows_read: u64) -> SequencePrefixBuild {
        let evidence = OpenCodeNativeSequencePrefixEvidence {
            count: self.count,
            max_key_digest: if self.count == 0 {
                self.empty_max_key_digest
            } else {
                self.max_key_digest
            },
            rolling_digest: super::super::schema::hex_digest(self.hasher.finalize().into()),
        };
        let prior_prefix_matches = self.prior.is_none_or(|prior| {
            self.observed_prior_prefix
                .as_ref()
                .is_some_and(|observed| observed == prior)
        });
        SequencePrefixBuild {
            evidence,
            prior_prefix_matches,
            rows_read,
        }
    }
}

pub(super) fn compute_ordered_prefix_evidence(
    connection: &Connection,
    prior: Option<&OpenCodeNativeOrderedPrefixEvidence>,
    profile: OpenCodeNativeProfile,
) -> Result<OrderedPrefixBuild> {
    let sessions = compute_session_prefix(connection, prior.map(|value| &value.sessions))?;
    let core_events = compute_core_event_prefix(connection, prior.map(|value| &value.core_events))?;
    let pro_units = match profile {
        OpenCodeNativeProfile::CoreOnly => SequencePrefixAccumulator::new(
            b"ctx-opencode-prefix-pro-units-v1\0",
            prior.map(|value| &value.pro_units),
        )
        .finish(0),
        OpenCodeNativeProfile::CoreAndPro => {
            compute_pro_prefix(connection, prior.map(|value| &value.pro_units))?
        }
    };
    let comparison = prior.map(|prior| OpenCodeNativeRestartPrefixComparison {
        prior_evidence_fingerprint: prior.fingerprint(),
        sessions_prefix_matches: sessions.prior_prefix_matches,
        core_events_prefix_matches: core_events.prior_prefix_matches,
        pro_units_prefix_matches: pro_units.prior_prefix_matches,
    });
    Ok(OrderedPrefixBuild {
        evidence: OpenCodeNativeOrderedPrefixEvidence {
            sessions: sessions.evidence,
            core_events: core_events.evidence,
            pro_units: pro_units.evidence,
        },
        comparison,
        session_rows_read: sessions.rows_read,
        event_rows_read: core_events.rows_read,
        pro_rows_read: pro_units.rows_read,
    })
}

fn compute_session_prefix(
    connection: &Connection,
    prior: Option<&OpenCodeNativeSequencePrefixEvidence>,
) -> Result<SequencePrefixBuild> {
    let mut accumulator =
        SequencePrefixAccumulator::new(b"ctx-opencode-prefix-sessions-v1\0", prior);
    let mut statement = connection.prepare(
        "select native_identity, time_created, content_digest
         from ordered_sessions
         order by scan_ordinal",
    )?;
    let mut rows = statement.query([])?;
    let mut rows_read = 0_u64;
    while let Some(row) = rows.next()? {
        rows_read = rows_read.saturating_add(1);
        let native_identity: String = row.get(0)?;
        let time_created: i64 = row.get(1)?;
        let content_digest: String = row.get(2)?;
        let mut key = Sha256::new();
        key.update(b"session-key");
        key.update(time_created.to_le_bytes());
        hash_str(&mut key, &native_identity);
        let mut unit = Sha256::new();
        unit.update(b"session-unit");
        hash_str(&mut unit, &native_identity);
        hash_str(&mut unit, &content_digest);
        accumulator.observe(key.finalize().into(), unit.finalize().into())?;
    }
    Ok(accumulator.finish(rows_read))
}

fn compute_core_event_prefix(
    connection: &Connection,
    prior: Option<&OpenCodeNativeSequencePrefixEvidence>,
) -> Result<SequencePrefixBuild> {
    let mut accumulator =
        SequencePrefixAccumulator::new(b"ctx-opencode-prefix-core-events-v1\0", prior);
    let mut statement = connection.prepare(
        "select native_identity, session_identity, order_tag, order_a, order_b,
                message_identity, projection
         from ordered_events
         order by scan_ordinal",
    )?;
    let mut rows = statement.query([])?;
    let mut rows_read = 0_u64;
    while let Some(row) = rows.next()? {
        rows_read = rows_read.saturating_add(1);
        let native_identity: String = row.get(0)?;
        let session_identity: String = row.get(1)?;
        let order_tag: i64 = row.get(2)?;
        let order_a: i64 = row.get(3)?;
        let order_b: i64 = row.get(4)?;
        let message_identity: String = row.get(5)?;
        let projection: Vec<u8> = row.get(6)?;
        let mut key = Sha256::new();
        key.update(b"core-event-key");
        hash_str(&mut key, &session_identity);
        key.update(order_tag.to_le_bytes());
        key.update(order_a.to_le_bytes());
        hash_str(&mut key, &message_identity);
        key.update(order_b.to_le_bytes());
        hash_str(&mut key, &native_identity);
        let mut unit = Sha256::new();
        unit.update(b"core-event-unit");
        hash_str(&mut unit, &native_identity);
        unit.update((projection.len() as u64).to_le_bytes());
        unit.update(projection);
        accumulator.observe(key.finalize().into(), unit.finalize().into())?;
    }
    Ok(accumulator.finish(rows_read))
}

fn compute_pro_prefix(
    connection: &Connection,
    prior: Option<&OpenCodeNativeSequencePrefixEvidence>,
) -> Result<SequencePrefixBuild> {
    let mut accumulator =
        SequencePrefixAccumulator::new(b"ctx-opencode-prefix-pro-units-v1\0", prior);
    let mut statement = connection.prepare(
        "select native_identity, subrecord_index, kind, call_id, tool_name, command,
                working_directory, outcome, exit_code, duration_ms, content, rejection
         from ordered_pro_units
         order by pro_ordinal",
    )?;
    let mut rows = statement.query([])?;
    let mut rows_read = 0_u64;
    while let Some(row) = rows.next()? {
        rows_read = rows_read.saturating_add(1);
        let native_identity: String = row.get(0)?;
        let subrecord_index: i64 = row.get(1)?;
        let mut key = Sha256::new();
        key.update(b"pro-unit-key");
        hash_str(&mut key, &native_identity);
        key.update(subrecord_index.to_le_bytes());
        let mut unit = Sha256::new();
        unit.update(b"pro-unit");
        hash_optional_i64(&mut unit, row.get(2)?);
        hash_optional_string(&mut unit, row.get(3)?);
        hash_optional_string(&mut unit, row.get(4)?);
        hash_optional_string(&mut unit, row.get(5)?);
        hash_optional_string(&mut unit, row.get(6)?);
        hash_optional_i64(&mut unit, row.get(7)?);
        hash_optional_i64(&mut unit, row.get(8)?);
        hash_optional_i64(&mut unit, row.get(9)?);
        hash_optional_bytes(&mut unit, row.get(10)?);
        hash_optional_string(&mut unit, row.get(11)?);
        accumulator.observe(key.finalize().into(), unit.finalize().into())?;
    }
    Ok(accumulator.finish(rows_read))
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_string(hasher: &mut Sha256, value: Option<String>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_str(hasher, &value);
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_bytes(hasher: &mut Sha256, value: Option<Vec<u8>>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value);
        }
        None => hasher.update([0]),
    }
}
