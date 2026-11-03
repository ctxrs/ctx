use super::*;

#[test]
fn saved_yaml_memory_preserves_multiline_text_and_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let args = SaveResultArgs {
        question: "How does '001' differ from null?".into(),
        answer: Some("First: preserve the text.\nSecond: keep café and quotes \"intact\".".into()),
        answer_file: None,
        query_type: "query".into(),
        nodes: Vec::new(),
        outcome: Some(Outcome::Corrected),
        correction: Some("Use the quoted value.".into()),
        memory_dir: directory.path().to_owned(),
    };
    let path = save_result_at(&args, None, 123).unwrap();
    assert!(path.is_file());
    let records = load_records(directory.path()).unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.question, args.question);
    assert_eq!(record.answer, args.answer.unwrap());
    assert_eq!(record.correction, args.correction);
    assert_eq!(record.outcome, Some(Outcome::Corrected));
    assert_eq!(record.created_unix_secs, 123);
    assert_eq!(record.contributor, "graf");
}

#[test]
fn existing_native_and_graphify_yaml_memory_remain_readable() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("native.md"),
        "---\nschema_version: 1\nevent_id: existing-event\ncreated_unix_secs: 2\ntype: query\nquestion: '001'\nanswer: |\n  first\n  second\ncontributor: graf\noutcome: useful\ncorrection: null\nsource_nodes: []\ngraph_identity: null\ngraph_generation: null\nevidence: []\n---\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("legacy.md"),
        "---\ncontributor: graphify\ntype: query\ndate: '1970-01-01T00:00:01Z'\nquestion: Legacy question\noutcome: dead_end\nsource_nodes: []\n---\n\n## Answer\nLegacy answer\n\n## Outcome\ndead_end\n",
    )
    .unwrap();
    let records = load_records(directory.path()).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].created_unix_secs, 1);
    assert_eq!(records[0].contributor, "graphify");
    assert_eq!(records[0].question, "Legacy question");
    assert_eq!(records[0].answer, "Legacy answer");
    assert_eq!(records[0].outcome, Some(Outcome::DeadEnd));
    assert_eq!(records[1].event_id, "existing-event");
    assert_eq!(records[1].question, "001");
    assert_eq!(records[1].answer, "first\nsecond\n");
    assert_eq!(records[1].outcome, Some(Outcome::Useful));
    assert!(records[1].correction.is_none());
}
