//! The in-process backend thread behind the `Backend` class: frames in and out,
//! startup failure, and shutdown.

use std::time::Duration;

use clawcode_backend::BackendThread;
use config::{
    AppConfig, ConfigHandle, ExtensionsConfig, SessionPersistenceConfig,
};
use futures::StreamExt;

/// The app's offline fixture configuration (`tests/fixture/config.toml`) with
/// its data moved under `home`: a second layer overrides `data_home`.
fn config_in(home: &tempfile::TempDir) -> ConfigHandle {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixture/config.toml");
    let data = home.path().join("data.toml");
    std::fs::write(
        &data,
        format!(
            "[session_persistence]\ndata_home = {:?}\n",
            home.path().join("data")
        ),
    )
    .expect("write config");
    config::load_from([fixture, data]).expect("load config")
}

/// An ACP `initialize` request frame with id 1.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":2,"info":{"name":"backend-test","version":"0.1.0"}}}"#;

/// The backend answers a frame with a frame.
#[tokio::test]
async fn start_answers_initialize_through_frames() {
    let home = tempfile::tempdir().expect("data home");
    let (mut backend, mut output) =
        BackendThread::start(config_in(&home)).await.expect("start");

    backend.send(INITIALIZE.to_owned()).expect("send");
    let reply =
        tokio::time::timeout(Duration::from_secs(10), output.frames.next())
            .await
            .expect("reply within 10s")
            .expect("a frame");
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("JSON");
    assert_eq!(reply["id"], 1);
    assert!(reply["result"]["protocolVersion"].is_number(), "{reply}");

    tokio::time::timeout(Duration::from_secs(10), backend.shutdown())
        .await
        .expect("shutdown within 10s")
        .expect("drained");
}

/// Shutdown ends the thread and the frame stream, and later sends fail.
#[tokio::test]
async fn shutdown_releases_the_backend() {
    let home = tempfile::tempdir().expect("data home");
    let (mut backend, output) =
        BackendThread::start(config_in(&home)).await.expect("start");

    tokio::time::timeout(Duration::from_secs(5), backend.shutdown())
        .await
        .expect("the thread exits within 5s")
        .expect("drained");

    assert!(backend.send(INITIALIZE.to_owned()).is_err());
    let frames: Vec<String> = output.frames.collect().await;
    assert!(frames.is_empty(), "{frames:?}");
    assert_eq!(output.ended.await.expect("end reported"), None);
}

/// The quit path blocks until the thread has stopped, within its bound.
#[tokio::test]
async fn shutdown_blocking_waits_for_the_thread() {
    let home = tempfile::tempdir().expect("data home");
    let (mut backend, output) =
        BackendThread::start(config_in(&home)).await.expect("start");

    let stopped = backend.shutdown_blocking(Duration::from_secs(10));

    assert!(stopped, "the thread stops within the bound");
    assert!(backend.send(INITIALIZE.to_owned()).is_err());
    assert_eq!(output.ended.await.expect("end reported"), None);
}

/// A configuration the application rejects fails `start` with its reason.
#[tokio::test]
async fn startup_failure_returns_the_reason() {
    let home = tempfile::tempdir().expect("data home");
    let config = ConfigHandle::from_config(AppConfig {
        session_persistence: SessionPersistenceConfig {
            data_home: Some(home.path().to_str().expect("utf8").to_owned()),
        },
        extensions: ExtensionsConfig {
            enabled: vec![
                protocol::ExtensionId::try_from("hook-examples")
                    .expect("extension id"),
            ],
        },
        ..AppConfig::default()
    });

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        BackendThread::start(config),
    )
    .await
    .expect("start must not hang");

    match result {
        Err(message) => assert!(message.contains("hook-examples"), "{message}"),
        Ok(_) => panic!("an uncompiled extension must fail startup"),
    }
}

