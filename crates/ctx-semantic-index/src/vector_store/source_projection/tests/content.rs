use super::*;

#[test]
fn control_filter_and_full_tail_remain_generation_exact() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let body = format!("{} {TAIL_TOKEN}", "prefix ".repeat(2_500));
    let index = fixture.publish(
        "content",
        &[(
            0,
            vec![
                "<environment_context>control</environment_context>".to_owned(),
                body,
            ],
        )],
    )?;
    let page = index.core_semantic_event_page(None, 2)?;
    let tail_event = page
        .items
        .iter()
        .find(|item| {
            item.core_record
                .content
                .meaningful_text()
                .ends_with(TAIL_TOKEN)
        })
        .ok_or_else(|| anyhow!("missing complete tail content"))?
        .event_id
        .as_uuid();
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_filtered, 1);
    assert_eq!(active_events(&store)?, 1);
    let pin = match store.source_backed_generation_pin_exact(index.generation_id(), 2)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!("nonempty reconciled generation was not pinned"));
        }
    };
    let mut query = vec![0.0; SEMANTIC_DIMENSIONS];
    query[0] = 1.0;
    let event_identity_digest = |event_id: Uuid| {
        let mut digest = [0; 32];
        digest[..16].copy_from_slice(event_id.as_bytes());
        digest[16..].copy_from_slice(event_id.as_bytes());
        Some(digest)
    };
    let search = scan_exact_generation(
        &pin,
        std::slice::from_ref(&query),
        1,
        &event_identity_digest,
        Instant::now(),
    )?;
    assert_eq!(search.hits[0].event_id, tail_event);
    for directory in [
        fixture.semantic_path.clone(),
        fixture.semantic_path.join("flat_segments"),
    ] {
        if directory.exists() {
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if path.is_file() {
                    let bytes = fs::read(path)?;
                    assert!(!bytes
                        .windows(TAIL_TOKEN.len())
                        .any(|window| window == TAIL_TOKEN.as_bytes()));
                }
            }
        }
    }
    Ok(())
}
