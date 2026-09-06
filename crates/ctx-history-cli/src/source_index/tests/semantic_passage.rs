use super::*;
use ctx_history_read_application::{
    execute_search_observed, plan_search, GenerationRead, GenerationReadRequest,
    GenerationReadTarget, HistorySemanticBatch, HistorySemanticError, HistorySemanticPort,
    HistorySemanticQuery, SearchApplicationReadModelInput, SearchApplicationRequest, SearchPolicy,
    SearchRenderMetrics, SearchResultCommands, SemanticAvailability,
};
use ctx_semantic_index::{
    semantic_model_contract, source_backed_semantic_vector_path, SemanticBatchEmbedder,
    SemanticChunkDocument, SemanticQueryPin, SemanticVectorStore,
    SourceBackedSemanticDocumentBuilder,
};

const PASSAGE_QUERY: &str = "quartzmarigold";

// Deterministic executor: the literal marker, not a model or network service,
// selects the chunk. All projection, scoring and output code is production.
struct MarkerEmbedder;

impl SemanticBatchEmbedder for MarkerEmbedder {
    fn document_fits(&mut self, _: &str) -> anyhow::Result<bool> {
        Ok(true)
    }

    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(chunks
            .iter()
            .map(|chunk| marker_vector(chunk.text()))
            .collect())
    }
}

fn marker_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; semantic_model_contract().dimensions()];
    vector[usize::from(!text.contains(PASSAGE_QUERY))] = 1.0;
    vector
}

struct PassagePort(PathBuf, usize, bool);
struct PassageQuery<'a> {
    index: &'a VerifiedIndex,
    pin: SemanticQueryPin,
    vectors: Vec<Vec<f32>>,
    query_dimension: usize,
    corrupt_passage: bool,
}

impl HistorySemanticPort for PassagePort {
    type Query<'a> = PassageQuery<'a>;

    fn begin_query<'a>(
        &'a self,
        index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError> {
        Ok(PassageQuery {
            index,
            pin: SemanticQueryPin::preflight(index, &self.0, semantic_model_contract())
                .map_err(|error| HistorySemanticError::failed(error.to_string()))?,
            vectors: Vec::new(),
            query_dimension: self.1,
            corrupt_passage: self.2,
        })
    }
}

impl HistorySemanticQuery for PassageQuery<'_> {
    fn resolve_passage(
        &mut self,
        event: &ctx_history_index::RankedEventRef,
        evidence: &ctx_history_index::SemanticSearchEvidence,
    ) -> Result<ctx_history_index::SemanticPassageSource, HistorySemanticError> {
        let mut evidence = evidence.clone();
        if self.corrupt_passage {
            evidence.source_text_hash = "00".repeat(32);
        }
        self.pin
            .resolve_passage(self.index, semantic_model_contract(), event, &evidence)
            .map_err(|error| {
                if let Some(typed) = error.downcast_ref::<ctx_semantic_index::SemanticNotReady>() {
                    HistorySemanticError::not_ready(
                        ctx_history_read_application::SemanticReason::from_adapter_code(
                            typed.code(),
                        ),
                        typed.detail(),
                        typed.retryable(),
                    )
                } else {
                    HistorySemanticError::failed(error.to_string())
                }
            })
    }

    fn prepare_alternative(&mut self, query: &str) -> Result<Value, HistorySemanticError> {
        assert!(!query.is_empty());
        let mut vector = vec![0.0; semantic_model_contract().dimensions()];
        vector[self.query_dimension] = 1.0;
        self.vectors.push(vector);
        Ok(json!({}))
    }

    fn candidates(
        &mut self,
        filter: &CompiledSearchFilter,
        limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError> {
        let (candidates, diagnostics) =
            self.pin
                .search(self.index, filter, &self.vectors, limit)
                .map_err(|error| HistorySemanticError::failed(error.to_string()))?;
        Ok(HistorySemanticBatch {
            candidates,
            diagnostics,
        })
    }
}

