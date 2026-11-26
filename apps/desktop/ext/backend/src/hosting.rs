//! Local-only identity, human approval and authenticated ACP hosting for the desktop.

use std::io;
use std::path::Path;
use std::sync::{Arc, Weak};

use acp::AcpServerFactory;
use anyhow::{Result, anyhow, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures::{sink, stream};
use nsh_core::prelude::*;
use qrcode::{QrCode, render::svg};
use remote::{
    DeviceProfile, HostEvent, NetworkConfig, Peer, RemoteHost, RemoteSession,
};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

use crate::Backend;
use crate::runtime::{NETWORK, finish_task};

/// An authenticated peer shown in the desktop settings or host switcher.
#[derive(Clone, Debug, JsObject)]
pub struct RemoteDevice {
    /// Public endpoint identity.
    pub id: String,
    /// Display name supplied by the device.
    pub name: String,
}

impl From<Peer> for RemoteDevice {
    /// Projects a native peer record onto the public NSH boundary.
    fn from(peer: Peer) -> Self {
        Self {
            id: peer.id,
            name: peer.name,
        }
    }
}

/// A one-time request awaiting the host user's explicit decision.
#[derive(Clone, Debug, JsObject)]
pub struct PairingRequest {
    /// Identifies this admission attempt, even when the same peer tries again.
    pub request_id: String,
    /// The public identity authenticated by the transport.
    pub id: String,
    /// The device name to display alongside the access warning.
    pub name: String,
}

/// The public state of this installation, including inbound and outgoing relationships.
#[derive(Clone, Debug, JsObject, TypedBuilder)]
pub struct HostingSnapshot {
    /// This installation's public endpoint identity.
    pub id: String,
    /// This installation's current device name.
    pub name: String,
    /// Whether the local backend is accepting pairing and trusted connections.
    pub enabled: bool,
    /// Public identifier of the invitation that can currently be claimed.
    #[builder(default)]
    pub invitation_generation: Option<String>,
    /// Whether this installation should start listening when its backend next starts.
    pub sharing_preference: bool,
    /// Devices permitted to connect to this installation.
    pub trusted: Vec<RemoteDevice>,
    /// Devices currently controlling this installation.
    pub online: Vec<RemoteDevice>,
    /// Pairing requests that still need the local user's decision.
    pub pending: Vec<PairingRequest>,
    /// Remote hosts this installation can reconnect to.
    pub known_hosts: Vec<RemoteDevice>,
}

/// A copyable invitation and the same payload rendered for a camera.
#[derive(Clone, Debug, JsObject, TypedBuilder)]
pub struct PairingInvitation {
    /// Public identifier used to match this secret against the host's current snapshot.
    pub generation: String,
    /// Opaque single-use pairing code.
    pub code: String,
    /// Expiration in seconds since the Unix epoch.
    pub expires_at: u64,
    /// SVG image URI encoding the exact invitation text.
    pub qr_uri: String,
}

/// Attaches hosting to the live ACP factory without extending the kernel lifetime after shutdown.
#[derive(TypedBuilder)]
pub(super) struct HostBackend {
    factory: Weak<AcpServerFactory>,
    runtime: tokio::runtime::Handle,
    attached: Mutex<Option<Weak<HostingState>>>,
    stopped: CancellationToken,
}

impl HostBackend {
    /// Captures the same application factory used to serve the local UI.
    pub(super) fn new(factory: &Arc<AcpServerFactory>) -> Self {
        Self::builder()
            .factory(Arc::downgrade(factory))
            .runtime(tokio::runtime::Handle::current())
            .attached(Mutex::new(None))
            .stopped(CancellationToken::new())
            .build()
    }

    /// Serializes listener registration with local application shutdown.
    async fn start(
        &self,
        state: Arc<HostingState>,
        config: NetworkConfig,
    ) -> Result<()> {
        let mut attached = self.attached.lock().await;
        ensure!(
            !self.stopped.is_cancelled(),
            "the local backend has stopped"
        );
        let factory = self
            .factory
            .upgrade()
            .ok_or_else(|| anyhow!("the local backend has stopped"))?;
        if let Some(other) = attached.as_ref().and_then(Weak::upgrade)
            && !Arc::ptr_eq(&other, &state)
        {
            ensure!(
                other.active.lock().await.is_none(),
                "the local backend is already hosting"
            );
        }
        state.start(factory, &self.runtime, config).await?;
        *attached = Some(Arc::downgrade(&state));
        Ok(())
    }

    /// Stops admissions and releases authenticated connections before kernel teardown.
    pub(super) async fn shutdown(&self) {
        self.stopped.cancel();
        if let Some(state) = self
            .attached
            .lock()
            .await
            .take()
            .and_then(|weak| weak.upgrade())
        {
            state.stop(Some(&self.factory)).await;
        }
    }
}

/// The listener and its authenticated ACP tasks for one generation of the local backend.
#[derive(TypedBuilder)]
struct HostBinding {
    factory: Weak<AcpServerFactory>,
    host: RemoteHost,
    stop: CancellationToken,
    worker: JoinHandle<()>,
}

impl HostBinding {
    /// Starts one remote ACP session per authenticated connection and drains them on shutdown.
    async fn run(
        factory: Arc<AcpServerFactory>,
        mut events: mpsc::Receiver<HostEvent>,
        changes: watch::Sender<()>,
        stop: CancellationToken,
    ) {
        let mut sessions = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                    if let Err(error) = result {
                        tracing::warn!("desktop remote ACP task ended unexpectedly: {}", error);
                    }
                }
                event = events.recv() => {
                    let Some(event) = event else { break };
                    if let HostEvent::Connected { session, .. } = event {
                        sessions.spawn(Self::serve(Arc::clone(&factory), session));
                    }
                    // The network layer already updated its admission/online lists before dispatch.
                    changes.send_replace(());
                }
            }
        }
        // Closing the host wakes each session. Wait until the shared factory is released.
        while let Some(result) = sessions.join_next().await {
            if let Err(error) = result {
                tracing::warn!(
                    "desktop remote ACP task stopped unexpectedly: {}",
                    error
                );
            }
        }
    }

    /// Adapts bounded transport queues to the ACP line interfaces, preserving backpressure.
    async fn serve(factory: Arc<AcpServerFactory>, session: RemoteSession) {
        let (incoming, outgoing, connection) = session.into_parts();
        let incoming = stream::unfold(incoming, |mut rx| async move {
            rx.recv().await.map(|frame| (Ok::<_, io::Error>(frame), rx))
        });
        let outgoing = sink::unfold(outgoing, |tx, frame| async move {
            tx.send(frame).await.map_err(|_undelivered_frame| {
                io::Error::other("remote peer disconnected")
            })?;
            Ok::<_, io::Error>(tx)
        });
        // An ACP handler may be awaiting a reply without polling the input stream.
        // Drop the entire driver as soon as QUIC closes, including host-side revocation.
        tokio::select! {
            biased;
            () = connection.wire_closed() => {},
            result = factory.serve_remote_lines(outgoing, incoming) => {
                if result.is_err() {
                    // ACP diagnostic strings may contain peer-controlled frame text.
                    tracing::warn!("desktop remote ACP connection ended with a protocol error");
                }
            }
        }
        connection.close();
    }
}

