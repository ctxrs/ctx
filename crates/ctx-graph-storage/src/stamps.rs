use ctx_graph_types::FileFacts;

pub fn semantic_counts(facts: &FileFacts) -> (usize, usize) {
    (
        facts
            .nodes
            .iter()
            .filter(|n| semantic_provenance(&n.metadata))
            .count(),
        facts
            .edges
            .iter()
            .filter(|e| semantic_provenance(&e.metadata))
            .count(),
    )
}

pub fn semantic_provenance(metadata: &serde_json::Value) -> bool {
    metadata["inferred"] == true
        && matches!(
            metadata["provenance"].as_str(),
            Some("semantic" | "visual_inference")
        )
}

pub fn indexed_source_digest(stamp: &str) -> Option<&str> {
    let (family, _) = stamp.split_once(':')?;
    let known = if let Some(version) = family.strip_prefix("python-v") {
        version
            .parse::<u32>()
            .ok()
            .is_some_and(|v| (1..=ctx_graph_types::EXTRACTOR_REVISION).contains(&v))
    } else if let Some(version) = family.strip_prefix("languages-native-languages-") {
        let current = ctx_graph_languages::revision()
            .strip_prefix("native-languages-")?
            .parse::<u32>()
            .ok()?;
        version
            .parse::<u32>()
            .ok()
            .is_some_and(|v| (1..=current).contains(&v))
    } else {
        family == "ingest-v1"
    };
    if !known || stamp.split(':').count() < 3 || stamp.contains(":oversized:") {
        return None;
    }
    let digest = stamp.rsplit(':').next()?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(digest)
}
