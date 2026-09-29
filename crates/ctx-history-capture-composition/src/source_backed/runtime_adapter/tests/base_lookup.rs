use super::*;
use crate::source_backed::{
    take_base_source_manifest_visits, SourceBackedGenerationSink, SourceBackedRouteResources,
};
use ctx_history_capture_model::SourceRouteIdentity;
use ctx_history_capture_runtime::{
    SourceBackedLogicalSourceFailures, SourceBackedRecordRejections,
};
use std::collections::HashMap;

#[test]
fn route_base_lookup_preserves_ownership_without_a_per_member_manifest_scan() {
    for count in [64_u64, 512] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let left = SourceRouteIdentity::from_sha256("01".repeat(32)).unwrap();
        let right = SourceRouteIdentity::from_sha256("02".repeat(32)).unwrap();
        let absent = SourceRouteIdentity::from_sha256("03".repeat(32)).unwrap();
        let mut expected = HashMap::new();
        let mut left_sources = Vec::new();
        let mut right_sources = Vec::new();
        for ordinal in 0..count {
            let mut lineage = [0; 32];
            lineage[..8].copy_from_slice(&ordinal.to_le_bytes());
            let source = SourceKey::derive(
                "codex",
                "lookup-jsonl",
                "lookup-v1",
                1,
                SourceAnchor::CatalogLineage(lineage),
            )
            .unwrap();
            let observation =
                SourceObservation::new(source.clone(), "lookup-observation", vec![1]).unwrap();
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                "lookup-parser",
                [1; 32],
                ScannedSourceCounts::default(),
            )
            .unwrap();
            writer.begin_source(source.clone()).unwrap();
            writer.certify_source(certificate.clone()).unwrap();
            if ordinal % 2 == 0 {
                left_sources.push(source.clone());
            } else {
                right_sources.push(source.clone());
            }
            expected.insert(source, certificate);
        }
        writer
            .set_present_source_routes(vec![
                SourceRouteSnapshot::present(left.clone(), left_sources.clone()).unwrap(),
                SourceRouteSnapshot::present(right.clone(), right_sources).unwrap(),
            ])
            .unwrap();
        writer.commit(|_| true).unwrap();

        let writer = GenerationWriter::open(temp.path(), WriterOptions::default())
            .unwrap()
            .into_writer()
            .unwrap();
        let mut lifecycle = IndexCaptureLifecycle(writer);
        let mut owners = HashMap::new();
        let mut inventories = Vec::new();
        let mut removals = Vec::new();
        let mut failures = SourceBackedLogicalSourceFailures::default();
        let mut rejections = SourceBackedRecordRejections::default();
        let mut sink = SourceBackedGenerationSink::new(
            &mut lifecycle,
            &mut owners,
            &mut inventories,
            &mut removals,
            0,
            left.clone(),
            None,
            SourceBackedRouteResources::production(1),
            &mut failures,
            &mut rejections,
            None,
            None,
            None,
        );
        take_base_source_manifest_visits();
        let selected = sink.base_route_sources().unwrap();
        let visits = take_base_source_manifest_visits();
        assert_eq!(selected.len(), left_sources.len());
        for source in &left_sources {
            assert_eq!(selected.get(source), expected.get(source));
            assert!(selected[source]
                .observation()
                .source()
                .exact_descriptor_eq(source));
        }
        assert!(visits >= selected.len() as u64);
        assert!(
            visits <= selected.len() as u64 * (u64::from(count.ilog2()) + 2),
            "retained members must use the index lookup rather than scan the full manifest"
        );
        let original = &left_sources[0];
        let changed_descriptor = SourceKey::derive(
            original.provider(),
            "changed-format",
            "changed-schema",
            1,
            original.anchor().clone(),
        )
        .unwrap();
        assert_eq!(original, &changed_descriptor);
        assert!(!original.exact_descriptor_eq(&changed_descriptor));
        assert!(sink.base_route_source(&changed_descriptor).is_none());
        assert!(sink.base_route_source(original).is_some());

        sink.base_route_aliases.insert(right);
        assert_eq!(sink.base_route_sources().unwrap(), expected);
        sink.base_route_aliases.insert(left);
        assert!(
            sink.base_route_sources().is_err(),
            "overlapping aliases must fail"
        );
        sink.base_route_aliases.clear();
        sink.route_identity = absent;
        assert!(sink.base_route_sources().unwrap().is_empty());
    }
}
