pub const CODEX_PROVIDER_ID: &str = "codex";
pub const CODEX_CRP_ADAPTER_ID: &str = "codex-crp";

#[cfg(test)]
mod tests {
    use super::{CODEX_CRP_ADAPTER_ID, CODEX_PROVIDER_ID};

    #[test]
    fn codex_provider_identity_is_not_an_adapter_alias() {
        assert_eq!(CODEX_PROVIDER_ID, "codex");
        assert_eq!(CODEX_CRP_ADAPTER_ID, "codex-crp");
        assert_ne!(CODEX_PROVIDER_ID, CODEX_CRP_ADAPTER_ID);
    }
}
