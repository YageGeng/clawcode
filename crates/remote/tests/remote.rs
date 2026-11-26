use std::{path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use remote::{
    DeviceProfile, HostEvent, NetworkConfig, RemoteClient, RemoteHost,
};
use tempfile::tempdir;
use tokio::{sync::mpsc, time::timeout};

/// Return the next owner approval prompt while ignoring connection count updates.
async fn next_request(
    events: &mut mpsc::Receiver<HostEvent>,
) -> Result<String> {
    while let Some(event) = events.recv().await {
        if let HostEvent::Approval { request_id, .. } = event {
            return Ok(request_id);
        }
    }
    anyhow::bail!("host stopped")
}

/// The owner must be notified when a new invitation replaces the old one and when it expires unused.
#[tokio::test]
async fn invitation_changes_wake_the_owner_without_a_controller() -> Result<()>
{
    let root = tempdir()?;
    let (host, mut events) = RemoteHost::start(
        DeviceProfile::open(root.path())?,
        NetworkConfig::direct_only(),
    )
    .await?;
    let first = host.invite().await?;
    ensure!(
        host.invitation_generation().await.as_deref()
            == Some(first.generation.as_str())
    );
    ensure!(
        matches!(events.try_recv(), Ok(HostEvent::Changed)),
        "Missing invitation notification"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    let replacement = host.invite().await?;
    ensure!(first.code != replacement.code);
    ensure!(first.generation != replacement.generation);
    ensure!(
        host.invitation_generation().await.as_deref()
            == Some(replacement.generation.as_str())
    );
    ensure!(
        matches!(events.try_recv(), Ok(HostEvent::Changed)),
        "Missing replacement notification"
    );
    tokio::time::advance(Duration::from_secs(291)).await;
    tokio::task::yield_now().await;
    ensure!(
        events.try_recv().is_err(),
        "Old timer cleared a replacement invitation"
    );
    ensure!(
        host.invitation_generation().await.as_deref()
            == Some(replacement.generation.as_str())
    );
    tokio::time::advance(Duration::from_secs(10)).await;
    ensure!(
        matches!(
            timeout(Duration::from_secs(1), events.recv()).await?,
            Some(HostEvent::Changed)
        ),
        "Missing expiry notification"
    );
    ensure!(host.invitation_generation().await.is_none());
    tokio::time::resume();
    host.shutdown().await;
    Ok(())
}

/// Verify the same user-only key wins when independent initializers start together.
#[test]
fn concurrent_identity_creation_preserves_one_device() -> Result<()> {
    let root = tempdir()?;
    let path: &Path = root.path();
    std::thread::scope(|scope| -> Result<()> {
        let jobs: Vec<_> = (0..6)
            .map(|_| scope.spawn(|| DeviceProfile::open(path)))
            .collect();
        let peers = jobs
            .into_iter()
            .map(|job| job.join().expect("initializer"))
            .collect::<Result<Vec<_>>>()?;
        let id = DeviceProfile::open(path)?.id();
        ensure!(peers.iter().all(|peer| peer.id() == id));
        Ok(())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path.join("device.key"))?
            .permissions()
            .mode();
        ensure!(
            mode & 0o777 == 0o600,
            "Device key permissions expose the signing identity"
        );
    }
    Ok(())
}

/// Require host confirmation, exchange ACP lines, reconnect with durable trust and revoke live access.
#[tokio::test]
async fn confirmed_pair_reconnect_frames_and_revocation() -> Result<()> {
    timeout(Duration::from_secs(30), async {
        let host_root = tempdir()?;
        let client_root = tempdir()?;
        let host_profile = DeviceProfile::open(host_root.path())?;
        let client = DeviceProfile::open(client_root.path())?;
        client.set_name("测试控制器")?;
        let direct = NetworkConfig::direct_only();
        let (host, mut events) =
            RemoteHost::start(host_profile.clone(), direct.clone()).await?;
        let invitation = host.invite().await?;
        let pairing =
            RemoteClient::pair(&client, direct.clone(), &invitation.code);
        let confirm = async {
            let id = next_request(&mut events).await?;
            ensure!(host.invitation_generation().await.is_none());
            ensure!(host.online().await.is_empty());
            ensure!(host_profile.trusted_devices().is_empty());
            host.approve(&id, true).await
        };
        let (pair, approval) = tokio::join!(pairing, confirm);
        approval?;
        let (peer, mut session) = pair?;
        ensure!(peer.id == host_profile.id());
        let mut accepted = loop {
            if let HostEvent::Connected { session, .. } = events
                .recv()
                .await
                .context("host stopped before delivering session")?
            {
                break session;
            }
        };
        let line = r#"{"text":"第一行\nsecond"}"#;
        session.outgoing.send(line.to_owned()).await?;
        ensure!(accepted.incoming.recv().await.as_deref() == Some(line));
        accepted.outgoing.send(line.to_owned()).await?;
        ensure!(session.incoming.recv().await.as_deref() == Some(line));
        // Disposing an idle frame reader must end this link even while its writer is retained.
        let (incoming, outgoing, _control) = session.into_parts();
        drop(incoming);
        timeout(Duration::from_secs(3), outgoing.closed())
            .await
            .context(
                "the wire stayed open after its frame reader was dropped",
            )?;
        ensure!(accepted.closed().await == "Remote device disconnected");
        ensure!(
            DeviceProfile::open(host_root.path())?
                .trusted_devices()
                .first()
                .context("missing trusted device")?
                .name
                == "测试控制器"
        );
        let restored_client = DeviceProfile::open(client_root.path())?;
        let (_, session) =
            RemoteClient::connect(&restored_client, direct.clone(), &peer.id)
                .await?;
        ensure!(host.online().await.len() == 1);
        // A disk failure must still terminate the live session and deny reconnection.
        let records = host_root.path().join("remote.json");
        let original = host_root.path().join("saved.json");
        std::fs::rename(&records, &original)?;
        std::fs::create_dir(&records)?;
        ensure!(host.revoke(&client.id()).await.is_err());
        let reason = session.closed().await;
        ensure!(
            reason == "Device access revoked",
            "Unexpected revocation result: {reason}"
        );
        ensure!(host.online().await.is_empty());
        ensure!(
            RemoteClient::connect(&restored_client, direct.clone(), &peer.id)
                .await
                .is_err()
        );
        std::fs::remove_dir(&records)?;
        std::fs::rename(&original, &records)?;
        host.revoke(&client.id()).await?;
        ensure!(
            RemoteClient::pair(&client, direct, &invitation.code)
                .await
                .is_err()
        );
        host.shutdown().await;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("timed out waiting for the pairing lifecycle")??;
    Ok(())
}

/// Revocation can withdraw a delivered owner decision until the handshake actually commits it.
#[tokio::test]
async fn revoke_before_admission_withdraws_an_owner_decision() -> Result<()> {
    timeout(Duration::from_secs(20), async {
        let h = tempdir()?;
        let c = tempdir()?;
        let config = NetworkConfig::direct_only();
        let profile = DeviceProfile::open(h.path())?;
        let controller = DeviceProfile::open(c.path())?;
        let (host, mut events) =
            RemoteHost::start(profile.clone(), config.clone()).await?;
        let invite = host.invite().await?;
        let owner = async {
            let request = next_request(&mut events).await?;
            let id = controller.id();
            // Both owner actions run on this thread before the handshake resumes.
            let (approved, revoked) = tokio::join!(
                biased;
                host.approve(&request, true),
                host.revoke(&id),
            );
            revoked?;
            ensure!(
                approved.is_err(),
                "An uncommitted decision was reported as granted"
            );
            Ok::<_, anyhow::Error>(())
        };
        let (pair, actions) = tokio::join!(
            RemoteClient::pair(&controller, config, &invite.code),
            owner
        );
        actions?;
        ensure!(pair.is_err());
        ensure!(profile.trusted_devices().is_empty());
        ensure!(host.online().await.is_empty());
        host.shutdown().await;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

/// A second OS process cannot race a live profile and re-persist a revoked grant.
#[test]
fn profile_has_one_process_owner() -> Result<()> {
    const PROBE: &str = "CLAWCODE_TEST_REMOTE_PROFILE_LOCK";
    if let Some(path) = std::env::var_os(PROBE) {
        let error = DeviceProfile::open(Path::new(&path))
            .err()
            .context("Second process opened the identity")?;
        ensure!(error.to_string().contains("already open"));
        return Ok(());
    }
    let root = tempdir()?;
    let owner = DeviceProfile::open(root.path())?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "profile_has_one_process_owner"])
        .env(PROBE, root.path())
        .status()?;
    ensure!(status.success());
    drop(owner);
    DeviceProfile::open(root.path())?;
    Ok(())
}

/// Restarting the offline host preserves both its grant and the controller's route.
#[tokio::test]
async fn hosting_restart_preserves_loopback_reconnection() -> Result<()> {
    timeout(Duration::from_secs(30), async {
        let h = tempdir()?;
        let c = tempdir()?;
        let direct = NetworkConfig::direct_only();
        let client = DeviceProfile::open(c.path())?;
        let host_id = {
            let profile = DeviceProfile::open(h.path())?;
            let (host, mut events) =
                RemoteHost::start(profile.clone(), direct.clone()).await?;
            let invitation = host.invite().await?;
            let (pair, confirm) = tokio::join!(
                RemoteClient::pair(&client, direct.clone(), &invitation.code),
                async {
                    host.approve(&next_request(&mut events).await?, true).await
                }
            );
            confirm?;
            let (_, session) = pair?;
            host.shutdown().await;
            session.closed().await;
            profile.id()
        };
        let (host, _events) =
            RemoteHost::start(DeviceProfile::open(h.path())?, direct.clone())
                .await?;
        let (_, session) =
            RemoteClient::connect(&client, direct, &host_id).await?;
        ensure!(host.online().await.len() == 1);
        session.close();
        host.shutdown().await;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

/// Edit the public test envelope without writing or logging its bearer secret.
fn edit_code(
    code: &str,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let (prefix, payload) =
        code.split_once('.').context("invitation prefix")?;
    let mut value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    edit(&mut value);
    Ok(format!(
        "{prefix}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value)?)
    ))
}

/// Enforce authoritative host TTL and invalidate a code after three wrong submissions.
#[tokio::test]
async fn expired_invalid_and_declined_invitations_never_grant_access()
-> Result<()> {
    timeout(Duration::from_secs(20), async {
        let h = tempdir()?;
        let c = tempdir()?;
        let config = NetworkConfig::direct_only();
        let client = DeviceProfile::open(c.path())?;
        let profile = DeviceProfile::open(h.path())?;
        let (host, mut events) =
            RemoteHost::start(profile.clone(), config.clone()).await?;
        let original = host.invite().await?;
        let expired = edit_code(&original.code, |ticket| {
            ticket["expires_at"] = 0.into()
        })?;
        ensure!(
            RemoteClient::pair(&client, config.clone(), &expired)
                .await
                .is_err()
        );
        let wrong = edit_code(&original.code, |ticket| {
            ticket["secret"] = "0".repeat(32).into()
        })?;
        for attempt in 1..=3 {
            let error = RemoteClient::pair(&client, config.clone(), &wrong)
                .await
                .err()
                .context("Wrong secret was accepted")?;
            ensure!(
                error.to_string().contains("denied"),
                "attempt {attempt}: {error:#}"
            );
            ensure!(
                host.invitation_generation().await.is_some() == (attempt < 3)
            );
        }
        ensure!(
            RemoteClient::pair(&client, config.clone(), &original.code)
                .await
                .is_err()
        );
        ensure!(host.pending().await.is_empty());
        let fresh = host.invite().await?;
        let (denied, confirmed) = tokio::join!(
            RemoteClient::pair(&client, config.clone(), &fresh.code),
            async {
                host.approve(&next_request(&mut events).await?, false).await
            }
        );
        confirmed?;
        ensure!(denied.is_err());
        ensure!(
            RemoteClient::pair(&client, config, &fresh.code)
                .await
                .is_err()
        );
        ensure!(profile.trusted_devices().is_empty());
        ensure!(client.known_hosts().is_empty());
        host.shutdown().await;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

/// Exercise real TLS ALPN negotiation against a future protocol with no shared version.
#[tokio::test]
async fn incompatible_protocol_has_upgrade_guidance() -> Result<()> {
    use iroh::{Endpoint, RelayMode, endpoint::presets};
    let h = tempdir()?;
    let c = tempdir()?;
    let config = NetworkConfig::direct_only();
    let (host, _events) =
        RemoteHost::start(DeviceProfile::open(h.path())?, config.clone())
            .await?;
    let invitation = host.invite().await?;
    let future = Endpoint::builder(presets::Minimal)
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")?
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![b"clawcode/acp/2".to_vec()])
        .bind()
        .await?;
    let addr = future.addr().with_addrs(
        future
            .bound_sockets()
            .into_iter()
            .map(iroh::TransportAddr::Ip),
    );
    let ticket = edit_code(&invitation.code, |ticket| {
        ticket["addr"] = serde_json::json!(addr)
    })?;
    let client = DeviceProfile::open(c.path())?;
    let accept = async {
        if let Some(incoming) = future.accept().await {
            let _ = incoming.await;
        }
    };
    let (result, _) = timeout(Duration::from_secs(25), async {
        tokio::join!(RemoteClient::pair(&client, config, &ticket), accept)
    })
    .await?;
    let error = result.err().context("ALPN mismatch was accepted")?;
    ensure!(
        error.to_string().contains("upgrade"),
        "unexpected reason: {error}"
    );
    future.close().await;
    host.shutdown().await;
    Ok(())
}

/// Expire a pending owner dialog using Tokio's monotonic clock and reject late owner input.
#[tokio::test]
async fn late_owner_confirmation_cannot_create_a_grant() -> Result<()> {
    let h = tempdir()?;
    let c = tempdir()?;
    let config = NetworkConfig::direct_only();
    let client = DeviceProfile::open(c.path())?;
    let profile = DeviceProfile::open(h.path())?;
    let (host, mut events) =
        RemoteHost::start(profile.clone(), config.clone()).await?;
    let invite = host.invite().await?;
    let confirm = async {
        let request = next_request(&mut events).await?;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(91)).await;
        tokio::time::resume();
        host.approve(&request, true)
            .await
            .err()
            .context("Expired owner prompt granted access")
    };
    let (pair, late) = timeout(Duration::from_secs(110), async {
        tokio::join!(RemoteClient::pair(&client, config, &invite.code), confirm)
    })
    .await?;
    late?;
    ensure!(pair.is_err());
    ensure!(profile.trusted_devices().is_empty());
    ensure!(host.pending().await.is_empty());
    host.shutdown().await;
    Ok(())
}

/// An outstanding approval write cannot delay closing another peer or resurrect a revoked decision.
#[test]
fn revoke_closes_live_peer_during_unrelated_pairing_write() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()?
        .block_on(async {
            let host_root = tempdir()?;
            let first_root = tempdir()?;
            let second_root = tempdir()?;
            let profile = DeviceProfile::open(host_root.path())?;
            let first = DeviceProfile::open(first_root.path())?;
            let second = DeviceProfile::open(second_root.path())?;
            let config = NetworkConfig::direct_only();
            let (host, mut events) =
                RemoteHost::start(profile.clone(), config.clone()).await?;
            let invite = host.invite().await?;
            let (pair, decision) = tokio::join!(
                RemoteClient::pair(&first, config.clone(), &invite.code),
                async {
                    host.approve(&next_request(&mut events).await?, true).await
                },
            );
            decision?;
            let (_, first_session) = pair?;
            let (_incoming, _outgoing, first_conn) = first_session.into_parts();
            let _accepted = loop {
                if let HostEvent::Connected { session, .. } =
                    events.recv().await.context("Host stopped")?
                {
                    break session;
                }
            };
            let invite = host.invite().await?;
            let owner = async {
                let request = next_request(&mut events).await?;
                // Occupy the only blocking worker; QUIC and owner actions still run on the runtime.
                let (release, wait) = std::sync::mpsc::channel::<()>();
                let (started, ready) = tokio::sync::oneshot::channel();
                let blocker = tokio::task::spawn_blocking(move || {
                    let _ = started.send(());
                    let _ = wait.recv();
                });
                ready.await?;
                let mut approve = Box::pin(host.approve(&request, true));
                ensure!(futures::poll!(&mut approve).is_pending());
                ensure!(matches!(
                    events.recv().await,
                    Some(HostEvent::Changed)
                ));
                let first_id = first.id();
                let mut revoke = Box::pin(host.revoke(&first_id));
                ensure!(futures::poll!(&mut revoke).is_pending());
                timeout(Duration::from_secs(2), first_conn.wire_closed())
                    .await
                    .context("Revocation waited for another peer's write")?;
                // Once live revocation takes effect, dropping the caller cannot cancel the durable write.
                drop(revoke);
                let snapshot = host.snapshot().await;
                ensure!(
                    snapshot.online.is_empty() && snapshot.trusted.is_empty()
                );
                let second_id = second.id();
                let mut revoke_pair = Box::pin(host.revoke(&second_id));
                ensure!(futures::poll!(&mut revoke_pair).is_pending());
                drop(release);
                blocker.await?;
                revoke_pair.await?;
                ensure!(approve.await.is_err(), "Revoked approval succeeded");
                Ok::<_, anyhow::Error>(())
            };
            let (second_pair, actions) = tokio::join!(
                RemoteClient::pair(&second, config, &invite.code),
                owner
            );
            actions?;
            ensure!(second_pair.is_err());
            ensure!(host.online().await.is_empty());
            host.shutdown().await;
            drop(host);
            drop(profile);
            ensure!(
                DeviceProfile::open(host_root.path())?
                    .trusted_devices()
                    .is_empty()
            );
            Ok(())
        })
}
