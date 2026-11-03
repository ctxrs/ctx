use super::*;
use tantivy::fieldnorm::FieldNormReader;

pub(super) const TERMS: [&str; 4] = ["alpha", "beta", "delta", "gamma"];

pub(super) struct ModelRow {
    pub(super) record: CoreRecord,
    pub(super) frequencies: [u32; 4],
    pub(super) length: u32,
    pub(super) selected: bool,
    pub(super) deleted: bool,
}

pub(super) fn random(state: &mut u64, bound: u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state % bound
}

// Fixture-only publication: preserve authored segment topology and tombstones.
pub(super) fn publish_model(
    temp: &TempDir,
    source: &SourceKey,
    rows: &[ModelRow],
    sizes: &[usize],
    malformed: bool,
) -> VerifiedIndex {
    let records = rows
        .iter()
        .filter(|row| !row.deleted)
        .map(|row| row.record.clone())
        .collect();
    drop(publish_records(temp, source, records));
    let (_, manifest) = open_unverified_generation(temp.path());
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let index = Index::open(directory).unwrap();
    let fields = fields_from_schema(&index.schema()).unwrap();
    let generation_id = manifest.generation_id().unwrap();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, 1024 * 1024 * 1024)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    writer.delete_all_documents().unwrap();
    writer.commit().unwrap();
    let mut sessions = HashSet::new();
    let mut start = 0;
    for &size in sizes {
        let chunk = &rows[start..start + size];
        start += size;
        for row in chunk {
            let authority = ctx_history_index_format::SessionAuthorityKey::exact(
                row.record.session_id,
                row.record.source.identity(),
            )
            .unwrap();
            let mut document = indexed_document(row.record.clone());
            if !row.deleted && sessions.insert(row.record.session_id) {
                document.add_bytes(fields.session_authority, authority.as_bytes());
            }
            if malformed && row.deleted {
                let mut replacement = TantivyDocument::default();
                for (field, value) in document.iter_fields_and_values() {
                    if field != fields.event_id_high {
                        replacement.add_field_value(field, value);
                    }
                }
                document = replacement;
            }
            writer.add_document(document).unwrap();
        }
        let mut prepared = writer.prepare_commit().unwrap();
        prepared.set_payload(
            &serde_json::to_string(&CommitPayload {
                version: COMMIT_PAYLOAD_VERSION,
                generation_id: generation_id.clone(),
            })
            .unwrap(),
        );
        prepared.commit().unwrap();
    }
    writer.delete_term(Term::from_field_text(fields.body_search, "graveyard"));
    let mut prepared = writer.prepare_commit().unwrap();
    prepared.set_payload(
        &serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id: generation_id.clone(),
        })
        .unwrap(),
    );
    prepared.commit().unwrap();
    writer.wait_merging_threads().unwrap();
    assert_eq!(index.load_metas().unwrap().segments.len(), sizes.len());
    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let digest =
        physical_integrity_digest(&index, &active_generation_path(temp.path()), Some(&pointer))
            .unwrap();
    let active = GenerationSlot::new(
        generation_id,
        pointer.active().directory().to_owned(),
        digest,
    )
    .unwrap();
    publish_active_generation_pointer(
        temp.path(),
        &ActiveGenerationPointer::new(active, None).unwrap(),
    )
    .unwrap();
    VerifiedIndex::open(temp.path()).unwrap()
}

