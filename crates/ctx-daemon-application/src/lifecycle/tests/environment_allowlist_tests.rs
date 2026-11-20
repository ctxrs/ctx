use super::*;

#[test]
fn daemon_child_allowlist_covers_every_discovery_env_key() -> Result<()> {
    let allowlist = daemon_child_environment_allowlist_names();
    for required in crate::supervisor::DISCOVERY_ENV_ALLOWLIST {
        assert!(
            allowlist.contains(required),
            "daemon child env allowlist is missing discovery key {required}"
        );
    }
    assert!(allowlist.contains(&"GEMINI_CLI_HOME"));

    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let vars = [
        ("GEMINI_CLI_HOME", "/ctx-finite-worker-gemini-home"),
        ("PI_CODING_AGENT_DIR", "/ctx-finite-worker-pi-dir"),
        ("PRIME_AGENT_SESSION_DIR", "/ctx-finite-worker-prime-dir"),
        ("QWEN_HOME", "/ctx-finite-worker-qwen-home"),
    ];
    let previous: Vec<_> = vars
        .iter()
        .map(|(name, _)| (*name, env::var_os(name)))
        .collect();
    for (name, value) in vars {
        env::set_var(name, value);
    }
    let temp = tempfile::tempdir()?;
    let finite = configured_finite_core_worker_command(
        Path::new("/usr/local/bin/ctx"),
        temp.path(),
        DaemonTrigger::Import,
    )?;
    let persistent = daemon_autostart_command(
        Path::new("/usr/local/bin/ctx"),
        temp.path(),
        DaemonTrigger::Import,
        None,
        None,
    )?;
    for (name, value) in previous {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }
    for launch in [&finite, &persistent] {
        for (name, expected) in vars {
            assert_eq!(
                launch
                    .environment()
                    .find_map(|(key, value)| (key == OsStr::new(name)).then_some(value)),
                Some(OsStr::new(expected)),
                "missing forwarded env {name}"
            );
        }
    }
    Ok(())
}