/// An installation has an identity without an application, and opening it never starts a listener.
#[tokio::test]
async fn profile_persists_without_a_local_backend() {
    use clawcode_backend::RemoteHosting;
    use nsh_core::prelude::Instance;

    let home = tempfile::tempdir().expect("profile");
    let path = home.path().display().to_string();
    let first =
        Instance::new(RemoteHosting::open(path.clone()).await.expect("open"));
    let initial = RemoteHosting::snapshot(first.clone())
        .await
        .expect("snapshot");
    assert!(!initial.id.is_empty());
    assert!(!initial.enabled);
    assert!(initial.pending.is_empty());
    assert!(initial.known_hosts.is_empty());
    RemoteHosting::rename(first.clone(), "Local workstation".into())
        .await
        .expect("rename");
    first.with_mut(RemoteHosting::close);
    drop(first);

    let next = Instance::new(RemoteHosting::open(path).await.expect("reopen"));
    let snapshot = RemoteHosting::snapshot(next).await.expect("snapshot");
    assert_eq!(snapshot.id, initial.id);
    assert_eq!(snapshot.name, "Local workstation");
    assert!(!snapshot.enabled);
}

/// Opt-out is durable even when the window closes immediately afterwards.
#[tokio::test]
async fn sharing_preference_is_acknowledged_on_disk_before_returning() {
    use clawcode_backend::RemoteHosting;
    use nsh_core::prelude::Instance;

    let home = tempfile::tempdir().expect("profile");
    let path = home.path().display().to_string();
    // An installation created before native sharing preferences must opt in explicitly.
    let settings_path = home.path().join("remote.json");
    std::fs::write(
        &settings_path,
        r#"{"name":"Existing desktop","devices":{},"hosts":{}}"#,
    )
    .expect("old settings");
    let first =
        Instance::new(RemoteHosting::open(path.clone()).await.expect("open"));
    assert!(
        !RemoteHosting::snapshot(first.clone())
            .await
            .expect("status")
            .sharing_preference
    );
    first
        .with(|hosting| hosting.set_sharing_preference(true))
        .expect("enable");
    let enabled: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&settings_path).expect("saved opt-in"),
    )
    .expect("JSON");
    assert_eq!(enabled["hosting_enabled"], true);
    first
        .with(|hosting| hosting.set_sharing_preference(false))
        .expect("disable");
    // Read the actual file immediately after the synchronous opt-out, before closing the window.
    let saved =
        std::fs::read_to_string(&settings_path).expect("saved preference");
    let saved: serde_json::Value = serde_json::from_str(&saved).expect("JSON");
    assert_eq!(saved["hosting_enabled"], false);
    first.with_mut(RemoteHosting::close);
    drop(first);
    let reopened =
        Instance::new(RemoteHosting::open(path).await.expect("reopen"));
    assert!(
        !RemoteHosting::snapshot(reopened)
            .await
            .expect("status")
            .sharing_preference
    );
}

/// A failed preference write is visible to the caller and does not opt in.
#[tokio::test]
async fn sharing_preference_reports_persistence_failures() {
    use clawcode_backend::RemoteHosting;
    use nsh_core::prelude::Instance;

    let home = tempfile::tempdir().expect("profile");
    let hosting = Instance::new(
        RemoteHosting::open(home.path().display().to_string())
            .await
            .expect("open"),
    );
    std::fs::create_dir(home.path().join("remote.json"))
        .expect("unwritable destination");
    assert!(
        hosting
            .with(|host| host.set_sharing_preference(true))
            .is_err()
    );
    assert!(
        !RemoteHosting::snapshot(hosting)
            .await
            .expect("status")
            .sharing_preference
    );
}

