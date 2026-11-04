use super::*;
use crate::protocol::{BlameResultFreshness, BlameTarget};
use ctx_history_snapshot_reader::{SnapshotContract, SnapshotError};

fn copy_fixture(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    restrict_private_directory(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            let mut permissions = fs::metadata(&target).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(0o600);
            }
            if matches!(
                target.extension().and_then(|ext| ext.to_str()),
                Some("fast" | "fieldnorm" | "idx" | "pos" | "store" | "term")
            ) {
                permissions.set_readonly(true);
            }
            fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

#[test]
fn main_schema_22_blame_survives_upgrade_and_reports_actual_pins() {
    let fixture = Fixture::new();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/schema-22");
    copy_fixture(&input.join("search"), &fixture.data_root.join("search"));
    let target = BlameTarget::Commit {
        oid: fs::read_to_string(input.join("commit.txt")).unwrap(),
        repository: None,
    };
    // Copying invalidates inode-bound physical certification. Restore it with
    // the ordinary query owner; pinned Core readers deliberately cannot repair it.
    drop(VerifiedIndex::open_pinned(&fixture.index_root).unwrap());
    let before = crate::query(&fixture.data_root, &target, 8, None).unwrap();
    assert_eq!(before.freshness, BlameResultFreshness::Current);
    assert_eq!(before.result.matches.len(), 1);
    assert_eq!(before.result.evidence.len(), 1);
    let old_generation = before.result.evidence[0]
        .citation
        .core_generation_id
        .clone();
    let contract = SnapshotContract::current().unwrap();
    let old = CoreSnapshot::open(&fixture.data_root, &old_generation, &contract).unwrap();
    assert_eq!(old.contract().schema.lexical_schema_version, 22);
    assert_ne!(
        old.contract().schema.policy_schema_hash,
        contract.schema.policy_schema_hash
    );
    // An explicit older expectation remains exact, and fingerprint failures
    // cannot be admitted by lexical predecessor compatibility.
    let mut wrong = old.contract().clone();
    wrong.schema.lexical_schema_version = 21;
    assert!(matches!(
        CoreSnapshot::open(&fixture.data_root, &old_generation, &wrong),
        Err(SnapshotError::SchemaMismatch { .. })
    ));
    let mut wrong = contract.clone();
    wrong.core_record_fingerprint = "f".repeat(64);
    assert!(CoreSnapshot::open(&fixture.data_root, &old_generation, &wrong).is_err());
    assert_eq!(
        crate::readiness(&fixture.data_root).unwrap().currentness,
        CoreProjectionCurrentness::Current
    );

    let index = VerifiedIndex::open_pinned(&fixture.index_root).unwrap();
    let certificate = index.manifest().sources[0].clone();
    let records = old
        .record_page(
            certificate.observation().source(),
            None,
            8,
            ctx_history_snapshot_reader::DEFAULT_SNAPSHOT_PAGE_BUDGET,
        )
        .unwrap()
        .items;
    let mut writer = GenerationWriter::open(&fixture.index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .begin_source(certificate.observation().source().clone())
        .unwrap();
    for record in &records {
        writer.add_core_record(record.core_record.clone()).unwrap();
    }
    writer.certify_source(certificate).unwrap();
    let during = crate::query(&fixture.data_root, &target, 8, None).unwrap();
    assert_eq!(during.result.evidence, before.result.evidence);
    let first_current = writer.commit(|_| true).unwrap().generation_id;
    assert_ne!(first_current, old_generation);
    drop(old);
    drop(index);
    let stale = crate::query(&fixture.data_root, &target, 8, None).unwrap();
    assert_eq!(stale.freshness, BlameResultFreshness::StaleCommitted);
    assert_eq!(stale.result.evidence, before.result.evidence);
    assert_eq!(
        crate::readiness(&fixture.data_root).unwrap().currentness,
        CoreProjectionCurrentness::Stale
    );
    // If further Core publications outrun attribution, its old pin can leave
    // the ordinary active/previous retention window. Report pending projection
    // state honestly until the normal worker catches up.
    let generation = fixture.publish(
        2,
        &[(
            source("new-history"),
            vec![(1, "additional history".to_owned())],
        )],
        None,
    );
    let pending = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(pending.currentness, CoreProjectionCurrentness::Stale);
    assert!(!pending.availability.commit_blame);
    assert_eq!(
        crate::query(&fixture.data_root, &target, 8, None)
            .unwrap_err()
            .error_code,
        "stale_source"
    );
    // The daemon's normal publication callback uses this same completion owner.
    crate::catch_up_current(&fixture.data_root, &|| false).unwrap();
    let after = crate::query(&fixture.data_root, &target, 8, None).unwrap();
    assert_eq!(after.freshness, BlameResultFreshness::Current);
    assert_eq!(after.result.matches.len(), 1);
    assert_eq!(after.result.evidence.len(), 1);
    assert_eq!(
        after.result.evidence[0].citation.event_id,
        before.result.evidence[0].citation.event_id
    );
    assert_eq!(
        after.result.evidence[0].citation.core_generation_id,
        generation
    );
    let ready = crate::readiness(&fixture.data_root).unwrap();
    assert_eq!(ready.currentness, CoreProjectionCurrentness::Current);
    assert_eq!(ready.receipt.unwrap().core_generation_id, generation);
    assert!(ready.availability.commit_blame);
}
