// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use crate::jev;
use crate::observation::{
    self as observed, HttpClass, ProviderOutcome, SemanticDisposition, SemanticFacts,
};
use crate::state::{self, SemanticMode, SemanticReceipt, SemanticUsage, Settings};
use serde::Deserialize;
use sift::{CompactResult, Compactor, Encoding};
use std::collections::HashSet;
use std::path::Path;

pub const THRESHOLD: f64 = 0.30;
const TASK_LIMIT: usize = 16 * 1024;
const ID_LIMIT: usize = 128;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    policy: String,
    task: String,
    path: String,
    kind: String,
    passages: Vec<Passage>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Passage {
    id: String,
    start: usize,
    end: usize,
    required: bool,
}

#[derive(Debug)]
pub struct Proposal {
    kept: Vec<usize>,
    omitted: Vec<String>,
}

impl Selection {
    pub fn parse(value: serde_json::Value, text: &str) -> Option<Self> {
        let selection: Self = serde_json::from_value(value).ok()?;
        selection.valid(text).then_some(selection)
    }

    fn valid(&self, text: &str) -> bool {
        if self.policy != jev::POLICY
            || self.kind != "pi-grep-v1"
            || self.task.trim().is_empty()
            || self.task.len() > TASK_LIMIT
            || self.task.contains('\0')
            || self.path.is_empty()
            || self.path.len() > 4096
            || self.path.contains('\0')
            || !(3..=40).contains(&self.passages.len())
            || text.is_empty()
        {
            return false;
        }
        let mut ids = HashSet::with_capacity(self.passages.len());
        let mut next = 0;
        for passage in &self.passages {
            if passage.id.is_empty()
                || passage.id.len() > ID_LIMIT
                || !passage
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
                || !ids.insert(&passage.id)
                || passage.start != next
                || passage.start >= passage.end
                || passage.end > text.len()
                || !text.is_char_boundary(passage.start)
                || !text.is_char_boundary(passage.end)
            {
                return false;
            }
            next = passage.end;
        }
        next == text.len()
    }

    pub fn task(&self) -> &str {
        &self.task
    }

    fn path_is_within(&self, project: &str) -> bool {
        let path = Path::new(&self.path);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            let Ok(current) = std::env::current_dir() else {
                return false;
            };
            current.join(path)
        };
        std::fs::canonicalize(path).is_ok_and(|path| path.starts_with(Path::new(project)))
    }

    pub fn candidates<'a>(&'a self, text: &'a str) -> Vec<jev::Candidate<'a>> {
        self.passages
            .iter()
            .map(|passage| jev::Candidate {
                id: &passage.id,
                text: &text[passage.start..passage.end],
            })
            .collect()
    }

    pub fn propose(&self, scores: &[(f64, f64)]) -> Option<Proposal> {
        if scores.len() != self.passages.len() {
            return None;
        }
        let mut kept = Vec::new();
        let mut omitted = Vec::new();
        for (index, (passage, score)) in self.passages.iter().zip(scores).enumerate() {
            if passage.required || score.0 >= THRESHOLD || score.1 >= THRESHOLD {
                kept.push(index);
            } else {
                omitted.push(passage.id.clone());
            }
        }
        (!kept.is_empty() && !omitted.is_empty()).then_some(Proposal { kept, omitted })
    }

    pub fn passage_count(&self) -> usize {
        self.passages.len()
    }

    pub fn render(&self, text: &str, proposal: &Proposal, original_id: &str) -> String {
        let mut output = format!(
            "Sift semantic selection: INCOMPLETE\nOmitted passage IDs: {}\nFull output: ctx sift recall {original_id}\n\n",
            proposal.omitted.join(", ")
        );
        for (position, index) in proposal.kept.iter().enumerate() {
            let passage = &self.passages[*index];
            if position != 0 {
                output.push_str("\n\n");
            }
            output.push('[');
            output.push_str(&passage.id);
            output.push_str("]\n");
            output.push_str(&text[passage.start..passage.end]);
        }
        output
    }
}

impl Proposal {
    pub fn kept_count(&self) -> usize {
        self.kept.len()
    }

    pub fn omitted_count(&self) -> usize {
        self.omitted.len()
    }