/// Coordinates the local profile and at most one enabled inbound listener.
#[derive(TypedBuilder)]
pub(super) struct HostingState {
    pub(super) profile: DeviceProfile,
    pub(super) changes: watch::Sender<()>,
    pub(super) closed: CancellationToken,
    active: Mutex<Option<HostBinding>>,
}

impl HostingState {
    /// Binds only after the local UI has explicitly enabled remote control on a running backend.
    async fn start(
        &self,
        factory: Arc<AcpServerFactory>,
        kernel_runtime: &tokio::runtime::Handle,
        config: NetworkConfig,
    ) -> Result<()> {
        let mut active = self.active.lock().await;
        ensure!(!self.closed.is_cancelled(), "remote controls are closed");
        ensure!(active.is_none(), "remote hosting is already enabled");
        let (host, events) =
            RemoteHost::start(self.profile.clone(), config).await?;
        let stop = CancellationToken::new();
        let owner = Arc::downgrade(&factory);
        // Keep endpoint ownership on the network executor. Run every ACP handler
        // and the kernel work it spawns on the local backend's generation.
        let worker = kernel_runtime.spawn(HostBinding::run(
            factory,
            events,
            self.changes.clone(),
            stop.clone(),
        ));
        *active = Some(
            HostBinding::builder()
                .factory(owner)
                .host(host)
                .stop(stop)
                .worker(worker)
                .build(),
        );
        self.changes.send_replace(());
        Ok(())
    }

