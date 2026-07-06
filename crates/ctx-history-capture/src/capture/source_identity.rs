#[allow(unused_imports)]
use super::*;

pub(crate) fn provider_scoped_source_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: Option<&str>,
) -> Uuid {
    stable_capture_uuid(
        &provider_scoped_source_identity_key(
            provider,
            provider_session_id,
            source_format,
            raw_source_path,
        ),
        "source",
    )
}
