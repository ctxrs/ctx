use crate::{hooks, rewrite, state, test_support};
use std::ffi::OsString;
use std::fs;
use std::process::{Command, Output};

fn command(root: &std::path::Path, args: &[&str]) -> Command {
    let mut command = test_support::command(args);
    command
        .current_dir(root)
        .env("SIFT_CONFIG_DIR", root.join("config"))
        .env("SIFT_STATE_DIR", root.join("state"));
    command
}

fn output(root: &std::path::Path, args: &[&str]) -> Output {
    test_support::clean(command(root, args).output().unwrap())
}

#[test]
fn namespace_rejects_other_ctx_commands_and_has_no_install_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["--help"],
        vec!["run", "--help"],
        vec!["filter", "--help"],
        vec!["semantic", "--help"],
        vec!["discover", "--help"],
        vec!["ccusage", "--help"],
    ] {
        assert!(output(root.path(), &args).status.success(), "{args:?}");
    }
    for name in [
        "graph", "history", "search", "blame", "init", "setup", "upgrade", "doctor",
    ] {
        let result = output(root.path(), &[name]);
        assert_eq!(result.status.code(), Some(1), "{name}");
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("unknown output command"));
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn raw_detection_matches_only_output_invocations() {
    for command in [
        "ctx run --raw -- git status",
        "ctx output run --capture --raw -- git status",
        "ctx --color never run --raw -- cat file",
        "ctx --color=never run --raw -- cat file",
        "ctx --color always output run --raw -- cat file",
        "ctx --color=auto output proxy -- cat file",
        "ctx --quiet run --raw -- cat file",
        "ctx --data-root '/tmp/ctx data' output run --raw -- cat file",
        "ctx --data-root='/tmp/ctx data' run --raw -- cat file",
        "command '/opt/ctx tools/ctx' --quiet --data-root run --color=never output proxy cat file",
        "ctx.exe --color never --quiet output run --raw -- cat file",
        "command '/opt/ctx tools/ctx' output proxy -- cat file",
        "ctx.exe output proxy cat file",
        "sift proxy cat file",
        "sift run --raw -- cat file",
    ] {
        assert!(
            hooks::explicit_raw(&hooks::literal_argv(command).unwrap()),
            "{command}"
        );
    }
    for command in [
        "ctx graph run --raw cat",
        "ctx search --raw cat",
        "ctx output graph --raw cat",
        "ctx run -- tool --raw",
        "ctx output run --capture -- tool --raw",
        "ctx --color never run -- tool --raw",
        "ctx --quiet --data-root '/tmp/ctx data' output run -- tool --raw",
        "ctx run -- tool --color never output run --raw",
        "ctx --color never graph run --raw cat",
        "ctx --quiet search --raw cat",
        "ctx --future run --raw cat",
        "ctx --color sometimes run --raw cat",
        "ctx --color= run --raw cat",
        "ctx --data-root= run --raw cat",
        "ctx --data-root --quiet run --raw cat",
        "ctx --data-root run --raw cat",
        "ctx --quiet=true run --raw cat",
        "ctx --help run --raw cat",
        "ctx --color",
        "ctx --data-root",
        "ctx output run --raw --help",
        "ctx proxy cat",
        "ctx run --raw",
        "echo ctx run --raw cat",
    ] {
        assert!(
            !hooks::explicit_raw(&hooks::literal_argv(command).unwrap()),
            "{command}"
        );
    }
    for command in [
        "ctx --color never run --raw cat | cat",
        "ctx --quiet output proxy cat; echo done",
        "ctx --data-root $(pwd) run --raw cat",
    ] {
        assert!(hooks::literal_argv(command).is_none(), "{command}");
    }
    for text in [
        "ctx graph; git status",
        "ctx search topic",
        "ctx output run -- git status",
    ] {
        assert!(
            rewrite::command(
                text,
                std::path::Path::new("/opt/ctx"),
                rewrite::Shell::Posix,
                &[]
            )
            .is_none()
        );
    }
}

