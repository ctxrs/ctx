use std::fs;

use super::*;

#[test]
fn integration_generation_is_idempotent_and_recovers_stale_temporaries() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("pair", 1, b"core", b"companion", b"pair-marker");
    let verifier = TestVerifier::new([(candidate.envelope.clone(), candidate.identity.clone())]);
    apply(&fixture, &candidate, &verifier);

    let integration = fixture.candidates.join("integrations.json");
    let bytes = br#"{"schema_version":1,"owners":[]}"#;
    fs::write(&integration, bytes).unwrap();
    let expected = fixture
        .install
        .join(format!("bin/ctx.install-integrations.{}", digest(bytes)));

    for _ in 0..2 {
        let (generation, sha256) = under_installation_lock(&fixture.install, || {
            publish_managed_pair_integration_generation_under_installation_lock(
                &fixture.install,
                &integration,
            )
            .unwrap()
        });
        assert_eq!(generation, expected);
        assert_eq!(sha256, digest(bytes));
    }
    assert_eq!(fs::read(&expected).unwrap(), bytes);
    assert!(!fixture
        .install
        .join("bin/ctx.install-integrations")
        .exists());
    let temporary = fixture.install.join("bin").join(format!(
        ".{}.new",
        expected.file_name().unwrap().to_string_lossy()
    ));
    for stale in [bytes.as_slice(), b"changed temporary", b""] {
        fs::remove_file(&expected).unwrap();
        fs::write(&temporary, stale).unwrap();
        under_installation_lock(&fixture.install, || {
            publish_managed_pair_integration_generation_under_installation_lock(
                &fixture.install,
                &integration,
            )
            .unwrap()
        });
        assert_eq!(fs::read(&expected).unwrap(), bytes);
    }
}
