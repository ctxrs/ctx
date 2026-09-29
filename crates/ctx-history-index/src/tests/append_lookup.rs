use super::*;

#[test]
fn append_lookup_requires_exact_descriptor_and_keeps_missing_sources_unstaged() {
    let temp = tempdir().unwrap();
    let original = source("append-lookup");
    let changed = source_for_provider("codex", "different-format", "append-lookup");
    assert_eq!(original, changed);
    assert!(!original.exact_descriptor_eq(&changed));
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(original.clone()).unwrap();
    writer
        .add_core_record(document(&original, 1, "retained body"))
        .unwrap();
    writer
        .certify_source(appendable_certificate(&original, 1, 1, 10))
        .unwrap();
    let initial = writer.commit(|_| true).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    assert!(matches!(
        writer.begin_source_append(changed),
        Err(IndexError::SourceNotAppendable(_))
    ));
    assert!(matches!(
        writer.begin_source_append(source("absent")),
        Err(IndexError::SourceNotAppendable(_))
    ));
    assert!(writer.pending.is_empty());
    assert!(writer.writer.is_none());
    let certificate = stage_exact_replay(&mut writer, &original);
    assert!(certificate
        .observation()
        .source()
        .exact_descriptor_eq(&original));
    let current = writer.commit(|_| true).unwrap();
    assert_eq!(current.generation_id, initial.generation_id);
}
