mod exact_locator_digest_tests {
    use super::super::*;

    #[test]
    fn exact_locator_digest_uses_canonical_length_prefixed_wire() {
        for (domain, value, expected) in [
            (
                EXACT_SOURCE_REVISION_DIGEST_DOMAIN,
                "mistral-vibe-session-v1:fixture",
                "25d78662f03d40916f48f7eb91ed1e151f203726f28e43154d5b83cd806c65ac",
            ),
            (
                EXACT_PATH_IDENTITY_DIGEST_DOMAIN,
                "unix:64513:42",
                "a36901bd6245237a6da04a9afa70ed8f5ffa18bece96493cfa9f8d3d9f31327e",
            ),
        ] {
            let actual = domain_digest(domain, value)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, expected);
        }
    }
}
