use ctx_history_core::{
    ActivityJsonCapture, ActivityTextCapture, CoreContent, CoreContentPolicyStatus,
};
use std::ops::Range;
use tantivy::tokenizer::TokenStream as _;

use crate::{IndexError, Result};

/// Derives the complete analyzed text stored in the lexical `body_search` field.
///
/// The projection is repository-neutral and follows the exact retained Core
/// content order: normalized body, structured content, invocation, result, and
/// provider-declared literal facts. Capture dispositions and provider call IDs
/// are not lexical content. A `NormalizedBody` result reference is not repeated.
pub fn project_body_search(content: CoreContent) -> Result<Option<String>> {
    project_search_content(content)
        .map(|projection| projection.map(SearchContentProjection::into_index_text))
}

/// Semantic role of one newline-delimited component in `body_search`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFragmentKind {
    NormalizedBody,
    StructuredContent,
    InvocationProtocol,
    InvocationServer,
    InvocationTool,
    InvocationArguments,
    ResultStatus,
    ResultText,
    ResultStructuredContent,
    LiteralFact,
}

/// One ordered fragment backed by its byte-exact range in the complete
/// index text and, for a JSON string scalar, its decoded human display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchContentFragment {
    kind: SearchFragmentKind,
    index_range: Range<usize>,
    decoded_json_string: Option<String>,
}

impl SearchContentFragment {
    pub fn kind(&self) -> SearchFragmentKind {
        self.kind
    }

    pub fn index_text<'a>(&self, projection: &'a SearchContentProjection) -> &'a str {
        &projection.index_text[self.index_range.clone()]
    }

    pub fn display_text<'a>(&'a self, projection: &'a SearchContentProjection) -> &'a str {
        self.decoded_json_string
            .as_deref()
            .unwrap_or_else(|| self.index_text(projection))
    }

    pub fn has_decoded_json_display(&self) -> bool {
        self.decoded_json_string.is_some()
    }
}

/// Complete byte-exact index text plus its bounded ordered fragment table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchContentProjection {
    index_text: String,
    fragments: Vec<SearchContentFragment>,
}

impl SearchContentProjection {
    pub fn index_text(&self) -> &str {
        &self.index_text
    }

    pub fn fragments(&self) -> &[SearchContentFragment] {
        &self.fragments
    }

    pub fn into_index_text(self) -> String {
        self.index_text
    }
}

/// Visits tokens using the exact analyzer that produces `body_search`
/// postings. Returning `false` stops the bounded scan early.
pub fn visit_body_analyzer_tokens(text: &str, mut visitor: impl FnMut(&str, Range<usize>) -> bool) {
    let mut analyzer = crate::analyzer::body_analyzer();
    let mut stream = analyzer.token_stream(text);
    while stream.advance() {
        let token = stream.token();
        if !visitor(&token.text, token.offset_from..token.offset_to) {
            break;
        }
    }
}

/// Derives presentation fragments and the canonical index text in one
/// traversal. Fragment count is capped by the Core activity contract and
/// all retained text is bounded by the admitted Core record.
pub fn project_search_content(mut content: CoreContent) -> Result<Option<SearchContentProjection>> {
    if !content.is_discovery_eligible()
        || !matches!(content.policy_status, CoreContentPolicyStatus::Selected)
    {
        return Ok(None);
    }

    let mut projection = ProjectionBuilder::default();
    if let Some(body) = content
        .normalized_body
        .take()
        .filter(|body| !body.is_empty())
    {
        projection.append(SearchFragmentKind::NormalizedBody, body, None)?;
    }
    if let Some(structured_content) = content.structured_content.take() {
        projection.append_json(SearchFragmentKind::StructuredContent, structured_content)?;
    }

    if let Some(activity) = content.activity.take() {
        if let Some(invocation) = activity.invocation {
            projection
                .append_optional(SearchFragmentKind::InvocationProtocol, invocation.protocol)?;
            projection.append_optional(SearchFragmentKind::InvocationServer, invocation.server)?;
            projection.append(SearchFragmentKind::InvocationTool, invocation.tool, None)?;
            projection.append_json_capture(
                SearchFragmentKind::InvocationArguments,
                invocation.arguments,
            )?;
        }
        if let Some(result) = activity.result {
            projection.append_optional(SearchFragmentKind::ResultStatus, result.status)?;
            if let ActivityTextCapture::Present { value } = result.text {
                projection.append(SearchFragmentKind::ResultText, value, None)?;
            }
            projection.append_json_capture(
                SearchFragmentKind::ResultStructuredContent,
                result.structured_content,
            )?;
        }
        for fact in activity.facts {
            projection.append(SearchFragmentKind::LiteralFact, fact.value, None)?;
        }
    }

    Ok(projection.finish())
}

