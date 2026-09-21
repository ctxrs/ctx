use crate::protocol::{AgentScope, CoreRecord, ProviderNativeSessionRelationship};
use sha2::{Digest, Sha256};

use super::ProducerAuthorityDisposition;

const CRUSH_SOURCE_FORMAT_DIGEST: [u8; 32] = [
    0x51, 0xc2, 0xcb, 0xe1, 0x36, 0x41, 0x98, 0xed, 0x93, 0x07, 0x57, 0x5b, 0xca, 0x85, 0x0a, 0xa8,
    0x09, 0xec, 0xc9, 0x74, 0xe8, 0x26, 0x5a, 0x9f, 0xba, 0x5c, 0x55, 0x65, 0x61, 0x71, 0xde, 0xbb,
];
const CRUSH_SCHEMA_VARIANT_DIGEST: [u8; 32] = [
    0x07, 0x60, 0x58, 0x0f, 0x44, 0x46, 0xc4, 0x96, 0xd0, 0x17, 0x94, 0x06, 0xbe, 0x4d, 0x3b, 0x79,
    0x85, 0x7e, 0x25, 0x6b, 0xce, 0xe9, 0x6e, 0xf9, 0xd7, 0x1a, 0xf1, 0xa3, 0xce, 0x39, 0xa4, 0x0c,
];
const CRUSH_PARSER_REVISION_DIGEST: [u8; 32] = [
    0xfe, 0x8c, 0x36, 0x00, 0xba, 0xa1, 0x0b, 0x5d, 0x26, 0xe1, 0x1e, 0xb0, 0xd9, 0xdc, 0xdd, 0xda,
    0xd8, 0xc8, 0x1a, 0x99, 0x48, 0xa1, 0xf7, 0x6b, 0x7e, 0x97, 0xb9, 0x27, 0x3e, 0x37, 0x53, 0x43,
];
const OPENCODE_SOURCE_FORMAT_DIGEST: [u8; 32] = [
    0xa3, 0x70, 0x3c, 0xd2, 0x05, 0x92, 0xeb, 0xe7, 0x21, 0x71, 0x92, 0x57, 0xaa, 0xa9, 0x0c, 0x7b,
    0x4d, 0xb0, 0xd7, 0x80, 0xe3, 0xb3, 0x6c, 0x08, 0x75, 0x21, 0xa4, 0x72, 0x52, 0x55, 0xdb, 0x24,
];
const ZED_SOURCE_FORMAT_DIGEST: [u8; 32] = [
    0x0e, 0xd7, 0xa3, 0xda, 0xd8, 0xb4, 0xd3, 0x0a, 0x6e, 0xcf, 0xdb, 0x3e, 0x90, 0xe0, 0xc7, 0x22,
    0xdc, 0xe0, 0x13, 0x71, 0x3d, 0x68, 0xec, 0xd5, 0x27, 0x0b, 0x4f, 0x66, 0x03, 0xed, 0x1e, 0xe7,
];
const ZED_SCHEMA_VARIANT_DIGEST: [u8; 32] = [
    0x3d, 0x6f, 0xe4, 0xba, 0xba, 0xe9, 0xfa, 0x24, 0x5f, 0xc6, 0xb6, 0x3e, 0x04, 0xa5, 0xb5, 0xba,
    0xf4, 0x8a, 0xa8, 0x89, 0xfe, 0xc2, 0x55, 0x1d, 0x98, 0x1a, 0xf6, 0x02, 0x98, 0x58, 0x2f, 0x2d,
];
const CODEX_CORE_ACTIVITY_REVISION: &str = "codex-nativepath-core-activity-v15-revert-lineage";
const CODEX_RELEASED_V14_REVISION: &str =
    "codex-nativepath-core-activity-v14-literal-patch-file-facts";
const CODEX_RELEASED_V11_REVISION: &str = "codex-nativepath-core-activity-v11-item-call-identity";
const MUX_CORE_ACTIVITY_REVISION: &str = "mux-source-backed-v16-explicit-root-only";
const OPENCLAW_CORE_ACTIVITY_REVISION: &str =
    "openclaw-source-backed-v20-direct-parent-explicit-root";