/// Submit a real invitation and approve the test controller through the native host API.
async fn pair_with_host(
    host: &nsh_core::prelude::Instance<clawcode_backend::RemoteHosting>,
    client: &remote::DeviceProfile,
) -> remote::RemoteSession {
    use clawcode_backend::RemoteHosting;
    use remote::{NetworkConfig, RemoteClient};

    let invitation = RemoteHosting::invite(host.clone())
        .await
        .expect("invitation");
    assert!(invitation.qr_uri.starts_with("data:image/svg+xml;base64,"));
    let pair = Box::pin(RemoteClient::pair(
        client,
        NetworkConfig::direct_only(),
        &invitation.code,
    ));
    let approve = async {
        loop {
            let snapshot =
                RemoteHosting::snapshot(host.clone()).await.expect("status");
            if let Some(request) = snapshot.pending.first() {
                assert_eq!(request.id, client.id());
                RemoteHosting::approve(
                    host.clone(),
                    request.request_id.clone(),
                    true,
                )
                .await
                .expect("approval");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let (paired, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(pair, approve)
    })
    .await
    .expect("pair in time");
    paired.expect("pair").1
}

/// Revocation interrupts a pending ACP handler; kernel shutdown also drains a detached model Run.
#[tokio::test]
async fn approved_peer_reaches_the_backend_and_shutdown_disconnects_it() {
    use clawcode_backend::{Backend, RemoteHosting};
    use nsh_core::prelude::Instance;
    use remote::DeviceProfile;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let config_home = tempfile::tempdir().expect("backend home");
    let provider = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local provider");
    let mut config = (*config_in(&config_home).current()).clone();
    let mcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local MCP service");
    config.mcp_servers.push(
        config::McpServerConfig::builder()
            .name("disposable".into())
            .url(format!(
                "http://{}/mcp",
                mcp.local_addr().expect("MCP address")
            ))
            .build(),
    );
    config.providers[0].base_url = Some(format!(
        "http://{}",
        provider.local_addr().expect("provider address")
    ));
    let local = BackendThread::start(ConfigHandle::from_config(config))
        .await
        .expect("backend");
    let backend = Instance::new(Backend::from(local));
    let host_home = tempfile::tempdir().expect("host profile");
    let client_home = tempfile::tempdir().expect("client profile");
    let host = Instance::new(
        RemoteHosting::open(host_home.path().display().to_string())
            .await
            .expect("open"),
    );
    let client = DeviceProfile::open(client_home.path()).expect("controller");
    RemoteHosting::start(host.clone(), backend.clone(), Some("direct".into()))
        .await
        .expect("enable");
    let session = pair_with_host(&host, &client).await;
    let (mut incoming, outgoing, connection) = session.into_parts();
    outgoing.send(INITIALIZE.to_string()).await.expect("send");
    let reply = tokio::time::timeout(Duration::from_secs(10), incoming.recv())
        .await
        .expect("timely reply")
        .expect("open");
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("JSON");
    assert!(reply["result"]["protocolVersion"].is_number(), "{reply}");
    let create = json!({"jsonrpc": "2.0", "id": 2, "method": "session/new",
        "params": {"cwd": config_home.path()}});
    outgoing
        .send(create.to_string())
        .await
        .expect("create session");
    let created = async {
        loop {
            let frame = incoming.recv().await.expect("open");
            let message: Value = serde_json::from_str(&frame).expect("JSON");
            if message["id"] == 2 {
                break message["result"]["sessionId"]
                    .as_str()
                    .expect("created session")
                    .to_owned();
            }
        }
    };
    // Let the first Session finish with an unavailable optional MCP service.
    let unavailable = async {
        loop {
            let (mut service, _) =
                mcp.accept().await.expect("MCP startup request");
            service.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await.expect("unavailable response");
        }
    };
    let session_id = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            id = created => id,
            () = unavailable => unreachable!(),
        }
    })
    .await
    .expect("created session in time");
    let prompt = json!({"jsonrpc": "2.0", "id": 3, "method": "session/prompt",
        "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": "Hello"}]}});
    outgoing
        .send(prompt.to_string())
        .await
        .expect("send prompt");
    // Hold the provider response open while the production kernel owns its detached Run.
    let (mut request, _) =
        tokio::time::timeout(Duration::from_secs(10), provider.accept())
            .await
            .expect("model request in flight")
            .expect("provider connection");
    // A second ordinary Session is still inside its connection-owned handler when the owner revokes.
    let create = json!({"jsonrpc": "2.0", "id": 4, "method": "session/new",
        "params": {"cwd": config_home.path()}});
    outgoing
        .send(create.to_string())
        .await
        .expect("start second session");
    let (_pending_mcp, _) =
        tokio::time::timeout(Duration::from_secs(5), mcp.accept())
            .await
            .expect("second handler is in flight")
            .expect("MCP request");
    RemoteHosting::revoke(host.clone(), client.id())
        .await
        .expect("revoke peer");
    let reason =
        tokio::time::timeout(Duration::from_secs(3), connection.closed())
            .await
            .expect("revocation closes the link");
    assert_eq!(reason, "Device access revoked");
    tokio::time::timeout(
        Duration::from_secs(3),
        RemoteHosting::stop(host.clone()),
    )
    .await
    .expect("all connection handlers drain without answering the MCP request")
    .expect("hosting stops");
    // A newly authorized session must also close when the backend itself stops.
    RemoteHosting::start(host.clone(), backend.clone(), Some("direct".into()))
        .await
        .expect("restart hosting");
    let active = pair_with_host(&host, &client).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        Backend::shutdown(backend.clone()),
    )
    .await
    .expect("backend stops during an active Run")
    .expect("drained");
    tokio::time::timeout(
        Duration::from_secs(3),
        request.read_to_end(&mut Vec::new()),
    )
    .await
    .expect("shutdown releases the in-flight provider request")
    .expect("provider request closes");
    tokio::time::timeout(Duration::from_secs(5), active.closed())
        .await
        .expect("remote connection should end");
    assert!(
        !RemoteHosting::snapshot(host.clone())
            .await
            .expect("status")
            .enabled
    );
    assert!(
        RemoteHosting::start(host, backend, Some("direct".into()))
            .await
            .is_err()
    );
}

