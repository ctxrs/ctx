#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SemanticComputeClass {
    Cpu,
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    Accelerator,
}

impl SemanticComputeClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Accelerator => "accelerator",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SemanticSystemResources {
    total_memory_bytes: Option<u64>,
    available_memory_bytes: Option<u64>,
    available_parallelism: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SemanticQuietPolicy {
    threads: usize,
    batch_size: usize,
    // Heuristic sizing target for batch selection, not an OS-enforced memory limit.
    memory_budget_bytes: u64,
    active_percent: u8,
}

const SEMANTIC_CPU_MODEL_LOAD_MIN_AVAILABLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES: u64 = 3 * 1024 * 1024 * 1024;

fn semantic_model_load_deferred(
    available_memory_bytes: Option<u64>,
    compute_class: SemanticComputeClass,
) -> Option<SemanticModelLoadDeferred> {
    let available_memory_bytes = available_memory_bytes?;
    let required_available_memory_bytes = match compute_class {
        SemanticComputeClass::Cpu => SEMANTIC_CPU_MODEL_LOAD_MIN_AVAILABLE_BYTES,
        SemanticComputeClass::Accelerator => {
            SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES
        }
    };
    (available_memory_bytes < required_available_memory_bytes).then_some(
        SemanticModelLoadDeferred {
            available_memory_bytes,
            required_available_memory_bytes,
        },
    )
}

fn semantic_cpu_model_load_deferred(
    available_memory_bytes: Option<u64>,
) -> Option<SemanticModelLoadDeferred> {
    semantic_model_load_deferred(available_memory_bytes, SemanticComputeClass::Cpu)
}

impl SemanticSystemResources {
    fn current() -> Self {
        let (total_memory_bytes, available_memory_bytes) = semantic_system_memory();
        Self {
            total_memory_bytes,
            available_memory_bytes,
            available_parallelism: std::thread::available_parallelism()
                .map(|value| value.get())
                .unwrap_or(1),
        }
    }
}

fn semantic_quiet_policy(
    resources: SemanticSystemResources,
    compute_class: SemanticComputeClass,
) -> SemanticQuietPolicy {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    const TWO_GIB: u64 = 2 * GIB;

    let memory_budget_bytes = resources
        .total_memory_bytes
        .map(|bytes| bytes / 10)
        .into_iter()
        .chain(resources.available_memory_bytes.map(|bytes| bytes / 4))
        .min()
        .unwrap_or(GIB)
        .clamp(512 * MIB, 4 * GIB);
    let parallelism = resources.available_parallelism.max(1);
    match compute_class {
        SemanticComputeClass::Cpu => {
            let threads = (parallelism / 4).clamp(1, 4);
            let batch_size = match memory_budget_bytes {
                0..GIB => 4,
                GIB..TWO_GIB => 8,
                TWO_GIB.. => 16,
            };
            SemanticQuietPolicy {
                threads,
                batch_size,
                memory_budget_bytes,
                active_percent: 25,
            }
        }
        SemanticComputeClass::Accelerator => SemanticQuietPolicy {
            threads: (parallelism / 8).clamp(1, 2),
            batch_size: match memory_budget_bytes {
                0..GIB => 32,
                GIB..=2_147_483_647 => 64,
                _ => 128,
            },
            memory_budget_bytes,
            active_percent: 50,
        },
    }
}

fn semantic_batch_rest(active: StdDuration, active_percent: u8) -> StdDuration {
    if active.is_zero() || !(1..100).contains(&active_percent) {
        return StdDuration::ZERO;
    }
    let active_nanos = active.as_nanos();
    let total_nanos = active_nanos
        .saturating_mul(100)
        .checked_div(u128::from(active_percent))
        .unwrap_or(active_nanos);
    let rest_nanos = total_nanos.saturating_sub(active_nanos);
    StdDuration::from_nanos(rest_nanos.min(u128::from(u64::MAX)) as u64)
}

fn semantic_limited_batch_rest(
    active: StdDuration,
    active_percent: u8,
    remaining: Option<StdDuration>,
) -> StdDuration {
    let rest = semantic_batch_rest(active, active_percent);
    remaining
        .map(|remaining| rest.min(remaining))
        .unwrap_or(rest)
}

fn throttle_semantic_batch(
    active: StdDuration,
    policy: SemanticQuietPolicy,
    remaining: Option<StdDuration>,
) {
    let rest = semantic_limited_batch_rest(active, policy.active_percent, remaining);
    if !rest.is_zero() {
        std::thread::sleep(rest);
    }
}

#[cfg(target_os = "linux")]
fn semantic_system_memory() -> (Option<u64>, Option<u64>) {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return (None, None);
    };
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        if let Some(value) = semantic_meminfo_kib(line, "MemTotal:") {
            total = Some(value);
        } else if let Some(value) = semantic_meminfo_kib(line, "MemAvailable:") {
            available = Some(value);
        }
    }
    let (cgroup_limit, cgroup_available) = semantic_linux_cgroup_memory();
    semantic_effective_linux_memory(total, available, cgroup_limit, cgroup_available)
}

