//! Check desktop networking's process-wide thread budget in an isolated test binary.

#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use clawcode_backend::{RemoteConnection, RemoteHosting};
use nsh_core::prelude::Instance;

/// Count live native network workers independently of the profile's reported listener state.
fn network_workers() -> usize {
    fs::read_dir("/proc/self/task")
        .expect("enumerate process threads")
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path().join("comm")).ok())
        // Linux truncates the Tokio thread name to fifteen bytes.
        .filter(|name| name.trim_end() == "clawcode-deskto")
        .count()
}

/// Ordinary startup, settings and cleanup need no network pool, and dialing uses a small pool.
#[tokio::test]
async fn idle_profile_leaves_network_workers_unstarted() {
    let home = tempfile::tempdir().expect("profile directory");
    let path = home.path().display().to_string();
    // Hold the settings read in the filesystem until this same engine thread can run again.
    let settings = home.path().join("remote.json");
    assert!(
        Command::new("mkfifo")
            .arg(&settings)
            .status()
            .expect("create FIFO")
            .success()
    );
    let (release, resumed) = mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        let responsive = resumed.recv_timeout(Duration::from_secs(3)).is_ok();
        fs::write(settings, r#"{"devices":{},"hosts":{}}"#)
            .expect("write profile settings");
        responsive
    });
    let mut opening = std::pin::pin!(RemoteHosting::open(path.clone()));
    assert!(
        futures::poll!(opening.as_mut()).is_pending(),
        "profile I/O occupied the engine"
    );
    release
        .send(())
        .expect("engine resumed before disk read finished");
    let profile = Instance::new(opening.await.expect("open profile"));
    assert!(
        writer.join().expect("profile writer"),
        "engine stalled on the profile read"
    );
    assert!(
        !RemoteHosting::snapshot(profile.clone())
            .await
            .expect("snapshot")
            .enabled
    );
    RemoteHosting::rename(profile.clone(), "Test device".into())
        .await
        .expect("rename");
    RemoteHosting::revoke(profile.clone(), "absent".into())
        .await
        .expect("offline revoke");
    RemoteHosting::stop(profile.clone())
        .await
        .expect("idle stop");
    profile.with_mut(RemoteHosting::close);
    drop(profile);
    assert_eq!(
        network_workers(),
        0,
        "idle profile started the network executor"
    );

    let profile =
        Instance::new(RemoteHosting::open(path).await.expect("reopen"));
    assert!(
        RemoteConnection::connect(
            profile,
            "absent".into(),
            Some("direct".into())
        )
        .await
        .is_err()
    );
    let workers = network_workers();
    assert!(
        (1..=2).contains(&workers),
        "dial started {workers} network workers"
    );
}
