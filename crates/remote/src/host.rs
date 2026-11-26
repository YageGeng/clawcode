use std::{
    collections::BTreeMap,
    sync::{Arc, Weak, atomic::Ordering},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use iroh::endpoint::Connection;
use tokio::{
    sync::{Mutex, Notify, mpsc, oneshot},
    task::JoinSet,
    time::{Instant, sleep_until, timeout},
};
use tokio_util::{
    sync::{CancellationToken, DropGuard},
    task::TaskTracker,
};
use typed_builder::TypedBuilder;

use crate::{
    DeviceProfile, Invitation, NetworkConfig, Peer, RemoteSession,
    network::SharedEndpoint,
    pairing::{ALPN, Challenge, Hello},
    profile::KnownHost,
    session::{DENIED, DISCONNECTED, REVOKED, STOPPED, Wire},
};

/// Only locally approved, TLS-authenticated sessions reach the application.
pub enum HostEvent {
    Approval { request_id: String, peer: Peer },
    Connected { peer: Peer, session: RemoteSession },
    Changed,
}

struct Approval {
    peer: Peer,
    deadline: Instant,
    decision: Option<oneshot::Sender<OwnerDecision>>,
}

struct OwnerDecision {
    allow: bool,
    completed: oneshot::Sender<Result<()>>,
}

/// Only admissions and revocations for the same authenticated identity must wait for each other.
#[derive(Default)]
struct DeviceAdmission {
    turn: Mutex<()>,
    revoked: CancellationToken,
}

#[derive(TypedBuilder)]
#[builder(field_defaults(default))]
struct Access {
    challenge: Option<Challenge>,
    approvals: BTreeMap<String, Approval>,
    online: BTreeMap<String, (Peer, Connection)>,
    admissions: BTreeMap<String, Weak<DeviceAdmission>>,
}

impl Access {
    /// Return the public generation only while the host accepts the outstanding invite.
    fn invitation_generation(
        &self,
        stop: &CancellationToken,
    ) -> Option<String> {
        self.challenge
            .as_ref()
            .filter(|challenge| challenge.active() && !stop.is_cancelled())
            .map(|challenge| challenge.generation.clone())
    }

    /// Collect unanswered owner decisions that still have time to complete.
    fn pending(&self) -> Vec<(String, Peer)> {
        self.approvals
            .iter()
            .filter(|(_, entry)| {
                entry.decision.is_some() && Instant::now() < entry.deadline
            })
            .map(|(id, entry)| (id.clone(), entry.peer.clone()))
            .collect()
    }

    /// List authenticated controllers whose transport is still open.
    fn online(&self) -> Vec<Peer> {
        self.online
            .values()
            .filter(|(_, conn)| conn.close_reason().is_none())
            .map(|(peer, _)| peer.clone())
            .collect()
    }

    /// Share an admission gate only while operations still hold it, including any revocation.
    fn admission(&mut self, id: &str) -> Arc<DeviceAdmission> {
        if let Some(gate) = self.admissions.get(id).and_then(Weak::upgrade) {
            return gate;
        }
        // The map is bounded by concurrent handshakes and owner actions, not historical identities.
        self.admissions.retain(|_, gate| gate.strong_count() > 0);
        let gate = Arc::new(DeviceAdmission::default());
        self.admissions.insert(id.to_owned(), Arc::downgrade(&gate));
        gate
    }
}

#[derive(TypedBuilder)]
struct Listener {
    profile: DeviceProfile,
    network: Weak<SharedEndpoint>,
    state: Mutex<Access>,
    invitation_updated: Notify,
    revocations: TaskTracker,
    events: mpsc::WeakSender<HostEvent>,
    stop: CancellationToken,
    stopped: CancellationToken,
}

/// A point-in-time view of owner controls and authenticated controllers.
#[derive(TypedBuilder)]
pub struct HostSnapshot {
    #[builder(default)]
    pub invitation_generation: Option<String>,
    pub pending: Vec<(String, Peer)>,
    pub online: Vec<Peer>,
    pub trusted: Vec<Peer>,
}

/// A running opt-in host; dropping the last owner also disables access.
#[derive(Clone)]
pub struct RemoteHost {
    listener: Arc<Listener>,
    _owner: Arc<DropGuard>,
}

impl RemoteHost {
    /// Enable the ACP ALPN and start accepting authenticated handshakes for this installation.
    pub async fn start(
        profile: DeviceProfile,
        config: NetworkConfig,
    ) -> Result<(Self, mpsc::Receiver<HostEvent>)> {
        let network = profile.endpoint(config, true).await?;
        ensure!(
            !network.hosting.swap(true, Ordering::AcqRel),
            "Remote hosting is already enabled"
        );
        network.endpoint.set_alpns(vec![ALPN.to_vec()]);
        let (events, receiver) = mpsc::channel(32);
        let stop = CancellationToken::new();
        let owner = Arc::new(stop.clone().drop_guard());
        let listener = Arc::new(
            Listener::builder()
                .profile(profile)
                .network(Arc::downgrade(&network))
                .state(Mutex::new(Access::builder().build()))
                .invitation_updated(Notify::new())
                .revocations(TaskTracker::new())
                .events(events.downgrade())
                .stop(stop)
                .stopped(CancellationToken::new())
                .build(),
        );
        tracing::info!(
            "Enabled remote hosting for {}",
            network.endpoint.id().fmt_short()
        );
        tokio::spawn(Arc::clone(&listener).run(network, events));
        Ok((
            Self {
                listener,
                _owner: owner,
            },
            receiver,
        ))
    }

    /// Open one five-minute invitation, replacing any unclaimed prior code.
    pub async fn invite(&self) -> Result<Invitation> {
        let network = self
            .listener
            .network
            .upgrade()
            .context("Remote hosting stopped")?;
        let addr = tokio::select! {
            _ = self.listener.stop.cancelled() => anyhow::bail!("Remote hosting stopped"),
            addr = network.address() => addr,
        };
        let mut state = self.listener.state.lock().await;
        ensure!(!self.listener.stop.is_cancelled(), "Remote hosting stopped");
        let (challenge, invitation) = Challenge::issue(addr)?;
        state.challenge = Some(challenge);
        self.listener.invitation_updated.notify_one();
        self.listener.changed();
        tracing::info!("Created remote pairing invitation");
        Ok(invitation)
    }

    /// Return the public generation only while the matching bearer code can still be claimed.
    pub async fn invitation_generation(&self) -> Option<String> {
        self.listener
            .state
            .lock()
            .await
            .invitation_generation(&self.listener.stop)
    }

    /// Deliver the owner's decision once and wait for the live handshake to report its outcome.
    pub async fn approve(&self, request_id: &str, allow: bool) -> Result<()> {
        let (completed, result) = oneshot::channel();
        {
            let mut state = self.listener.state.lock().await;
            ensure!(
                !self.listener.stop.is_cancelled(),
                "Remote hosting stopped"
            );
            let approval = state
                .approvals
                .get_mut(request_id)
                .context("Pairing request is no longer pending")?;
            ensure!(
                Instant::now() < approval.deadline,
                "Pairing request has expired"
            );
            // Keep the entry revocable until the handshake takes the admission lock.
            approval
                .decision
                .take()
                .context("Pairing request already answered")?
                .send(OwnerDecision { allow, completed })
                .map_err(|_undelivered| {
                    anyhow::anyhow!("Pairing request has ended")
                })?;
        }
        tracing::info!(
            "Owner {} remote pairing request {}",
            if allow { "approved" } else { "denied" },
            request_id
        );
        result.await.context("Pairing request has ended")?
    }

    /// Get outstanding pairing prompts for a refreshed settings panel.
    pub async fn pending(&self) -> Vec<(String, Peer)> {
        self.listener.state.lock().await.pending()
    }

    /// List unique controllers with an authenticated open ACP connection.
    pub async fn online(&self) -> Vec<Peer> {
        self.listener.state.lock().await.online()
    }

    /// Read the invitation, prompts and published sessions under one admission-state lock.
    pub async fn snapshot(&self) -> HostSnapshot {
        let state = self.listener.state.lock().await;
        let mut trusted = self.listener.profile.trusted_devices();
        // Live access ends as soon as the owner revokes, even before a pending disk write finishes.
        trusted.retain(|peer| {
            !state
                .admissions
                .get(&peer.id)
                .and_then(Weak::upgrade)
                .is_some_and(|gate| gate.revoked.is_cancelled())
        });
        HostSnapshot::builder()
            .invitation_generation(
                state.invitation_generation(&self.listener.stop),
            )
            .pending(state.pending())
            .online(state.online())
            .trusted(trusted)
            .build()
    }

    /// Close this controller's connection while preserving the grant for later reconnection.
    pub async fn disconnect(&self, id: &str) -> Result<()> {
        if let Some((_, conn)) =
            self.listener.state.lock().await.online.remove(id)
        {
            tracing::info!(
                "Disconnecting remote controller {}",
                conn.remote_id().fmt_short()
            );
            conn.close(DISCONNECTED.into(), b"");
        }
        self.listener.changed();
        Ok(())
    }

    /// Close the live connection immediately and persist revocation after same-device work drains.
    pub async fn revoke(&self, id: &str) -> Result<()> {
        let mut state = self.listener.state.lock().await;
        ensure!(!self.listener.stop.is_cancelled(), "Remote hosting stopped");
        let gate = state.admission(id);
        gate.revoked.cancel();
        state.approvals.retain(|_, approval| approval.peer.id != id);
        if let Some((_, conn)) = state.online.remove(id) {
            tracing::info!(
                "Revoked access for remote controller {}",
                conn.remote_id().fmt_short()
            );
            conn.close(REVOKED.into(), b"");
        }
        self.listener.changed();
        let id = id.to_owned();
        let listener = Arc::clone(&self.listener);
        // Once the live connection closes, finish the durable removal even if the UI goes away.
        let task = self.listener.revocations.spawn(async move {
            let _turn = gate.turn.lock().await;
            let profile = listener.profile.clone();
            let result =
                tokio::task::spawn_blocking(move || profile.revoke(&id))
                    .await?;
            listener.changed();
            result
        });
        drop(state);
        task.await?
    }

    /// Disable host admission, closing all inbound handshakes and authenticated sessions.
    pub async fn shutdown(&self) {
        self.listener.stop.cancel();
        self.listener.stopped.cancelled().await;
    }
}

impl Listener {
    /// Notify the bridge to refresh its snapshot; repeated state changes can be coalesced.
    fn changed(&self) {
        if let Some(events) = self.events.upgrade() {
            let _ = events.try_send(HostEvent::Changed);
        }
    }

    /// Own every inbound task so quitting the listener tears down incomplete handshakes as well.
    async fn run(
        self: Arc<Self>,
        network: Arc<SharedEndpoint>,
        events: mpsc::Sender<HostEvent>,
    ) {
        let mut tasks = JoinSet::new();
        loop {
            let deadline = self
                .state
                .lock()
                .await
                .challenge
                .as_ref()
                .map(|challenge| challenge.deadline);
            tokio::select! {
                biased;
                _ = self.stop.cancelled() => break,
                _ = events.closed() => break,
                _ = self.invitation_updated.notified() => continue,
                _ = async {
                    match deadline {
                        Some(deadline) => sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let mut state = self.state.lock().await;
                    // Replacement and timer expiry may become ready on the same turn.
                    if state.challenge.as_ref().is_some_and(|challenge| !challenge.active()) {
                        state.challenge = None;
                        self.changed();
                    }
                }
                Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Err(error) = result { tracing::warn!("Remote host connection task ended: {error}"); }
                }
                incoming = network.endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    if tasks.len() >= 64 { incoming.refuse(); continue; }
                    let listener = Arc::clone(&self);
                    tasks.spawn(async move {
                        let accepted = tokio::select! {
                            _ = listener.stop.cancelled() => return,
                            result = timeout(Duration::from_secs(10), incoming.into_future()) => result,
                        };
                        if let Ok(Ok(connection)) = accepted {
                            let id = connection.remote_id();
                            if let Err(error) = listener.serve(&connection).await {
                                tracing::debug!("Remote connection {} refused or ended: {error}", id.fmt_short());
                                connection.close(DENIED.into(), b"");
                            }
                        }
                    });
                }
            }
        }
        self.stop.cancel();
        network.endpoint.set_alpns(vec![]);
        {
            let mut state = self.state.lock().await;
            state.challenge = None;
            state.approvals.clear();
            for (_, (_, conn)) in std::mem::take(&mut state.online) {
                conn.close(STOPPED.into(), b"");
            }
        }
        // Let pairing workers complete any pending rollback before this profile can host again.
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(
                    "Remote host connection task ended during shutdown: {error}"
                );
            }
        }
        self.revocations.close();
        self.revocations.wait().await;
        network.hosting.store(false, Ordering::Release);
        let id = network.endpoint.id();
        network.release().await;
        self.stopped.cancel();
        tracing::info!("Disabled remote hosting for {}", id.fmt_short());
    }

    /// Claim the code before displaying the owner's prompt and cancel it when the peer leaves.
    async fn confirm(
        &self,
        conn: &Connection,
        secret: &str,
        peer: Peer,
    ) -> Result<(String, OwnerDecision)> {
        let (tx, rx) = oneshot::channel();
        let (id, deadline) = {
            let mut state = self.state.lock().await;
            let challenge = state
                .challenge
                .as_mut()
                .context("No active pairing invitation")?;
            let claim = challenge.claim(secret);
            if !challenge.active() {
                // Withdrawal is observable even if the final attempt was unsuccessful.
                state.challenge = None;
                self.changed();
            }
            let expiry = claim?;
            let deadline = expiry.min(Instant::now() + Duration::from_secs(90));
            // Each owner prompt gets an opaque, fresh identifier across host restarts.
            let mut nonce = [0u8; 16];
            getrandom::fill(&mut nonce)
                .context("Could not generate pairing request identifier")?;
            let id = URL_SAFE_NO_PAD.encode(nonce);
            state.approvals.insert(
                id.clone(),
                Approval {
                    peer: peer.clone(),
                    deadline,
                    decision: Some(tx),
                },
            );
            (id, deadline)
        };
        tracing::info!(
            "Awaiting owner approval for {}",
            conn.remote_id().fmt_short()
        );
        let decision = async {
            self.events
                .upgrade()
                .context("Local host interface closed")?
                .send(HostEvent::Approval {
                    request_id: id.clone(),
                    peer,
                })
                .await
                .context("Local host interface closed")?;
            rx.await.context("Pairing request withdrawn")
        };
        let decision = tokio::select! {
            biased;
            _ = self.stop.cancelled() => Err(anyhow::anyhow!("Remote hosting stopped")),
            _ = conn.closed() => Err(anyhow::anyhow!("Pairing controller disconnected")),
            _ = sleep_until(deadline) => Err(anyhow::anyhow!("Pairing request has expired")),
            result = decision => result,
        };
        if decision.is_err() {
            self.state.lock().await.approvals.remove(&id);
            self.changed();
        }
        Ok((id, decision?))
    }

    /// Authenticate before sending the welcome message or handing any frames to the ACP server.
    async fn serve(&self, conn: &Connection) -> Result<()> {
        let hello = timeout(Duration::from_secs(10), async {
            let mut wire =
                Wire::from(conn.accept_bi().await.context(
                    "Peer closed before opening its handshake stream",
                )?);
            let hello = wire.read::<Hello>().await?;
            Ok::<_, anyhow::Error>((wire, hello))
        });
        let (mut wire, hello) = tokio::select! {
            _ = self.stop.cancelled() => anyhow::bail!("Remote hosting stopped"),
            result = hello => result.context("Remote handshake timed out")??,
        };
        let id = conn.remote_id();
        let welcome = {
            let network =
                self.network.upgrade().context("Remote hosting stopped")?;
            KnownHost {
                addr: network.config.contact_hints(network.endpoint.addr()),
                name: self.profile.name(),
            }
        };
        let decision = match hello {
            Hello::Pair { secret, name } => {
                Some(self.confirm(conn, &secret, Peer::new(id, &name)?).await?)
            }
            Hello::Connect => None,
        };
        let peer = {
            let mut state = self.state.lock().await;
            let gate = state.admission(&id.to_string());
            let pairing = if let Some((request, decision)) = decision {
                let approval = state
                    .approvals
                    .remove(&request)
                    .context("Pairing request withdrawn")?;
                self.changed();
                if !decision.allow {
                    let _ = decision.completed.send(Ok(()));
                    anyhow::bail!("Pairing request declined");
                }
                Some((approval, decision.completed))
            } else {
                None
            };
            drop(state);
            let deadline = pairing.as_ref().map_or_else(
                || Instant::now() + Duration::from_secs(10),
                |(approval, _)| approval.deadline,
            );
            let _turn = tokio::select! {
                biased;
                _ = gate.revoked.cancelled() => anyhow::bail!("Device access revoked"),
                _ = self.stop.cancelled() => anyhow::bail!("Remote hosting stopped"),
                _ = conn.closed() => anyhow::bail!("Remote controller disconnected"),
                _ = sleep_until(deadline) => anyhow::bail!("Remote handshake timed out"),
                turn = gate.turn.lock() => turn,
            };
            let handshake = Admission {
                listener: self,
                conn,
                revoked: &gate.revoked,
            };
            if let Some((approval, owner)) = pairing {
                let result =
                    handshake.pair(&mut wire, &welcome, approval).await;
                let _ = owner.send(
                    result
                        .as_ref()
                        .map(|_| ())
                        .map_err(|e| anyhow::anyhow!("{e}")),
                );
                result?
            } else {
                let peer = self
                    .profile
                    .trusted(id)
                    .context("Device has no access grant")?;
                handshake
                    .welcome(&mut wire, &welcome, peer, deadline)
                    .await?
            }
        };
        let result = async {
            let session = wire.session(None, conn.clone());
            let events = self.events.upgrade().context("Local interface closed")?;
            tokio::select! {
                biased;
                _ = self.stop.cancelled() => anyhow::bail!("Remote hosting stopped"),
                _ = conn.closed() => anyhow::bail!("Remote connection closed"),
                result = events.send(HostEvent::Connected { peer, session }) => result.context("Local interface closed")?,
            }
            tracing::info!(
                "Authenticated remote controller {}",
                id.fmt_short()
            );
            conn.closed().await;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let mut state = self.state.lock().await;
        // An older session must never evict the replacement which reconnected.
        if state
            .online
            .get(&id.to_string())
            .is_some_and(|(_, active)| active.stable_id() == conn.stable_id())
        {
            state.online.remove(&id.to_string());
        }
        self.changed();
        tracing::info!("Remote controller {} disconnected", id.fmt_short());
        result
    }
}

