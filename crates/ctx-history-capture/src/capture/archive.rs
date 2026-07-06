#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ArchiveCounts {
    pub(crate) records: usize,
}

impl ArchiveCounts {
    pub(crate) fn add(&mut self, other: Self) {
        self.records += other.records;
    }
}

pub fn archive_from_envelopes(envelopes: &[CaptureEnvelope]) -> Result<SessionHistoryArchive> {
    let mut archive = SessionHistoryArchive::default();

    for envelope in envelopes {
        validate_envelope(envelope)?;
        if let Some(archive_value) = envelope.payload.get("archive") {
            let nested: SessionHistoryArchive = serde_json::from_value(archive_value.clone())?;
            archive.records.extend(nested.records);
            archive.capture_sources.extend(nested.capture_sources);
            archive.sessions.extend(nested.sessions);
            archive.runs.extend(nested.runs);
            archive.events.extend(nested.events);
            archive.artifact_records.extend(nested.artifact_records);
            archive.vcs_workspaces.extend(nested.vcs_workspaces);
            archive.vcs_changes.extend(nested.vcs_changes);
            archive
                .history_record_links
                .extend(nested.history_record_links);
            archive.summaries.extend(nested.summaries);
            archive.files_touched.extend(nested.files_touched);
            continue;
        }

        let record_value = envelope
            .payload
            .get("record")
            .filter(|value| value.is_object());
        let should_create_record =
            record_value.is_some() || payload_has_record_fields(&envelope.payload);

        if should_create_record {
            let value = record_value.unwrap_or(&envelope.payload);
            let record = record_from_envelope(envelope, value)?;
            archive.records.push(record);
        }
    }

    Ok(archive)
}

pub(crate) fn import_processing_file(path: &Path, store: &mut Store) -> Result<ArchiveCounts> {
    let envelopes = read_jsonl(path)?;
    let mut counts = ArchiveCounts::default();
    for envelope in envelopes {
        counts.add(import_envelope(store, &envelope)?);
    }
    Ok(counts)
}

pub(crate) fn import_envelope(
    store: &mut Store,
    envelope: &CaptureEnvelope,
) -> Result<ArchiveCounts> {
    let archive = archive_from_envelopes(std::slice::from_ref(envelope))?;
    let source_id = stable_capture_uuid(&envelope.dedupe_key, "source");
    store.import_archive_from_capture_source(
        &archive,
        source_id,
        &envelope.source,
        envelope.occurred_at,
        envelope.fidelity,
        true,
    )?;
    Ok(ArchiveCounts {
        records: archive.records.len(),
    })
}