    /// Ends the current listener, or just the generation owned by the backend being stopped.
    async fn stop(&self, owner: Option<&Weak<AcpServerFactory>>) {
        let mut active = self.active.lock().await;
        if let Some(owner) = owner
            && active
                .as_ref()
                .is_some_and(|binding| !owner.ptr_eq(&binding.factory))
        {
            return;
        }
        if let Some(binding) = active.take() {
            binding.stop.cancel();
            binding.host.shutdown().await;
            if let Err(error) = binding.worker.await {
                tracing::warn!(
                    "failed to drain desktop hosting task: {}",
                    error
                );
            }
            self.changes.send_replace(());
        }
    }

    /// Returns a live listener for operations requiring host-side authorization.
    async fn host(&self) -> Result<RemoteHost> {
        self.active
            .lock()
            .await
            .as_ref()
            .map(|binding| binding.host.clone())
            .ok_or_else(|| anyhow!("remote hosting is disabled"))
    }

    /// Borrow a live listener with the stop token that identifies its generation.
    async fn listener(&self) -> Option<(RemoteHost, CancellationToken)> {
        self.active
            .lock()
            .await
            .as_ref()
            .map(|binding| (binding.host.clone(), binding.stop.clone()))
    }

    /// Revoke on the current listener and reconcile any generation that changed during persistence.
    async fn revoke(&self, id: String) -> Result<()> {
        let mut live = self.listener().await;
        loop {
            if let Some((host, stopped)) = live {
                let result = host.revoke(&id).await;
                if !stopped.is_cancelled() {
                    return result;
                }
                // Stop cancels this token before draining. A retired listener may
                // reject a late revoke, and its replacement may already have admitted the peer.
                live = self.listener().await;
            } else {
                let profile = self.profile.clone();
                let target = id.clone();
                let result = tokio::task::spawn_blocking(move || {
                    profile.revoke(&target)
                })
                .await?;
                live = self.listener().await;
                if live.is_none() {
                    return result;
                }
                // A concurrent start may have accepted the old grant before the disk job ran.
            }
        }
    }

    /// Read one listener generation without holding the registration mutex across its admission reads.
    async fn snapshot(&self) -> HostingSnapshot {
        let snapshot = loop {
            let Some((host, stopped)) = self.listener().await else {
                break None;
            };
            let snapshot = host.snapshot().await;
            if !stopped.is_cancelled() {
                break Some(snapshot);
            }
        };
        let enabled = snapshot.is_some();
        let (invitation_generation, trusted, online, pending) = match snapshot {
            Some(snapshot) => (
                snapshot.invitation_generation,
                snapshot.trusted,
                snapshot.online,
                snapshot.pending,
            ),
            None => {
                (None, self.profile.trusted_devices(), Vec::new(), Vec::new())
            }
        };
        HostingSnapshot::builder()
            .id(self.profile.id())
            .name(self.profile.name())
            .enabled(enabled)
            .invitation_generation(invitation_generation)
            .sharing_preference(self.profile.hosting_enabled())
            .trusted(trusted.into_iter().map(RemoteDevice::from).collect())
            .known_hosts(
                self.profile
                    .known_hosts()
                    .into_iter()
                    .map(RemoteDevice::from)
                    .collect(),
            )
            .online(online.into_iter().map(RemoteDevice::from).collect())
            .pending(
                pending
                    .into_iter()
                    .map(|(request_id, peer)| PairingRequest {
                        request_id,
                        id: peer.id,
                        name: peer.name,
                    })
                    .collect(),
            )
            .build()
    }
}

/// Local installation settings and pairing decisions, kept outside the ACP protocol.
#[class]
pub struct RemoteHosting {
    state: Arc<HostingState>,
}

impl RemoteHosting {
    /// Borrows shared state between engine calls without taking an NSH instance across threads.
    pub(super) fn state(&self) -> Arc<HostingState> {
        Arc::clone(&self.state)
    }
}

#[class]
impl RemoteHosting {
    /// Reserve an outgoing pairing attempt scoped to one dialog and this window's lifetime.
    pub fn begin_pairing(&self) -> crate::RemotePairing {
        self.state().into()
    }

    /// Opens the app's own data directory and initializes its identity without opening a listener.
    pub async fn open(data_dir: String) -> Result<RemoteHosting, String> {
        finish_task(
            "profile open",
            blocking(move || {
                ensure!(
                    Path::new(&data_dir).is_absolute(),
                    "installation data directory must be absolute"
                );
                let profile = DeviceProfile::open(Path::new(&data_dir))?;
                let state = HostingState::builder()
                    .profile(profile)
                    .active(Mutex::new(None))
                    .changes(watch::Sender::new(()))
                    .closed(CancellationToken::new())
                    .build();
                Ok(Self {
                    state: Arc::new(state),
                })
            }),
        )
        .await
    }