fn passage_fixture(bodies: &[(&str, String)]) -> (tempfile::TempDir, Vec<CoreEventRecord>) {
    passage_fixture_with_embedder(bodies, &mut MarkerEmbedder)
}

fn passage_fixture_with_embedder(
    bodies: &[(&str, String)],
    embedder: &mut dyn SemanticBatchEmbedder,
) -> (tempfile::TempDir, Vec<CoreEventRecord>) {
    let temp = tempdir().unwrap();
    let records = bodies
        .iter()
        .enumerate()
        .map(|(position, (role, body))| {
            let mut event = fixture_event(
                CaptureProvider::Codex,
                "codex_session_jsonl",
                91,
                position as u64 + 1,
            );
            event.role = Some(role.to_string());
            fixture_core_event(&event, body.clone())
        })
        .collect::<Vec<_>>();
    append_fixture_session(temp.path(), &records, 91);
    let index = VerifiedIndex::open_pinned(index_root(temp.path())).unwrap();
    let mut store = SemanticVectorStore::open(
        &source_backed_semantic_vector_path(temp.path()),
        semantic_model_contract(),
    )
    .unwrap();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    loop {
        let outcome = store
            .reconcile_source_backed_index(&index, &mut builder, embedder)
            .unwrap();
        if !outcome.work_remaining() {
            break;
        }
    }
    drop(store);
    (temp, records)
}

fn passage_output(
    data_root: &Path,
    requested: SourceSearchRequest,
    query_dimension: usize,
) -> (Value, String) {
    let result = passage_application(data_root, requested, query_dimension, false).unwrap();
    let commands = result
        .query()
        .collection
        .result_window
        .hits
        .iter()
        .map(|_| SearchResultCommands {
            suggested_next_commands: Vec::new(),
        })
        .collect::<Vec<_>>();
    let value = result
        .render_read_model(SearchApplicationReadModelInput {
            commands: &commands,
            freshness_mode: "off",
            generated_at: "2026-09-06T00:00:00Z",
            semantic_fallback_code: None,
            semantic_fallback_detail: None,
            metrics: SearchRenderMetrics {
                refresh_status: "existing_generation",
                refresh_source_count: 1,
                query_duration: result.query_duration(),
            },
        })
        .unwrap();
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let compact = result.project_read_model(&value).unwrap();
    let human = render::render_search_document(&compact, false, &context).render_plain();
    (value, human)
}

fn semantic_request(query: &str) -> SourceSearchRequest {
    let mut requested = request(RefreshArg::Off);
    requested.query = query.to_owned();
    requested.backend = Some(SearchBackendArg::Semantic);
    requested
}

fn assert_cited_text(value: &Value, records: &[CoreEventRecord]) {
    let result = &value["results"][0];
    let snippet = result["snippet"].as_str().unwrap();
    for citation in result["semantic_passage"]["citations"].as_array().unwrap() {
        let record = records
            .iter()
            .find(|record| citation["ctx_event_id"] == record.event_id.to_string())
            .unwrap();
        let range = citation["normalized_body_char_range"].as_array().unwrap();
        let start = range[0].as_u64().unwrap() as usize;
        let end = range[1].as_u64().unwrap() as usize;
        let cited = record
            .core_record
            .content
            .meaningful_text()
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>();
        assert!(!cited.is_empty());
        assert!(
            snippet.contains(&cited),
            "snippet must contain its exact cited Core text: {citation}"
        );
    }
}