#[derive(Default)]
struct ProjectionBuilder {
    index_text: Option<String>,
    fragments: Vec<SearchContentFragment>,
}

impl ProjectionBuilder {
    fn append_json_capture(
        &mut self,
        kind: SearchFragmentKind,
        capture: ActivityJsonCapture,
    ) -> Result<()> {
        if let ActivityJsonCapture::Present { value } = capture {
            self.append_json(kind, value)?;
        }
        Ok(())
    }

    fn append_json(&mut self, kind: SearchFragmentKind, value: serde_json::Value) -> Result<()> {
        let index_text = serde_json::to_string(&value)?;
        let decoded_json_string = match value {
            serde_json::Value::String(value) => Some(value),
            _ => None,
        };
        self.append(kind, index_text, decoded_json_string)
    }

    fn append_optional(&mut self, kind: SearchFragmentKind, value: Option<String>) -> Result<()> {
        if let Some(value) = value {
            self.append(kind, value, None)?;
        }
        Ok(())
    }

    fn append(
        &mut self,
        kind: SearchFragmentKind,
        value: String,
        decoded_json_string: Option<String>,
    ) -> Result<()> {
        if value.is_empty() {
            return Ok(());
        }
        let index_range = match self.index_text.as_mut() {
            Some(index_text) => {
                let start = index_text
                    .len()
                    .checked_add(1)
                    .ok_or(IndexError::CountOverflow)?;
                let end = start
                    .checked_add(value.len())
                    .ok_or(IndexError::CountOverflow)?;
                index_text.reserve(value.len() + 1);
                index_text.push('\n');
                index_text.push_str(&value);
                start..end
            }
            None => {
                let end = value.len();
                self.index_text = Some(value);
                0..end
            }
        };
        self.fragments.push(SearchContentFragment {
            kind,
            index_range,
            decoded_json_string,
        });
        Ok(())
    }

    fn finish(self) -> Option<SearchContentProjection> {
        self.index_text.map(|index_text| SearchContentProjection {
            index_text,
            fragments: self.fragments,
        })
    }
}
#[cfg(test)]
mod tests {
    use ctx_history_core::{
        ActivityInvocation, ActivityResult, CoreActivity, LiteralFactKind, ProviderDeclaredFact,
        CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION,
    };

    use super::*;

