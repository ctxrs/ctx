use super::*;

#[path = "virtualization/queue.rs"]
mod queue;
#[path = "virtualization/sizing.rs"]
mod sizing;

pub(super) use queue::*;
pub(super) use sizing::*;

#[cfg(target_os = "macos")]
fn build_shared_data_root_device(
    data_root: &Path,
) -> Result<Retained<VZVirtioFileSystemDeviceConfiguration>> {
    let shared_dir_url = file_url_for_path(data_root);
    let shared_dir = unsafe {
        VZSharedDirectory::initWithURL_readOnly(VZSharedDirectory::alloc(), &shared_dir_url, false)
    };
    let share = unsafe {
        VZSingleDirectoryShare::initWithDirectory(VZSingleDirectoryShare::alloc(), &shared_dir)
    };
    let device = unsafe {
        VZVirtioFileSystemDeviceConfiguration::initWithTag(
            VZVirtioFileSystemDeviceConfiguration::alloc(),
            &NSString::from_str(SHARED_VM_DATA_ROOT_SHARE_TAG),
        )
    };
    unsafe {
        device.setShare(Some(share.as_super()));
    }
    Ok(device)
}

#[cfg(target_os = "macos")]
pub(super) fn validate_real_avf_linux_vm_configuration(
    data_root: &Path,
    rootfs_image: &Path,
    data_disk_image: &Path,
    kernel_path: &Path,
    initrd_path: &Path,
    kernel_cmdline: &str,
) -> Result<String> {
    if !unsafe { VZVirtualMachine::isSupported() } {
        bail!("Virtualization.framework reported that virtualization is unavailable on this host");
    }

    let kernel_url = file_url_for_path(kernel_path);
    let initrd_url = file_url_for_path(initrd_path);
    let rootfs_url = file_url_for_path(rootfs_image);
    let data_disk_url = file_url_for_path(data_disk_image);

    let boot_loader =
        unsafe { VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url) };
    unsafe {
        boot_loader.setCommandLine(&NSString::from_str(kernel_cmdline));
        boot_loader.setInitialRamdiskURL(Some(&initrd_url));
    }

    let root_storage_attachment = unsafe {
        VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &rootfs_url,
            false,
            VZDiskImageCachingMode::Automatic,
            VZDiskImageSynchronizationMode::Fsync,
        )
    }
    .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    let root_storage_device = unsafe {
        VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            root_storage_attachment.as_super(),
        )
    };
    let data_storage_attachment = unsafe {
        VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &data_disk_url,
            false,
            VZDiskImageCachingMode::Automatic,
            VZDiskImageSynchronizationMode::Fsync,
        )
    }
    .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    let data_storage_device = unsafe {
        VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            data_storage_attachment.as_super(),
        )
    };
    let storage_devices: Retained<NSArray<VZStorageDeviceConfiguration>> = NSArray::from_slice(&[
        root_storage_device.as_super(),
        data_storage_device.as_super(),
    ]);

    let nat_attachment = unsafe { VZNATNetworkDeviceAttachment::new() };
    let network_device = unsafe { VZVirtioNetworkDeviceConfiguration::new() };
    unsafe {
        network_device.setAttachment(Some(nat_attachment.as_super()));
    }
    let network_devices: Retained<NSArray<VZNetworkDeviceConfiguration>> =
        NSArray::from_slice(&[network_device.as_super()]);

    let socket_device = unsafe { VZVirtioSocketDeviceConfiguration::new() };
    let socket_devices: Retained<NSArray<VZSocketDeviceConfiguration>> =
        NSArray::from_slice(&[socket_device.as_super()]);
    let balloon_device = unsafe { VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new() };
    let balloon_devices: Retained<NSArray<VZMemoryBalloonDeviceConfiguration>> =
        NSArray::from_slice(&[balloon_device.as_super()]);
    let shared_data_root_device = build_shared_data_root_device(data_root)?;
    let directory_sharing_devices: Retained<NSArray<VZDirectorySharingDeviceConfiguration>> =
        NSArray::from_slice(&[shared_data_root_device.as_super()]);

    let configuration = unsafe { VZVirtualMachineConfiguration::new() };
    let platform = unsafe { VZGenericPlatformConfiguration::new() };
    let min_cpu = unsafe { VZVirtualMachineConfiguration::minimumAllowedCPUCount() };
    let max_cpu = unsafe { VZVirtualMachineConfiguration::maximumAllowedCPUCount() };
    let min_memory = unsafe { VZVirtualMachineConfiguration::minimumAllowedMemorySize() };
    let max_memory = unsafe { VZVirtualMachineConfiguration::maximumAllowedMemorySize() };
    let sizing = resolved_avf_vm_sizing_for_host(min_cpu, max_cpu, min_memory, max_memory)?;
    unsafe {
        configuration.setBootLoader(Some(boot_loader.as_super()));
        configuration.setPlatform(platform.as_super());
        configuration.setCPUCount(sizing.cpu_count);
        configuration.setMemorySize(sizing.memory_size_bytes);
        configuration.setStorageDevices(&storage_devices);
        configuration.setNetworkDevices(&network_devices);
        configuration.setSocketDevices(&socket_devices);
        configuration.setMemoryBalloonDevices(&balloon_devices);
        configuration.setDirectorySharingDevices(&directory_sharing_devices);
        configuration
            .validateWithError()
            .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    }

    let save_restore_note = if shared_vm_save_restore_supported() {
        #[cfg(target_arch = "aarch64")]
        {
            match unsafe { configuration.validateSaveRestoreSupportWithError() } {
                Ok(()) => "save/restore supported".to_string(),
                Err(err) => format!(
                    "save/restore unavailable for this VM configuration: {}",
                    format_nserror(&err)
                ),
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            "save/restore unavailable on this host".to_string()
        }
    } else {
        "save/restore unavailable on this host".to_string()
    };

    Ok(format!(
        "native AVF configuration validated (cpu={}, memory={} MiB; {}; {})",
        sizing.cpu_count,
        sizing.memory_size_bytes / MEBIBYTE_BYTES,
        sizing.policy_note,
        save_restore_note,
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn load_or_create_shared_vm_machine_identifier(
    data_root: &Path,
) -> Result<Retained<VZGenericMachineIdentifier>> {
    let path = shared_vm_machine_identifier_path(data_root);
    if path.is_file() {
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let data = NSData::from_vec(bytes);
        return unsafe {
            VZGenericMachineIdentifier::initWithDataRepresentation(
                VZGenericMachineIdentifier::alloc(),
                &data,
            )
        }
        .ok_or_else(|| anyhow::anyhow!("invalid AVF machine identifier at {}", path.display()));
    }

    let identifier = unsafe { VZGenericMachineIdentifier::new() };
    let data = unsafe { identifier.dataRepresentation() };
    fs::write(&path, data.to_vec()).with_context(|| format!("writing {}", path.display()))?;
    Ok(identifier)
}

#[cfg(target_os = "macos")]
pub(super) fn load_or_create_shared_vm_mac_address(
    data_root: &Path,
) -> Result<Retained<VZMACAddress>> {
    let path = shared_vm_mac_address_path(data_root);
    if path.is_file() {
        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let value = raw.trim();
        let ns_value = NSString::from_str(value);
        return unsafe { VZMACAddress::initWithString(VZMACAddress::alloc(), &ns_value) }
            .ok_or_else(|| {
                anyhow::anyhow!("invalid AVF MAC address `{value}` at {}", path.display())
            });
    }

    let address = unsafe { VZMACAddress::randomLocallyAdministeredAddress() };
    let address_string = unsafe { address.string() }.to_string();
    fs::write(&path, address_string).with_context(|| format!("writing {}", path.display()))?;
    Ok(address)
}

#[cfg(target_os = "macos")]
pub(super) fn build_real_avf_linux_vm_configuration(
    data_root: &Path,
    rootfs_image: &Path,
    data_disk_image: &Path,
    kernel_path: &Path,
    initrd_path: &Path,
    seed_image: Option<&Path>,
    kernel_cmdline: &str,
) -> Result<Retained<VZVirtualMachineConfiguration>> {
    if !unsafe { VZVirtualMachine::isSupported() } {
        bail!("Virtualization.framework reported that virtualization is unavailable on this host");
    }

    let kernel_url = file_url_for_path(kernel_path);
    let initrd_url = file_url_for_path(initrd_path);
    let rootfs_url = file_url_for_path(rootfs_image);
    let data_disk_url = file_url_for_path(data_disk_image);

    let boot_loader =
        unsafe { VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url) };
    unsafe {
        boot_loader.setCommandLine(&NSString::from_str(kernel_cmdline));
        boot_loader.setInitialRamdiskURL(Some(&initrd_url));
    }

    let root_storage_attachment = unsafe {
        VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &rootfs_url,
            false,
            VZDiskImageCachingMode::Automatic,
            VZDiskImageSynchronizationMode::Fsync,
        )
    }
    .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    let root_storage_device = unsafe {
        VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            root_storage_attachment.as_super(),
        )
    };
    let data_storage_attachment = unsafe {
        VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
            VZDiskImageStorageDeviceAttachment::alloc(),
            &data_disk_url,
            false,
            VZDiskImageCachingMode::Automatic,
            VZDiskImageSynchronizationMode::Fsync,
        )
    }
    .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    let data_storage_device = unsafe {
        VZVirtioBlockDeviceConfiguration::initWithAttachment(
            VZVirtioBlockDeviceConfiguration::alloc(),
            data_storage_attachment.as_super(),
        )
    };
    let mut storage_devices_owned = vec![root_storage_device, data_storage_device];
    if let Some(seed_image) = seed_image {
        let seed_url = file_url_for_path(seed_image);
        let seed_storage_attachment = unsafe {
            VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_cachingMode_synchronizationMode_error(
                VZDiskImageStorageDeviceAttachment::alloc(),
                &seed_url,
                true,
                VZDiskImageCachingMode::Automatic,
                VZDiskImageSynchronizationMode::Fsync,
            )
        }
        .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
        let seed_storage_device = unsafe {
            VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                seed_storage_attachment.as_super(),
            )
        };
        storage_devices_owned.push(seed_storage_device);
    }
    let storage_device_refs = storage_devices_owned
        .iter()
        .map(|device| device.as_super())
        .collect::<Vec<_>>();
    let storage_devices: Retained<NSArray<VZStorageDeviceConfiguration>> =
        NSArray::from_slice(&storage_device_refs);

    let nat_attachment = unsafe { VZNATNetworkDeviceAttachment::new() };
    let network_device = unsafe { VZVirtioNetworkDeviceConfiguration::new() };
    let mac_address = load_or_create_shared_vm_mac_address(data_root)?;
    unsafe {
        network_device.setAttachment(Some(nat_attachment.as_super()));
        network_device.setMACAddress(&mac_address);
    }
    let network_devices: Retained<NSArray<VZNetworkDeviceConfiguration>> =
        NSArray::from_slice(&[network_device.as_super()]);

    let socket_device = unsafe { VZVirtioSocketDeviceConfiguration::new() };
    let socket_devices: Retained<NSArray<VZSocketDeviceConfiguration>> =
        NSArray::from_slice(&[socket_device.as_super()]);
    let balloon_device = unsafe { VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new() };
    let balloon_devices: Retained<NSArray<VZMemoryBalloonDeviceConfiguration>> =
        NSArray::from_slice(&[balloon_device.as_super()]);
    let shared_data_root_device = build_shared_data_root_device(data_root)?;
    let directory_sharing_devices: Retained<NSArray<VZDirectorySharingDeviceConfiguration>> =
        NSArray::from_slice(&[shared_data_root_device.as_super()]);
    let guest_console_log_path = shared_vm_guest_console_log_path(data_root);
    if let Some(parent) = guest_console_log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(&guest_console_log_path, b"")
        .with_context(|| format!("resetting {}", guest_console_log_path.display()))?;
    let guest_console_url = file_url_for_path(&guest_console_log_path);
    let guest_console_attachment = unsafe {
        VZFileSerialPortAttachment::initWithURL_append_error(
            VZFileSerialPortAttachment::alloc(),
            &guest_console_url,
            true,
        )
    }
    .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    let serial_port = unsafe { VZVirtioConsoleDeviceSerialPortConfiguration::new() };
    unsafe {
        serial_port.setAttachment(Some(guest_console_attachment.as_super()));
    }
    let serial_ports: Retained<NSArray<VZSerialPortConfiguration>> =
        NSArray::from_slice(&[serial_port.as_super()]);

    let configuration = unsafe { VZVirtualMachineConfiguration::new() };
    let platform = unsafe { VZGenericPlatformConfiguration::new() };
    let machine_identifier = load_or_create_shared_vm_machine_identifier(data_root)?;
    let min_cpu = unsafe { VZVirtualMachineConfiguration::minimumAllowedCPUCount() };
    let max_cpu = unsafe { VZVirtualMachineConfiguration::maximumAllowedCPUCount() };
    let min_memory = unsafe { VZVirtualMachineConfiguration::minimumAllowedMemorySize() };
    let max_memory = unsafe { VZVirtualMachineConfiguration::maximumAllowedMemorySize() };
    let sizing = resolved_avf_vm_sizing_for_host(min_cpu, max_cpu, min_memory, max_memory)?;
    unsafe {
        configuration.setBootLoader(Some(boot_loader.as_super()));
        platform.setMachineIdentifier(&machine_identifier);
        configuration.setPlatform(platform.as_super());
        configuration.setCPUCount(sizing.cpu_count);
        configuration.setMemorySize(sizing.memory_size_bytes);
        configuration.setStorageDevices(&storage_devices);
        configuration.setNetworkDevices(&network_devices);
        configuration.setSerialPorts(&serial_ports);
        configuration.setSocketDevices(&socket_devices);
        configuration.setMemoryBalloonDevices(&balloon_devices);
        configuration.setDirectorySharingDevices(&directory_sharing_devices);
        configuration
            .validateWithError()
            .map_err(|err| anyhow::anyhow!(format_nserror(&err)))?;
    }
    Ok(configuration)
}