    pub fn clears_byte_floor(&self, selection: &Selection, text: &str) -> bool {
        let pessimistic = selection.render(text, self, &"f".repeat(100));
        text.len().saturating_sub(pessimistic.len()) >= 300
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply(
    value: serde_json::Value,
    text: &str,
    ordinary: &CompactResult,
    settings: &Settings,
    project: Option<&str>,
    compactor: &Compactor,
    client: &mut jev::Client,
    facts: &mut Option<SemanticFacts>,
) -> Option<CompactResult> {
    let facts = facts.insert(SemanticFacts {
        mode: match settings.semantic_selection.mode {
            SemanticMode::Off => observed::SemanticMode::Off,
            SemanticMode::Shadow => observed::SemanticMode::Shadow,
            SemanticMode::Select => observed::SemanticMode::Select,
        },
        disposition: SemanticDisposition::Off,
        provider: ProviderOutcome::NotAttempted,
        request_attempted: false,
        cache_hit: false,
        http_class: None,
        request_input_tokens: None,
        request_output_tokens: None,
        provider_duration: None,
        passages: None,
        selected: None,
        omitted: None,
        ordinary_tokens: Some(ordinary.output_tokens as u64),
        candidate_tokens: None,
    });
    if settings.semantic_selection.mode == SemanticMode::Off {
        return None;
    }
    facts.disposition = SemanticDisposition::ProjectNotAllowed;
    let project = project.filter(|project| {
        settings
            .semantic_selection
            .allowed_projects
            .iter()
            .any(|allowed| allowed == project)
    })?;
    facts.disposition = SemanticDisposition::InvalidSelection;
    let selection = Selection::parse(value, text)?;
    if !selection.path_is_within(project) {
        facts.disposition = SemanticDisposition::OutsideProject;
        return None;
    }
    let judgment = client.select(selection.task(), selection.candidates(text));
    let result = select_output(
        &selection,
        text,
        ordinary,
        settings.semantic_selection.mode,
        compactor,
        &judgment,
        facts,
        state::save_semantic_original,
    );
    let _ = state::record_semantic_receipt(&SemanticReceipt {
        unix_millis: state::unix_millis(),
        status: match judgment.status {
            ProviderOutcome::Oversized => "oversize",
            ProviderOutcome::MissingCredential => "disabled",
            ProviderOutcome::Unavailable => "unavailable",
            ProviderOutcome::HttpFailure => "http",
            ProviderOutcome::InvalidResponse => "invalid_response",
            ProviderOutcome::Success => "ok",
            ProviderOutcome::Memoized => "memoized",
            ProviderOutcome::NotAttempted => "disabled",
        },
        disposition: match facts.disposition {
            SemanticDisposition::Marginal => "marginal",
            SemanticDisposition::NotSmaller => "not_smaller",
            SemanticDisposition::ShadowSelected => "shadow_selected",
            SemanticDisposition::Selected => "selected",
            SemanticDisposition::StorageUnavailable => "storage_unavailable",
            SemanticDisposition::Rejected => "rejected",
            _ => "fallback",
        },
        model: jev::MODEL,
        http_status: judgment.http_status,
        usage: judgment.usage.map(|usage| SemanticUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        }),
        latency_ms: judgment.latency_ms,
        passage_count: selection.passage_count(),
        selected_count: facts
            .selected
            .map_or(selection.passage_count(), |n| n as usize),
        omitted_count: facts.omitted.unwrap_or(0) as usize,
        ordinary_tokens: ordinary.output_tokens,
        semantic_tokens: facts.candidate_tokens.map(|n| n as usize),
        memoized: judgment.memoized,
    });
    result
}

// The selection decision has one effect: retaining the original before omission.
// Supplying that operation also lets tests cover storage failure without a provider.
#[allow(clippy::too_many_arguments)]
fn select_output(
    selection: &Selection,
    text: &str,
    ordinary: &CompactResult,
    mode: SemanticMode,
    compactor: &Compactor,
    judgment: &jev::Judgment,
    facts: &mut SemanticFacts,
    save: impl FnOnce(&[u8]) -> anyhow::Result<Option<String>>,
) -> Option<CompactResult> {
    observe_judgment(facts, judgment);
    let mut disposition = SemanticDisposition::Fallback;
    let mut semantic_tokens = None;
    let mut result = None;
    if let Some(scores) = judgment.scores.as_deref()
        && let Some(proposal) = selection.propose(scores)
    {
        let selected_count = proposal.kept_count();
        let omitted_count = proposal.omitted_count();
        facts.selected = Some(selected_count as u64);
        facts.omitted = Some(omitted_count as u64);
        if !proposal.clears_byte_floor(selection, text) {
            disposition = SemanticDisposition::Marginal;
        } else {
            let estimate = selection.render(text, &proposal, &"f".repeat(100));
            let estimate_tokens = compactor.count_tokens(&estimate);
            semantic_tokens = Some(estimate_tokens);
            if estimate_tokens >= ordinary.output_tokens {
                disposition = SemanticDisposition::NotSmaller;
            } else if mode == SemanticMode::Shadow {
                disposition = SemanticDisposition::ShadowSelected;
            } else {
                match save(text.as_bytes()) {
                    Ok(Some(id)) => {
                        let frame = selection.render(text, &proposal, &id);
                        let tokens = compactor.count_tokens(&frame);
                        semantic_tokens = Some(tokens);
                        if tokens < ordinary.output_tokens {
                            disposition = SemanticDisposition::Selected;
                            result = Some(CompactResult {
                                text: frame,
                                encoding: Encoding::Raw,
                                input_tokens: ordinary.input_tokens,
                                output_tokens: tokens,
                            });
                        } else {
                            disposition = SemanticDisposition::NotSmaller;
                        }
                    }
                    _ => disposition = SemanticDisposition::StorageUnavailable,
                }
            }
        }
    } else if judgment.scores.is_some() {
        disposition = SemanticDisposition::Rejected;
    }
    facts.disposition = disposition;
    facts.passages = Some(selection.passage_count() as u64);
    facts.candidate_tokens = semantic_tokens.map(|tokens| tokens as u64);
    result
}

fn observe_judgment(facts: &mut SemanticFacts, judgment: &jev::Judgment) {
    facts.provider = judgment.status;
    facts.request_attempted = matches!(
        judgment.status,
        ProviderOutcome::Success
            | ProviderOutcome::Unavailable
            | ProviderOutcome::HttpFailure
            | ProviderOutcome::InvalidResponse
    );
    facts.cache_hit = judgment.memoized;
    facts.http_class = judgment
        .http_status
        .filter(|_| facts.request_attempted)
        .map(|status| match status {
            100..=199 => HttpClass::Informational,
            200..=299 => HttpClass::Success,
            300..=399 => HttpClass::Redirect,
            400..=499 => HttpClass::ClientError,
            500..=599 => HttpClass::ServerError,
            _ => HttpClass::Other,
        });
    facts.provider_duration = facts
        .request_attempted
        .then(|| std::time::Duration::from_millis(judgment.latency_ms));
    facts.request_input_tokens = None;
    facts.request_output_tokens = None;
    if facts.request_attempted {
        facts.request_input_tokens = judgment.usage.as_ref().map(|u| u.input_tokens);
        facts.request_output_tokens = judgment.usage.as_ref().map(|u| u.output_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn value(text: &str) -> serde_json::Value {
        let a = text.find('β').unwrap();
        let b = a + 'β'.len_utf8();
        json!({"policy":jev::POLICY,"task":"find beta","path":".","kind":"pi-grep-v1","passages":[
            {"id":"a","start":0,"end":a,"required":false},
            {"id":"b","start":a,"end":b,"required":true},
            {"id":"c","start":b,"end":text.len(),"required":false}
        ]})
    }

    #[test]
    fn validates_exact_utf8_coverage_and_unique_safe_ids() {
        let text = "aaaβccc";
        assert!(Selection::parse(value(text), text).is_some());
        for mutation in [
            ("/passages/1/start", json!(4)),
            ("/passages/1/id", json!("a")),
            ("/passages/1/id", json!("bad\nframe")),
            ("/passages/2/end", json!(text.len() - 1)),
        ] {
            let mut input = value(text);
            *input.pointer_mut(mutation.0).unwrap() = mutation.1;
            assert!(Selection::parse(input, text).is_none(), "{}", mutation.0);
        }
    }

    #[test]
    fn required_and_thresholded_passages_keep_exact_text_order() {
        let text = format!("{}β{}", "a".repeat(400), "c".repeat(400));
        let selection = Selection::parse(value(&text), &text).unwrap();
        let proposal = selection
            .propose(&[(0.31, 0.0), (0.0, 0.0), (0.0, 0.0)])
            .unwrap();
        assert_eq!(proposal.kept_count(), 2);
        assert_eq!(proposal.omitted, ["c"]);
        let frame = selection.render(&text, &proposal, "abc-123");
        assert!(frame.contains("INCOMPLETE"));
        assert!(frame.contains("ctx sift recall abc-123"));
        assert!(frame.find(&"a".repeat(400)).unwrap() < frame.find('β').unwrap());
    }

    #[test]
    fn semantic_path_must_resolve_inside_the_allowed_project() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sift-semantic-scope-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let project = root.join("project");
        let nested = project.join("nested");
        let outside = root.join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let project = std::fs::canonicalize(&project).unwrap();
        let text = "aaaβccc";
        let mut selection = Selection::parse(value(text), text).unwrap();

        selection.path = nested.to_string_lossy().into_owned();
        assert!(selection.path_is_within(project.to_str().unwrap()));
        selection.path = nested
            .join("..")
            .join("..")
            .join("outside")
            .to_string_lossy()
            .into_owned();
        assert!(!selection.path_is_within(project.to_str().unwrap()));
        selection.path = root.join("missing").to_string_lossy().into_owned();
        assert!(!selection.path_is_within(project.to_str().unwrap()));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, project.join("escape")).unwrap();
            selection.path = project.join("escape").to_string_lossy().into_owned();
            assert!(!selection.path_is_within(project.to_str().unwrap()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use serde_json::json;

    fn facts() -> SemanticFacts {
        SemanticFacts {
            mode: observed::SemanticMode::Select,
            disposition: SemanticDisposition::Off,
            provider: ProviderOutcome::NotAttempted,
            request_attempted: false,
            cache_hit: false,
            http_class: None,
            request_input_tokens: None,
            request_output_tokens: None,
            provider_duration: None,
            passages: None,
            selected: None,
            omitted: None,
            ordinary_tokens: None,
            candidate_tokens: None,
        }
    }

    fn judgment() -> jev::Judgment {
        jev::Judgment {
            status: ProviderOutcome::Success,
            http_status: Some(200),
            usage: Some(jev::Usage {
                input_tokens: 1000,
                output_tokens: 12,
            }),
            latency_ms: 25,
            scores: Some(vec![(0.0, 0.0); 3]),
            memoized: false,
        }
    }

    #[test]
    fn cached_usage_and_http_metadata_are_not_current_request_usage() {
        let mut facts = facts();
        let mut judgment = judgment();
        observe_judgment(&mut facts, &judgment);
        assert!(facts.request_attempted);
        assert_eq!(facts.request_input_tokens, Some(1000));
        assert_eq!(facts.request_output_tokens, Some(12));
        assert_eq!(facts.http_class, Some(HttpClass::Success));
        judgment.status = ProviderOutcome::Memoized;
        judgment.memoized = true;
        observe_judgment(&mut facts, &judgment);
        assert!(facts.cache_hit);
        assert!(!facts.request_attempted);
        assert_eq!(facts.request_input_tokens, None);
        assert_eq!(facts.request_output_tokens, None);
        assert_eq!(facts.http_class, None);
        assert_eq!(facts.provider_duration, None);
        judgment.status = ProviderOutcome::MissingCredential;
        judgment.memoized = false;
        judgment.usage = None;
        observe_judgment(&mut facts, &judgment);
        assert!(!facts.request_attempted && !facts.cache_hit);
        assert_eq!(facts.request_input_tokens, None);
    }

    #[test]
    fn selected_shadow_and_storage_failure_keep_candidates_distinct_from_output() {
        let first = "first relevant match\n";
        let middle = (0..300)
            .map(|n| {
                format!(
                    "row_{n:04} result_{n} detail_{} diagnostic_{}\n",
                    n * 13,
                    n * 37
                )
            })
            .collect::<String>();
        let text = format!("{first}{middle}last relevant match\n");
        let a = first.len();
        let b = a + middle.len();
        let selection = Selection::parse(json!({"policy":jev::POLICY,"task":"keep the first match", "path":".", "kind":"pi-grep-v1", "passages":[
            {"id":"first","start":0,"end":a,"required":true},
            {"id":"middle","start":a,"end":b,"required":false},
            {"id":"last","start":b,"end":text.len(),"required":false}
        ]}), &text).unwrap();
        let compactor = Compactor::new().unwrap();
        let ordinary = compactor.compact(&text);
        let mut observed = facts();
        let selected = select_output(
            &selection,
            &text,
            &ordinary,
            SemanticMode::Select,
            &compactor,
            &judgment(),
            &mut observed,
            |bytes| {
                assert_eq!(bytes, text.as_bytes());
                Ok(Some("test-original".into()))
            },
        )
        .unwrap();
        assert_eq!(observed.disposition, SemanticDisposition::Selected);
        assert_eq!(observed.selected, Some(1));
        assert_eq!(observed.omitted, Some(2));
        assert!(selected.text.contains("first relevant match"));
        assert!(!selected.text.contains("row_0000"));
        assert_eq!(selected.input_tokens, ordinary.input_tokens);
        let tokenizer = tiktoken_rs::o200k_base().unwrap();
        assert_eq!(
            observed.candidate_tokens,
            Some(tokenizer.encode_ordinary(&selected.text).len() as u64)
        );
        assert!(selected.output_tokens < ordinary.output_tokens);
        let mut shadow = facts();
        assert!(
            select_output(
                &selection,
                &text,
                &ordinary,
                SemanticMode::Shadow,
                &compactor,
                &judgment(),
                &mut shadow,
                |_| panic!("shadow must not retain an original")
            )
            .is_none()
        );
        assert_eq!(shadow.disposition, SemanticDisposition::ShadowSelected);
        let mut failed = facts();
        assert!(
            select_output(
                &selection,
                &text,
                &ordinary,
                SemanticMode::Select,
                &compactor,
                &judgment(),
                &mut failed,
                |_| Err(anyhow::anyhow!("PRIVATE_CANARY"))
            )
            .is_none()
        );
        assert_eq!(failed.disposition, SemanticDisposition::StorageUnavailable);
        assert!(!format!("{failed:?}").contains("PRIVATE_CANARY"));
    }
}
