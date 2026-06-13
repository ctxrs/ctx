use super::*;

#[test]
fn launch_ready_gap_message_distinguishes_runtime_substrate_from_image() {
    assert_eq!(
        launch_ready_gap_message(
            ContainerRuntimeKind::SharedVmContainer,
            "avf-linux",
            false,
            false,
        ),
        "runtime prewarm completed but shared VM substrate for 'avf-linux' is not launch-ready"
    );
    assert_eq!(
        launch_ready_gap_message(
            ContainerRuntimeKind::SharedVmContainer,
            "avf-linux",
            true,
            false,
        ),
        "runtime prewarm completed but launch image for 'avf-linux' is not present in the shared VM runtime"
    );
    assert_eq!(
        launch_ready_gap_message(
            ContainerRuntimeKind::NativeContainer,
            "docker",
            false,
            false,
        ),
        "runtime prewarm completed but local sandbox runtime is not launch-ready for 'docker'"
    );
    assert_eq!(
        launch_ready_gap_message(ContainerRuntimeKind::NativeContainer, "docker", true, false),
        "runtime prewarm completed but launch image for 'docker' is not present in the local sandbox runtime"
    );
}

#[test]
fn launch_ready_detail_message_distinguishes_shared_vm_from_native_container() {
    assert_eq!(
        launch_ready_detail_message(&ContainerRuntimeKind::SharedVmContainer),
        "shared VM substrate and launch image are ready"
    );
    assert_eq!(
        launch_ready_detail_message(&ContainerRuntimeKind::NativeContainer),
        "local sandbox runtime and launch image are ready"
    );
}

#[test]
fn runtime_prewarm_ready_message_distinguishes_runtime_artifacts_from_launch_ready_state() {
    assert_eq!(
        runtime_prewarm_ready_message(&ContainerRuntimeKind::SharedVmContainer, false),
        "shared VM runtime artifacts are ready; launch image loads when the shared VM starts"
    );
    assert_eq!(
        runtime_prewarm_ready_message(&ContainerRuntimeKind::SharedVmContainer, true),
        "shared VM substrate and launch image are ready"
    );
    assert_eq!(
        runtime_prewarm_ready_message(&ContainerRuntimeKind::NativeContainer, false),
        "local sandbox runtime and launch image are ready"
    );
}

#[test]
fn workspace_launch_ready_message_mentions_runtime_type() {
    assert_eq!(
        workspace_launch_ready_message(&ContainerRuntimeKind::NativeContainer),
        "workspace sandbox is ready in the local sandbox runtime"
    );
    assert_eq!(
        workspace_launch_ready_message(&ContainerRuntimeKind::SharedVmContainer),
        "workspace sandbox is ready on the shared VM substrate"
    );
}
