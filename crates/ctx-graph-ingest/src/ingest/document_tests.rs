use super::*;

fn parse(text: &str) -> Result<FileFacts> {
    extract_bytes(
        Path::new("manifest.yaml"),
        "manifest.yaml",
        text.as_bytes(),
        "hash",
        &IngestOptions::default(),
    )
}

#[test]
fn yaml_stream_preserves_repeated_fields_with_document_identity() {
    let facts = parse("kind: First\n---\nkind: Second\n").unwrap();
    let fields: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == "document_field")
        .collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].label, "kind");
    assert_eq!(fields[1].label, "kind");
    assert_ne!(fields[0].id, fields[1].id);
    assert_eq!(fields[0].metadata["value"], "First");
    assert_eq!(fields[1].metadata["value"], "Second");
    assert_eq!(fields[0].metadata["yaml_document"], 1);
    assert_eq!(fields[1].metadata["yaml_document"], 2);
    assert_eq!(facts.edges[0].metadata["yaml_document"], 1);
    assert_eq!(facts.edges[1].metadata["yaml_document"], 2);
    assert_eq!(facts.nodes[0].metadata["yaml_documents"], 2);
    assert!(fields.iter().all(|node| node.line.is_none()));

    let single = parse("kind: First\n").unwrap();
    assert_eq!(fields[0].id, single.nodes[1].id);
    assert!(single.nodes[1].metadata.get("yaml_document").is_none());
    assert!(single.nodes[0].metadata.get("yaml_documents").is_none());
}

#[test]
fn yaml_empty_documents_and_scalar_markers_use_native_stream_rules() {
    let facts = parse("---\n---\ntext: |\n  ---\n  ...\nquoted: '---'\n...\n---\n").unwrap();
    assert_eq!(facts.nodes[0].metadata["yaml_documents"], 3);
    let fields: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == "document_field")
        .collect();
    assert_eq!(fields.len(), 2);
    assert!(
        fields
            .iter()
            .all(|node| node.metadata["yaml_document"] == 2)
    );
    assert!(
        fields
            .iter()
            .any(|node| node.metadata["value"] == "---\n...\n")
    );
    assert!(fields.iter().any(|node| node.metadata["value"] == "---"));
    assert_eq!(parse("").unwrap().nodes.len(), 1);
}

#[test]
fn malformed_later_yaml_and_invalid_encoding_have_typed_locations() {
    let error = parse("kind: First\n---\nkind: [broken\n").unwrap_err();
    let rejected = error.downcast_ref::<InputRejected>().unwrap();
    assert_eq!(rejected.file, "manifest.yaml");
    assert_eq!(rejected.document, Some(2));
    assert!(rejected.line.unwrap() >= 3);
    assert!(rejected.column.is_some());
    assert!(parse("key: first\nkey: second\n").is_err());
    for extension in ["txt", "yaml", "md", "gdoc"] {
        let path = format!("invalid.{extension}");
        let error = extract_bytes(
            Path::new(&path),
            &path,
            b"\xff",
            "hash",
            &IngestOptions::default(),
        )
        .unwrap_err();
        assert!(error.is::<InputRejected>(), "{error:#}");
    }
}

#[test]
fn yaml_limits_apply_to_the_whole_file_and_remain_fatal() {
    let document = format!("[{}]\n", vec!["0"; 10_000].join(","));
    parse(&document).unwrap();
    let error = parse(&format!("{document}---\n{document}")).unwrap_err();
    assert!(!error.is::<InputRejected>());
    assert!(error.to_string().contains("limit"));
    for depth in [65, 130] {
        let nested = format!("{}value{}", "[".repeat(depth), "]".repeat(depth));
        let error = parse(&nested).unwrap_err();
        assert!(!error.is::<InputRejected>(), "{error:#}");
    }
    let error = parse("recursive: &recursive [*recursive]\n").unwrap_err();
    assert!(!error.is::<InputRejected>(), "{error:#}");
    let options = IngestOptions {
        max_input_bytes: 1,
        ..Default::default()
    };
    let error = extract_bytes(
        Path::new("input.txt"),
        "input.txt",
        b"large",
        "hash",
        &options,
    )
    .unwrap_err();
    assert!(!error.is::<InputRejected>());
}