#[cfg(target_os = "macos")]
pub(super) fn build_real_avf_linux_virtual_machine(
    data_root: &Path,
    rootfs_image: &Path,
    data_disk_image: &Path,
    kernel_path: &Path,
    initrd_path: &Path,
    seed_image: Option<&Path>,
    kernel_cmdline: &str,
    queue: &DispatchQueue,
) -> Result<Retained<VZVirtualMachine>> {
    let configuration = build_real_avf_linux_vm_configuration(
        data_root,
        rootfs_image,
        data_disk_image,
        kernel_path,
        initrd_path,
        seed_image,
        kernel_cmdline,
    )?;
    Ok(unsafe {
        VZVirtualMachine::initWithConfiguration_queue(
            VZVirtualMachine::alloc(),
            &configuration,
            queue,
        )
    })
}

#[cfg(not(target_os = "macos"))]
pub(super) fn validate_real_avf_linux_vm_configuration(
    _data_root: &Path,
    _rootfs_image: &Path,
    _data_disk_image: &Path,
    _kernel_path: &Path,
    _initrd_path: &Path,
    _kernel_cmdline: &str,
) -> Result<String> {
    bail!("AVF Linux VM validation requires macOS")
}

#[cfg(not(target_os = "macos"))]
pub(super) fn build_real_avf_linux_virtual_machine(
    _data_root: &Path,
    _rootfs_image: &Path,
    _data_disk_image: &Path,
    _kernel_path: &Path,
    _initrd_path: &Path,
    _seed_image: Option<&Path>,
    _kernel_cmdline: &str,
    _queue: &(),
) -> Result<()> {
    bail!("AVF Linux VM launch requires macOS")
}
