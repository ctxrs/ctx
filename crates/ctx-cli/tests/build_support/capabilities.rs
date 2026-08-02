#[path = "../support/mod.rs"]
mod support;

#[test]
fn selected_support_capabilities_are_available() {
    let _fixture_initializer: fn(&std::path::Path) -> String =
        support::initialize_generation_only_core;
    let _release_key = support::TEST_RELEASE_PUBLIC_KEY_PEM;

    #[cfg(unix)]
    assert!(support::select_python3_interpreter("linux", |_| None, |_| false).is_none());
}
