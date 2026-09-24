// Adapted from Sift tests, MIT; see this crate’s NOTICE.
// Test the independent contract while the CLI/runner wiring is integrated.
use crate::views;

use std::ffi::OsString;
use views::{Action, JsonOptions, ReadOptions, View, parse, render};

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn input_view(name: &str, values: &[&str]) -> View {
    match parse(name, &args(values)).unwrap() {
        Action::Input { view, .. } => view,
        _ => panic!("expected an input view"),
    }
}

#[test]
fn read_preserves_line_bytes_and_orders_start_match_limit() {
    let bytes = b"match first\r\nmiss\r\nmatch third\r\nmatch fourth\nlast\xff";
    let view = input_view("read", &["--from", "2", "--grep", "match", "--lines=1"]);
    assert_eq!(render(&view, bytes).unwrap(), b"match third\r\n");
    assert!(view.is_read_selection());
    let plain = input_view("read", &[]);
    assert!(!plain.is_read_selection());
    assert_eq!(render(&plain, bytes).unwrap(), bytes);
    assert_eq!(
        render(&input_view("read", &["--from=5"]), bytes).unwrap(),
        b"last\xff"
    );
    for options in [&["--lines=0"][..], &["--from=99"], &["--grep=absent"]] {
        assert!(
            render(&input_view("read", options), bytes)
                .unwrap()
                .is_empty()
        );
    }
    assert!(input_view("read", &["--grep="]).is_read_selection());
    assert!(input_view("read", &["--from=1"]).is_read_selection());
    assert_eq!(render(&plain, b"\n\n").unwrap(), b"\n\n");
    assert!(
        render(
            &View::Read(ReadOptions {
                from: Some(0),
                ..Default::default()
            }),
            b"a"
        )
        .is_err()
    );
}