/// An outgoing controller works before startup and survives the shutdown of its own hosted backend.
#[tokio::test]
async fn native_controller_works_without_a_local_backend() {
    use clawcode_backend::{Backend, RemoteHosting, RemotePairing};
    use nsh_core::prelude::Instance;
    use remote::{DeviceProfile, HostEvent, NetworkConfig, RemoteHost};

    let server_dir = tempfile::tempdir().expect("server directory");
    let controller_dir = tempfile::tempdir().expect("controller directory");
    let server =
        DeviceProfile::open(server_dir.path()).expect("server identity");
    let (listener, mut events) =
        RemoteHost::start(server.clone(), NetworkConfig::direct_only())
            .await
            .expect("host");
    let controller = Instance::new(
        RemoteHosting::open(controller_dir.path().display().to_string())
            .await
            .expect("controller identity"),
    );
    let invitation = listener.invite().await.expect("invitation");
    let attempt = Instance::new(controller.with(RemoteHosting::begin_pairing));
    let dial = RemotePairing::dial(
        attempt.clone(),
        invitation.code,
        Some("direct".into()),
    );
    let host = async {
        loop {
            match events.recv().await.expect("host event") {
                HostEvent::Approval { request_id, .. } => {
                    listener.approve(&request_id, true).await.expect("approve")
                }
                HostEvent::Connected { session, .. } => break session,
                HostEvent::Changed => {}
            }
        }
    };
    let (paired, mut session) =
        tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(dial, host)
        })
        .await
        .expect("pair within deadline");
    paired.expect("prepared pairing");
    let paired = attempt.with_mut(RemotePairing::accept).expect("adopt");
    assert_eq!(paired.peer().id, server.id());
    paired.send(INITIALIZE.to_string()).expect("send ACP frame");
    let frame =
        tokio::time::timeout(Duration::from_secs(5), session.incoming.recv())
            .await
            .expect("frame deadline");
    assert_eq!(frame.as_deref(), Some(INITIALIZE));

    // Enabling and stopping this device's own local agent must preserve its outgoing connection.
    let home = tempfile::tempdir().expect("local backend");
    let backend = Instance::new(Backend::from(
        BackendThread::start(config_in(&home))
            .await
            .expect("start backend"),
    ));
    RemoteHosting::start(
        controller.clone(),
        backend.clone(),
        Some("direct".into()),
    )
    .await
    .expect("share");
    Backend::shutdown(backend).await.expect("drained");
    paired
        .send("{}".into())
        .expect("controller still connected");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), session.incoming.recv())
            .await
            .expect("frame")
            .as_deref(),
        Some("{}")
    );

    // Window disposal still closes QUIC after the kernel has gone.
    controller.with_mut(RemoteHosting::close);
    tokio::time::timeout(Duration::from_secs(5), session.closed())
        .await
        .expect("release remote session");
    listener.shutdown().await;
}

/// Stopping a local backend releases its database lease even though the desktop runtime stays up.
#[tokio::test]
async fn local_backend_reopens_the_same_profile_after_shutdown() {
    let home = tempfile::tempdir().expect("backend home");
    let config = config_in(&home);
    let (mut first, _frames) = BackendThread::start(config.clone())
        .await
        .expect("first start");
    first.shutdown().await.expect("drained");
    let (mut reopened, _frames) = BackendThread::start(config)
        .await
        .expect("reuse released lease");
    reopened.shutdown().await.expect("drained");
}

