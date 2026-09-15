//! Released 1012e523 v3 envelope, copied without the new optional field.
use super::tests::{body, event_id, read_current, test_outbox, ENDPOINT, NOW, ROOT};
use super::*;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyState {
    schema_version: u16,
    entries: Vec<OutboxEntry>,
    roots: BTreeMap<String, LegacyRoot>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyRoot {
    retry_attempts: u64,
    dropped: u64,
    failure_sequence: u64,
    last_failure_class: Option<AnalyticsDeliveryFailureClass>,
    observation_due: bool,
}

fn old_rewrite(path: &Path) {
    let _lock = OutboxLock::acquire(&path.with_extension("lock")).unwrap();
    let bytes = fs::read(path).unwrap();
    let old: LegacyState =
        serde_json::from_slice(&bytes).expect("old v3 reader must accept new state");
    assert_eq!(old.schema_version, 3);
    let rewritten = serde_json::to_vec(&old).unwrap();
    assert_eq!(
        rewritten, bytes,
        "exact old v3 field ordering and payload bytes"
    );
    write_private_file_durably(path, &rewritten).unwrap();
}

fn record_timeout(outbox: &AnalyticsOutbox, now: i64) {
    let first = outbox.snapshot_at(ENDPOINT, now).unwrap().remove(0);
    outbox
        .reconcile_at(
            &[(
                first,
                DeliveryDisposition::Retry {
                    class: AnalyticsDeliveryFailureClass::Transport,
                    reason: Some(AnalyticsDeliveryFailureReason::RequestTimeout),
                    retry_after: None,
                },
            )],
            now,
        )
        .unwrap();
}

#[test]
fn old_new_old_new_preserves_the_shared_queue_and_invalidates_old_evidence() {
    const OTHER: &str = "00000000-0000-4000-8000-000000000003";
    let (_dir, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    let other = AnalyticsOutbox::open_at(path.clone(), OTHER, NOW).unwrap();
    other.append_at(ENDPOINT, &body(&event_id(2)), NOW).unwrap();
    old_rewrite(&path);
    for now in [NOW, NOW + RETRY_MAX_SECONDS as i64 + 1] {
        record_timeout(&outbox, now);
        assert_eq!(
            read_current(&path).root(ROOT).last_failure_reason,
            Some(AnalyticsDeliveryFailureReason::RequestTimeout)
        );
        let before = fs::read(&path).unwrap();
        let expected: Value = serde_json::from_slice(&before).unwrap();
        assert!(expected["roots"][ROOT].get("last_failure_reason").is_none());
        assert_eq!(expected["entries"].as_array().unwrap().len(), 2);
        old_rewrite(&path); // Identical bytes, different authoritative file generation.
        let reopened = AnalyticsOutbox::open_at(path.clone(), ROOT, now).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(read_current(&path).root(ROOT).last_failure_reason, None);
        assert_eq!(other.snapshot_at(ENDPOINT, now).unwrap().len(), 1);
        assert!(reopened.pending_observation_at(now).unwrap().is_none());
    }
    // Negative control: the previous implementation's extra root field is rejected.
    let mut incompatible: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    incompatible["roots"][ROOT]["last_failure_reason"] = Value::String("request_timeout".into());
    assert!(serde_json::from_value::<LegacyState>(incompatible).is_err());
}

#[test]
fn metadata_unavailability_never_changes_authoritative_queue_bytes() {
    for mode in [
        "missing",
        "corrupt",
        "oversized",
        "directory",
        "unknown",
        "version",
        "roots_bound",
        "sequence",
        "class",
        "pair",
        "digest",
    ] {
        let (_dir, path, outbox) = test_outbox();
        outbox
            .append_at(ENDPOINT, &body(&event_id(1)), NOW)
            .unwrap();
        record_timeout(&outbox, NOW);
        let before = fs::read(&path).unwrap();
        let sidecar = path.with_extension("reasons.json");
        let mut metadata: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        match mode {
            "missing" => fs::remove_file(&sidecar).unwrap(),
            "corrupt" => write_private_file_durably(&sidecar, b"invalid").unwrap(),
            "oversized" => write_private_file_durably(&sidecar, &vec![b'x'; 65537]).unwrap(),
            "directory" => {
                fs::remove_file(&sidecar).unwrap();
                fs::create_dir(&sidecar).unwrap();
            }
            _ => {
                match mode {
                    "unknown" => metadata["roots"][ROOT]["reason"] = serde_json::json!("future"),
                    "version" => metadata["version"] = serde_json::json!(2),
                    "roots_bound" => {
                        let reason = metadata["roots"][ROOT].clone();
                        for index in 0..OUTBOX_MAX_ENTRIES {
                            metadata["roots"][format!("extra-{index}")] = reason.clone();
                        }
                    }
                    "sequence" => metadata["roots"][ROOT]["sequence"] = serde_json::json!(999),
                    "class" => metadata["roots"][ROOT]["class"] = serde_json::json!("local_io"),
                    "pair" => metadata["roots"][ROOT]["reason"] = serde_json::json!("file_open"),
                    "digest" => {
                        let original = metadata["authority"]["sha256"][0].as_u64().unwrap();
                        metadata["authority"]["sha256"][0] = serde_json::json!(original ^ 1);
                    }
                    _ => unreachable!(),
                }
                write_private_file_durably(&sidecar, &serde_json::to_vec(&metadata).unwrap())
                    .unwrap();
            }
        }
        let reopened = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert_eq!(
            read_current(&path).root(ROOT).last_failure_reason,
            None,
            "{mode}"
        );
        assert_eq!(fs::read(&path).unwrap(), before, "{mode}");
        assert_eq!(
            reopened
                .snapshot_at(ENDPOINT, NOW + RETRY_MAX_SECONDS as i64 + 1)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn sidecar_write_failure_cannot_block_append_retry_or_future_open() {
    for extension in ["reasons.json", "reasons.tmp"] {
        let (_dir, path, outbox) = test_outbox();
        fs::create_dir(path.with_extension(extension)).unwrap();
        outbox
            .append_at(ENDPOINT, &body(&event_id(1)), NOW)
            .unwrap();
        record_timeout(&outbox, NOW);
        let before = fs::read(&path).unwrap();
        let _reopened = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert_eq!(read_current(&path).root(ROOT).last_failure_reason, None);
        assert_eq!(read_current(&path).root(ROOT).retry_attempts, 1);
        assert_eq!(fs::read(&path).unwrap(), before);
        old_rewrite(&path);
        let later = NOW + RETRY_MAX_SECONDS as i64 + 1;
        let retained = outbox.snapshot_at(ENDPOINT, later).unwrap().remove(0);
        assert_eq!(retained.payload(), body(&event_id(1)));
        outbox
            .reconcile_at(&[(retained, DeliveryDisposition::Accepted)], later)
            .unwrap();
        let recovered = outbox.pending_observation_at(later).unwrap().unwrap();
        assert_eq!(recovered.event.failure_reason, None);
        outbox
            .queue_observation_at(ENDPOINT, &body(&event_id(2)), &recovered, later)
            .unwrap();
        let health = outbox.snapshot_at(ENDPOINT, later).unwrap().remove(0);
        outbox
            .reconcile_at(&[(health, DeliveryDisposition::Accepted)], later)
            .unwrap();
        assert!(outbox.pending_observation_at(later).unwrap().is_none());
        assert!(read_current(&path).roots.is_empty());
    }
}

#[test]
fn stale_sidecar_after_queue_commit_is_ignored_and_purge_invalidates_metadata() {
    let (_dir, path, outbox) = test_outbox();
    outbox
        .append_at(ENDPOINT, &body(&event_id(1)), NOW)
        .unwrap();
    record_timeout(&outbox, NOW);
    let sidecar = path.with_extension("reasons.json");
    let stale = fs::read(&sidecar).unwrap();
    outbox
        .append_at(ENDPOINT, &body(&event_id(2)), NOW)
        .unwrap();
    // Model a crash/failure after the queue committed but before the sidecar did.
    write_private_file_durably(&sidecar, &stale).unwrap();
    assert_eq!(read_current(&path).root(ROOT).last_failure_reason, None);
    AnalyticsOutbox::purge(&path, Some(ROOT)).unwrap();
    assert!(!sidecar.exists());
    let reopened = AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
    reopened
        .append_at(ENDPOINT, &body(&event_id(3)), NOW)
        .unwrap();
    write_private_file_durably(&sidecar, &stale).unwrap();
    assert_eq!(read_current(&path).root(ROOT).last_failure_reason, None);
    assert_eq!(reopened.snapshot_at(ENDPOINT, NOW).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn unsafe_or_unreadable_sidecar_is_optional_and_symlink_target_is_untouched() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};
    for mode in ["symlink", "unreadable", "public"] {
        let (dir, path, outbox) = test_outbox();
        outbox
            .append_at(ENDPOINT, &body(&event_id(1)), NOW)
            .unwrap();
        record_timeout(&outbox, NOW);
        let before = fs::read(&path).unwrap();
        let sidecar = path.with_extension("reasons.json");
        let target = dir.path().join("unrelated");
        write_private_file_durably(&target, b"sentinel").unwrap();
        match mode {
            "symlink" => {
                fs::remove_file(&sidecar).unwrap();
                symlink(&target, &sidecar).unwrap();
            }
            "unreadable" => {
                fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o000)).unwrap()
            }
            "public" => fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o644)).unwrap(),
            _ => unreachable!(),
        }
        AnalyticsOutbox::open_at(path.clone(), ROOT, NOW).unwrap();
        assert_eq!(
            read_current(&path).root(ROOT).last_failure_reason,
            None,
            "{mode}"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
    }
}
