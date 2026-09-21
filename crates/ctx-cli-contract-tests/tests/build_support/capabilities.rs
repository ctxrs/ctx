#[path = "../contracts/support/mod.rs"]
mod support;

#[test]
fn selected_support_capabilities_are_available() {
    let _fixture_initializer: fn(&std::path::Path) -> String =
        support::initialize_generation_only_core;
    let _release_key: fn() -> String = support::test_release_public_key_pem;
}
