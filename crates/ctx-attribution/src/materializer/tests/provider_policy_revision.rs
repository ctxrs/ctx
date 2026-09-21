//! Retained Core replay, not a new native capture or a relabeling of old rows.
use super::*;
use crate::core_materialization::provider_contract_test_support::{PROVIDERS, provider_record};

#[test]
fn provider_policy_replays_same_core_preserves_released_support_and_then_noops() -> TestResult {
    for provider in PROVIDERS {
        for released in [true, false] {
            let record = provider_record(provider, released)?;
            let prior = if released {
                IndexedCoreEventOriginKind::UniqueToSession
            } else {
                IndexedCoreEventOriginKind::Unknown
            };
            let expected = if provider == "gemini" && !released {
                IndexedCoreEventOriginKind::Unknown
            } else {
                IndexedCoreEventOriginKind::UniqueToSession
            };
            super::codex_policy_revision::assert_record_transition(
                record.clone(),
                "2026.09.15.1+codex-literal-patch-file-facts",
                prior,
                expected,
            )?;
            let mut copied = record.clone();
            copied.event_copy = Some(crate::protocol::ProviderNativeEventCopy {
                ancestor_session_id: provider_record(provider, true)?.parent_session_id.unwrap(),
                ancestor_event_id: stable_entity(&record.source, StableEntityKind::Event, 0x42)?,
                proof: crate::protocol::ProviderNativeCopyProof::NativeEventIdentity,
            });
            super::codex_policy_revision::assert_record_transition(
                copied,
                "2026.09.15.1+codex-literal-patch-file-facts",
                IndexedCoreEventOriginKind::CopiedFromAncestor,
                IndexedCoreEventOriginKind::CopiedFromAncestor,
            )?;
            let mut unsupported = record;
            unsupported.parser_revision.push_str("-unknown");
            super::codex_policy_revision::assert_record_transition(
                unsupported,
                "2026.09.15.1+codex-literal-patch-file-facts",
                IndexedCoreEventOriginKind::Unknown,
                IndexedCoreEventOriginKind::Unknown,
            )?;
        }
    }
    Ok(())
}
