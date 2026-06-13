use super::*;

#[test]
fn container_machine_memory_profiles_scale_with_host_ram() {
    let mut settings = ContainerExecutionSettings::default();

    settings.machine.memory_profile = ctx_settings_model::ContainerMachineMemoryProfile::Economy;
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 48 * 1024),
        6144
    );

    settings.machine.memory_profile = ctx_settings_model::ContainerMachineMemoryProfile::Balanced;
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 48 * 1024),
        12 * 1024
    );

    settings.machine.memory_profile =
        ctx_settings_model::ContainerMachineMemoryProfile::Performance;
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 48 * 1024),
        24 * 1024
    );
}

#[test]
fn container_machine_memory_profiles_apply_expected_floors_and_caps() {
    let mut settings = ContainerExecutionSettings::default();

    settings.machine.memory_profile = ctx_settings_model::ContainerMachineMemoryProfile::Economy;
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 16 * 1024),
        4096
    );
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 128 * 1024),
        8192
    );

    settings.machine.memory_profile = ctx_settings_model::ContainerMachineMemoryProfile::Balanced;
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 16 * 1024),
        4096
    );
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 128 * 1024),
        16 * 1024
    );

    settings.machine.memory_profile =
        ctx_settings_model::ContainerMachineMemoryProfile::Performance;
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 16 * 1024),
        8192
    );
    assert_eq!(
        container_machine_memory_mb_for_host_memory(&settings, 128 * 1024),
        32 * 1024
    );
}

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
        launch_ready_gap_message(
            ContainerRuntimeKind::NativeContainer,
            "docker",
            true,
            false,
        ),
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
