// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use crate::jev;
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

pub fn apply(
    value: serde_json::Value,
    text: &str,
    ordinary: &CompactResult,
    settings: &Settings,
    project: Option<&str>,
    compactor: &Compactor,
    client: &mut jev::Client,
) -> Option<CompactResult> {
    if settings.semantic_selection.mode == SemanticMode::Off {
        return None;
    }
    let project = project.filter(|project| {
        settings
            .semantic_selection
            .allowed_projects
            .iter()
            .any(|allowed| allowed == project)
    })?;
    let selection = Selection::parse(value, text)?;
    if !selection.path_is_within(project) {
        return None;
    }
    let judgment = client.select(selection.task(), selection.candidates(text));
    let mut disposition = "fallback";
    let mut selected_count = selection.passage_count();
    let mut omitted_count = 0;
    let mut semantic_tokens = None;
    let mut result = None;
    if let Some(scores) = judgment.scores.as_deref()
        && let Some(proposal) = selection.propose(scores)
    {
        selected_count = proposal.kept_count();
        omitted_count = proposal.omitted_count();
        if !proposal.clears_byte_floor(&selection, text) {
            disposition = "marginal";
        } else {
            let estimate = selection.render(text, &proposal, &"f".repeat(100));
            let estimate_tokens = compactor.count_tokens(&estimate);
            semantic_tokens = Some(estimate_tokens);
            if estimate_tokens >= ordinary.output_tokens {
                disposition = "not_smaller";
            } else if settings.semantic_selection.mode == SemanticMode::Shadow {
                disposition = "shadow_selected";
            } else {
                match state::save_semantic_original(text.as_bytes()) {
                    Ok(Some(id)) => {
                        let frame = selection.render(text, &proposal, &id);
                        let tokens = compactor.count_tokens(&frame);
                        semantic_tokens = Some(tokens);
                        if tokens < ordinary.output_tokens {
                            disposition = "selected";
                            result = Some(CompactResult {
                                text: frame,
                                encoding: Encoding::Raw,
                                input_tokens: ordinary.input_tokens,
                                output_tokens: tokens,
                            });
                        } else {
                            disposition = "not_smaller";
                        }
                    }
                    _ => disposition = "storage_unavailable",
                }
            }
        }
    } else if judgment.scores.is_some() {
        disposition = "rejected";
    }
    let _ = state::record_semantic_receipt(&SemanticReceipt {
        unix_millis: state::unix_millis(),
        status: judgment.status,
        disposition,
        model: jev::MODEL,
        http_status: judgment.http_status,
        usage: judgment.usage.map(|usage| SemanticUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        }),
        latency_ms: judgment.latency_ms,
        passage_count: selection.passage_count(),
        selected_count,
        omitted_count,
        ordinary_tokens: ordinary.output_tokens,
        semantic_tokens,
        memoized: judgment.memoized,
    });
    result
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