#[cfg(target_os = "linux")]
fn semantic_linux_cgroup_memory() -> (Option<u64>, Option<u64>) {
    let cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    semantic_linux_cgroup_memory_at(Path::new("/sys/fs/cgroup"), &cgroup)
}

#[cfg(any(target_os = "linux", test))]
fn semantic_linux_cgroup_memory_at(root: &Path, cgroup: &str) -> (Option<u64>, Option<u64>) {
    let mut v2_candidates = Vec::new();
    let mut v1_candidates = Vec::new();
    for line in cgroup.lines() {
        let mut fields = line.splitn(3, ':');
        let _hierarchy = fields.next();
        let controllers = fields.next().unwrap_or_default();
        let relative = semantic_cgroup_relative_path(fields.next().unwrap_or_default());
        if controllers.is_empty() {
            semantic_push_cgroup_ancestors(&mut v2_candidates, root, &relative);
        } else if controllers.split(',').any(|value| value == "memory") {
            semantic_push_cgroup_ancestors(&mut v1_candidates, &root.join("memory"), &relative);
        }
    }
    if v2_candidates.is_empty() {
        v2_candidates.push(root.to_path_buf());
    }

    let mut effective_limit = None;
    let mut effective_available = None;
    for directory in v2_candidates {
        semantic_tighten_cgroup_memory(
            &directory,
            "memory.max",
            "memory.current",
            &mut effective_limit,
            &mut effective_available,
        );
    }
    for directory in v1_candidates {
        semantic_tighten_cgroup_memory(
            &directory,
            "memory.limit_in_bytes",
            "memory.usage_in_bytes",
            &mut effective_limit,
            &mut effective_available,
        );
    }
    (effective_limit, effective_available)
}

