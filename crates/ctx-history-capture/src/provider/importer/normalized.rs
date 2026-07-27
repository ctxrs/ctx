use std::collections::HashSet;

use ctx_history_core::{
    CaptureProvider, EventRole, EventType, ProviderCaptureEnvelope, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde_json::Value;

use crate::{ProviderFileTouchedEnvelope, ProviderImportFailure, ProviderImportSummary};

pub(super) fn filter_provider_capture_lines_without_real_session_messages(
    summary: &mut ProviderImportSummary,
    captures: &mut Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: &mut Vec<(usize, ProviderFileTouchedEnvelope)>,
) {
    let native_session_keys = captures
        .iter()
        .filter_map(|(_, capture)| provider_capture_policy_session_key(capture))
        .collect::<HashSet<_>>();
    if native_session_keys.is_empty() {
        return;
    }

    let real_session_keys = captures
        .iter()
        .filter_map(|(_, capture)| {
            let key = provider_capture_policy_session_key(capture)?;
            capture
                .event
                .as_ref()
                .is_some_and(provider_event_is_real_conversation_message)
                .then_some(key)
        })
        .collect::<HashSet<_>>();
    let rejected_session_keys = native_session_keys
        .difference(&real_session_keys)
        .cloned()
        .collect::<HashSet<_>>();
    if rejected_session_keys.is_empty() {
        return;
    }

    summary.skipped_sessions += rejected_session_keys.len();
    captures.retain(|(_, capture)| {
        let Some(key) = provider_capture_policy_session_key(capture) else {
            return true;
        };
        if !rejected_session_keys.contains(&key) {
            return true;
        }
        summary.skipped += 1;
        if capture.event.is_some() {
            summary.skipped_events += 1;
        }
        false
    });
    files_touched.retain(|(_, file)| {
        let Some(key) = provider_file_touch_policy_session_key(file) else {
            return true;
        };
        if !rejected_session_keys.contains(&key) {
            return true;
        }
        summary.skipped += 1;
        false
    });

    if real_session_keys.is_empty() && summary.failed == 0 {
        summary.failed += 1;
        summary.failures.push(ProviderImportFailure {
            line: 0,
            error: "provider source contained no real conversation message".to_owned(),
        });
    }
}

fn provider_capture_policy_session_key(capture: &ProviderCaptureEnvelope) -> Option<String> {
    provider_policy_session_key(
        capture.provider,
        &capture.source.trust,
        &capture.source.source_format,
        &capture.session.provider_session_id,
        capture.source.raw_source_path.as_deref(),
    )
}

fn provider_file_touch_policy_session_key(file: &ProviderFileTouchedEnvelope) -> Option<String> {
    provider_policy_session_key(
        file.provider,
        &ProviderSourceTrust::ProviderNative,
        &file.source_format,
        &file.provider_session_id,
        file.raw_source_path.as_deref(),
    )
}

fn provider_policy_session_key(
    provider: CaptureProvider,
    trust: &ProviderSourceTrust,
    source_format: &str,
    provider_session_id: &str,
    raw_source_path: Option<&str>,
) -> Option<String> {
    if provider == CaptureProvider::Custom
        || !matches!(
            trust,
            ProviderSourceTrust::ProviderNative | ProviderSourceTrust::ProviderExport
        )
    {
        return None;
    }
    Some(format!(
        "{}\0{}\0{}\0{}",
        provider.as_str(),
        source_format,
        provider_session_id,
        raw_source_path.unwrap_or_default()
    ))
}

pub(super) fn provider_capture_lines_have_real_message(
    captures: &[(usize, ProviderCaptureEnvelope)],
) -> bool {
    captures
        .iter()
        .filter(|(_, capture)| capture.provider != CaptureProvider::Custom)
        .all(|(_, capture)| {
            !matches!(
                capture.source.trust,
                ProviderSourceTrust::ProviderNative | ProviderSourceTrust::ProviderExport
            )
        })
        || captures.iter().any(|(_, capture)| {
            capture.provider != CaptureProvider::Custom
                && matches!(
                    capture.source.trust,
                    ProviderSourceTrust::ProviderNative | ProviderSourceTrust::ProviderExport
                )
                && capture
                    .event
                    .as_ref()
                    .is_some_and(provider_event_is_real_conversation_message)
        })
}

fn provider_event_is_real_conversation_message(event: &ProviderEventEnvelope) -> bool {
    event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        )
        && provider_event_payload_has_text(&event.payload)
}

fn provider_event_payload_has_text(payload: &Value) -> bool {
    payload
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("body")
                .and_then(|body| body.get("text"))
                .and_then(Value::as_str)
        })
        .is_some_and(|text| !text.trim().is_empty())
}