#[cfg(unix)]
#[test]
fn configured_converter_receives_snapshot_and_overrides_native_yaml() {
    let mut options = IngestOptions::default();
    options.converters.insert(
        "yaml".into(),
        CommandAdapter {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "test \"$(cat \"$1\")\" = 'key: [broken' && printf 'converted snapshot'".into(),
                "converter".into(),
                "{input}".into(),
            ],
            output_file: false,
        },
    );
    let facts = extract_bytes(
        Path::new("missing.yaml"),
        "renamed.txt",
        b"key: [broken",
        "hash",
        &options,
    )
    .unwrap();
    assert_eq!(facts.nodes[0].metadata["converter"], "configured_converter");
    assert_eq!(facts.nodes[0].metadata["text"], "converted snapshot");
    options.converters.get_mut("yaml").unwrap().args = vec!["-c".into(), "printf '\\377'".into()];
    let error = extract_bytes(
        Path::new("missing.yaml"),
        "renamed.txt",
        b"key: [broken",
        "hash",
        &options,
    )
    .unwrap_err();
    assert!(
        !error.is::<InputRejected>(),
        "converter output must not be a local input rejection"
    );
}

#[test]
fn yaml_aliases_are_scoped_to_each_document() {
    let facts =
        parse("name: &name First\ncopy: *name\n---\nname: &name Second\ncopy: *name\n").unwrap();
    let copies: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.label == "copy")
        .collect();
    assert_eq!(copies.len(), 2);
    assert_eq!(copies[0].metadata["value"], "First");
    assert_eq!(copies[1].metadata["value"], "Second");
    assert_eq!(copies[0].metadata["yaml_document"], 1);
    assert_eq!(copies[1].metadata["yaml_document"], 2);
    let error = parse("name: &name First\n---\ncopy: *name\n").unwrap_err();
    assert_eq!(
        error.downcast_ref::<InputRejected>().unwrap().document,
        Some(2)
    );
}

#[test]
fn frontmatter_and_pointer_syntax_are_rejected_but_native_limits_are_fatal() {
    for (path, text, expected_line) in [
        ("note.md", "---\nkey: [broken\n---\nBody\n", 3),
        ("note.gdoc", "{\n \"doc_id\": }", 2),
    ] {
        let error = extract_text(path, text, "hash", &IngestOptions::default()).unwrap_err();
        let rejected = error.downcast_ref::<InputRejected>().unwrap();
        assert_eq!(rejected.file, path);
        assert_eq!(rejected.line, Some(expected_line));
        assert!(rejected.column.is_some());
    }
    let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    for (path, text) in [
        ("note.md", format!("---\n{deep}\n---\nBody\n")),
        ("note.gdoc", deep),
    ] {
        let error = extract_text(path, &text, "hash", &IngestOptions::default()).unwrap_err();
        assert!(!error.is::<InputRejected>(), "{error:#}");
    }
    // A valid pointer still parses locally without exporting it or calling its adapter.
    let facts = extract_text(
        "note.gdoc",
        r#"{"doc_id":"example-id"}"#,
        "hash",
        &IngestOptions::default(),
    )
    .unwrap();
    assert_eq!(facts.nodes[0].metadata["pointer"]["file_id"], "example-id");
}

#[test]
fn plain_converter_output_does_not_parse_frontmatter() {
    let text = "---\ntitle: [unfinished\n---\nBody\n";
    let facts = converted(
        "document.docx",
        text,
        "hash",
        &IngestOptions::default(),
        "configured_converter",
    )
    .unwrap();
    assert_eq!(facts.nodes[0].metadata["text"], text);
    assert_eq!(facts.nodes[0].metadata["converter"], "configured_converter");
    let options = IngestOptions {
        max_text_bytes: 1,
        ..Default::default()
    };
    let error = converted(
        "document.docx",
        text,
        "hash",
        &options,
        "configured_converter",
    )
    .unwrap_err();
    assert!(!error.is::<InputRejected>());
}
