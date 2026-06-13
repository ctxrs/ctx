use super::*;

use ctx_core::models::Session;
use ctx_providers::fake::FakeProviderAdapter;

#[path = "tests/title_generation.rs"]
mod title_generation_tests;

async fn setup_state() -> (crate::test_support::TestDaemonFixture, Session) {
    let providers = std::collections::HashMap::from([(
        "fake".to_string(),
        std::sync::Arc::new(FakeProviderAdapter::new())
            as std::sync::Arc<dyn ctx_providers::adapters::ProviderAdapter>,
    )]);
    let fixture =
        crate::test_support::TestDaemonFixture::with_providers(providers, "http://127.0.0.1:0")
            .await;
    let session = fixture
        .daemon()
        .seed_title_generation_session_for_test(fixture.data_root())
        .await
        .unwrap();

    (fixture, session)
}
