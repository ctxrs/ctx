#[allow(unused_imports)]
use super::*;

pub(crate) const KIMI_CODE_CLI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".kimi-code"],
    source_format: "kimi_code_cli_wire_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];