// Independent exhaustive oracle: authored token counts, documented BM25 f32
// formula and full sorting. No production scorer, pruning, heap or filter helper.
pub(super) fn exhaustive(
    rows: &[ModelRow],
    query: &[usize],
    filters: &EventSearchFilters,
) -> Vec<([u8; 32], u32, u8)> {
    let count = rows.len() as u64;
    let average = rows.iter().map(|row| u64::from(row.length)).sum::<u64>() as f32 / count as f32;
    let document_frequencies: [u64; 4] = std::array::from_fn(|term| {
        rows.iter().filter(|row| row.frequencies[term] > 0).count() as u64
    });
    let mut scored = Vec::new();
    for row in rows {
        let class = row.record.event_type.as_str();
        let allowed = match filters.content_scope {
            SearchContentScope::All => true,
            SearchContentScope::Transcript => matches!(class, "message" | "summary"),
            SearchContentScope::Calls => matches!(class, "tool_call" | "command_started"),
            SearchContentScope::Outputs => {
                matches!(class, "tool_output" | "command_output" | "command_finished")
            }
        };
        if row.deleted
            || !allowed
            || (filters.file.is_some() && !row.selected)
            || filters.since_unix_ms.is_some_and(|since| {
                row.record
                    .occurred_at_unix_ms
                    .is_none_or(|time| time < since)
            })
            || filters
                .excluded_session_ids
                .contains(&row.record.session_id.as_uuid())
        {
            continue;
        }
        let coverage = query
            .iter()
            .filter(|&&term| term < 4 && row.frequencies[term] > 0)
            .count() as u8;
        if coverage == 0 {
            continue;
        }
        let length =
            FieldNormReader::id_to_fieldnorm(FieldNormReader::fieldnorm_to_id(row.length)) as f32;
        let norm = 1.2_f32 * (1.0 - 0.75 + 0.75 * length / average);
        let mut score = 0.0_f32;
        for &term in query {
            let df = if term < 4 {
                document_frequencies[term]
            } else {
                0
            };
            let frequency = if term < 4 {
                row.frequencies[term] as f32
            } else {
                0.0
            };
            let idf = (1.0 + ((count - df) as f32 + 0.5) / (df as f32 + 0.5)).ln();
            score += (idf * (1.0 + 1.2)) * (frequency / (frequency + norm));
        }
        score *= match filters.content_scope {
            SearchContentScope::Calls | SearchContentScope::Outputs => 1.0,
            _ => match class {
                "message" => 1.0,
                "summary" => 0.9,
                "tool_output" | "command_output" | "command_finished" => 0.6,
                _ => 0.8,
            },
        };
        scored.push((row.record.event_id.digest(), score, coverage));
    }
    scored.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    scored
        .into_iter()
        .map(|(id, score, coverage)| (id, score.to_bits(), coverage))
        .collect()
}

