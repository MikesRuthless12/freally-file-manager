//! Phase 14f smoke test — queue-while-locked / volume-arrival watcher.
//!
//! Drives `freally_cli::volume_watch::watch_volumes` with a
//! tempdir-as-fake-volume timeline. Asserts:
//!
//! 1. A previously-absent root that becomes reachable surfaces as
//!    `VolumeEvent::Arrival`.
//! 2. A previously-reachable root that disappears surfaces as
//!    `VolumeEvent::Departure`.
//! 3. Cancellation terminates the loop promptly.
//! 4. Phase 17a — a traversal-laden root path is rejected before
//!    the watcher takes ownership.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use freally_cli::volume_watch::{
    StdProbe, VolumeEvent, VolumeProbe, VolumeWatchCancel, VolumeWatchError, VolumeWatchOptions,
    watch_volumes,
};
use tempfile::TempDir;

struct ScriptedProbe {
    present: Arc<Mutex<Vec<PathBuf>>>,
}
impl VolumeProbe for ScriptedProbe {
    fn is_reachable(&self, root: &Path) -> bool {
        let p = self.present.lock().unwrap();
        p.iter().any(|x| x.as_path() == root)
    }
}

#[test]
fn empty_roots_errors() {
    let err = watch_volumes(
        &[],
        VolumeWatchOptions::default(),
        VolumeWatchCancel::new(),
        StdProbe,
        |_| {},
    )
    .unwrap_err();
    assert!(matches!(err, VolumeWatchError::NoRoots));
}

#[test]
fn traversal_root_is_rejected_lexically() {
    let err = watch_volumes(
        &[PathBuf::from("foo/../etc/passwd")],
        VolumeWatchOptions::default(),
        VolumeWatchCancel::new(),
        StdProbe,
        |_| {},
    )
    .unwrap_err();
    match err {
        VolumeWatchError::PathRejected { .. } => {}
        other => panic!("expected PathRejected, got {other:?}"),
    }
}

#[test]
fn arrival_then_departure_observed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let present: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let probe = ScriptedProbe {
        present: present.clone(),
    };
    let cancel = VolumeWatchCancel::new();
    let observed: Arc<Mutex<Vec<VolumeEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let observed_for_loop = observed.clone();
    let cancel_for_loop = cancel.clone();
    let roots = vec![root.clone()];
    let handle = std::thread::spawn(move || {
        watch_volumes(
            &roots,
            VolumeWatchOptions {
                poll_interval: Duration::from_millis(20),
                // Generous ceiling: `cancel` is what ends this loop.
                // A 2 s cap raced the waits below on a slow runner.
                max_run_time: Some(Duration::from_secs(60)),
            },
            cancel_for_loop,
            probe,
            move |e| observed_for_loop.lock().unwrap().push(e),
        )
    });

    // Wait for each transition to actually be observed rather than
    // sleeping a fixed span and hoping the poll loop got a slice. A
    // loaded CI runner can skip straight past a 120 ms window, which
    // is how this passed on Windows and failed on macOS.
    let wait_for = |want_departure: bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let seen = observed.lock().unwrap();
            let hit = seen.iter().any(|e| {
                if want_departure {
                    matches!(e, VolumeEvent::Departure { .. })
                } else {
                    matches!(e, VolumeEvent::Arrival { .. })
                }
            });
            drop(seen);
            if hit {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    };

    // Let the loop establish an "absent" baseline first — pushing
    // before its first poll makes the root part of the initial state
    // and no Arrival is ever emitted.
    std::thread::sleep(Duration::from_millis(60));
    present.lock().unwrap().push(root.clone());
    assert!(wait_for(false), "watcher never reported the arrival");
    present.lock().unwrap().clear();
    assert!(wait_for(true), "watcher never reported the departure");
    cancel.cancel();

    let res = handle.join().unwrap();
    assert!(res.is_ok());
    let events = observed.lock().unwrap().clone();
    let arrival = events
        .iter()
        .position(|e| matches!(e, VolumeEvent::Arrival { .. }))
        .unwrap_or_else(|| panic!("no Arrival in {events:?}"));
    let departure = events
        .iter()
        .position(|e| matches!(e, VolumeEvent::Departure { .. }))
        .unwrap_or_else(|| panic!("no Departure in {events:?}"));
    assert!(arrival < departure);
    if let VolumeEvent::Arrival { root: r } = &events[arrival] {
        assert_eq!(r, &root);
    }
}

#[test]
fn cancel_terminates_quickly() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let present = Arc::new(Mutex::new(vec![root.clone()]));
    let probe = ScriptedProbe { present };
    let cancel = VolumeWatchCancel::new();
    let cancel_for_loop = cancel.clone();
    let roots = vec![root];
    let started = Instant::now();
    let handle = std::thread::spawn(move || {
        watch_volumes(
            &roots,
            VolumeWatchOptions {
                poll_interval: Duration::from_millis(50),
                max_run_time: Some(Duration::from_secs(30)),
            },
            cancel_for_loop,
            probe,
            |_| {},
        )
    });
    std::thread::sleep(Duration::from_millis(80));
    cancel.cancel();
    handle.join().unwrap().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "watcher took too long to cancel: {:?}",
        started.elapsed()
    );
}

#[test]
fn watcher_handles_real_filesystem_roundtrip_via_std_probe() {
    // Phase 14f's real-world consumer is the StdProbe — make sure
    // it agrees with the temp-dir lifecycle we'll see in production.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    assert!(StdProbe.is_reachable(&root));
    drop(dir);
    assert!(!StdProbe.is_reachable(&root));
}