#[test]
fn defaults_and_legacy_overrides_have_one_authoritative_location() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old-sift-config");
    fs::create_dir(&old).unwrap();
    let old_bytes = b"{\"enabled\":false,\"keep_originals\":true}";
    fs::write(old.join("config.json"), old_bytes).unwrap();
    let xdg = root.path().join("xdg");
    let appdata = root.path().join("appdata");
    let defaults = if cfg!(windows) { &appdata } else { &xdg };
    let mut create = command(root.path(), &["config", "--create"]);
    create
        .env("SIFT_CONFIG_DIR", "")
        .env("XDG_CONFIG_HOME", &xdg)
        .env("APPDATA", &appdata);
    assert!(create.output().unwrap().status.success());
    assert!(defaults.join("ctx/output/config.json").is_file());
    assert_eq!(fs::read(old.join("config.json")).unwrap(), old_bytes);
    let result = test_support::clean(
        command(root.path(), &["config"])
            .env("SIFT_CONFIG_DIR", &old)
            .env("XDG_CONFIG_HOME", &xdg)
            .env("APPDATA", &appdata)
            .output()
            .unwrap(),
    );
    assert!(
        !serde_json::from_slice::<state::Settings>(&result.stdout)
            .unwrap()
            .enabled
    );

    let old_state = root.path().join("old-sift-state");
    let settings = state::Settings {
        keep_originals: true,
        ..Default::default()
    };
    state::record_at(
        &old_state,
        &settings,
        state::Event {
            unix_millis: state::unix_millis(),
            command: "fixture".into(),
            input_tokens: None,
            output_tokens: None,
            input_bytes: 11,
            output_bytes: 11,
            duration_ms: 0,
            exit_code: Some(0),
            source: None,
            original_id: None,
        },
        Some((b"legacy\xff", b"err\x00")),
    )
    .unwrap();
    let id = state::list_originals_at(&old_state).unwrap().remove(0).id;
    for (flags, expected) in [
        (vec!["recall", id.as_str()], &b"legacy\xff"[..]),
        (vec!["recall", id.as_str(), "--stderr"], &b"err\x00"[..]),
    ] {
        let result = test_support::clean(
            command(root.path(), &flags)
                .env("SIFT_STATE_DIR", &old_state)
                .env("XDG_STATE_HOME", root.path().join("unused"))
                .env("LOCALAPPDATA", root.path().join("unused"))
                .output()
                .unwrap(),
        );
        assert!(result.status.success());
        assert_eq!(result.stdout, expected);
    }
    assert!(!root.path().join("unused").exists());
    assert!(!root.path().join("state").exists());
}

#[cfg(unix)]
#[test]
fn native_non_utf8_argv_and_paths_survive_the_public_entry_point() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let literal = b"literal\xff $(touch marker) * ''";
    let mut args = [
        "run",
        "--raw",
        "--",
        "/bin/sh",
        "-c",
        "printf '%s' \"$1\"; printf 'error' >&2; exit 23",
        "child",
    ]
    .map(OsString::from)
    .to_vec();
    args.push(OsString::from_vec(literal.to_vec()));
    let result = test_support::clean(
        test_support::command(args)
            .current_dir(root.path())
            .env("SIFT_CONFIG_DIR", root.path().join("config"))
            .env("SIFT_STATE_DIR", root.path().join("state"))
            .output()
            .unwrap(),
    );
    assert_eq!(result.status.code(), Some(23));
    assert_eq!(result.stdout, literal);
    assert_eq!(result.stderr, b"error");
    assert!(!root.path().join("marker").exists());

    let path = root.path().join(OsString::from_vec(b"file\xfe".to_vec()));
    let bytes = b"\xff\x00unmodified\r\n";
    fs::write(&path, bytes).unwrap();
    let args = [OsString::from("compact"), path.into_os_string()];
    let result = test_support::clean(test_support::command(args).output().unwrap());
    assert!(result.status.success());
    assert_eq!(result.stdout, bytes);
}

#[cfg(windows)]
#[test]
fn windows_preserves_status_above_one_byte() {
    let root = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let args = vec![
        OsString::from("run"),
        "--raw".into(),
        "--".into(),
        exe.into_os_string(),
        "--exact".into(),
        "boundary_tests::status_child".into(),
        "--nocapture".into(),
    ];
    let result = test_support::command(args)
        .current_dir(root.path())
        .env("SIFT_CONFIG_DIR", root.path().join("config"))
        .env("SIFT_STATE_DIR", root.path().join("state"))
        .env("CTX_OUTPUT_STATUS_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(0x1234));
}

#[cfg(windows)]
#[test]
fn status_child() {
    if std::env::var_os("CTX_OUTPUT_STATUS_CHILD").is_some() {
        std::process::exit(0x1234);
    }
}