/// Bounds the handshake by owner revocation, transport liveness and its original deadline.
struct Admission<'a> {
    listener: &'a Listener,
    conn: &'a Connection,
    revoked: &'a CancellationToken,
}

impl Admission<'_> {
    /// Commit a grant while its peer holds the admission turn, undoing it on a failed welcome.
    async fn pair(
        &self,
        wire: &mut Wire,
        welcome: &KnownHost,
        approval: Approval,
    ) -> Result<Peer> {
        let deadline = approval.deadline;
        let profile = self.listener.profile.clone();
        let previous = profile.trusted(self.conn.remote_id());
        let conn = self.conn.clone();
        let stop = self.listener.stop.clone();
        let revoked = self.revoked.clone();
        let peer = approval.peer.clone();
        tokio::task::spawn_blocking(move || {
            profile.trust_while(peer, || {
                ensure!(!stop.is_cancelled(), "Remote hosting stopped");
                ensure!(!revoked.is_cancelled(), "Device access revoked");
                ensure!(
                    conn.close_reason().is_none(),
                    "Pairing controller disconnected"
                );
                ensure!(
                    Instant::now() < deadline,
                    "Pairing request has expired"
                );
                Ok(())
            })
        })
        .await??;
        let id = approval.peer.id.clone();
        let result = self.welcome(wire, welcome, approval.peer, deadline).await;
        if let Err(error) = &result {
            tracing::warn!(
                "Rolling back remote pairing for {} after welcome failed: {error}",
                self.conn.remote_id().fmt_short()
            );
            let profile = self.listener.profile.clone();
            // Same-device admissions and revocations wait until the previous grant has been restored.
            tokio::task::spawn_blocking(move || match previous {
                Some(peer) => profile.trust_while(peer, || Ok(())),
                None => profile.revoke(&id),
            })
            .await??;
        }
        result
    }

    /// Send the welcome outside shared state, then publish the connection if consent is still live.
    async fn welcome(
        &self,
        wire: &mut Wire,
        welcome: &KnownHost,
        peer: Peer,
        deadline: Instant,
    ) -> Result<Peer> {
        tokio::select! {
            biased;
            _ = self.revoked.cancelled() => anyhow::bail!("Device access revoked"),
            _ = self.listener.stop.cancelled() => anyhow::bail!("Remote hosting stopped"),
            _ = self.conn.closed() => anyhow::bail!("Remote controller disconnected"),
            _ = sleep_until(deadline) => anyhow::bail!("Remote welcome timed out"),
            result = wire.write(welcome) => result?,
        }
        let mut state = self.listener.state.lock().await;
        // Revoke or shutdown may win between the network write and publication.
        ensure!(!self.revoked.is_cancelled(), "Device access revoked");
        ensure!(!self.listener.stop.is_cancelled(), "Remote hosting stopped");
        ensure!(
            self.conn.close_reason().is_none(),
            "Remote controller disconnected"
        );
        ensure!(Instant::now() < deadline, "Remote welcome timed out");
        if let Some((_, old)) = state
            .online
            .insert(peer.id.clone(), (peer.clone(), self.conn.clone()))
        {
            old.close(DISCONNECTED.into(), b"");
        }
        Ok(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RemoteClient;
    use tempfile::tempdir;

    /// Stopping local sharing must drain iroh before returning even when nobody connected.
    #[tokio::test]
    async fn stopping_empty_host_drains_its_endpoint() -> Result<()> {
        let root = tempdir()?;
        let profile = DeviceProfile::open(root.path())?;
        let config = NetworkConfig::direct_only();
        let (host, _events) =
            RemoteHost::start(profile.clone(), config.clone()).await?;
        let observed = profile.endpoint(config, false).await?.endpoint.clone();
        host.shutdown().await;
        ensure!(
            observed.is_closed(),
            "Sharing stopped before iroh endpoint shutdown"
        );
        Ok(())
    }

    /// Stopping one of two simultaneous roles preserves the controller until its own shutdown.
    #[tokio::test]
    async fn controller_closure_drains_after_host_role_has_stopped()
    -> Result<()> {
        let local_root = tempdir()?;
        let remote_root = tempdir()?;
        let local = DeviceProfile::open(local_root.path())?;
        let config = NetworkConfig::direct_only();
        let (local_host, _local_events) =
            RemoteHost::start(local.clone(), config.clone()).await?;
        let (other, mut events) = RemoteHost::start(
            DeviceProfile::open(remote_root.path())?,
            config.clone(),
        )
        .await?;
        let invitation = other.invite().await?;
        let confirm = async {
            while let Some(event) = events.recv().await {
                if let HostEvent::Approval { request_id, .. } = event {
                    return other.approve(&request_id, true).await;
                }
            }
            anyhow::bail!("No confirmation request")
        };
        let (pair, approved) = tokio::join!(
            RemoteClient::pair(&local, config.clone(), &invitation.code),
            confirm
        );
        approved?;
        let (_, session) = pair?;
        let observed = local.endpoint(config, false).await?.endpoint.clone();
        local_host.shutdown().await;
        ensure!(
            !observed.is_closed(),
            "Stopping the host interrupted an outbound session"
        );
        session.close();
        session.closed().await;
        ensure!(
            observed.is_closed(),
            "Controller closed before draining its endpoint"
        );
        other.shutdown().await;
        Ok(())
    }
}
