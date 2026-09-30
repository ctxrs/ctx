use super::*;
use std::sync::{Arc, Barrier};
#[test]
fn concurrent_hook_launches_share_one_claim_before_any_spawn() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("launch.json");
    let barrier = Arc::new(Barrier::new(64));
    let threads = (0..64)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                claim(&path, 1_800_000_000).unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
    assert!(!claim(&path, 1_800_000_059).unwrap());
    assert!(claim(&path, 1_800_000_060).unwrap());
}
#[test]
fn an_unknown_or_future_launch_hint_cannot_trigger_repeated_spawns() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("launch.json");
    let mut file = crate::analytics_state::StateFile::try_open(&path)
        .unwrap()
        .unwrap();
    file.write(&LaunchState {
        schema_version: 2,
        next_allowed_at: 0,
    })
    .unwrap();
    drop(file);
    assert!(!claim(&path, 1_800_000_000).unwrap());
    let mut file = crate::analytics_state::StateFile::try_open(&path)
        .unwrap()
        .unwrap();
    file.write(&LaunchState {
        schema_version: 1,
        next_allowed_at: i64::MAX,
    })
    .unwrap();
    drop(file);
    assert!(!claim(&path, 1_800_000_000).unwrap());
}

#[test]
fn torn_launch_hint_defers_once_then_recovers_without_an_immediate_spawn() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("launch.json");
    let file = crate::analytics_state::StateFile::try_open(&path)
        .unwrap()
        .unwrap();
    drop(file);
    std::fs::write(&path, b"{\"schema_version\":1,").unwrap();
    assert!(!claim(&path, 1_800_000_000).unwrap());
    assert!(!claim(&path, 1_800_000_059).unwrap());
    assert!(claim(&path, 1_800_000_060).unwrap());
}

#[test]
fn clock_correction_repairs_once_and_recovers_after_one_interval() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("launch.json");
    let now = 1_800_000_000;
    assert!(claim(&path, now + 86_400).unwrap());
    assert!(!claim(&path, now).unwrap());
    assert!(!claim(&path, now + 1).unwrap());
    assert!(!claim(&path, now + 59).unwrap());
    assert!(claim(&path, now + 60).unwrap());
    assert!(!claim(&path, now + 119).unwrap());
    assert!(claim(&path, now + 120).unwrap());
}
