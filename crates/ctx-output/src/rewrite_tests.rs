// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use crate::rewrite;
use rewrite::Shell;
use std::path::Path;

fn rewrite(text: &str) -> Option<String> {
    rewrite::command(text, Path::new("/opt/ctx tools/ctx"), Shell::Posix, &[])
}

#[test]
fn preserves_literal_arguments_and_operator_order() {
    assert!(rewrite("LANG=C git status --short").is_none());
    assert!(rewrite("rg '$literal' $FILE").is_none());
    assert_eq!(rewrite("rg 'a b' \"x*y\" file"), Some("command true || rg 'a b' \"x*y\" file; command '/opt/ctx tools/ctx' output run --capture -- rg 'a b' \"x*y\" file".into()));
    assert_eq!(rewrite("cd project && git status; cargo test"), Some("command true || git status; command true || cargo test; cd project && command '/opt/ctx tools/ctx' output run --capture -- git status; command '/opt/ctx tools/ctx' output run --capture -- cargo test".into()));
}

#[test]
fn leaves_pipeline_data_and_file_output_untouched() {
    assert!(rewrite("git show | wc -l").is_none());
    for text in [
        "git show > artifact",
        "git status 2>&1",
        "rg foo | custom-consumer",
        "git show | sed -n '1p'",
        "cat <<EOF\nhi\nEOF",
    ] {
        assert!(rewrite(text).is_none(), "{text}");
    }
}

#[test]
fn unknown_syntax_and_existing_wrappers_are_passthrough() {
    for text in [
        "sift git status",
        "'/opt/ctx tools/ctx' output run -- git status",
        "git show $(touch marker)",
        "git show `date`",
        "git status &",
        "for x in a; do git show $x; done",
        "git 'status",
        "echo hi",
        "git status # comment",
        "function git() { echo x; }",
    ] {
        assert!(rewrite(text).is_none(), "{text}");
    }
}

#[test]
fn interactive_or_open_ended_commands_keep_streaming_mode() {
    assert_eq!(
        rewrite("npm run dev"),
        Some(
            "command true || npm run dev; command '/opt/ctx tools/ctx' output run -- npm run dev"
                .into()
        )
    );
    assert_eq!(rewrite("tail -f app.log"), Some("command true || tail -f app.log; command '/opt/ctx tools/ctx' output run -- tail -f app.log".into()));
    assert_eq!(rewrite("git add --patch"), Some("command true || git add --patch; command '/opt/ctx tools/ctx' output run -- git add --patch".into()));
}

#[test]
fn powershell_quotes_wrapper_and_keeps_native_arguments() {
    assert!(
        rewrite::command(
            "git status --short",
            Path::new("C:\\O'Brien tools\\sift.exe"),
            Shell::PowerShell,
            &[]
        )
        .is_none()
    );
    assert_eq!(
        rewrite::quote("C:\\O'Brien tools\\sift.exe", Shell::PowerShell),
        "'C:\\O''Brien tools\\sift.exe'"
    );
    assert!(
        rewrite::command(
            "git status",
            Path::new("sift"),
            Shell::Posix,
            &["git".into()]
        )
        .is_none()
    );
    assert_eq!(
        rewrite::quote("/opt/O'Brien/sift", Shell::Posix),
        "'/opt/O'\"'\"'Brien/sift'"
    );
}

#[cfg(unix)]
#[test]
fn escaped_final_whitespace_keeps_execution_arguments_streams_and_status() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let root = std::env::temp_dir().join(format!("sift-rewrite-shell-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let git = root.join("git");
    std::fs::write(&git, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf 'diagnostic\\n' >&2\ncase \"$1\" in exit17) exit 17;; esac\n").unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
    let wrapper = root.join("ctx");
    std::fs::write(&wrapper, b"#!/bin/sh\n[ \"$1\" = output ] || exit 80\nshift\n[ \"$1\" = run ] || exit 81\nshift\n[ \"$1\" != --capture ] || shift\n[ \"$1\" != -- ] || shift\nexec \"$@\"\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let paths = std::env::join_paths(std::iter::once(root.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    for shell in ["/bin/sh", "/bin/bash"] {
        for input in [
            "git status path\\ ",
            "git exit17 path\\ ",
            "git status \\\n",
            "git status path\\\t",
            "git status 'path ' ''",
            "git exit17 && git status || git diff",
        ] {
            let changed = rewrite::command(input, &wrapper, Shell::Posix, &[]).unwrap();
            let run = |text: &str| {
                Command::new(shell)
                    .args(["-c", text])
                    .current_dir(&root)
                    .env("PATH", &paths)
                    .env("SIFT_CONFIG_DIR", root.join("config"))
                    .env("SIFT_STATE_DIR", root.join("state"))
                    .output()
                    .unwrap()
            };
            let original = run(input);
            let actual = run(&changed);
            assert_eq!(
                (actual.status.code(), actual.stdout, actual.stderr),
                (original.status.code(), original.stdout, original.stderr),
                "{shell}: {input:?}"
            );
        }
    }
}