    /// Returns the public identity, incoming devices, pending requests and saved outgoing hosts.
    pub async fn snapshot(
        this: Instance<Self>,
    ) -> Result<HostingSnapshot, String> {
        let state = this.with(Self::state);
        Ok(state.snapshot().await)
    }

    /// Reports the current snapshot and subsequent changes until the local controls close.
    pub async fn watch(
        this: Instance<Self>,
        #[args(snapshot)] listener: Callback<HostingSnapshot>,
    ) -> Result<(), String> {
        let state = this.with(Self::state);
        let mut updates = state.changes.subscribe();
        loop {
            updates.borrow_and_update();
            if state.closed.is_cancelled() {
                break;
            }
            listener.post(state.snapshot().await);
            tokio::select! {
                () = state.closed.cancelled() => break,
                changed = updates.changed() => if changed.is_err() { break; },
            }
        }
        Ok(())
    }

    /// Enables inbound connections against the supplied local backend.
    pub async fn start(
        this: Instance<Self>,
        backend: Instance<Backend>,
        relay: Option<String>,
    ) -> Result<(), String> {
        let state = this.with(Self::state);
        let backend =
            backend.with(|backend| Arc::clone(&backend.thread.hosting));
        NETWORK
            .start("hosting start", async move {
                backend
                    .start(
                        state,
                        NetworkConfig::from_relay_url(relay.as_deref())?,
                    )
                    .await
            })
            .await
    }

    /// Disables inbound connections while keeping the installation's trust records.
    pub async fn stop(this: Instance<Self>) -> Result<(), String> {
        let state = this.with(Self::state);
        NETWORK
            .controls("hosting stop", async move {
                state.stop(None).await;
                Ok(())
            })
            .await
    }

    /// Persists opt-in or opt-out synchronously so window teardown cannot discard it.
    pub fn set_sharing_preference(&self, enabled: bool) -> Result<(), String> {
        self.state
            .profile
            .set_hosting_enabled(enabled)
            .map_err(|error| format!("{error:#}"))?;
        self.state.changes.send_replace(());
        Ok(())
    }

    /// Saves the local device display name.
    pub async fn rename(
        this: Instance<Self>,
        name: String,
    ) -> Result<(), String> {
        let state = this.with(Self::state);
        finish_task(
            "device rename",
            blocking(move || {
                state.profile.set_name(&name)?;
                state.changes.send_replace(());
                Ok(())
            }),
        )
        .await
    }

    /// Generates a single-use pairing code and a QR image carrying that exact code.
    pub async fn invite(
        this: Instance<Self>,
    ) -> Result<PairingInvitation, String> {
        let state = this.with(Self::state);
        NETWORK
            .controls("pairing invitation", async move {
                let invitation = state.host().await?.invite().await?;
                let qr_uri = Self::qr(invitation.code.clone())
                    .map_err(anyhow::Error::msg)?;
                Ok(PairingInvitation::builder()
                    .generation(invitation.generation)
                    .code(invitation.code)
                    .expires_at(invitation.expires_at)
                    .qr_uri(qr_uri)
                    .build())
            })
            .await
    }

    /// Encodes text as a local SVG image URI with a standard QR quiet zone.
    pub fn qr(code: String) -> Result<String, String> {
        let image = QrCode::new(code.as_bytes())
            .map_err(|error| {
                tracing::warn!("desktop QR encoding failed: {}", error);
                error.to_string()
            })?
            .render::<svg::Color<'_>>()
            .min_dimensions(256, 256)
            .build();
        Ok(format!(
            "data:image/svg+xml;base64,{}",
            STANDARD.encode(image)
        ))
    }

    /// Approves or rejects one specific pending pairing request.
    pub async fn approve(
        this: Instance<Self>,
        request_id: String,
        allow: bool,
    ) -> Result<(), String> {
        let state = this.with(Self::state);
        NETWORK
            .controls("pairing decision", async move {
                state.host().await?.approve(&request_id, allow).await
            })
            .await
    }