#[test]
fn randomized_top_k_matches_independent_exhaustive_bm25() {
    let mut checked = 0;
    for seed in 1..=16_u64 {
        let mut state = seed * 0x9e3779b9;
        let temp = tempdir().unwrap();
        let source = source(&format!("randomized-top-k-{seed}.jsonl"));
        let mut rows = Vec::new();
        for offset in 0_usize..576 {
            let mut frequencies = [0; 4];
            for (term, frequency) in frequencies.iter_mut().enumerate() {
                *frequency = if offset / 192 == term {
                    0
                } else {
                    random(&mut state, 8) as u32
                };
            }
            if offset % 11 == 0 {
                frequencies = [2; 4];
            }
            let filler = if offset % 11 == 0 {
                8
            } else {
                random(&mut state, [16, 160, 1600][offset / 192]) as u32
            };
            let mut body = String::new();
            for (term, frequency) in TERMS.iter().zip(frequencies) {
                body.push_str(&format!("{term} ").repeat(frequency as usize));
            }
            body.push_str(&"ballast ".repeat(filler as usize));
            let mut record = document_for_session(
                &source,
                &format!("session-{}", offset % 7),
                (offset + 1) as u64,
                &body,
            );
            record.event_type = ["message", "summary", "tool_call", "tool_output"]
                [random(&mut state, 4) as usize]
                .to_owned();
            record.occurred_at_unix_ms = if random(&mut state, 7) == 0 {
                None
            } else {
                Some(random(&mut state, 200) as i64)
            };
            let selected = random(&mut state, 2) == 0;
            if selected {
                add_literal_fact(&mut record, LiteralFactKind::File, "selected");
            }
            let mut digest = [0x30; 32];
            // Equal 48-bit prefixes exercise the exact digest tie-break path.
            if offset % 3 != 0 {
                digest[..8].copy_from_slice(&random(&mut state, u64::MAX).to_be_bytes());
            }
            digest[24..].copy_from_slice(&((offset + 1) as u64).to_be_bytes());
            digest[12..16].copy_from_slice(&((offset + 1) as u32).to_be_bytes());
            record = with_event_identity_digest(record, digest);
            rows.push(ModelRow {
                deleted: false,
                record,
                frequencies,
                length: frequencies.iter().sum::<u32>() + filler + u32::from(selected),
                selected,
            });
        }
        let index = publish_model(&temp, &source, &rows, &[rows.len() / 3; 3], false);
        for query in [
            vec![0],
            vec![0, 1],
            vec![0, 1, 2],
            vec![0, 1, 2, 3],
            vec![0, 1, 2, 3, 4],
        ] {
            let text = query
                .iter()
                .map(|&term| if term < 4 { TERMS[term] } else { "missingterm" })
                .collect::<Vec<_>>()
                .join(" ");
            for scope in [
                SearchContentScope::All,
                SearchContentScope::Transcript,
                SearchContentScope::Calls,
                SearchContentScope::Outputs,
            ] {
                for filter_case in 0..4 {
                    let filters = EventSearchFilters {
                        content_scope: scope,
                        file: (filter_case & 1 != 0).then(|| "selected".to_owned()),
                        since_unix_ms: (filter_case & 2 != 0).then_some(100),
                        excluded_session_ids: if filter_case == 3 {
                            vec![rows[0].record.session_id.as_uuid()]
                        } else {
                            vec![]
                        },
                        ..EventSearchFilters::default()
                    };
                    let expected = exhaustive(&rows, &query, &filters);
                    for limit in [1, 3, 10, 41, 576] {
                        let batch =
                            lexical_search_batch(&index, &[&text], &filters, limit).unwrap();
                        assert!(
                            batch.complete,
                            "seed={seed} exhaustion={:?}",
                            batch.exhaustion
                        );
                        let actual = batch
                            .candidates
                            .iter()
                            .map(|candidate| {
                                (
                                    candidate.event.event_identity_digest,
                                    candidate.score.to_bits(),
                                    candidate.coverage.matched_terms,
                                )
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            actual,
                            expected[..expected.len().min(limit)],
                            "seed={seed} query={text} scope={scope:?} filter={filter_case} limit={limit}"
                        );
                        assert_eq!(batch.candidate_set_exhaustive, expected.len() <= limit);
                        for candidate in &batch.candidates {
                            let authored = &rows
                                .iter()
                                .find(|row| {
                                    row.record.event_id.digest()
                                        == candidate.event.event_identity_digest
                                })
                                .unwrap()
                                .record;
                            assert_eq!(candidate.event.event_id, authored.event_id.as_uuid());
                            assert_eq!(candidate.event.session_id, authored.session_id.as_uuid());
                            assert_eq!(
                                candidate.event.source_owner_digest,
                                authored.source.identity().digest()
                            );
                            assert_eq!(candidate.event.event_sequence, authored.event_sequence);
                            assert_eq!(
                                candidate.event.occurred_at_unix_ms,
                                authored.occurred_at_unix_ms
                            );
                        }

                        assert!(batch
                            .candidates
                            .iter()
                            .all(|candidate| usize::from(candidate.coverage.query_terms)
                                == query.len()));
                        checked += 1;
                    }
                }
            }
        }
    }
    eprintln!(
        "top_k_oracle corpora=16 segments_per_corpus=3 comparisons={checked} exact_score_bits=true"
    );
}

#[test]
fn top_k_block_bounds_use_global_statistics_when_the_local_winner_changes() {
    let temp = tempdir().unwrap();
    let source = source("global-block-bound.jsonl");
    let mut rows = Vec::new();
    for offset in 0..1152 {
        let (frequency, length) = match offset {
            0 => (5, 10),
            383 => (10, 100),
            1..=382 => (1, 1),
            _ => (0, 2000),
        };
        let body = format!(
            "{}{}",
            "alpha ".repeat(frequency as usize),
            "ballast ".repeat((length - frequency) as usize)
        );
        rows.push(ModelRow {
            deleted: false,
            record: document(&source, offset + 1, &body),
            frequencies: [frequency, 0, 0, 0],
            length,
            selected: false,
        });
    }
    let filters = EventSearchFilters::default();
    let local = exhaustive(&rows[..384], &[0], &filters);
    let global = exhaustive(&rows, &[0], &filters);
    let target = rows[383].record.event_id.digest();
    let seed = rows[0].record.event_id.digest();
    // A full native block's locally winning (tf=1, length=1) pair becomes
    // weaker than both the seed and the tail target under global statistics.
    // Using that stored pair with the global scorer would skip the winner.
    assert_ne!(local[0].0, target);
    assert_ne!(local[0].0, seed);
    assert_eq!(global[0].0, target);
    assert_eq!(global[1].0, seed);
    let index = publish_model(&temp, &source, &rows, &[rows.len() / 3; 3], false);
    let batch = lexical_search_batch(&index, &["alpha"], &filters, 1).unwrap();
    assert!(batch.complete, "{:?}", batch.exhaustion);
    assert_eq!(batch.candidates[0].event.event_identity_digest, target);
    assert_eq!(batch.candidates[0].score.to_bits(), global[0].1);
    assert!(!batch.candidate_set_exhaustive);
    assert!(batch.counters.body_posting_advances < 384);
}
