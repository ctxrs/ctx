use std::{
    io,
    sync::{Arc, Mutex},
};

use ctx_pro_host_protocol::{
    AgentAttribution, BlameAttribution, BlameCoverage, BlameCoverageUnit, BlameMatch, BlameOutcome,
    BlameResult, CoreMaterializationReceiptIdentity, FactConfidence, FactState, FileBlameMatch,
    LineRange, ProductionRelationship, QuerySnapshotExpectation, ResolvedBlameTarget, ResourceKind,
    ResourceRef,
};

use crate::ui::{ColorMode, Document, RenderContext, StreamKind, TestContext, Ui};

const PARENT_ID: &str = "018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2";
const ROOT_ID: &str = "018f0f65-8b1f-7f30-9dc4-a81c7e36a1b3";

fn context(width: usize) -> RenderContext {
    RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Never))
}

fn resource(kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: format!("{}:{display}", kind.wire_name()),
        kind,
        display: display.to_owned(),
    }
}

fn render_lineage(
    width: usize,
    indent: usize,
    parent: Option<&ResourceRef>,
    root: Option<&ResourceRef>,
) -> String {
    let mut document = Document::new();
    super::render_session_lineage(&mut document, &context(width), indent, parent, root);
    document.render_plain()
}

#[test]
fn distinct_parent_and_root_render_as_exact_typed_roles() {
    let parent = resource(ResourceKind::Session, PARENT_ID);
    let root = resource(ResourceKind::Run, ROOT_ID);

    assert_eq!(
        render_lineage(80, 0, Some(&parent), Some(&root)),
        concat!(
            "parent        session 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2\n",
            "owning root   run 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b3\n",
        )
    );
}

#[test]
fn same_display_parent_and_root_remain_exact_typed_roles() {
    let parent = resource(ResourceKind::Session, PARENT_ID);
    let root = resource(ResourceKind::Run, PARENT_ID);

    assert_eq!(
        render_lineage(80, 0, Some(&parent), Some(&root)),
        concat!(
            "parent        session 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2\n",
            "owning root   run 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2\n",
        )
    );
}

#[test]
fn lineage_is_exact_and_copyable_at_supported_widths() {
    let parent = resource(ResourceKind::Session, PARENT_ID);
    let root = resource(ResourceKind::Run, ROOT_ID);
    let stacked = concat!(
        "    parent\n",
        "      session 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2\n",
        "    owning root\n",
        "      run 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b3\n",
    );
    let aligned = concat!(
        "    parent        session 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b2\n",
        "    owning root   run 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b3\n",
    );

    for (width, expected) in [(32, stacked), (48, stacked), (80, aligned), (120, aligned)] {
        let rendered = render_lineage(width, 4, Some(&parent), Some(&root));
        assert_eq!(rendered, expected, "width {width}");
        assert!(rendered.contains(PARENT_ID), "width {width}");
        assert!(rendered.contains(ROOT_ID), "width {width}");
    }
}

#[test]
fn absent_parent_preserves_existing_root_and_empty_behavior() {
    let root = resource(ResourceKind::Run, ROOT_ID);

    assert_eq!(render_lineage(80, 0, None, None), "");
    assert_eq!(
        render_lineage(80, 0, None, Some(&root)),
        "owning root   run 018f0f65-8b1f-7f30-9dc4-a81c7e36a1b3\n"
    );
}

#[derive(Clone, Default)]
struct SharedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().unwrap().clone()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("shared writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn same_display_lineage_leaves_machine_json_bytes_and_schema_unchanged() {
    let parent = resource(ResourceKind::Session, PARENT_ID);
    let root = resource(ResourceKind::Run, PARENT_ID);
    let result = BlameResult {
        snapshot: QuerySnapshotExpectation::Core {
            receipt: CoreMaterializationReceiptIdentity {
                core_generation_id: "a".repeat(64),
                materializer_revision: "materializer-v1".to_owned(),
            },
        },
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(ResourceKind::Repository, "ctxrs/ctx"),
            requested_lines: None,
        },
        git_snapshot: None,
        outcome: BlameOutcome {
            attribution: BlameAttribution::Proven,
            coverage: BlameCoverage {
                unit: BlameCoverageUnit::CommittedLine,
                evaluated: 1,
                proven: 1,
                possible: 0,
                conflicting: 0,
                none: 0,
            },
        },
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: "file:src/lib.rs".to_owned(),
            lines: LineRange { start: 1, end: 1 },
            commit: resource(ResourceKind::Commit, "abcdef"),
            line_evidence_numbers: Vec::new(),
            production: vec![AgentAttribution {
                id: "fact:producer".to_owned(),
                relationship: ProductionRelationship::ProducedBy,
                producing_session: resource(ResourceKind::Session, "worker"),
                parent_session: Some(parent.clone()),
                direct_actor: None,
                owning_root: Some(root.clone()),
                fact_occurred_at_ms: Some(1_721_000_000_000),
                confidence: FactConfidence::Explicit,
                state: FactState::Asserted,
                evidence_numbers: Vec::new(),
            }],
        })],
        evidence: Vec::new(),
        next: None,
        lineage: None,
    };
    let result = crate::pro::HostedBlameResult {
        result,
        freshness: crate::pro::BlameResultFreshness::Current,
    };
    let mut expected_value = serde_json::to_value(&result.result).unwrap();
    expected_value.as_object_mut().unwrap().insert(
        "evidence_context".to_owned(),
        serde_json::json!({"status": "unavailable", "items": []}),
    );
    expected_value.as_object_mut().unwrap().insert(
        "freshness".to_owned(),
        serde_json::json!({"state": "current"}),
    );
    let mut expected_bytes = serde_json::to_vec_pretty(&expected_value).unwrap();
    expected_bytes.push(b'\n');

    for width in [32, 48, 80, 120] {
        let writer = SharedWriter::default();
        let captured = writer.clone();
        let stdout_context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, width).color(ColorMode::Always),
        );
        let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
        let mut ui = Ui::with_writers(writer, stdout_context, io::sink(), stderr_context);

        let measured = super::super::print_blame_result(&result, true, &mut ui).unwrap();
        ui.flush().unwrap();
        let actual_bytes = captured.bytes();

        assert_eq!(actual_bytes, expected_bytes, "width {width}");
        assert_eq!(measured, expected_bytes.len(), "width {width}");
        let actual_value: serde_json::Value = serde_json::from_slice(&actual_bytes).unwrap();
        let production = &actual_value["matches"][0]["value"]["production"][0];
        assert_eq!(
            production["parent_session"],
            serde_json::to_value(&parent).unwrap()
        );
        assert_eq!(
            production["owning_root"],
            serde_json::to_value(&root).unwrap()
        );
    }
}