const CRUSH_CORE_ACTIVITY_REVISION: &str = "crush-sqlite-source-backed-v5-record-rejections";
const OPENCODE_CORE_ACTIVITY_REVISION: &str =
    "opencode-family-source-backed-v12-known-file-carriers";
const ZED_CORE_ACTIVITY_REVISION: &str =
    "zed-nativepath-source-backed-v5-neutral-core-agent-scope-optional-admission";

fn protocol_label_matches(value: &str, expected: [u8; 32]) -> bool {
    let actual: [u8; 32] = Sha256::digest(value.as_bytes()).into();
    actual == expected
}

/// Recovers only provider contracts whose neutral Core fields are sufficient
/// to prove that the event is native to this session. Every near miss
/// deliberately abstains; an explicit copy proof always wins.
pub fn producer_authority_disposition(record: &CoreRecord) -> ProducerAuthorityDisposition {
    if record.event_copy.is_some() {
        return ProducerAuthorityDisposition::IneligibleCopied;
    }
    if record.agent_scope != Some(AgentScope::Subagent) || record.parent_session_id.is_none() {
        return ProducerAuthorityDisposition::AbstainUnknown;
    }

    let source = &record.source;
    let exact =
        |provider: &str, source_format: &str, schema_variant: &str, parser_revision: &str| {
            source.provider() == provider
                && source.source_format() == source_format
                && source.schema_variant() == schema_variant
                && source.provider_identity_version() == 1
                && record.parser_revision == parser_revision
        };

    let codex = [
        CODEX_CORE_ACTIVITY_REVISION,
        CODEX_RELEASED_V14_REVISION,
        CODEX_RELEASED_V11_REVISION,
    ]
    .into_iter()
    .any(|revision| {
        exact(
            "codex",
            "codex_session_jsonl",
            "codex-nativepath-jsonl-v0",
            revision,
        )
    }) && record
        .provider_session_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && record.native_event_id.is_some()
        && matches!(
            record.session_relationship,
            Some(
                ProviderNativeSessionRelationship::Delegated
                    | ProviderNativeSessionRelationship::Forked
                    | ProviderNativeSessionRelationship::ResumedFrom
                    | ProviderNativeSessionRelationship::WorkflowChild
            )
        );
    let delegated =
        record.session_relationship == Some(ProviderNativeSessionRelationship::Delegated);
    let opencode_schema = matches!(
        source.schema_variant(),
        "opencode-family-session_message_seq-v1"
            | "opencode-family-session_message_synthesized_seq-v1"
            | "opencode-family-session_entry-v1"
            | "opencode-family-legacy_message-v1"
            | "opencode-family-message_part-v1"
    );
    // These current emitters retain the native session/item and exact direct
    // parent. A root is optional, not a parent-derived fallback. Keep released
    // tuples below unchanged: retained Core need not be reimportable.
    // Gemini v2 deliberately has no resolved parent/relationship; neither its
    // header scope nor a directory hint establishes a unique parent recording.
    let current = record
        .provider_session_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
        && record
            .native_event_id
            .as_ref()
            .is_some_and(|key| *key != crate::protocol::TypedKey::Null)
        && (exact(
            "mux",
            "mux_session_jsonl",
            "mux-session-tree-source-backed-v2",
            MUX_CORE_ACTIVITY_REVISION,
        ) || exact(
            "openclaw",
            "openclaw_session_jsonl_tree",
            "openclaw-legacy-jsonl-v2",
            OPENCLAW_CORE_ACTIVITY_REVISION,
        ) || exact(
            "crush",
            "crush_sqlite",
            "crush-project-sqlite-v0",
            CRUSH_CORE_ACTIVITY_REVISION,
        ) || (opencode_schema
            && exact(
                "opencode",
                "opencode_sqlite",
                source.schema_variant(),
                OPENCODE_CORE_ACTIVITY_REVISION,
            ))
            || exact(
                "zed",
                "zed_threads_sqlite",
                "zed-nativepath-sqlite-v0",
                ZED_CORE_ACTIVITY_REVISION,
            ));
    let eligible = codex
        || delegated
            && (current
                || exact(
                    "gemini",
                    "gemini_cli_chat_recording_jsonl",
                    "gemini-nativepath-jsonl-v0",
                    "gemini-nativepath-core-activity-v1",
                )
                || (record.root_session_id.is_some()
                    && exact(
                        "mux",
                        "mux_session_jsonl",
                        "mux-session-tree-source-backed-v2",
                        "mux-source-backed-v7-core-activity",
                    ))
                || (record.root_session_id.is_some()
                    && exact(
                        "openclaw",
                        "openclaw_session_jsonl_tree",
                        "openclaw-legacy-jsonl-v2",
                        "openclaw-source-backed-v13-core-activity",
                    ))
                || (source.provider() == "crush"
                    && source.provider_identity_version() == 1
                    && protocol_label_matches(source.source_format(), CRUSH_SOURCE_FORMAT_DIGEST)
                    && protocol_label_matches(
                        source.schema_variant(),
                        CRUSH_SCHEMA_VARIANT_DIGEST,
                    )
                    && protocol_label_matches(
                        &record.parser_revision,
                        CRUSH_PARSER_REVISION_DIGEST,
                    ))
                || (source.provider() == "opencode"
                    && protocol_label_matches(
                        source.source_format(),
                        OPENCODE_SOURCE_FORMAT_DIGEST,
                    )
                    && source.provider_identity_version() == 1
                    && record.parser_revision == "opencode-family-source-backed-v9-neutral-core"
                    && opencode_schema)
                || (source.provider() == "zed"
                    && source.provider_identity_version() == 1
                    && protocol_label_matches(source.source_format(), ZED_SOURCE_FORMAT_DIGEST)
                    && protocol_label_matches(source.schema_variant(), ZED_SCHEMA_VARIANT_DIGEST)
                    && record.parser_revision == "zed-nativepath-source-backed-v3-neutral-core"));

    if eligible {
        ProducerAuthorityDisposition::EligibleUnique
    } else {
        ProducerAuthorityDisposition::AbstainUnknown
    }
}