#[test]
fn semantic_passage_supported_output_keeps_anchor_and_selected_assistant() {
    let bodies = [
        ("user", "Explain the rollout decision.".to_owned()),
        ("assistant", "Earlier incorrect answer.".to_owned()),
        (
            "assistant",
            format!(
                "{} The final answer is {PASSAGE_QUERY}.",
                "ordinary context ".repeat(200)
            ),
        ),
        ("user", "Next unrelated turn.".to_owned()),
        ("assistant", "Unrelated answer.".to_owned()),
    ];
    let (temp, records) = passage_fixture(&bodies);
    assert_eq!(
        bodies
            .iter()
            .filter(|(_, body)| body.contains(PASSAGE_QUERY))
            .count(),
        1
    );
    for backend in [SearchBackendArg::Semantic, SearchBackendArg::Hybrid] {
        let mut request = semantic_request(PASSAGE_QUERY);
        request.backend = Some(backend);
        // Keep the semantic anchor as champion when the lexical assistant also matches.
        request.semantic_weight = 0.75;
        let (value, human) = passage_output(temp.path(), request, 0);
        println!(
            "SEARCH_JSON\n{}\nSEARCH_HUMAN\n{}",
            serde_json::to_string_pretty(&value).unwrap(),
            human
        );
        let result = &value["results"][0];
        assert_eq!(result["ctx_event_id"], records[0].event_id.to_string());
        assert_eq!(result["item_id"], records[0].session_id.to_string());
        assert!(
            result["snippet"].as_str().unwrap().contains(PASSAGE_QUERY),
            "supported search output lost the winning assistant passage"
        );
        let citations = result["semantic_passage"]["citations"].as_array().unwrap();
        assert_eq!(citations.len(), 1);
        assert_eq!(
            citations[0]["ctx_event_id"],
            records[2].event_id.to_string()
        );
        assert_eq!(citations[0]["role"], "assistant");
        assert!(human.contains(PASSAGE_QUERY));
        assert!(human.contains("Passage") && human.contains("assistant"));
        assert!(human.contains(&records[2].event_id.as_uuid().simple().to_string()[..8]));
        assert_cited_text(&value, &records);
    }
}

#[test]
fn semantic_passage_cross_member_unicode_trim_and_dense_identity() {
    let (temp, records) = passage_fixture(&[
        ("user", "  \t請解釋 e\u{301} 👩‍💻  ".to_owned()),
        (
            "assistant",
            format!("\n  {PASSAGE_QUERY} 答案 e\u{301} 👩‍💻   "),
        ),
    ]);
    let mut request = semantic_request(PASSAGE_QUERY);
    request.events = true;
    let (value, human) = passage_output(temp.path(), request, 0);
    let result = &value["results"][0];
    assert_eq!(result["item_id"], records[0].event_id.to_string());
    let citations = result["semantic_passage"]["citations"].as_array().unwrap();
    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0]["normalized_body_char_range"][0], 3);
    assert_eq!(citations[1]["normalized_body_char_range"][0], 3);
    assert!(human.contains("user") && human.contains("assistant"));
    assert_cited_text(&value, &records);
}

