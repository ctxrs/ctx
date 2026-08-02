use super::*;

pub(super) fn relevant_schema_evidence(schema: &OpenCodeNativeSchema) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-relevant-schema-v1\0");
    hash_str(&mut hasher, schema.family.label());
    hasher.update(schema.user_version.to_le_bytes());
    hasher.update([u8::from(schema.event_has_type)]);
    for column in ["parent_id", "directory", "branch", "agent"] {
        hash_str(&mut hasher, column);
        hasher.update([u8::from(schema.session_columns.contains(column))]);
    }
    hasher.finalize().to_vec()
}

pub(super) fn hash_session(hasher: &mut Sha256, session: &SourceSession) {
    hasher.update(b"session\0");
    hash_str(hasher, &session.native_identity);
    hash_optional_str(hasher, session.parent_native_identity.as_deref());
    hash_str(hasher, &session.root_native_identity);
    hash_optional_str(hasher, session.directory.as_deref());
    hash_optional_str(hasher, session.branch.as_deref());
    hash_optional_str(hasher, session.agent_identity.as_deref());
}

pub(super) fn hash_source_event(hasher: &mut Sha256, event: &SourceEventRow) {
    hasher.update(b"event\0");
    hash_str(hasher, &event.native_identity);
    hash_str(hasher, &event.message_identity);
    hash_str(hasher, &event.session_identity);
    hash_native_order(hasher, &event.native_order);
    hasher.update(event.time_created.to_le_bytes());
    hasher.update(event.time_updated.to_le_bytes());
    hasher.update(event.content_bytes.to_le_bytes());
    hasher.update([match projection_disposition(&event.projection) {
        ProjectionDisposition::Retained => 1,
        ProjectionDisposition::Rejected => 2,
        ProjectionDisposition::Ignored => 3,
    }]);
    hash_projection(hasher, &event.projection);
    event.source_data.hash_into(hasher);
    event.parent_source_data.hash_into(hasher);
}

fn hash_projection(hasher: &mut Sha256, projection: &OpenCodeJsonProjection) {
    match projection {
        OpenCodeJsonProjection::Retained(retained) => {
            hasher.update([1]);
            hash_str(hasher, &retained.effective_type);
            hash_str(hasher, &retained.role);
        }
        OpenCodeJsonProjection::Output(output) => {
            hasher.update([2]);
            if let Some(diagnostic) = &output.diagnostic {
                hasher.update([1]);
                hash_str(hasher, &diagnostic.effective_type);
                hash_str(hasher, &diagnostic.role);
            } else {
                hasher.update([0]);
            }
        }
        OpenCodeJsonProjection::ExcludedOutput => hasher.update([3]),
        OpenCodeJsonProjection::Rejected(kind) => {
            hasher.update([4, rejection_kind_tag(*kind)]);
        }
        OpenCodeJsonProjection::RejectedWithReason(kind, reason) => {
            hasher.update([5, rejection_kind_tag(*kind)]);
            hash_str(hasher, reason);
        }
    }
}

fn rejection_kind_tag(kind: OpenCodeNativeRejectionKind) -> u8 {
    match kind {
        OpenCodeNativeRejectionKind::MalformedJson => 1,
        OpenCodeNativeRejectionKind::MalformedResultJson => 2,
        OpenCodeNativeRejectionKind::UnsupportedStorageClass => 3,
        OpenCodeNativeRejectionKind::OversizedRetainedContent => 4,
        OpenCodeNativeRejectionKind::MissingSession => 5,
        OpenCodeNativeRejectionKind::MissingMessage => 6,
        OpenCodeNativeRejectionKind::SessionRelationshipMismatch => 7,
        OpenCodeNativeRejectionKind::UnknownRecordType => 8,
        OpenCodeNativeRejectionKind::InvalidTimestamp => 9,
    }
}

fn hash_native_order(hasher: &mut Sha256, order: &super::super::model::OpenCodeNativeOrder) {
    match order {
        super::super::model::OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            sequence,
            message_id,
        } => {
            hasher.update([1]);
            hash_str(hasher, session_id);
            hasher.update(sequence.to_le_bytes());
            hash_str(hasher, message_id);
        }
        super::super::model::OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            time_created,
            message_id,
        } => {
            hasher.update([2]);
            hash_str(hasher, session_id);
            hasher.update(time_created.to_le_bytes());
            hash_str(hasher, message_id);
        }
        super::super::model::OpenCodeNativeOrder::MessagePart {
            session_id,
            message_time_created,
            message_id,
            part_time_created,
            part_id,
        } => {
            hasher.update([3]);
            hash_str(hasher, session_id);
            hasher.update(message_time_created.to_le_bytes());
            hash_str(hasher, message_id);
            hasher.update(part_time_created.to_le_bytes());
            hash_str(hasher, part_id);
        }
    }
}

pub(super) fn projection_disposition(projection: &OpenCodeJsonProjection) -> ProjectionDisposition {
    match projection {
        OpenCodeJsonProjection::Retained(_) => ProjectionDisposition::Retained,
        OpenCodeJsonProjection::Output(output) if output.diagnostic.is_some() => {
            ProjectionDisposition::Retained
        }
        OpenCodeJsonProjection::Rejected(_) | OpenCodeJsonProjection::RejectedWithReason(_, _) => {
            ProjectionDisposition::Rejected
        }
        OpenCodeJsonProjection::Output(_) | OpenCodeJsonProjection::ExcludedOutput => {
            ProjectionDisposition::Ignored
        }
    }
}

fn hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_str(hasher: &mut Sha256, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

pub(super) fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}