#[cfg(any(target_os = "linux", test))]
fn semantic_cgroup_relative_path(path: &str) -> PathBuf {
    path.split('/')
        .filter(|component| !component.is_empty() && *component != "." && *component != "..")
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn semantic_push_cgroup_ancestors(candidates: &mut Vec<PathBuf>, root: &Path, relative: &Path) {
    let mut directory = root.join(relative);
    loop {
        if !candidates.iter().any(|candidate| candidate == &directory) {
            candidates.push(directory.clone());
        }
        if directory == root || !directory.pop() || !directory.starts_with(root) {
            break;
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn semantic_tighten_cgroup_memory(
    directory: &Path,
    limit_name: &str,
    current_name: &str,
    effective_limit: &mut Option<u64>,
    effective_available: &mut Option<u64>,
) {
    let limit = fs::read_to_string(directory.join(limit_name))
        .ok()
        .and_then(|value| semantic_parse_cgroup_memory_value(&value));
    let current = fs::read_to_string(directory.join(current_name))
        .ok()
        .and_then(|value| semantic_parse_cgroup_memory_value(&value));
    if let Some(limit) = limit {
        *effective_limit = Some(effective_limit.map_or(limit, |known| known.min(limit)));
        let available = limit.saturating_sub(current.unwrap_or(0));
        *effective_available = Some(
            effective_available.map_or(available, |known| known.min(available)),
        );
    }
}

#[cfg(any(target_os = "linux", test))]
fn semantic_parse_cgroup_memory_value(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value == "max" {
        return None;
    }
    value.parse().ok()
}

#[cfg(any(target_os = "linux", test))]
fn semantic_effective_linux_memory(
    host_total: Option<u64>,
    host_available: Option<u64>,
    cgroup_limit: Option<u64>,
    cgroup_available: Option<u64>,
) -> (Option<u64>, Option<u64>) {
    let total = match (host_total, cgroup_limit) {
        (Some(host), Some(limit)) => Some(host.min(limit)),
        (host, limit) => host.or(limit),
    };
    let available = match (host_available, cgroup_available) {
        (Some(host), Some(cgroup)) => Some(host.min(cgroup)),
        (host, cgroup) => host.or(cgroup),
    };
    (total, available)
}

#[cfg(any(target_os = "linux", test))]
fn semantic_meminfo_kib(line: &str, key: &str) -> Option<u64> {
    let mut fields = line.strip_prefix(key)?.split_whitespace();
    let kib = fields.next()?.parse::<u64>().ok()?;
    if fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    kib.checked_mul(1024)
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn semantic_sysctl_number(name: &'static [u8]) -> Option<u64> {
    let mut value = 0_u64;
    let mut size = std::mem::size_of::<u64>();
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    match size {
        4 => Some(value & u64::from(u32::MAX)),
        8 => Some(value),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn semantic_system_memory() -> (Option<u64>, Option<u64>) {
    let total = semantic_sysctl_number(b"hw.memsize\0");
    let page_size = semantic_sysctl_number(b"hw.pagesize\0");
    let available_pages = [
        b"vm.page_free_count\0".as_slice(),
        b"vm.page_inactive_count\0".as_slice(),
        b"vm.page_speculative_count\0".as_slice(),
        b"vm.page_purgeable_count\0".as_slice(),
    ]
    .into_iter()
    .map(semantic_sysctl_number)
    .try_fold(0_u64, |total, value| total.checked_add(value?));
    let available = page_size.and_then(|size| available_pages?.checked_mul(size));
    (total, available)
}

#[cfg(target_os = "freebsd")]
fn semantic_system_memory() -> (Option<u64>, Option<u64>) {
    let total = semantic_sysctl_number(b"hw.physmem\0");
    let page_size = semantic_sysctl_number(b"vm.stats.vm.v_page_size\0");
    let available_pages = [
        b"vm.stats.vm.v_free_count\0".as_slice(),
        b"vm.stats.vm.v_inactive_count\0".as_slice(),
        b"vm.stats.vm.v_cache_count\0".as_slice(),
    ]
    .into_iter()
    .map(semantic_sysctl_number)
    .try_fold(0_u64, |total, value| total.checked_add(value?));
    let available = page_size.and_then(|size| available_pages?.checked_mul(size));
    (total, available)
}

#[cfg(target_os = "windows")]
fn semantic_system_memory() -> (Option<u64>, Option<u64>) {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return (None, None);
    }
    (Some(status.total_phys), Some(status.avail_phys))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
fn semantic_system_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

#[cfg(test)]
mod semantic_resource_policy_tests {
    use super::*;

    #[test]
    fn quiet_cpu_policy_caps_threads_batch_and_memory_target() {
        let policy = semantic_quiet_policy(
            SemanticSystemResources {
                total_memory_bytes: Some(64 * 1024 * 1024 * 1024),
                available_memory_bytes: Some(32 * 1024 * 1024 * 1024),
                available_parallelism: 32,
            },
            SemanticComputeClass::Cpu,
        );
        assert_eq!(policy.threads, 4);
        assert_eq!(policy.batch_size, 16);
        assert_eq!(policy.memory_budget_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(policy.active_percent, 25);
    }

    #[test]
    fn quiet_cpu_batch_scales_at_memory_target_boundaries() {
        const GIB: u64 = 1024 * 1024 * 1024;
        const TWO_GIB: u64 = 2 * GIB;

        for (memory_target_bytes, expected_batch_size) in
            [(GIB - 1, 4), (GIB, 8), (TWO_GIB - 1, 8), (TWO_GIB, 16)]
        {
            let policy = semantic_quiet_policy(
                SemanticSystemResources {
                    total_memory_bytes: None,
                    available_memory_bytes: Some(memory_target_bytes * 4),
                    available_parallelism: 8,
                },
                SemanticComputeClass::Cpu,
            );
            assert_eq!(policy.memory_budget_bytes, memory_target_bytes);
            assert_eq!(policy.batch_size, expected_batch_size);
        }
    }

    #[test]
    fn accelerator_policy_keeps_tokenizer_parallelism_small() {
        let policy = semantic_quiet_policy(
            SemanticSystemResources {
                total_memory_bytes: Some(16 * 1024 * 1024 * 1024),
                available_memory_bytes: Some(8 * 1024 * 1024 * 1024),
                available_parallelism: 12,
            },
            SemanticComputeClass::Accelerator,
        );
        assert_eq!(policy.threads, 1);
        assert_eq!(policy.batch_size, 64);
        assert_eq!(policy.memory_budget_bytes, 1_717_986_918);
        assert_eq!(policy.active_percent, 50);
    }

    #[test]
    fn batch_rest_enforces_target_fraction() {
        assert_eq!(
            semantic_batch_rest(StdDuration::from_millis(100), 25),
            StdDuration::from_millis(300)
        );
        assert_eq!(
            semantic_batch_rest(StdDuration::from_secs(3), 25),
            StdDuration::from_secs(9)
        );
        assert_eq!(
            semantic_batch_rest(StdDuration::ZERO, 25),
            StdDuration::ZERO
        );
        assert_eq!(
            semantic_limited_batch_rest(
                StdDuration::from_secs(3),
                25,
                Some(StdDuration::from_secs(2)),
            ),
            StdDuration::from_secs(2),
        );
        assert_eq!(
            semantic_limited_batch_rest(
                StdDuration::from_secs(3),
                25,
                Some(StdDuration::ZERO),
            ),
            StdDuration::ZERO,
        );
    }

    #[test]
    fn meminfo_parser_is_strict() {
        assert_eq!(
            semantic_meminfo_kib("MemTotal: 1024 kB", "MemTotal:"),
            Some(1024 * 1024)
        );
        assert_eq!(semantic_meminfo_kib("MemTotal: 1024 MB", "MemTotal:"), None);
    }

    #[test]
    fn cgroup_memory_tightens_host_memory_admission() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(semantic_parse_cgroup_memory_value("max\n"), None);
        assert_eq!(semantic_parse_cgroup_memory_value("1234\n"), Some(1234));
        assert_eq!(
            semantic_effective_linux_memory(
                Some(64 * GIB),
                Some(32 * GIB),
                Some(3 * GIB),
                Some(GIB),
            ),
            (Some(3 * GIB), Some(GIB))
        );
        assert_eq!(
            semantic_effective_linux_memory(
                Some(64 * GIB),
                Some(32 * GIB),
                Some(4 * GIB),
                Some(GIB / 2),
            ),
            (Some(4 * GIB), Some(GIB / 2))
        );
        assert!(semantic_cpu_model_load_deferred(Some(GIB)).is_some());
    }

    #[test]
    fn nested_cgroup_uses_tightest_v2_and_v1_limits() -> Result<()> {
        const GIB: u64 = 1024 * 1024 * 1024;
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        fs::write(root.join("memory.max"), format!("{}\n", 16 * GIB))?;
        fs::write(root.join("memory.current"), format!("{}\n", 2 * GIB))?;
        let nested = root.join("user.slice/ctx.scope");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("memory.max"), format!("{}\n", 3 * GIB))?;
        fs::write(nested.join("memory.current"), format!("{}\n", 2 * GIB))?;

        assert_eq!(
            semantic_linux_cgroup_memory_at(root, "0::/user.slice/ctx.scope\n"),
            (Some(3 * GIB), Some(GIB))
        );

        let v1 = root.join("memory/build.slice");
        fs::create_dir_all(&v1)?;
        fs::write(v1.join("memory.limit_in_bytes"), format!("{}\n", 2 * GIB))?;
        fs::write(v1.join("memory.usage_in_bytes"), format!("{GIB}\n"))?;
        assert_eq!(
            semantic_linux_cgroup_memory_at(root, "5:cpu,memory:/build.slice\n"),
            (Some(2 * GIB), Some(GIB))
        );
        Ok(())
    }

    #[test]
    fn cpu_model_load_defers_only_below_known_memory_floor() {
        let floor = SEMANTIC_CPU_MODEL_LOAD_MIN_AVAILABLE_BYTES;
        assert!(semantic_cpu_model_load_deferred(Some(floor - 1)).is_some());
        assert!(semantic_cpu_model_load_deferred(Some(floor)).is_none());
        assert!(semantic_cpu_model_load_deferred(Some(floor + 1)).is_none());
        assert!(semantic_cpu_model_load_deferred(None).is_none());
    }

    #[test]
    fn accelerator_model_load_uses_larger_measured_memory_floor() {
        let floor = SEMANTIC_ACCELERATOR_MODEL_LOAD_MIN_AVAILABLE_BYTES;
        let deferred = semantic_model_load_deferred(
            Some(floor - 1),
            SemanticComputeClass::Accelerator,
        )
        .expect("accelerator load should defer below its memory floor");
        assert_eq!(deferred.required_available_memory_bytes, floor);
        assert!(semantic_model_load_deferred(
            Some(floor),
            SemanticComputeClass::Accelerator
        )
        .is_none());
        assert!(semantic_model_load_deferred(None, SemanticComputeClass::Accelerator).is_none());
    }
}