#[cfg(test)]
#[path = "producer_authority_tests.rs"]
mod current_contract_tests;

#[cfg(test)]
mod tests {
    use crate::protocol::{
        EventIdentityInput, NativeItemKey, NativeSessionKey, ProviderNativeCopyProof,
        ProviderNativeEventCopy, SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
        derive_event_id, derive_session_id,
    };

    use super::*;

    fn record(
        provider: &str,
        source_format: &str,
        schema_variant: &str,
        parser_revision: &str,
    ) -> CoreRecord {
        let source = SourceKey::derive(
            provider,
            source_format,
            schema_variant,
            1,
            SourceAnchor::provider_native(
                "authority-test",
                TypedKey::utf8("source").expect("source key"),
            )
            .expect("source anchor"),
        )
        .expect("source");
        let session = |native: &str| {
            derive_session_id(SessionIdentityInput {
                source: &source,
                logical_session_kind: "thread",
                native_session_key: &NativeSessionKey::native_id(
                    "session",
                    TypedKey::utf8(native).expect("session key"),
                )
                .expect("native session"),
            })
            .expect("session")
        };
        let session_id = session("child");
        let parent_session_id = session("parent");
        let event_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: "message",
            native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(1))
                .expect("native event"),
            subrecord_selector: None,
        })
        .expect("event");
        let mut record = CoreRecord::new_selected(
            event_id,
            session_id,
            source,
            1,
            "message",
            parser_revision,
            "body",
        )
        .expect("record");
        record.parent_session_id = Some(parent_session_id);
        record.root_session_id = Some(parent_session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
        record.agent_scope = Some(AgentScope::Subagent);
        record
    }

    #[test]
    fn exact_provider_contract_is_eligible_but_near_misses_abstain() {
        let gemini = record(
            "gemini",
            "gemini_cli_chat_recording_jsonl",
            "gemini-nativepath-jsonl-v0",
            "gemini-nativepath-core-activity-v1",
        );
        assert_eq!(
            producer_authority_disposition(&gemini),
            ProducerAuthorityDisposition::EligibleUnique
        );

        let wrong_parser = record(
            "gemini",
            "gemini_cli_chat_recording_jsonl",
            "gemini-nativepath-jsonl-v0",
            "gemini-nativepath-core-activity-v2",
        );
        assert_eq!(
            producer_authority_disposition(&wrong_parser),
            ProducerAuthorityDisposition::AbstainUnknown
        );

        for eligible in [
            record(
                "crush",
                "crush_sqlite",
                "crush-project-sqlite-v0",
                "crush-sqlite-source-backed-v3-neutral-core",
            ),
            record(
                "opencode",
                "opencode_sqlite",
                "opencode-family-session_message_seq-v1",
                "opencode-family-source-backed-v9-neutral-core",
            ),
            record(
                "zed",
                "zed_threads_sqlite",
                "zed-nativepath-sqlite-v0",
                "zed-nativepath-source-backed-v3-neutral-core",
            ),
        ] {
            assert_eq!(
                producer_authority_disposition(&eligible),
                ProducerAuthorityDisposition::EligibleUnique
            );
        }

        let kilo = record(
            "kilo",
            "opencode_sqlite",
            "opencode-family-session_message_seq-v1",
            "opencode-family-source-backed-v9-neutral-core",
        );
        assert_eq!(
            producer_authority_disposition(&kilo),
            ProducerAuthorityDisposition::AbstainUnknown
        );
    }

    #[test]
    fn explicit_copy_is_ineligible_and_required_root_is_fail_closed() {
        let mut copied = record(
            "zed",
            "zed_threads_sqlite",
            "zed-nativepath-sqlite-v0",
            "zed-nativepath-source-backed-v3-neutral-core",
        );
        let ancestor_session_id = copied.parent_session_id.expect("parent");
        let ancestor_event_id = derive_event_id(EventIdentityInput {
            source: &copied.source,
            session_id: ancestor_session_id,
            logical_item_kind: "message",
            native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(2))
                .expect("native event"),
            subrecord_selector: None,
        })
        .expect("ancestor event");
        copied.event_copy = Some(ProviderNativeEventCopy {
            ancestor_session_id,
            ancestor_event_id,
            proof: ProviderNativeCopyProof::NativeEventIdentity,
        });
        assert_eq!(
            producer_authority_disposition(&copied),
            ProducerAuthorityDisposition::IneligibleCopied
        );

        let mut mux = record(
            "mux",
            "mux_session_jsonl",
            "mux-session-tree-source-backed-v2",
            "mux-source-backed-v7-core-activity",
        );
        mux.root_session_id = None;
        assert_eq!(
            producer_authority_disposition(&mux),
            ProducerAuthorityDisposition::AbstainUnknown
        );
    }

    #[test]
    fn exact_noncopied_codex_subagent_is_eligible_without_a_synthesized_root() {
        for admitted_revision in [
            "codex-nativepath-core-activity-v11-item-call-identity",
            "codex-nativepath-core-activity-v14-literal-patch-file-facts",
            "codex-nativepath-core-activity-v15-revert-lineage",
        ] {
            let mut codex = record(
                "codex",
                "codex_session_jsonl",
                "codex-nativepath-jsonl-v0",
                admitted_revision,
            );
            codex.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
            codex.root_session_id = None;
            codex.provider_session_id = Some("codex-child".to_owned());
            codex.native_event_id = Some(TypedKey::utf8("codex-event").expect("native event"));
            for relationship in [
                ProviderNativeSessionRelationship::Delegated,
                ProviderNativeSessionRelationship::Forked,
                ProviderNativeSessionRelationship::ResumedFrom,
                ProviderNativeSessionRelationship::WorkflowChild,
            ] {
                codex.session_relationship = Some(relationship);
                assert_eq!(
                    producer_authority_disposition(&codex),
                    ProducerAuthorityDisposition::EligibleUnique
                );
            }

            for revision in [
                "codex-nativepath-core-activity-v1",
                "codex-nativepath-core-activity-v10-item-completed-plan",
                "codex-nativepath-core-activity-v11-item-call-identity-unknown",
                "codex-nativepath-core-activity-v13-bounded-json-argument-facts",
                "codex-nativepath-core-activity-v14-literal-patch-file-facts-unknown",
                "codex-nativepath-core-activity-v15-revert-lineage-unknown",
            ] {
                codex.parser_revision = revision.to_owned();
                assert_eq!(
                    producer_authority_disposition(&codex),
                    ProducerAuthorityDisposition::AbstainUnknown
                );
            }
            codex.parser_revision = admitted_revision.to_owned();
            for field in ["provider", "format", "schema", "identity"] {
                let mut miss = codex.clone();
                let source = &codex.source;
                miss.source = SourceKey::derive(
                    if field == "provider" {
                        "fx"
                    } else {
                        source.provider()
                    },
                    if field == "format" {
                        "unknown"
                    } else {
                        source.source_format()
                    },
                    if field == "schema" {
                        "unknown"
                    } else {
                        source.schema_variant()
                    },
                    if field == "identity" { 2 } else { 1 },
                    SourceAnchor::provider_native("test", TypedKey::utf8("source").unwrap())
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{admitted_revision} {field}"
                );
            }
            for missing in [
                "scope",
                "parent",
                "relationship",
                "session",
                "empty-session",
                "event",
            ] {
                let mut near_miss = codex.clone();
                match missing {
                    "scope" => near_miss.agent_scope = Some(AgentScope::Primary),
                    "parent" => near_miss.parent_session_id = None,
                    "relationship" => near_miss.session_relationship = None,
                    "session" => near_miss.provider_session_id = None,
                    "empty-session" => near_miss.provider_session_id = Some(String::new()),
                    "event" => near_miss.native_event_id = None,
                    _ => unreachable!(),
                }
                assert_eq!(
                    producer_authority_disposition(&near_miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{missing}"
                );
            }
        }
    }

    #[test]
    fn exact_copied_codex_subagent_is_rejected_before_contract_admission() {
        for admitted_revision in [
            "codex-nativepath-core-activity-v11-item-call-identity",
            "codex-nativepath-core-activity-v14-literal-patch-file-facts",
            "codex-nativepath-core-activity-v15-revert-lineage",
        ] {
            let mut codex = record(
                "codex",
                "codex_session_jsonl",
                "codex-nativepath-jsonl-v0",
                admitted_revision,
            );
            codex.session_relationship = Some(ProviderNativeSessionRelationship::ResumedFrom);
            codex.root_session_id = None;
            codex.provider_session_id = Some("codex-child".to_owned());
            codex.native_event_id = Some(TypedKey::utf8("codex-event").expect("native event"));
            let ancestor_session_id = codex.parent_session_id.expect("parent");
            let ancestor_event_id = derive_event_id(EventIdentityInput {
                source: &codex.source,
                session_id: ancestor_session_id,
                logical_item_kind: "message",
                native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(3))
                    .expect("native event"),
                subrecord_selector: None,
            })
            .expect("ancestor event");
            codex.event_copy = Some(ProviderNativeEventCopy {
                ancestor_session_id,
                ancestor_event_id,
                proof: ProviderNativeCopyProof::NativeCallResultIdentity,
            });
            assert_eq!(
                producer_authority_disposition(&codex),
                ProducerAuthorityDisposition::IneligibleCopied
            );
            codex.parser_revision.push_str("-unknown");
            codex.parent_session_id = None;
            codex.agent_scope = None;
            assert_eq!(
                producer_authority_disposition(&codex),
                ProducerAuthorityDisposition::IneligibleCopied
            );
        }
    }
}