    fn selected_content(normalized_body: Option<&str>) -> CoreContent {
        CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: normalized_body.map(str::to_owned),
            structured_content: None,
            discovery_exclusion: None,
            activity: None,
        }
    }

    fn activity() -> CoreActivity {
        CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(ctx_history_core::TypedKey::U64(7)),
            invocation: Some(ActivityInvocation {
                protocol: Some("mcp".to_owned()),
                server: Some("服务器".to_owned()),
                tool: "lookup_tool".to_owned(),
                arguments: ActivityJsonCapture::Present {
                    value: serde_json::json!({"argument_key": "argument value"}),
                },
                started_at_unix_ms: Some(10),
            }),
            result: Some(ActivityResult {
                status: Some("provider::ok".to_owned()),
                completed_at_unix_ms: Some(20),
                duration_ns: Some(30),
                text: ActivityTextCapture::NormalizedBody,
                structured_content: ActivityJsonCapture::Present {
                    value: serde_json::json!({"result_key": "result value"}),
                },
            }),
            facts: vec![
                ProviderDeclaredFact {
                    kind: LiteralFactKind::Branch,
                    value: "Feature/ExactCase".to_owned(),
                },
                ProviderDeclaredFact {
                    kind: LiteralFactKind::File,
                    value: "file:///Work/Repo/src/lib.rs".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn ordinary_body_projection_moves_and_reuses_the_body_allocation() {
        let content = selected_content(Some("ordinary normalized body"));
        let body_pointer = content.normalized_body.as_ref().unwrap().as_ptr();
        let projection = project_body_search(content).unwrap().unwrap();

        assert_eq!(projection.as_ptr(), body_pointer);
        assert_eq!(projection, "ordinary normalized body");
    }

    #[test]
    fn retrieval_derived_content_has_no_body_projection() {
        let mut content = selected_content(Some("retrieval payload canary"));
        content.discovery_exclusion =
            Some(ctx_history_core::CoreDiscoveryExclusion::CtxRetrievalDerived);

        assert_eq!(project_body_search(content).unwrap(), None);
    }

    #[test]
    fn activity_and_literal_facts_extend_complete_content_in_exact_order() {
        let mut body = String::with_capacity(512);
        body.push_str("normalized body");
        let body_pointer = body.as_ptr();
        let mut content = selected_content(None);
        content.normalized_body = Some(body);
        content.structured_content = Some(serde_json::json!({
            "top_level_key": "top level value"
        }));
        content.activity = Some(activity());

        let projection = project_body_search(content).unwrap().unwrap();

        assert_eq!(projection.as_ptr(), body_pointer);
        assert_eq!(
            projection,
            "normalized body\n{\"top_level_key\":\"top level value\"}\nmcp\n服务器\nlookup_tool\n{\"argument_key\":\"argument value\"}\nprovider::ok\n{\"result_key\":\"result value\"}\nFeature/ExactCase\nfile:///Work/Repo/src/lib.rs"
        );
        assert!(!projection.contains("NormalizedBody"));
        assert!(!projection.ends_with('\n'));
    }

    #[test]
    fn present_result_text_is_indexed_but_capture_dispositions_are_not() {
        let mut content = selected_content(None);
        let mut activity = activity();
        let result = activity.result.as_mut().unwrap();
        result.text = ActivityTextCapture::Present {
            value: "complete terminal text".to_owned(),
        };
        result.structured_content = ActivityJsonCapture::Omitted {
            reason: "size limit canary".to_owned(),
            observed_encoded_bytes: Some(998_877),
        };
        content.activity = Some(activity);

        let projection = project_body_search(content).unwrap().unwrap();
        assert!(projection.contains("complete terminal text"));
        assert!(!projection.contains("size limit canary"));
        assert!(!projection.contains("998877"));
    }

    #[test]
    fn fragment_projection_keeps_exact_index_bytes_and_human_json_display() {
        let mut content = selected_content(Some("readable body"));
        content.structured_content = Some(serde_json::json!(
            "escaped \"wrapper\"\nwith a decisive clause"
        ));
        let mut activity = activity();
        activity.invocation.as_mut().unwrap().arguments = ActivityJsonCapture::Present {
            value: serde_json::json!(["serialized", "arguments"]),
        };
        activity.result.as_mut().unwrap().text = ActivityTextCapture::Present {
            value: "readable result".to_owned(),
        };
        content.activity = Some(activity);

        let exact = project_body_search(content.clone()).unwrap().unwrap();
        let projection = project_search_content(content).unwrap().unwrap();

        assert_eq!(projection.index_text(), exact);
        let fragments = projection.fragments();
        assert_eq!(
            fragments
                .iter()
                .map(SearchContentFragment::kind)
                .collect::<Vec<_>>(),
            vec![
                SearchFragmentKind::NormalizedBody,
                SearchFragmentKind::StructuredContent,
                SearchFragmentKind::InvocationProtocol,
                SearchFragmentKind::InvocationServer,
                SearchFragmentKind::InvocationTool,
                SearchFragmentKind::InvocationArguments,
                SearchFragmentKind::ResultStatus,
                SearchFragmentKind::ResultText,
                SearchFragmentKind::ResultStructuredContent,
                SearchFragmentKind::LiteralFact,
                SearchFragmentKind::LiteralFact,
            ]
        );
        assert_eq!(
            fragments[1].index_text(&projection),
            "\"escaped \\\"wrapper\\\"\\nwith a decisive clause\""
        );
        assert_eq!(
            fragments[1].display_text(&projection),
            "escaped \"wrapper\"\nwith a decisive clause"
        );
        assert!(fragments[1].has_decoded_json_display());
        assert_eq!(
            fragments[5].display_text(&projection),
            "[\"serialized\",\"arguments\"]"
        );
        assert!(!fragments[5].has_decoded_json_display());
    }
}
