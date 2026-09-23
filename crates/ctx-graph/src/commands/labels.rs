use super::*;

pub(crate) const LABEL_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const LABEL_SIGNATURE: &str = "blake3-sorted-json-members-v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommunityLabel {
    pub(crate) community_id: usize,
    pub(crate) members: Vec<String>,
    pub(crate) label: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LabelFile {
    pub(crate) schema_version: u32,
    pub(crate) generation: u64,
    pub(crate) signature_algorithm: String,
    pub(crate) labels: BTreeMap<String, CommunityLabel>,
}

pub(crate) fn member_signature(members: &[String]) -> Result<String> {
    // JSON strings are unambiguous even for IDs containing delimiters or NULs.
    Ok(blake3::hash(&serde_json::to_vec(members)?)
        .to_hex()
        .to_string())
}

pub(crate) fn read_labels(path: &Path) -> Result<LabelFile> {
    regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LABEL_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= LABEL_BYTES,
        "label file exceeds 8 MiB"
    );
    let mut file: LabelFile = serde_json::from_slice(&bytes).context("invalid Graf label file")?;
    ensure!(
        file.schema_version == 1 && file.signature_algorithm == LABEL_SIGNATURE,
        "unsupported Graf label schema or signature algorithm"
    );
    for (signature, entry) in &mut file.labels {
        entry.members.sort();
        ensure!(
            !entry.members.is_empty() && !entry.members.windows(2).any(|m| m[0] == m[1]),
            "label membership must be nonempty and contain no duplicates"
        );
        ensure!(
            *signature == member_signature(&entry.members)?,
            "label membership signature does not match its members"
        );
    }
    Ok(file)
}

pub(crate) fn label(args: &LabelArgs, db: Option<&Path>, json_output: bool) -> Result<()> {
    let (graph, source) = load(&args.source, db)?;
    let exists = destination(&args.output, std::slice::from_ref(&source))?;
    let input = args
        .input
        .as_deref()
        .or_else(|| exists.then_some(args.output.as_path()));
    let previous = input.map(read_labels).transpose()?;
    let report = analysis::analyze(&graph, &args.source.analysis.options())?;
    let mut labels = BTreeMap::new();
    let mut reused = 0;
    for community in report.communities {
        let mut members = community.nodes;
        members.sort();
        let signature = member_signature(&members)?;
        let prior = previous
            .as_ref()
            .and_then(|file| file.labels.get(&signature))
            .filter(|entry| entry.members == members);
        let label = if let Some(prior) = prior {
            reused += 1;
            prior.label.clone()
        } else {
            community.label
        };
        labels.insert(
            signature,
            CommunityLabel {
                community_id: community.id,
                members,
                label,
            },
        );
    }
    let count = labels.len();
    let file = LabelFile {
        schema_version: 1,
        generation: graph.generation,
        signature_algorithm: LABEL_SIGNATURE.into(),
        labels,
    };
    let bytes = serde_json::to_vec_pretty(&file)?;
    ensure!(
        bytes.len() as u64 <= LABEL_BYTES,
        "label output exceeds 8 MiB"
    );
    write_atomic(&args.output, &bytes, &[source])?;
    print(
        &json!({"output":args.output,"generation":graph.generation,"communities":count,
        "reused":reused,"generated":count-reused,"signature_algorithm":LABEL_SIGNATURE}),
        json_output,
    )
}
