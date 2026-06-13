use super::*;

pub(in crate::daemon::workspace_runtime::tests) struct TestManagedAvfLinuxRuntimeFixtureGuard {
    _runtime: ctx_avf_linux_runtime::TestManagedAvfLinuxRuntimeSourceGuard,
    _image: TestManagedCtxHarnessImageSourceGuard,
}

pub(in crate::daemon::workspace_runtime::tests) fn avf_runtime_archive_bytes() -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut tar = tar::Builder::new(&mut encoder);
        let payload = b"rootfs";
        let mut header = tar::Header::new_gnu();
        header
            .set_path("runtime/rootfs.img")
            .expect("set AVF runtime tar path");
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, &payload[..])
            .expect("append AVF rootfs image");
        tar.finish().expect("finish AVF runtime tar");
    }
    encoder.finish().expect("finish AVF runtime gzip")
}

pub(in crate::daemon::workspace_runtime::tests) async fn install_test_managed_avf_linux_runtime_source(
) -> (TestManagedAvfLinuxRuntimeFixtureGuard, Vec<JoinHandle<()>>) {
    let archive_bytes = avf_runtime_archive_bytes();
    let kernel_bytes = b"kernel".to_vec();
    let initrd_bytes = b"initrd".to_vec();
    let guest_agent_bytes = b"guest-agent".to_vec();
    let egress_proxy_bytes = b"egress-proxy".to_vec();
    let container_stack_bytes = b"container-stack".to_vec();
    let (archive_url, archive_server) =
        spawn_static_http_server_with_suffix(archive_bytes.clone(), "guest-runtime.tar.gz").await;
    let (kernel_url, kernel_server) =
        spawn_static_http_server_with_suffix(kernel_bytes.clone(), "vmlinuz").await;
    let (initrd_url, initrd_server) =
        spawn_static_http_server_with_suffix(initrd_bytes.clone(), "initrd.img").await;
    let (guest_agent_url, guest_agent_server) = spawn_static_http_server_with_suffix(
        guest_agent_bytes.clone(),
        "ctx-avf-linux-guest-agent",
    )
    .await;
    let (egress_proxy_url, egress_proxy_server) =
        spawn_static_http_server_with_suffix(egress_proxy_bytes.clone(), "ctx-egress-proxy").await;
    let (container_stack_url, container_stack_server) = spawn_static_http_server_with_suffix(
        container_stack_bytes.clone(),
        "container-stack.tar.gz",
    )
    .await;
    let (image_guard, image_server) =
        install_test_managed_harness_image_source(b"ctx-harness-image".to_vec()).await;
    let source = bundled_assets::ManagedRuntimeSource {
        uri: archive_url,
        sha256: hex::encode(Sha256::digest(&archive_bytes)),
        version: "ubuntu-minimal-test".to_string(),
        bin: "rootfs.img".to_string(),
        helpers: [
            (
                "kernel".to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: kernel_url,
                    sha256: hex::encode(Sha256::digest(&kernel_bytes)),
                },
            ),
            (
                "initrd".to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: initrd_url,
                    sha256: hex::encode(Sha256::digest(&initrd_bytes)),
                },
            ),
            (
                "guest-agent".to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: guest_agent_url,
                    sha256: hex::encode(Sha256::digest(&guest_agent_bytes)),
                },
            ),
            (
                "egress-proxy".to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: egress_proxy_url,
                    sha256: hex::encode(Sha256::digest(&egress_proxy_bytes)),
                },
            ),
            (
                "container-stack".to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: container_stack_url,
                    sha256: hex::encode(Sha256::digest(&container_stack_bytes)),
                },
            ),
        ]
        .into_iter()
        .collect(),
    };
    let runtime_guard =
        ctx_avf_linux_runtime::override_managed_avf_linux_runtime_source_for_test(source);
    (
        TestManagedAvfLinuxRuntimeFixtureGuard {
            _runtime: runtime_guard,
            _image: image_guard,
        },
        vec![
            archive_server,
            kernel_server,
            initrd_server,
            guest_agent_server,
            egress_proxy_server,
            container_stack_server,
            image_server,
        ],
    )
}
