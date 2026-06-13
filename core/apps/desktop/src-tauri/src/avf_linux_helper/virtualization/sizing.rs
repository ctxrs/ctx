use super::*;

pub(super) const MEBIBYTE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedAvfVmSizing {
    pub(crate) cpu_count: usize,
    pub(crate) memory_size_bytes: u64,
    pub(crate) policy_note: String,
}

fn align_down_to_mebibyte(bytes: u64) -> u64 {
    bytes - (bytes % MEBIBYTE_BYTES)
}

fn align_up_to_mebibyte(bytes: u64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    let remainder = bytes % MEBIBYTE_BYTES;
    if remainder == 0 {
        bytes
    } else {
        bytes + (MEBIBYTE_BYTES - remainder)
    }
}

pub(crate) fn resolve_avf_vm_sizing(
    min_cpu: usize,
    max_cpu: usize,
    min_memory: u64,
    max_memory: u64,
    host_cpu_count: usize,
    host_memory_bytes: u64,
    cpu_override: Option<usize>,
    memory_override_bytes: Option<u64>,
) -> ResolvedAvfVmSizing {
    let min_cpu = min_cpu.max(1);
    let max_cpu = max_cpu.max(min_cpu);
    let requested_cpu = cpu_override.unwrap_or_else(|| host_cpu_count.max(1));
    let cpu_count = requested_cpu.clamp(min_cpu, max_cpu);

    let min_memory = align_up_to_mebibyte(min_memory.max(MEBIBYTE_BYTES));
    let max_memory = align_down_to_mebibyte(max_memory).max(min_memory);
    let default_memory_bytes = host_memory_bytes
        .saturating_sub(SHARED_VM_HOST_MEMORY_RESERVE_BYTES)
        .max(SHARED_VM_MIN_DEFAULT_MEMORY_BYTES);
    let requested_memory = memory_override_bytes
        .unwrap_or(default_memory_bytes)
        .max(SHARED_VM_MIN_DEFAULT_MEMORY_BYTES);
    let memory_size_bytes = align_down_to_mebibyte(requested_memory).clamp(min_memory, max_memory);

    let cpu_policy = if cpu_override.is_some() {
        format!("{SHARED_VM_CPU_COUNT_ENV} override")
    } else {
        "host logical CPU count".to_string()
    };
    let memory_policy = if memory_override_bytes.is_some() {
        format!("{SHARED_VM_MEMORY_CEILING_BYTES_ENV} override")
    } else {
        format!(
            "host RAM minus {} MiB reserve",
            SHARED_VM_HOST_MEMORY_RESERVE_BYTES / MEBIBYTE_BYTES
        )
    };

    ResolvedAvfVmSizing {
        cpu_count,
        memory_size_bytes,
        policy_note: format!(
            "vm sizing policy: cpu={cpu_policy}, bounded by AVF limits; memory ceiling={memory_policy}, bounded by AVF limits and a {} MiB floor",
            SHARED_VM_MIN_DEFAULT_MEMORY_BYTES / MEBIBYTE_BYTES
        ),
    }
}

#[cfg(target_os = "macos")]
fn read_optional_env_usize(name: &str) -> Result<Option<usize>> {
    match std::env::var(name) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("{name} is set but empty");
            }
            let value = trimmed
                .parse::<usize>()
                .with_context(|| format!("parsing {name} as an integer"))?;
            if value == 0 {
                bail!("{name} must be greater than zero");
            }
            Ok(Some(value))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => bail!("{name} is not valid UTF-8"),
    }
}

#[cfg(target_os = "macos")]
fn read_optional_env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                bail!("{name} is set but empty");
            }
            let value = trimmed
                .parse::<u64>()
                .with_context(|| format!("parsing {name} as an integer"))?;
            if value == 0 {
                bail!("{name} must be greater than zero");
            }
            Ok(Some(value))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => bail!("{name} is not valid UTF-8"),
    }
}

#[cfg(target_os = "macos")]
fn read_sysctl_u64(name: &str) -> Result<u64> {
    let name_cstr =
        std::ffi::CString::new(name).with_context(|| format!("building sysctl name {name}"))?;
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let status = unsafe {
        libc::sysctlbyname(
            name_cstr.as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("reading sysctl {name}"));
    }
    if size == 0 {
        bail!("sysctl {name} returned no data");
    }
    Ok(value)
}

#[cfg(target_os = "macos")]
fn host_logical_cpu_count() -> Result<usize> {
    let cpu_count = read_sysctl_u64("hw.logicalcpu")? as usize;
    if cpu_count == 0 {
        bail!("hw.logicalcpu reported zero logical CPUs");
    }
    Ok(cpu_count)
}

#[cfg(target_os = "macos")]
fn host_memory_size_bytes() -> Result<u64> {
    let memory_bytes = read_sysctl_u64("hw.memsize")?;
    if memory_bytes == 0 {
        bail!("hw.memsize reported zero bytes");
    }
    Ok(memory_bytes)
}

#[cfg(target_os = "macos")]
pub(crate) fn resolved_avf_vm_sizing_for_host(
    min_cpu: usize,
    max_cpu: usize,
    min_memory: u64,
    max_memory: u64,
) -> Result<ResolvedAvfVmSizing> {
    let host_cpu_count = host_logical_cpu_count()?;
    let host_memory_bytes = host_memory_size_bytes()?;
    let cpu_override = read_optional_env_usize(SHARED_VM_CPU_COUNT_ENV)?;
    let memory_override_bytes = read_optional_env_u64(SHARED_VM_MEMORY_CEILING_BYTES_ENV)?;
    Ok(resolve_avf_vm_sizing(
        min_cpu,
        max_cpu,
        min_memory,
        max_memory,
        host_cpu_count,
        host_memory_bytes,
        cpu_override,
        memory_override_bytes,
    ))
}
