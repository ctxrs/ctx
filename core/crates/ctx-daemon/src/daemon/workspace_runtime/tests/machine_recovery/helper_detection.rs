use super::*;

#[path = "helper_detection/fixtures.rs"]
mod fixtures;

use fixtures::HelperDetectionFixture;

#[test]
fn collect_ctx_managed_sandbox_helper_pids_matches_only_ctx_scoped_helpers() {
    let fixture = HelperDetectionFixture::new();
    let matches = collect_ctx_managed_sandbox_helper_pids(
        vec![
            (42, fixture.collection_gvproxy_command()),
            (77, fixture.matching_vfkit_command()),
            (88, fixture.host_gvproxy_command()),
        ],
        fixture.root_path(),
        fixture.machine_name(),
    );
    assert_eq!(matches, vec![42, 77]);
}

#[test]
fn ctx_managed_sandbox_helper_process_detection_matches_expected_shapes() {
    let fixture = HelperDetectionFixture::new();

    assert!(is_ctx_managed_sandbox_helper_process_command(
        &fixture.process_shape_gvproxy_command(),
        fixture.root_path(),
        fixture.machine_name()
    ));
    assert!(is_ctx_managed_sandbox_helper_process_command(
        &fixture.matching_vfkit_command(),
        fixture.root_path(),
        fixture.machine_name()
    ));
    assert!(!is_ctx_managed_sandbox_helper_process_command(
        &fixture.wrong_machine_vfkit_command(),
        fixture.root_path(),
        fixture.machine_name()
    ));
    assert!(!is_ctx_managed_sandbox_helper_process_command(
        &fixture.host_gvproxy_command(),
        fixture.root_path(),
        fixture.machine_name()
    ));
}

#[test]
fn collect_ctx_managed_sandbox_helper_pids_from_ps_output_matches_real_macos_shapes() {
    let fixture = HelperDetectionFixture::new();
    let ps_output = fixture.macos_ps_output_with_host_helper();

    let matches = collect_ctx_managed_sandbox_helper_pids_from_ps_output(
        &ps_output,
        fixture.root_path(),
        fixture.machine_name(),
    );
    assert_eq!(matches, vec![6622, 12484]);
}
