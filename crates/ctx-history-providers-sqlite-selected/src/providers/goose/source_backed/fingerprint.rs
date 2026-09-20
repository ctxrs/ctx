use sha2::{Digest, Sha256};

use super::GooseSourceBackedResultV0;
use crate::{
    fingerprint::{hash_optional_i64, hash_optional_text, hash_text, RelationFingerprint},
    providers::goose::{
        schema::{GooseNativeSchema, GooseSessionRow},
        stream::{GooseMessageCellDisposition, GooseScannedMessage, GooseScannedSession},
    },
};

const GOOSE_LOGICAL_DATABASE_DOMAIN: &[u8] = b"ctx.goose.logical-database.v3-neutral-activity\0";
const GOOSE_LOGICAL_SESSION_DOMAIN: &[u8] = b"ctx.goose.logical-session.v1\0";
const GOOSE_LOGICAL_MESSAGE_DOMAIN: &[u8] = b"ctx.goose.logical-message.v1\0";

pub(super) struct GooseLogicalFingerprint(RelationFingerprint);

impl GooseLogicalFingerprint {
    pub(super) fn new(schema: &GooseNativeSchema) -> Self {
        Self(RelationFingerprint::new(
            GOOSE_LOGICAL_DATABASE_DOMAIN,
            &schema.capability_digest,
        ))
    }

    pub(super) fn record_session(
        &mut self,
        session: &GooseScannedSession,
    ) -> GooseSourceBackedResultV0<()> {
        self.record(0, goose_session_evidence(session))
    }

    pub(super) fn record_message(
        &mut self,
        message: &GooseScannedMessage,
    ) -> GooseSourceBackedResultV0<()> {
        self.record(1, goose_message_evidence(message))
    }

    fn record(&mut self, relation: u8, evidence: [u8; 32]) -> GooseSourceBackedResultV0<()> {
        self.0.record(relation, evidence)
    }

    pub(super) fn finish(self) -> GooseSourceBackedResultV0<[u8; 32]> {
        Ok(self.0.finish())
    }
}

fn goose_session_evidence(session: &GooseScannedSession) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(GOOSE_LOGICAL_SESSION_DOMAIN);
    hash_optional_text(&mut digest, Some(&session.native_identity));
    digest.update(session.observed_bytes.to_be_bytes());
    digest.update([u8::from(session.storage_class_supported)]);
    if let Some(row) = &session.row {
        digest.update([1]);
        hash_session_row(&mut digest, row);
    } else {
        digest.update([0]);
    }
    digest.finalize().into()
}

fn hash_session_row(digest: &mut Sha256, row: &GooseSessionRow) {
    hash_text(digest, &row.id);
    for value in [
        row.name.as_deref(),
        row.description.as_deref(),
        row.session_type.as_deref(),
        row.working_dir.as_deref(),
        row.created_at.as_deref(),
        row.updated_at.as_deref(),
        row.extension_data.as_deref(),
        row.provider_name.as_deref(),
        row.model_config_json.as_deref(),
        row.goose_mode.as_deref(),
        row.archived_at.as_deref(),
        row.project_id.as_deref(),
        row.parent_session_id.as_deref(),
    ] {
        hash_optional_text(digest, value);
    }
    digest.update([u8::from(row.user_set_name)]);
    for value in [
        row.total_tokens,
        row.input_tokens,
        row.output_tokens,
        row.accumulated_total_tokens,
        row.accumulated_input_tokens,
        row.accumulated_output_tokens,
    ] {
        hash_optional_i64(digest, value);
    }
    match row.accumulated_cost {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn goose_message_evidence(message: &GooseScannedMessage) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(GOOSE_LOGICAL_MESSAGE_DOMAIN);
    digest.update(message.native_order.to_be_bytes());
    hash_text(&mut digest, &message.native_identity);
    hash_text(&mut digest, &message.session_identity);
    hash_text(&mut digest, &message.role);
    digest.update([message_disposition_code(message.disposition)]);
    if let Some(row) = message.logical_row_digest {
        digest.update([1]);
        digest.update(row);
    } else {
        digest.update([0]);
        digest.update(message.content_bytes.to_be_bytes());
    }
    digest.finalize().into()
}

fn message_disposition_code(disposition: GooseMessageCellDisposition) -> u8 {
    match disposition {
        GooseMessageCellDisposition::Retained => 0,
        GooseMessageCellDisposition::ToolOutput => 1,
        GooseMessageCellDisposition::MalformedJson => 5,
        GooseMessageCellDisposition::UnsupportedJsonRoot => 6,
        GooseMessageCellDisposition::NonObjectBlock => 7,
        GooseMessageCellDisposition::UnknownBlockType => 8,
        GooseMessageCellDisposition::OversizedRetainedContent => 9,
        GooseMessageCellDisposition::MissingSession => 10,
        GooseMessageCellDisposition::UnsupportedStorageClass => 11,
        GooseMessageCellDisposition::DuplicateBlockType => 12,
    }
}