#[test]
fn json_pointer_limit_and_projection_have_plain_json_semantics() {
    let bytes = br#"{"items":[{"id":1,"label":"one","extra":true},{"id":2,"label":"two"},{"id":3,"label":"last"}]}"#;
    let view = input_view(
        "json",
        &[
            "--pointer=/items",
            "--field=label",
            "--field=id",
            "--limit=2",
        ],
    );
    assert_eq!(
        render(&view, bytes).unwrap(),
        b"[{\"label\":\"one\",\"id\":1},{\"label\":\"two\",\"id\":2}]\n"
    );
    let value = input_view("json", &["--pointer=/items/2/id"]);
    assert_eq!(render(&value, bytes).unwrap(), b"3\n");
    assert_eq!(
        render(&input_view("json", &["--limit=0"]), b"[1,2]").unwrap(),
        b"[]\n"
    );
    assert_eq!(
        render(&input_view("json", &[]), b"  null  ").unwrap(),
        b"null\n"
    );
    let empty = input_view("json", &["--pointer=/a~1b/~0key/"]);
    assert_eq!(
        render(&empty, br#"{"a/b":{"~key":{"":false}}}"#).unwrap(),
        b"false\n"
    );
    assert_eq!(
        render(&input_view("json", &["--field="]), br#"{"":42}"#).unwrap(),
        b"{\"\":42}\n"
    );
}

#[test]
fn json_numbers_and_reserved_markers_remain_literal() {
    let bytes = br#"{"items":[{"n":1234567890123456789012345678901234567890,"e":1.2300E+9999,"z":-0,"marker":{"$serde_json::private::Number":"not a number","$serde_json::private::RawValue":"literal"}}]}"#;
    let view = input_view(
        "json",
        &[
            "--pointer=/items",
            "--field=n",
            "--field=e",
            "--field=z",
            "--field=marker",
        ],
    );
    assert_eq!(render(&view, bytes).unwrap(), br#"[{"n":1234567890123456789012345678901234567890,"e":1.2300E+9999,"z":-0,"marker":{"$serde_json::private::Number":"not a number","$serde_json::private::RawValue":"literal"}}]
"#);
    let marker = input_view("json", &["--field=$serde_json::private::Number"]);
    assert_eq!(
        render(&marker, br#"{"$serde_json::private::Number":"123"}"#).unwrap(),
        b"{\"$serde_json::private::Number\":\"123\"}\n"
    );
}

#[test]
fn json_rejects_invalid_unselected_data_and_ambiguous_keys() {
    let view = input_view("json", &["--pointer=/selected"]);
    for bytes in [
        &br#"{"selected":1,"ignored":{"a":1,"a":2}}"#[..],
        br#"{"selected":1,"ignored":{"a":1,"\u0061":2}}"#,
        br#"{"selected":1,"ignored":"\ud800"}"#,
        br#"{"selected":1} {"another":2}"#,
        br#"{"selected":01}"#,
        br#"{"selected":NaN}"#,
        br#"banner {"selected":1}"#,
        b"{\"selected\":\"\xff\"}",
    ] {
        assert!(render(&view, bytes).is_err(), "accepted {bytes:?}");
    }
    let deep = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    assert!(render(&input_view("json", &[]), deep.as_bytes()).is_err());
    let shallow = format!("{}0{}", "[".repeat(64), "]".repeat(64));
    assert!(render(&input_view("json", &[]), shallow.as_bytes()).is_ok());
}

#[test]
fn json_selection_never_silently_coerces_or_fills_missing_values() {
    for (options, source) in [
        (&["--pointer=/missing"][..], "{}"),
        (&["--pointer=/01"], "[1,2]"),
        (&["--pointer=/-"], "[1,2]"),
        (&["--pointer=/2"], "[1,2]"),
        (&["--pointer=/child"], "1"),
        (&["--limit=1"], "{}"),
        (&["--field=x"], "{\"y\":1}"),
        (&["--field=x"], "[{\"x\":1},{\"y\":2}]"),
        (&["--field=x"], "[1]"),
    ] {
        assert!(
            render(&input_view("json", options), source.as_bytes()).is_err(),
            "{options:?}: {source}"
        );
    }
    // Array prefix is applied before field projection, as declared in help.
    assert_eq!(
        render(
            &input_view("json", &["--field=x", "--limit=1"]),
            b"[{\"x\":1},2]"
        )
        .unwrap(),
        b"[{\"x\":1}]\n"
    );
    let duplicate = View::Json(JsonOptions {
        fields: vec!["x".into(), "x".into()],
        ..Default::default()
    });
    assert!(render(&duplicate, b"{\"x\":1}").is_err());
}

#[test]
fn command_views_normalize_argv_without_running_native_test_or_a_shell() {
    for name in ["err", "test", "summary"] {
        for prefix in [&[][..], &["--"][..]] {
            let mut command = args(prefix);
            command.extend(args(&[
                "cargo",
                "test",
                "--",
                "name with spaces",
                "$(touch marker)",
            ]));
            match parse(name, &command).unwrap() {
                Action::Command { argv, .. } => assert_eq!(
                    argv,
                    args(&["cargo", "test", "--", "name with spaces", "$(touch marker)"])
                ),
                _ => panic!("expected command"),
            }
        }
    }
    let Action::Command { argv, view } =
        parse("test", &args(&["--context=0", "--", "my tester", "--help"])).unwrap()
    else {
        panic!()
    };
    assert_eq!(argv, args(&["my tester", "--help"]));
    assert_eq!(view, View::Test { context: 0 });
    assert!(parse("test", &args(&["-f", "file"])).is_err());
    assert!(parse("test", &args(&["--"])).is_err());
    assert!(parse("summary", &[]).is_err());
}

#[test]
fn parser_rejects_bad_options_and_respects_filename_separator() {
    for (name, options) in [
        ("read", &["--from=0"][..]),
        ("read", &["--lines=-1"]),
        ("read", &["--lines=+1"]),
        ("read", &["--lines=1", "--lines=2"]),
        ("read", &["--from"]),
        ("read", &["one", "two"]),
        ("json", &["--pointer=x"]),
        ("json", &["--pointer=/x~2"]),
        ("json", &["--pointer=/x~"]),
        ("json", &["--field=x", "--field=x"]),
        ("json", &["--limit=999999999999999999999999999999999999"]),
        ("summary", &["--grep=x", "tool"]),
        ("test", &["--context=", "tool"]),
    ] {
        assert!(parse(name, &args(options)).is_err(), "{name}: {options:?}");
    }
    let Action::Input { path, .. } = parse("read", &args(&["--", "--grep"])).unwrap() else {
        panic!()
    };
    assert_eq!(path, Some("--grep".into()));
    assert_eq!(parse("json", &args(&["--help"])).unwrap(), Action::Help);
}

#[cfg(unix)]
#[test]
fn non_utf8_filenames_and_child_arguments_survive_parsing() {
    use std::os::unix::ffi::OsStringExt;
    let name = OsString::from_vec(b"name-\xff".to_vec());
    let Action::Input { path, .. } = parse("read", std::slice::from_ref(&name)).unwrap() else {
        panic!()
    };
    assert_eq!(path, Some(name.clone()));
    let Action::Command { argv, .. } = parse("test", &["tester".into(), name.clone()]).unwrap()
    else {
        panic!()
    };
    assert_eq!(argv[1], name);
}

#[test]
fn summary_marks_exact_omissions_and_keeps_ends() {
    let out = render(&View::Summary { lines: 1 }, b"first\r\nsecond\nthird\nlast").unwrap();
    assert_eq!(out, b"Sift summary head/tail view: 2/4 source lines; 2 omitted.\nfirst\r\n[ctx sift: 2 lines omitted]\nlast");
    assert_eq!(
        render(&View::Summary { lines: 1 }, b"one\ntwo").unwrap(),
        b"one\ntwo"
    );
    assert_eq!(
        render(&View::Summary { lines: usize::MAX }, b"one\ntwo").unwrap(),
        b"one\ntwo"
    );
    assert_eq!(
        render(&View::Summary { lines: 0 }, b"one\ntwo").unwrap(),
        b"Sift summary head/tail view: 0/2 source lines; 2 omitted.\n[ctx sift: 2 lines omitted]\n"
    );
}

#[test]
fn diagnostics_keep_context_and_fallback_without_inventing_results() {
    let source = b"noise\nsetup\nassertion\nERROR mismatch\nstack frame\nmore noise\nend\n";
    let out = render(&View::Errors { context: 1 }, source).unwrap();
    assert_eq!(out, b"Sift diagnostic keyword/context view: 3/7 source lines; 4 omitted.\n[ctx sift: 2 lines omitted]\nassertion\nERROR mismatch\nstack frame\n[ctx sift: 2 lines omitted]\n");
    let source = b"start\nFAILED item\nexplanation\npanic next\nend\n";
    let out = render(&View::Test { context: 1 }, source).unwrap();
    assert!(out.ends_with(source));
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("5/5 source lines; 0 omitted")
    );
    for view in [
        View::Errors { context: 0 },
        View::Test { context: 0 },
        View::Summary { lines: 0 },
    ] {
        for source in [&b"binary\xff\nerror\n"[..], b"zero\0\nerror\n"] {
            assert_eq!(render(&view, source).unwrap(), source);
        }
    }
    for source in [&b"unrecognized output\n"[..], b"", b"all checks passed\n"] {
        assert_eq!(render(&View::Test { context: 2 }, source).unwrap(), source);
    }
    // This is deliberately a keyword view, not a fabricated failure count.
    let out = render(
        &View::Test {
            context: usize::MAX,
        },
        b"0 failures\nunchanged detail\n",
    )
    .unwrap();
    assert!(out.ends_with(b"0 failures\nunchanged detail\n"));
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("not a test result parser")
    );
}

#[test]
fn view_size_limit_is_an_error_not_silent_truncation() {
    let bytes = vec![b'x'; 16 * 1024 * 1024 + 1];
    assert!(render(&View::Summary { lines: 1 }, &bytes).is_err());
}