#[test]
fn semantic_passage_unpaired_and_no_positive_winner() {
    let (temp, records) = passage_fixture(&[("user", format!("  {PASSAGE_QUERY} alone  "))]);
    let (value, _) = passage_output(temp.path(), semantic_request(PASSAGE_QUERY), 0);
    assert_eq!(
        value["results"][0]["semantic_passage"]["citations"][0]["ctx_event_id"],
        records[0].event_id.to_string()
    );
    assert_cited_text(&value, &records);
    let (empty, _) = passage_output(temp.path(), semantic_request(PASSAGE_QUERY), 2);
    assert_eq!(empty["results"].as_array().unwrap().len(), 0);
    let mut request = semantic_request(PASSAGE_QUERY);
    request.backend = Some(SearchBackendArg::Hybrid);
    let (lexical, _) = passage_output(temp.path(), request, 2);
    assert!(lexical["results"][0].get("semantic_passage").is_none());
    assert!(lexical["results"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains(PASSAGE_QUERY));
}

#[test]
fn semantic_passage_ties_keep_first_chunk_and_query_and_ignore_terms_outside_winner() {
    let first = format!(
        "{PASSAGE_QUERY} first winning passage. {}",
        "padding ".repeat(250)
    );
    let body = format!(
        "{first}{PASSAGE_QUERY} second passage. {} offspanquery",
        "padding ".repeat(250)
    );
    let (temp, records) = passage_fixture(&[("user", "Question.".to_owned()), ("assistant", body)]);
    let mut request = semantic_request("offspanquery");
    request.terms = vec!["secondalternative".to_owned()];
    let (value, _) = passage_output(temp.path(), request, 0);
    let result = &value["results"][0];
    assert_eq!(result["semantic_passage"]["query_ordinal"], 0);
    assert_eq!(result["semantic_passage"]["source_char_range"][0], 0);
    assert!(result["snippet"]
        .as_str()
        .unwrap()
        .contains("first winning passage"));
    assert!(!result["snippet"].as_str().unwrap().contains("offspanquery"));
    assert_cited_text(&value, &records);
}

#[test]
fn semantic_passage_source_cap_and_snippet_limits_preserve_exact_citation() {
    use unicode_segmentation::UnicodeSegmentation as _;
    let body = format!(
        "  {PASSAGE_QUERY} {} outsidecaptoken",
        "界e\u{301}👩‍💻 ".repeat(12000)
    );
    let (temp, records) = passage_fixture(&[("user", "Question.".to_owned()), ("assistant", body)]);
    let (value, _) = passage_output(temp.path(), semantic_request(PASSAGE_QUERY), 0);
    let result = &value["results"][0];
    let snippet = result["snippet"].as_str().unwrap();
    assert!(snippet.graphemes(true).count() <= SEARCH_SNIPPET_MAX_CHARS);
    assert!(snippet.len() <= SEARCH_SNIPPET_MAX_BYTES);
    assert_eq!(result["snippet_truncated"], true);
    assert!(
        result["semantic_passage"]["source_char_range"][1]
            .as_u64()
            .unwrap()
            <= 65536
    );
    assert!(!snippet.contains("outsidecaptoken"));
    assert_cited_text(&value, &records);
}

#[test]
fn semantic_passage_rejects_wrong_hash_generation_identity_and_coordinates() {
    let (temp, records) = passage_fixture(&[
        ("user", "Question".to_owned()),
        ("assistant", PASSAGE_QUERY.to_owned()),
    ]);
    let index = VerifiedIndex::open_pinned(index_root(temp.path())).unwrap();
    let mut pin =
        SemanticQueryPin::preflight(&index, temp.path(), semantic_model_contract()).unwrap();
    let filter = CompiledSearchFilter::compile(EventSearchFilters::default()).unwrap();
    let (candidates, _) = pin
        .search(&index, &filter, &[marker_vector(PASSAGE_QUERY)], 10)
        .unwrap();
    let hit = &candidates[0];
    let evidence = hit.semantic_evidence.as_ref().unwrap();
    let resolved = pin
        .resolve_passage(&index, semantic_model_contract(), &hit.event, evidence)
        .unwrap();
    assert_eq!(resolved.members[1].event.event_id, records[1].event_id);
    for changed in 0..4 {
        let mut evidence = evidence.clone();
        let mut event = hit.event.clone();
        match changed {
            0 => evidence.source_text_hash = "00".repeat(32),
            1 => evidence.core_generation_id = "wrong-generation".to_owned(),
            2 => event.event_identity_digest = [0; 32],
            _ => evidence.end_char = usize::MAX,
        }
        assert!(pin
            .resolve_passage(&index, semantic_model_contract(), &event, &evidence)
            .is_err());
    }
}

fn passage_application(
    data_root: &Path,
    requested: SourceSearchRequest,
    query_dimension: usize,
    corrupt_passage: bool,
) -> Result<
    ctx_history_read_application::SearchApplicationResult,
    ctx_history_read_application::ObservedSearchApplicationError<anyhow::Error>,
> {
    let plan = plan_search(
        requested,
        SearchPolicy {
            default_backend: SearchBackendArg::Semantic,
            semantic: SemanticAvailability::Available,
        },
    )
    .unwrap();
    let mut generation = |_: &GenerationReadRequest| -> anyhow::Result<GenerationRead> {
        Ok(GenerationRead::new(
            VerifiedIndex::open_pinned(index_root(data_root))?,
            None,
        ))
    };
    execute_search_observed(
        SearchApplicationRequest {
            plan,
            generation_target: GenerationReadTarget::Active,
            compact_projection: false,
            active_session: None,
        },
        &mut generation,
        &PassagePort(data_root.to_owned(), query_dimension, corrupt_passage),
    )
}

struct SelectedWindowEmbedder(fn(&str) -> bool);
impl SemanticBatchEmbedder for SelectedWindowEmbedder {
    fn document_fits(&mut self, _: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(chunks
            .iter()
            .map(|chunk| {
                let mut vector = vec![0.0; semantic_model_contract().dimensions()];
                vector[usize::from(!(self.0)(chunk.text()))] = 1.0;
                vector
            })
            .collect())
    }
}

#[test]
fn semantic_passage_cap_does_not_display_a_grapheme_continuing_after_scalar_65536() {
    // Composite prefix is six scalars. Z is source scalar65535, and its mark
    // lies outside the unchanged scalar-capped indexing/hash source.
    let mut body = "p ".repeat(32700);
    body.push_str("endcapmarker ");
    body.push_str(&" ".repeat(65529 - body.chars().count()));
    body.push_str("Z\u{301}");
    let (temp, records) = passage_fixture_with_embedder(
        &[("user", body)],
        &mut SelectedWindowEmbedder(|text| text.ends_with('Z')),
    );
    let (value, human) = passage_output(temp.path(), semantic_request("endcapmarker Z"), 0);
    let result = &value["results"][0];
    assert_eq!(result["semantic_passage"]["source_char_range"][1], 65536);
    assert!(result["snippet"].as_str().unwrap().contains("endcapmarker"));
    assert!(!result["snippet"].as_str().unwrap().contains('Z'));
    assert!(!human.contains('Z'));
    for citation in result["semantic_passage"]["citations"].as_array().unwrap() {
        assert!(citation["normalized_body_char_range"][1].as_u64().unwrap() <= 65529);
    }
    assert_cited_text(&value, &records);
}

#[test]
fn semantic_passage_undisplayable_grapheme_is_empty_truncated_without_citation() {
    let body = format!("Z{}", "\u{301}".repeat(5000));
    let (temp, records) = passage_fixture_with_embedder(
        &[("user", body)],
        &mut SelectedWindowEmbedder(|text| {
            !text.contains('Z') && text.chars().filter(|ch| *ch == '\u{301}').count() > 900
        }),
    );
    let (value, human) = passage_output(temp.path(), semantic_request("unrelatedquery"), 0);
    let result = &value["results"][0];
    assert_eq!(result["ctx_event_id"], records[0].event_id.to_string());
    assert_eq!(result["snippet"], "");
    assert_eq!(result["snippet_truncated"], true);
    assert!(result.get("semantic_passage").is_none());
    assert!(!human.contains("Passage"));
}

#[test]
fn semantic_passage_integrity_failure_is_typed_and_hybrid_uses_existing_fallback() {
    let (temp, _) = passage_fixture(&[
        ("user", format!("{PASSAGE_QUERY} question")),
        ("assistant", "answer".to_owned()),
    ]);
    let error = passage_application(temp.path(), semantic_request(PASSAGE_QUERY), 0, true)
        .err()
        .unwrap();
    assert!(format!("{error:?}").contains("ProjectionEventMismatch"));
    let mut request = semantic_request(PASSAGE_QUERY);
    request.backend = Some(SearchBackendArg::Hybrid);
    let result = passage_application(temp.path(), request, 0, true).unwrap();
    assert_eq!(
        result.query().collection.effective_backend,
        SearchBackendArg::Lexical
    );
    assert_eq!(
        result
            .query()
            .collection
            .semantic_fallback
            .as_ref()
            .unwrap()
            .reason,
        Some(ctx_history_read_application::SemanticReason::ProjectionEventMismatch)
    );
    assert!(result
        .query()
        .presentations
        .iter()
        .all(|presentation| presentation.semantic_passage.is_none()));
}
