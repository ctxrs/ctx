mod routes;

use ctx_history_core::CaptureProvider;

use super::locator::{
    CompleteContentSourceFamily, VerifiedContentRole, COMPLETE_CONTENT_MAX_LOCATOR_KIND_BYTES,
    VERIFIED_CONTENT_PROFILE_MAX_BYTES,
};
use super::{jsonl, structured};

pub use routes::VERIFIED_CONTENT_ROUTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerifiedContentPlatform {
    Linux,
    MacOs,
    Windows,
    FreeBsd,
}

pub const VERIFIED_CONTENT_RELEASE_PLATFORMS: [VerifiedContentPlatform; 4] = [
    VerifiedContentPlatform::Linux,
    VerifiedContentPlatform::MacOs,
    VerifiedContentPlatform::Windows,
    VerifiedContentPlatform::FreeBsd,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifiedContentRouteStatus {
    Supported,
    NotNeeded,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedContentContract {
    pub family: CompleteContentSourceFamily,
    pub content_profile: &'static str,
    pub locator_kind: &'static str,
    pub fixture_reference: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedContentPlatformDisposition {
    pub platform: VerifiedContentPlatform,
    pub status: VerifiedContentRouteStatus,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedContentRoute {
    pub provider: CaptureProvider,
    pub source_format: &'static str,
    pub role: VerifiedContentRole,
    pub platform_dispositions: [VerifiedContentPlatformDisposition; 4],
    pub contracts: &'static [VerifiedContentContract],
}

impl VerifiedContentRoute {
    pub fn disposition(
        &self,
        platform: VerifiedContentPlatform,
    ) -> Option<&VerifiedContentPlatformDisposition> {
        self.platform_dispositions
            .iter()
            .find(|disposition| disposition.platform == platform)
    }
}

pub fn verified_content_profile(
    provider: CaptureProvider,
    source_format: &str,
    family: CompleteContentSourceFamily,
    role: VerifiedContentRole,
) -> Option<&'static str> {
    let source_format = verified_content_registry_source_format(provider, source_format);
    VERIFIED_CONTENT_ROUTES
        .iter()
        .flat_map(|route| {
            route
                .contracts
                .iter()
                .map(move |contract| (route, contract))
        })
        .find(|(route, contract)| {
            route.provider == provider
                && route.source_format == source_format
                && route.role == role
                && verified_content_route_is_supported(route)
                && contract.family == family
        })
        .map(|(_, contract)| contract.content_profile)
}

/// Returns the profile bound to one exact provider/family/role/address tuple.
pub fn verified_content_profile_for_locator(
    provider: CaptureProvider,
    source_format: &str,
    family: CompleteContentSourceFamily,
    role: VerifiedContentRole,
    locator_kind: &str,
) -> Option<&'static str> {
    let source_format = verified_content_registry_source_format(provider, source_format);
    VERIFIED_CONTENT_ROUTES
        .iter()
        .filter(|route| {
            route.provider == provider
                && route.source_format == source_format
                && route.role == role
                && verified_content_route_is_supported(route)
        })
        .flat_map(|route| route.contracts.iter())
        .find(|contract| contract.family == family && contract.locator_kind == locator_kind)
        .map(|contract| contract.content_profile)
}

pub fn verified_content_profile_matches(
    profile: &str,
    provider: CaptureProvider,
    source_format: &str,
    family: CompleteContentSourceFamily,
    role: VerifiedContentRole,
) -> bool {
    verified_content_profile(provider, source_format, family, role) == Some(profile)
}

pub fn verified_content_route_matches(
    profile: &str,
    provider: CaptureProvider,
    source_format: &str,
    family: CompleteContentSourceFamily,
    role: VerifiedContentRole,
    locator_kind: &str,
) -> bool {
    let source_format = verified_content_registry_source_format(provider, source_format);
    VERIFIED_CONTENT_ROUTES.iter().any(|route| {
        route.provider == provider
            && route.source_format == source_format
            && route.role == role
            && verified_content_route_is_supported(route)
            && route.contracts.iter().any(|contract| {
                contract.content_profile == profile
                    && contract.family == family
                    && contract.locator_kind == locator_kind
            })
    })
}

pub fn verified_content_route_supported(
    provider: CaptureProvider,
    source_format: &str,
    family: CompleteContentSourceFamily,
    role: VerifiedContentRole,
) -> bool {
    let source_format = verified_content_registry_source_format(provider, source_format);
    VERIFIED_CONTENT_ROUTES.iter().any(|route| {
        route.provider == provider
            && route.source_format == source_format
            && route.role == role
            && verified_content_route_is_supported(route)
            && route
                .contracts
                .iter()
                .any(|contract| contract.family == family)
    })
}

pub fn verified_content_address_supported(
    provider: CaptureProvider,
    source_format: &str,
    family: CompleteContentSourceFamily,
    role: VerifiedContentRole,
    locator_kind: &str,
) -> bool {
    let source_format = verified_content_registry_source_format(provider, source_format);
    VERIFIED_CONTENT_ROUTES.iter().any(|route| {
        route.provider == provider
            && route.source_format == source_format
            && route.role == role
            && verified_content_route_is_supported(route)
            && route
                .contracts
                .iter()
                .any(|contract| contract.family == family && contract.locator_kind == locator_kind)
    })
}

pub(super) fn verified_content_contract_exists(
    profile: &str,
    role: VerifiedContentRole,
    family: CompleteContentSourceFamily,
    locator_kind: &str,
) -> bool {
    VERIFIED_CONTENT_ROUTES.iter().any(|route| {
        route.role == role
            && route
                .platform_dispositions
                .iter()
                .any(|disposition| disposition.status == VerifiedContentRouteStatus::Supported)
            && route.contracts.iter().any(|contract| {
                contract.content_profile == profile
                    && contract.family == family
                    && contract.locator_kind == locator_kind
            })
    })
}

fn verified_content_route_is_supported(route: &VerifiedContentRoute) -> bool {
    current_verified_content_platform()
        .and_then(|platform| route.disposition(platform))
        .is_some_and(|disposition| disposition.status == VerifiedContentRouteStatus::Supported)
}

fn current_verified_content_platform() -> Option<VerifiedContentPlatform> {
    if cfg!(target_os = "linux") {
        Some(VerifiedContentPlatform::Linux)
    } else if cfg!(target_os = "macos") {
        Some(VerifiedContentPlatform::MacOs)
    } else if cfg!(target_os = "windows") {
        Some(VerifiedContentPlatform::Windows)
    } else if cfg!(target_os = "freebsd") {
        Some(VerifiedContentPlatform::FreeBsd)
    } else {
        None
    }
}

/// Adapters normalize individual source files under several public tree
/// formats. Registry policy remains keyed by the public matrix format; this
/// bounded mapping is the sole file-to-tree alias seam.
fn verified_content_registry_source_format(provider: CaptureProvider, source_format: &str) -> &str {
    match (provider, source_format) {
        (CaptureProvider::Codex, crate::CODEX_SESSION_SOURCE_FORMAT) => "codex_session_jsonl_tree",
        (CaptureProvider::Qoder, crate::QODER_SOURCE_FORMAT) => "qoder_transcript_jsonl_tree",
        (CaptureProvider::Cursor, crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT) => {
            "cursor_agent_transcript_jsonl_tree"
        }
        (CaptureProvider::Windsurf, crate::WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT) => {
            "windsurf_cascade_hook_transcript_jsonl_tree"
        }
        (CaptureProvider::QwenCode, crate::QWEN_CODE_SOURCE_FORMAT) => "qwen_code_chat_jsonl_tree",
        (CaptureProvider::KimiCodeCli, crate::KIMI_CODE_CLI_SOURCE_FORMAT) => {
            "kimi_code_cli_wire_jsonl_tree"
        }
        (CaptureProvider::MistralVibe, crate::MISTRAL_VIBE_SOURCE_FORMAT) => {
            "mistral_vibe_session_jsonl_tree"
        }
        (CaptureProvider::Mux, crate::MUX_SOURCE_FORMAT) => "mux_session_jsonl_tree",
        _ => source_format,
    }
}

pub(super) fn valid_content_profile(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= VERIFIED_CONTENT_PROFILE_MAX_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

pub(super) fn valid_opaque_locator(
    family: CompleteContentSourceFamily,
    kind: &str,
    value: &[u8],
) -> bool {
    match (family, kind) {
        (CompleteContentSourceFamily::Jsonl, "jsonl-range-v1") => valid_jsonl_range(value),
        (CompleteContentSourceFamily::Jsonl, "jsonl-exact-range-v1") => {
            value.len() == 80 && valid_jsonl_range(&value[..16])
        }
        (CompleteContentSourceFamily::Jsonl, "junie-jsonl-record-set-v1") => {
            jsonl::valid_junie_record_set_locator(value)
        }
        (CompleteContentSourceFamily::Jsonl, "mux-record-v1") => jsonl::valid_mux_locator(value),
        (CompleteContentSourceFamily::Structured, "structured-message-v1") => {
            structured::decode_structured_locator(value).is_some()
        }
        (CompleteContentSourceFamily::Structured, "structured-result-v1") => {
            structured::decode_structured_result_locator(value).is_some()
        }
        (CompleteContentSourceFamily::Sqlite, "firebender-chat-session-row-v1")
        | (CompleteContentSourceFamily::Sqlite, "zed-thread-row-v1")
        | (CompleteContentSourceFamily::Sqlite, "lingma-chat-record-v1")
        | (CompleteContentSourceFamily::Sqlite, "forgecode-conversation-row-v1") => {
            value.len() == 8
        }
        (CompleteContentSourceFamily::Sqlite, "kiro-conversation-row-v1") => {
            value.len() == 9 && matches!(value.first(), Some(1 | 2))
        }
        (CompleteContentSourceFamily::Sqlite, "crush-sqlite-row-v1")
        | (CompleteContentSourceFamily::Sqlite, "goose-logical-row-v3")
        | (CompleteContentSourceFamily::Sqlite, "hermes-sqlite-row-v1") => {
            value.len() == 9 && value.first() == Some(&2)
        }
        (CompleteContentSourceFamily::Sqlite, "opencode-sqlite-logical-row-v1") => {
            value.len() == 10 && matches!(value.first(), Some(1..=4)) && value.last() == Some(&2)
        }
        (CompleteContentSourceFamily::Sqlite, "deepagents-write-message-v1") => {
            crate::provider::providers::deepagents::decode_deepagents_content_address(value)
                .is_some()
        }
        (CompleteContentSourceFamily::Sqlite, "astrbot-conversation-message-v1") => {
            value.len() == 12
        }
        (CompleteContentSourceFamily::Sqlite, "trae-itemtable-message-v1") => value.len() == 10,
        (CompleteContentSourceFamily::Sqlite, "warp-task-message-v1") => value.len() == 12,
        (CompleteContentSourceFamily::Sqlite, "shelley-compound-message-row-v1") => {
            value.len() == 17 && matches!(value.first(), Some(1 | 2))
        }
        (CompleteContentSourceFamily::Sqlite, "nanoclaw-project-message-v1") => {
            crate::captured_batch::NativeLocator::new(kind, value.to_vec())
                .ok()
                .and_then(|locator| {
                    crate::provider::providers::nanoclaw::decode_nanoclaw_message_locator(&locator)
                        .ok()
                })
                .is_some()
        }
        _ => false,
    }
}

fn valid_jsonl_range(value: &[u8]) -> bool {
    if value.len() != 16 {
        return false;
    }
    let Some(start) = value[..8].try_into().ok().map(u64::from_be_bytes) else {
        return false;
    };
    let Some(end) = value[8..].try_into().ok().map(u64::from_be_bytes) else {
        return false;
    };
    start < end
}

pub(super) fn valid_locator_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= COMPLETE_CONTENT_MAX_LOCATOR_KIND_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn encode_hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty()
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}
