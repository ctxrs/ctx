use super::*;

#[test]
fn maximum_escaped_labels_fit_certificates_without_changing_identity() {
    let temp = tempdir().unwrap();
    let original = record(&source(), "session", 1, "retained original");
    let archive = archive(temp.path(), std::slice::from_ref(&original));
    let mut manifest = io::read_manifest(&archive).unwrap();
    manifest.identity = ArchiveIdentity {
        origin: "\\".repeat(1024),
        view: "\"".repeat(1024),
    };
    fs::remove_file(archive.join("manifest.json")).unwrap();
    io::write_json(&archive.join("manifest.json"), &manifest).unwrap();
    let mut opts = options();
    opts.binding.identity = manifest.identity.clone();
    let root = temp.path().join("restored");
    restore(&archive, &root, &opts).unwrap();
    let index = VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap();
    let labels = source_identities(&index).unwrap();
    let member = members(&archive).pop().unwrap();
    let mapped = mapped_source(&opts.binding, &member).unwrap();
    assert_eq!(
        labels.get(&mapped.identity().digest()),
        Some(&manifest.identity)
    );
    let event_id = mapped_event(&opts.binding, original.session_id, original.event_id).unwrap();
    assert!(index
        .core_record_by_id(event_id.as_uuid())
        .unwrap()
        .is_some());
    assert!(index.manifest().sources[0].observation().revision().len() < 4096);
}

#[test]
fn legacy_restore_enriches_labels_once_without_changing_records_or_ids() {
    let temp = tempdir().unwrap();
    let original = record(&source(), "session", 1, "legacy original");
    let archive = archive(temp.path(), std::slice::from_ref(&original));
    let root = temp.path().join("restored");
    let receipt = restore(&archive, &root, &options()).unwrap();
    let member = members(&archive).pop().unwrap();
    let mapped = map_record(&options().binding, &member, original).unwrap();
    let index = VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap();
    let certificate = index.manifest().sources[0].clone();
    drop(index);
    let mut writer = GenerationWriter::open(root.join("search/lexical"), writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(mapped.source.clone()).unwrap();
    writer.add_core_record(mapped.clone()).unwrap();
    let observation = SourceObservation::new(
        mapped.source.clone(),
        crate::retained::OBSERVATION_KIND,
        serde_json::to_vec(
            &serde_json::json!({"snapshot":receipt.snapshot_id, "member":member.path}),
        )
        .unwrap(),
    )
    .unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                certificate.parser_revision(),
                *certificate.content_digest(),
                certificate.counts(),
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap();
    assert!(source_identities(&index).unwrap().is_empty());
    drop(index);
    let all = ctx_history_index::CoreEventRangeSelection::all(Default::default()).unwrap();
    let plan = plan_export(&root, identity(), &Selection::default(), &all).unwrap();
    assert_eq!(plan.preview().selected_conversations, 1);
    assert_eq!(plan.preview().scope.untimed_records, 1);
    let output = temp.path().join("legacy-export");
    assert_eq!(plan.export(&output).unwrap().manifest.identity, identity());
    assert_eq!(archive_records(&output), archive_records(&archive));
    let updated = restore(&archive, &root, &options()).unwrap();
    assert_eq!(updated.imported_members, 1);
    let index = VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap();
    assert_eq!(
        source_identities(&index)
            .unwrap()
            .get(&mapped.source.identity().digest()),
        Some(&identity())
    );
    assert_eq!(
        index.core_record_by_id(mapped.event_id.as_uuid()).unwrap(),
        Some(mapped)
    );
    let repeated = restore(&archive, &root, &options()).unwrap();
    assert_eq!(repeated.imported_members, 0);
    assert_eq!(repeated.generation_id, updated.generation_id);
}
