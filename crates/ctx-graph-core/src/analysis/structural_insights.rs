use super::*;

pub(super) fn structural_insights(
    snapshot: &GraphSnapshot,
    metrics: &[NodeMetrics],
    communities: &[Community],
    hubs: &[String],
) -> (
    Vec<SurprisingConnection>,
    usize,
    Vec<SuggestedQuestion>,
    usize,
) {
    let nodes: BTreeMap<_, _> = snapshot.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let metrics: BTreeMap<_, _> = metrics.iter().map(|n| (n.id.as_str(), n)).collect();
    let eligible: BTreeSet<_> = snapshot
        .nodes
        .iter()
        .filter(|n| insight_node(n))
        .map(|n| n.id.as_str())
        .collect();
    let mut edges: Vec<_> = snapshot.edges.iter().collect();
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    let mut incidents = BTreeMap::<&str, Vec<&Edge>>::new();
    let mut surprises = Vec::<SurprisingConnection>::new();
    let mut candidate_count = 0;
    let mut ambiguous = Vec::new();
    let mut ambiguous_count = 0;
    for edge in edges {
        if !eligible.contains(edge.source.as_str()) || !eligible.contains(edge.target.as_str()) {
            continue;
        }
        let source = nodes[edge.source.as_str()];
        let target = nodes[edge.target.as_str()];
        let a = metrics[edge.source.as_str()];
        let b = metrics[edge.target.as_str()];
        incidents.entry(&edge.source).or_default().push(edge);
        if edge.source != edge.target {
            incidents.entry(&edge.target).or_default().push(edge);
        }
        if edge.confidence == "AMBIGUOUS" {
            ambiguous_count += 1;
        }
        if edge.confidence == "AMBIGUOUS" && ambiguous.len() < QUESTION_LIMIT {
            ambiguous.push(SuggestedQuestion {
                kind:"ambiguous_edge".into(),
                question:format!("What source evidence confirms or rejects the recorded {} relation between {} and {}?",edge.relation,source.label,target.label),
                why:"The edge is explicitly tagged AMBIGUOUS; this question does not assert that the relation is correct.".into(),
                node_ids:vec![source.id.clone(),target.id.clone()],community_ids:vec![a.community,b.community],edge_evidence:vec![edge.clone()],evidence_count:1,
            });
        }
        let source_file = source.file.replace('\\', "/");
        let target_file = target.file.replace('\\', "/");
        if structural_relation(&edge.relation)
            || (source_file == target_file && a.community == b.community)
        {
            continue;
        }
        candidate_count += 1;
        let confidence = match edge.confidence.as_str() {
            "AMBIGUOUS" => 3,
            "INFERRED" => 2,
            "EXTRACTED" => 1,
            _ => 0,
        };
        let mut signals = vec![SurpriseSignal {
            code: "confidence".into(),
            points: confidence,
            detail: format!("Recorded confidence: {}", edge.confidence),
        }];
        let mut signal = |code: &str, points, detail: String| {
            signals.push(SurpriseSignal {
                code: code.into(),
                points,
                detail,
            })
        };
        if source_file != target_file {
            signal(
                "cross_source",
                1,
                "Endpoints have different recorded source files".into(),
            );
        }
        let source_dir = source_file.rsplit_once('/').map_or("", |(dir, _)| dir);
        let target_dir = target_file.rsplit_once('/').map_or("", |(dir, _)| dir);
        if source_dir != target_dir {
            signal(
                "cross_directory",
                2,
                format!("Different recorded source directories: {source_dir} / {target_dir}"),
            );
        }
        let source_kind = source_category(&source_file);
        let target_kind = source_category(&target_file);
        if source_kind != target_kind {
            signal(
                "cross_category",
                2,
                format!("Source extension categories: {source_kind} / {target_kind}"),
            );
        }
        if a.community != b.community {
            signal(
                "cross_community",
                1,
                format!(
                    "Endpoints belong to structural communities {} and {}",
                    a.community, b.community
                ),
            );
        }
        if a.degree.min(b.degree) <= 2 && a.degree.max(b.degree) >= 5 {
            signal(
                "peripheral_hub",
                1,
                format!(
                    "Endpoint incidence degrees are {} and {}",
                    a.degree, b.degree
                ),
            );
        }
        surprises.push(SurprisingConnection {
            score: signals.iter().map(|s| s.points).sum(),
            signals,
            edge: edge.clone(),
            source_file: source.file.clone(),
            target_file: target.file.clone(),
            source_community: a.community,
            target_community: b.community,
        });
        surprises.sort_by(|a, b| b.score.cmp(&a.score).then(a.edge.id.cmp(&b.edge.id)));
        surprises.truncate(SURPRISE_LIMIT);
    }
    // Cross-community incidence is linear in graph size; never claim all-pairs
    // betweenness or runtime importance from this bounded structural signal.
    let mut bridge_nodes = Vec::new();
    for (&id, incident) in &incidents {
        let community = metrics[id].community;
        let bridge_edges: Vec<_> = incident
            .iter()
            .copied()
            .filter(|e| {
                !structural_relation(&e.relation)
                    && metrics[e.source.as_str()].community != metrics[e.target.as_str()].community
            })
            .collect();
        let others: BTreeSet<_> = bridge_edges
            .iter()
            .flat_map(|e| {
                [
                    metrics[e.source.as_str()].community,
                    metrics[e.target.as_str()].community,
                ]
            })
            .filter(|c| *c != community)
            .collect();
        if !others.is_empty() {
            bridge_nodes.push((id, others, bridge_edges));
        }
    }
    bridge_nodes.sort_by(|(a, ac, ae), (b, bc, be)| {
        bc.len()
            .cmp(&ac.len())
            .then(be.len().cmp(&ae.len()))
            .then(a.cmp(b))
    });
    let bridge_count = bridge_nodes.len();
    let bridges = bridge_nodes.into_iter().take(QUESTION_LIMIT).map(|(id,others,evidence)|SuggestedQuestion {
        kind:"bridge_node".into(),question:format!("Which recorded relations explain how {} connects to other structural communities?",nodes[id].label),
        why:format!("{} nonstructural edges reach {} other communities; ranked by distinct community count then edge count, not betweenness.",evidence.len(),others.len()),
        node_ids:vec![id.to_owned()],community_ids:std::iter::once(metrics[id].community).chain(others).take(3).collect(),edge_evidence:evidence.iter().take(3).map(|e|(*e).clone()).collect(),evidence_count:evidence.len(),
    }).collect::<Vec<_>>();
    let mut inferred = Vec::new();
    for id in hubs
        .iter()
        .filter(|id| eligible.contains(id.as_str()))
        .take(5)
    {
        let evidence: Vec<_> = incidents
            .get(id.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|e| e.confidence == "INFERRED")
            .collect();
        if evidence.len() < 2 {
            continue;
        }
        inferred.push(SuggestedQuestion {kind:"verify_inferred".into(),question:format!("Which of the {} recorded INFERRED edges involving {} can be checked against their sources?",evidence.len(),nodes[id.as_str()].label),why:format!("The node ranks among the top five eligible hubs and has incidence degree {}; inference is recorded metadata, not verified fact.",metrics[id.as_str()].degree),node_ids:vec![id.clone()],community_ids:vec![metrics[id.as_str()].community],edge_evidence:evidence.iter().take(3).map(|e|(*e).clone()).collect(),evidence_count:evidence.len()});
    }
    let weak: Vec<_> = eligible
        .iter()
        .copied()
        .filter(|id| metrics[id].degree <= 1)
        .collect();
    let isolated = if weak.is_empty() {
        Vec::new()
    } else {
        vec![SuggestedQuestion {
            kind: "isolated_nodes".into(),
            question: format!(
                "Are any relationships involving {} absent from this graph?",
                weak.iter()
                    .take(3)
                    .map(|id| nodes[id].label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            why: format!(
                "{} eligible nodes have recorded degree at most one. This does not prove missing functionality; rationale and source-less/container noise are excluded.",
                weak.len()
            ),
            node_ids: weak.iter().take(3).map(|id| (*id).to_owned()).collect(),
            community_ids: Vec::new(),
            edge_evidence: Vec::new(),
            evidence_count: weak.len(),
        }]
    };
    let mut low: Vec<_> = communities
        .iter()
        .filter(|c| {
            c.cohesion < 0.15
                && c.nodes
                    .iter()
                    .filter(|id| eligible.contains(id.as_str()))
                    .count()
                    >= 5
        })
        .collect();
    low.sort_by(|a, b| a.cohesion.total_cmp(&b.cohesion).then(a.id.cmp(&b.id)));
    let low_count = low.len();
    let low = low.into_iter().take(QUESTION_LIMIT).map(|c|SuggestedQuestion {kind:"low_cohesion".into(),question:cohesion_question(c.id, &c.label),why:format!("Recorded pair cohesion is {:.6} across {} nodes; threshold is below 0.15 with at least five eligible non-noise members. This is not a recommendation to restructure the code.",c.cohesion,c.nodes.len()),node_ids:c.nodes.iter().filter(|id|eligible.contains(id.as_str())).take(3).cloned().collect(),community_ids:vec![c.id],edge_evidence:Vec::new(),evidence_count:c.nodes.len()}).collect::<Vec<_>>();
    // Rotate across signal kinds so many ambiguous edges cannot crowd out every
    // other useful question. Within kinds, ordering is stable and evidence-based.
    let question_count =
        ambiguous_count + bridge_count + inferred.len() + isolated.len() + low_count;
    let categories = [ambiguous, bridges, inferred, isolated, low];
    let mut questions = Vec::new();
    for row in 0..QUESTION_LIMIT {
        for category in &categories {
            if let Some(question) = category.get(row) {
                questions.push(question.clone());
            }
            if questions.len() == QUESTION_LIMIT {
                return (surprises, candidate_count, questions, question_count);
            }
        }
    }
    (surprises, candidate_count, questions, question_count)
}