    /// Drops a live device's connections while allowing a later authenticated reconnect.
    pub async fn disconnect(
        this: Instance<Self>,
        id: String,
    ) -> Result<(), String> {
        let state = this.with(Self::state);
        NETWORK
            .controls("device disconnect", async move {
                state.host().await?.disconnect(&id).await
            })
            .await
    }

    /// Removes trust persistently and terminates any live connections for this identity.
    pub async fn revoke(
        this: Instance<Self>,
        id: String,
    ) -> Result<(), String> {
        let state = this.with(Self::state);
        NETWORK
            .controls("device revocation", async move {
                let result = state.revoke(id).await;
                // Live admission is removed even when persisting the owner's decision fails.
                state.changes.send_replace(());
                result
            })
            .await
    }

    /// Ends native observation and closes inbound hosting during root/window disposal.
    pub fn close(&mut self) {
        if self.state.closed.is_cancelled() {
            return;
        }
        self.state.closed.cancel();
        let state = self.state();
        if let Some(runtime) = NETWORK.existing() {
            runtime.spawn(async move { state.stop(None).await });
        }
    }
}

impl Drop for RemoteHosting {
    /// Also releases a listener when the NSH instance is disposed without an explicit close.
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::RemoteHosting;
    use crate::{Backend, BackendThread};
    use nsh_core::prelude::Instance;

    /// Put the production kernel's offline configuration and state under this test's own directory.
    fn kernel_config(home: &tempfile::TempDir) -> config::ConfigHandle {
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
        .expect("config");
        config::load_from([fixture, data]).expect("load test configuration")
    }

    /// A stuck blocking tool must terminate shutdown observably for all waiters on the same generation.
    #[tokio::test]
    async fn uninterruptible_kernel_work_reports_a_bounded_shutdown_failure() {
        let home = tempfile::tempdir().expect("kernel directory");
        let (mut backend, output) = BackendThread::start(kernel_config(&home))
            .await
            .expect("start");
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, gate) = mpsc::channel::<()>();
        let worker = backend.hosting.runtime.spawn_blocking(move || {
            started.send(()).expect("waiter");
            let _ = gate.recv();
        });
        running.await.expect("blocking worker started");
        let first = backend.shutdown();
        let second = backend.shutdown();
        let reason = tokio::time::timeout(Duration::from_secs(8), output.ended)
            .await
            .expect("a stuck worker must not strand the generation")
            .expect("end reported")
            .expect("drain failure reported");
        assert!(reason.contains("timed out"), "{reason}");
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first, Err(reason.clone()));
        assert_eq!(second, Err(reason.clone()));
        assert_eq!(backend.shutdown().await, Err(reason));
        assert!(!backend.shutdown_blocking(Duration::ZERO));
        assert!(!backend.shutdown_blocking(Duration::ZERO));
        release.send(()).expect("release worker");
        worker.await.expect("worker finished");
    }
    /// Live settings remain readable while a controller's revocation waits to reach disk.
    #[test]
    fn snapshot_during_live_revocation() {
        let engine = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("engine runtime");
        engine.block_on(async {
            let home =
                tempfile::tempdir().expect("profile and kernel directory");
            let local = BackendThread::start(kernel_config(&home))
                .await
                .expect("start");
            let backend = Instance::new(Backend::from(local));
            let controls = Instance::new(
                RemoteHosting::open(home.path().display().to_string())
                    .await
                    .expect("profile"),
            );
            RemoteHosting::start(
                controls.clone(),
                backend.clone(),
                Some("direct".into()),
            )
            .await
            .expect("listener");
            // Queue the real revocation write behind a known outstanding filesystem job.
            let (started, running) = tokio::sync::oneshot::channel();
            let (release, gate) = mpsc::channel::<()>();
            let filesystem = tokio::task::spawn_blocking(move || {
                started.send(()).expect("waiter");
                let _ = gate.recv();
            });
            running.await.expect("filesystem busy");
            let state = controls.with(RemoteHosting::state);
            let mut revoke = std::pin::pin!(state.revoke("controller".into()));
            assert!(futures::poll!(revoke.as_mut()).is_pending());
            let snapshot = tokio::time::timeout(
                Duration::from_secs(2),
                RemoteHosting::snapshot(controls.clone()),
            )
            .await
            .expect("status while the write is pending")
            .expect("status");
            assert!(snapshot.enabled);
            release.send(()).expect("release filesystem");
            filesystem.await.expect("filesystem released");
            revoke.await.expect("revoked");
            Backend::shutdown(backend).await.expect("drained");
        });
    }
}
