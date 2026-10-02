use super::*;
use crate::core_materialization::provider_contract_test_support::{PROVIDERS, provider_record};
use crate::protocol::{
    ProviderNativeCopyProof, ProviderNativeEventCopy, SourceAnchor, SourceKey, TypedKey,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

macro_rules! current_positive {
    ($name:ident, $provider:literal) => {
        #[test]
        fn $name() -> TestResult {
            assert_eq!(
                producer_authority_disposition(&provider_record($provider, false)?),
                ProducerAuthorityDisposition::EligibleUnique
            );
            Ok(())
        }
    };
}
current_positive!(current_mux_child, "mux");
current_positive!(current_openclaw_legacy_child, "openclaw");
current_positive!(current_crush_child, "crush");
current_positive!(current_opencode_task_child, "opencode");
current_positive!(current_zed_child, "zed");

#[test]
fn current_provider_contracts_and_released_support_keep_exact_boundaries() -> TestResult {
    for provider in PROVIDERS {
        for released in [true, false] {
            let record = provider_record(provider, released)?;
            let expected = if provider == "gemini" && !released {
                ProducerAuthorityDisposition::AbstainUnknown
            } else {
                ProducerAuthorityDisposition::EligibleUnique
            };
            assert_eq!(
                producer_authority_disposition(&record),
                expected,
                "{provider} released={released}"
            );
            for missing in ["scope", "parent", "relationship"] {
                let mut miss = record.clone();
                match missing {
                    "scope" => miss.agent_scope = Some(AgentScope::Primary),
                    "parent" => miss.parent_session_id = None,
                    "relationship" => miss.session_relationship = None,
                    _ => unreachable!(),
                }
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} {missing}"
                );
            }
            for revision in [
                format!("{}-unknown", record.parser_revision),
                "unknown".to_owned(),
            ] {
                let mut miss = record.clone();
                miss.parser_revision = revision;
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} revision"
                );
            }
            for field in ["provider", "format", "schema", "identity"] {
                let mut miss = record.clone();
                let source = &record.source;
                miss.source = SourceKey::derive(
                    if field == "provider" {
                        "kilo"
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
                    if field == "identity" {
                        99
                    } else {
                        source.provider_identity_version()
                    },
                    SourceAnchor::provider_native("test", TypedKey::utf8("source")?)?,
                )?;
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} {field}"
                );
            }
            let mut copied = record.clone();
            // Copies win even over an unknown tuple or unresolved parent claim.
            copied.event_copy = Some(ProviderNativeEventCopy {
                ancestor_session_id: provider_record(provider, true)?.parent_session_id.unwrap(),
                ancestor_event_id: crate::protocol::derive_event_id(
                    crate::protocol::EventIdentityInput {
                        source: &record.source,
                        session_id: record.session_id,
                        logical_item_kind: "event",
                        native_item_key: &crate::protocol::NativeItemKey::native_id(
                            "event",
                            TypedKey::utf8("ancestor-event")?,
                        )?,
                        subrecord_selector: None,
                    },
                )?,
                proof: ProviderNativeCopyProof::NativeEventIdentity,
            });
            copied.validate_contract()?;
            assert_eq!(
                producer_authority_disposition(&copied),
                ProducerAuthorityDisposition::IneligibleCopied,
                "{provider}"
            );
            if !released {
                for field in ["session", "empty-session", "event", "null-event"] {
                    let mut miss = record.clone();
                    match field {
                        "session" => miss.provider_session_id = None,
                        "empty-session" => miss.provider_session_id = Some(String::new()),
                        "event" => miss.native_event_id = None,
                        "null-event" => miss.native_event_id = Some(TypedKey::Null),
                        _ => unreachable!(),
                    }
                    assert_eq!(
                        producer_authority_disposition(&miss),
                        ProducerAuthorityDisposition::AbstainUnknown,
                        "{provider} {field}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn only_current_mux_openclaw_allow_missing_root_and_no_provider_invents_relationship() -> TestResult
{
    for provider in PROVIDERS {
        let mut current = provider_record(provider, false)?;
        current.root_session_id = provider_record(provider, true)?.parent_session_id;
        assert_eq!(
            producer_authority_disposition(&current).permits_positive_authority(),
            provider != "gemini",
            "{provider}"
        );
        for relation in [
            None,
            Some(ProviderNativeSessionRelationship::Forked),
            Some(ProviderNativeSessionRelationship::ResumedFrom),
            Some(ProviderNativeSessionRelationship::WorkflowChild),
        ] {
            current.session_relationship = relation;
            assert_eq!(
                producer_authority_disposition(&current),
                ProducerAuthorityDisposition::AbstainUnknown,
                "{provider}"
            );
        }
    }
    for provider in ["mux", "openclaw"] {
        let mut old = provider_record(provider, true)?;
        old.root_session_id = None;
        assert_eq!(
            producer_authority_disposition(&old),
            ProducerAuthorityDisposition::AbstainUnknown
        );
    }
    // Gemini v2 cannot resolve a recording from a directory parent hint. Even a
    // fabricated edge must not turn this unreviewed identity-v2 tuple positive.
    let mut gemini = provider_record("gemini", false)?;
    gemini.parent_session_id = provider_record("gemini", true)?.parent_session_id;
    gemini.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    assert_eq!(
        producer_authority_disposition(&gemini),
        ProducerAuthorityDisposition::AbstainUnknown
    );
    Ok(())
}

#[test]
fn reviewed_provider_primary_shapes_do_not_require_child_lineage() -> TestResult {
    for provider in PROVIDERS {
        for released in [false, true] {
            let mut primary = provider_record(provider, released)?;
            let other_session =
                crate::protocol::derive_session_id(crate::protocol::SessionIdentityInput {
                    source: &primary.source,
                    logical_session_kind: "session",
                    native_session_key: &crate::protocol::NativeSessionKey::native_id(
                        "session",
                        TypedKey::utf8("ancestor")?,
                    )?,
                })?;
            primary.agent_scope = Some(AgentScope::Primary);
            primary.parent_session_id = None;
            primary.root_session_id = None;
            primary.session_relationship = None;
            primary.validate_contract()?;
            // The current reviewed emitters distinguish a native primary from
            // unknown lineage. Older child-only contracts and Gemini v2 remain
            // outside this proof; a provider label alone is not admission.
            let expected = if !released && provider != "gemini" {
                ProducerAuthorityDisposition::EligibleUnique
            } else {
                ProducerAuthorityDisposition::AbstainUnknown
            };
            assert_eq!(
                producer_authority_disposition(&primary),
                expected,
                "{provider} released={released}"
            );
            for missing in [
                "scope",
                "parent",
                "root",
                "relationship",
                "session",
                "event",
                "null-event",
                "revision",
            ] {
                let mut miss = primary.clone();
                match missing {
                    "scope" => miss.agent_scope = None,
                    "parent" => miss.parent_session_id = Some(other_session),
                    "root" => miss.root_session_id = Some(other_session),
                    "relationship" => {
                        miss.session_relationship =
                            Some(ProviderNativeSessionRelationship::Delegated)
                    }
                    "session" => miss.provider_session_id = None,
                    "event" => miss.native_event_id = None,
                    "null-event" => miss.native_event_id = Some(TypedKey::Null),
                    "revision" => miss.parser_revision.push_str("-unknown"),
                    _ => unreachable!(),
                }
                assert_eq!(
                    producer_authority_disposition(&miss),
                    ProducerAuthorityDisposition::AbstainUnknown,
                    "{provider} {missing}"
                );
            }
            primary.event_copy = Some(ProviderNativeEventCopy {
                ancestor_session_id: other_session,
                ancestor_event_id: crate::protocol::derive_event_id(
                    crate::protocol::EventIdentityInput {
                        source: &primary.source,
                        session_id: other_session,
                        logical_item_kind: "event",
                        native_item_key: &crate::protocol::NativeItemKey::native_id(
                            "event",
                            TypedKey::utf8("copied-event")?,
                        )?,
                        subrecord_selector: None,
                    },
                )?,
                proof: ProviderNativeCopyProof::NativeCallResultIdentity,
            });
            assert_eq!(
                producer_authority_disposition(&primary),
                ProducerAuthorityDisposition::IneligibleCopied,
                "{provider}"
            );
        }
    }
    Ok(())
}

#[test]
fn opencode_retained_and_migrated_tuples_preserve_native_primary_and_child_admission() -> TestResult
{
    for revision in [
        "opencode-family-source-backed-v12-known-file-carriers",
        "opencode-family-source-backed-v13-bounded-oversized-content",
        "opencode-family-source-backed-v14-proven-v2-overlap",
    ] {
        for primary in [false, true] {
            let mut record = provider_record("opencode", false)?;
            record.parser_revision = revision.to_owned();
            if primary {
                record.agent_scope = Some(AgentScope::Primary);
                record.parent_session_id = None;
                record.session_relationship = None;
            }
            record.validate_contract()?;
            assert_eq!(
                producer_authority_disposition(&record),
                ProducerAuthorityDisposition::EligibleUnique,
                "{revision} primary={primary}"
            );
            record.parser_revision.push_str("-unknown");
            assert_eq!(
                producer_authority_disposition(&record),
                ProducerAuthorityDisposition::AbstainUnknown
            );
        }
    }
    Ok(())
}