/// Dismissal before and after approval releases only that attempt; adoption saves a host once.
#[tokio::test]
async fn outgoing_pairing_has_an_explicit_adoption_boundary() {
    use clawcode_backend::{RemoteConnection, RemoteHosting, RemotePairing};
    use nsh_core::prelude::Instance;
    use remote::{DeviceProfile, HostEvent, NetworkConfig, RemoteHost};

    let host_dir = tempfile::tempdir().expect("host directory");
    let client_dir = tempfile::tempdir().expect("controller directory");
    let host = DeviceProfile::open(host_dir.path()).expect("host identity");
    let (listener, mut events) =
        RemoteHost::start(host, NetworkConfig::direct_only())
            .await
            .expect("listener");
    let client = Instance::new(
        RemoteHosting::open(client_dir.path().display().to_string())
            .await
            .expect("controller"),
    );
    // Keep the controller's endpoint in use so a disposed dial must explicitly close its own wire.
    let profile = DeviceProfile::open(client_dir.path()).expect("same profile");
    let (other_role, _other_events) =
        RemoteHost::start(profile, NetworkConfig::direct_only())
            .await
            .expect("independent host role");
    // A still-pending decision must disappear from the host when dismissed.
    let attempt = Instance::new(client.with(RemoteHosting::begin_pairing));
    let invite = listener.invite().await.expect("invitation");
    let dialing = RemotePairing::dial(
        attempt.clone(),
        invite.code,
        Some("direct".into()),
    );
    let dismiss = async {
        while !matches!(
            events.recv().await.expect("event"),
            HostEvent::Approval { .. }
        ) {}
        attempt.with_mut(RemotePairing::cancel);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(dialing, dismiss)
    })
    .await
    .expect("cancel during approval");
    assert!(result.is_err());
    tokio::time::timeout(Duration::from_secs(3), async {
        while !listener.pending().await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owner prompt disappears");

    // Even an already approved wire is disposable until the UI adopts the result.
    for adopt in [false, true] {
        let attempt = Instance::new(client.with(RemoteHosting::begin_pairing));
        let invite = listener.invite().await.expect("invitation");
        let dialing = RemotePairing::dial(
            attempt.clone(),
            invite.code,
            Some("direct".into()),
        );
        let approve = async {
            loop {
                match events.recv().await.expect("event") {
                    HostEvent::Approval { request_id, .. } => listener
                        .approve(&request_id, true)
                        .await
                        .expect("approve"),
                    HostEvent::Connected { peer, session } => {
                        break (peer, session);
                    }
                    HostEvent::Changed => {}
                }
            }
        };
        let (result, (peer, mut session)) =
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(dialing, approve)
            })
            .await
            .expect("approval");
        result.expect("prepared pairing");
        assert!(
            RemoteHosting::snapshot(client.clone())
                .await
                .expect("status")
                .known_hosts
                .is_empty()
        );
        if adopt {
            let accepted =
                attempt.with_mut(RemotePairing::accept).expect("adopt");
            attempt.with_mut(RemotePairing::cancel);
            assert_eq!(
                RemoteHosting::snapshot(client.clone())
                    .await
                    .expect("status")
                    .known_hosts
                    .len(),
                1
            );
            // Reserve every outbound slot on the real wire to deterministically simulate a busy writer.
            accepted.send("{}".into()).expect("adopted link stays open");
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(3),
                    session.incoming.recv()
                )
                .await
                .expect("adopted link delivers")
                .as_deref(),
                Some("{}")
            );
            let outgoing = session.outgoing.clone();
            let capacity = outgoing
                .try_reserve_many(outgoing.max_capacity())
                .expect("reserve capacity");
            let congested = RemoteConnection::from((peer, session));
            assert!(congested.send("{}".into()).is_err());
            tokio::time::timeout(Duration::from_secs(3), outgoing.closed())
                .await
                .expect("congestion closes the link");
            drop(capacity);
            assert!(congested.send("{}".into()).is_err());
        } else {
            attempt.with_mut(RemotePairing::cancel);
            assert!(attempt.with_mut(RemotePairing::accept).is_err());
            tokio::time::timeout(Duration::from_secs(3), session.closed())
                .await
                .expect("discarded wire ends");
        }
    }
    other_role
        .invite()
        .await
        .expect("independent host still active");
    other_role.shutdown().await;
    listener.shutdown().await;
}
