use super::*;

fn state(label: &str) -> GenerationStateEnvelope {
    GenerationStateEnvelope::new("ctx.test-source-state.v1", label.as_bytes().to_vec()).unwrap()
}

fn stage_initial(root: &Path, source: &SourceKey) -> GenerationWriter {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer
        .add_core_record(document(source, 1, "generation state identity"))
        .unwrap();
    writer
        .certify_source(appendable_certificate(source, 1, 1, 10))
        .unwrap();
    writer
}

fn stage_replay(
    root: &Path,
    source: &SourceKey,
    inventory: &CertifiedSourceInventory,
) -> GenerationWriter {
    let mut writer = GenerationWriter::open(root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .certify_complete_inventory(inventory.clone())
        .unwrap();
    stage_exact_replay(&mut writer, source);
    writer
}

fn generation_directories(root: &Path) -> HashSet<std::ffi::OsString> {
    fs::read_dir(root.join(INDEX_GENERATIONS_DIRECTORY))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

#[test]
fn generation_state_participates_in_reuse_and_generation_identity() {
    let temp = tempdir().unwrap();
    let source = source("generation-state.jsonl");
    let original_state = state("state-a");
    let mut initial_stages = Vec::new();
    let initial = stage_initial(temp.path(), &source)
        .commit_with_generation_state(
            |_| true,
            |_| false,
            |_| Ok(original_state.clone()),
            |stage| {
                initial_stages.push(stage);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        initial_stages,
        vec![
            PublicationStage::Merging,
            PublicationStage::Syncing,
            PublicationStage::PhysicalVerification,
            PublicationStage::LogicalVerification,
            PublicationStage::Activation,
        ]
    );
    let pointer_before = fs::read(temp.path().join("active-generation.json")).unwrap();
    let directories_before = generation_directories(temp.path());
    let inventory = complete_inventory(&source, 1, vec![source.clone()]);

    let mut reuse_stages = Vec::new();
    let reused = stage_replay(temp.path(), &source, &inventory)
        .commit_with_generation_state(
            |_| true,
            |current| current == &inventory,
            |context| {
                assert_eq!(context.manifest().generation_state(), Some(&original_state));
                Ok(original_state.clone())
            },
            |stage| {
                reuse_stages.push(stage);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(reused.disposition(), PublicationDisposition::Reused);
    assert_eq!(
        reuse_stages,
        vec![
            PublicationStage::PhysicalVerification,
            PublicationStage::Activation,
        ]
    );
    assert_eq!(
        reused.receipt().generation_id,
        initial.receipt().generation_id
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer_before
    );
    assert_eq!(generation_directories(temp.path()), directories_before);

    let changed_state = state("state-b");
    let changed = stage_replay(temp.path(), &source, &inventory)
        .commit_with_generation_state(
            |_| true,
            |current| current == &inventory,
            |_| Ok(changed_state.clone()),
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(changed.disposition(), PublicationDisposition::Published);
    assert_ne!(
        changed.receipt().generation_id,
        initial.receipt().generation_id
    );
    assert_eq!(
        changed.receipt().indexed_documents,
        initial.receipt().indexed_documents
    );
    assert_eq!(
        changed.receipt().manifest().generation_state(),
        Some(&changed_state)
    );
}
