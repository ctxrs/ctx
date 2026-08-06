use super::*;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_env = "gnu"
    ),
    all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "freebsd", target_arch = "x86_64")
))]
use ctx_semantic_model::test_support::{
    load_missing_semantic_onnxruntime as load_missing_semantic_onnxruntime_for_test,
    map_daemon_coreml_load_error, write_test_semantic_cache,
};

fn test_daemon_run_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: Some(1),
        max_seconds: Some(1),
        force: false,
        start_mode: Some(DaemonStartModeArg::Manual),
        trigger_command: None,
        format: crate::output::JsonOutputFormat::Json,
    }
}

fn write_semantic_enabled_config(data_root: &Path) -> Result<()> {
    fs::create_dir_all(data_root)?;
    let path = data_root.join(CONFIG_FILE);
    fs::write(
        path,
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )?;
    Ok(())
}

fn daemon_semantic_indexed_test_job(_data_root: &Path) -> Value {
    daemon_semantic_job_json(
        "budget_exhausted",
        None,
        utc_now().timestamp_millis(),
        Some(1),
        None,
    )
}

fn install_test_daemon_jobs(
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    semantic_index: Option<Value>,
) -> DaemonTestJobHookGuard {
    install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls,
        semantic_index,
    })
}

mod lifecycle;
mod locking;
mod workflow;
