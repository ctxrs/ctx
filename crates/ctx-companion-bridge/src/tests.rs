use super::*;

#[test]
fn detached_install_verifier_rejects_unsigned_input() {
    let expectations = ManagedPairExpectations::new(ReleaseChannel::Staging);
    let unsigned = br#"{"manifest_base64":"e30=","schema_version":1,"signature_base64":""}"#;
    assert!(matches!(
        verify_signed_managed_pair_envelope(&expectations, unsigned),
        Err(BridgeError::Verification(_))
    ));
}

#[test]
fn detached_install_verifier_embeds_the_canonical_contracts() {
    assert_eq!(
        crate::verifier::embedded_authority_for_tests(),
        include_bytes!("../../../contracts/ctx-managed-pair-release-authority-v1.json")
    );
    assert_eq!(
        crate::verifier::embedded_state_schema_for_tests(),
        include_bytes!("../../../contracts/ctx-managed-pair-state-v1.schema.json")
    );
    assert_eq!(
        crate::verifier::embedded_target_matrix_for_tests(),
        include_bytes!("../../../contracts/release-targets-v1.json")
    );
}
