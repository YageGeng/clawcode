use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
};

use anyhow::{Context, Result, ensure};
use iroh::{EndpointAddr, EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;
use typed_builder::TypedBuilder;

use crate::network::SharedEndpoint;

/// Public device information safe to include in the desktop UI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Peer {
    pub id: String,
    pub name: String,
}

impl Peer {
    /// Bind a display name to an authenticated iroh identity.
    pub(crate) fn new(id: EndpointId, name: &str) -> Result<Self> {
        let name = name.trim();
        // Exclude directional formatting and control characters from an owner-facing prompt.
        let misleading = name.chars().any(|ch| ch.is_control() || matches!(ch,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        ));
        ensure!(
            !name.is_empty() && name.len() <= 128 && !misleading,
            "Invalid device name"
        );
        Ok(Self {
            id: id.to_string(),
            name: name.to_owned(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KnownHost {
    pub(crate) addr: EndpointAddr,
    pub(crate) name: String,
}

#[derive(Clone, Serialize, Deserialize, Default, TypedBuilder)]
struct Settings {
    #[serde(default)]
    #[builder(default)]
    hosting_enabled: bool,
    #[serde(default)]
    #[builder(default)]
    loopback_port: Option<u16>,
    #[builder(default)]
    name: Option<String>,
    #[builder(default)]
    devices: BTreeMap<String, Peer>,
    #[builder(default)]
    hosts: BTreeMap<String, KnownHost>,
}

#[derive(TypedBuilder)]
struct ProfileData {
    root: PathBuf,
    _ownership: File,
    key: SecretKey,
    settings: Mutex<Arc<Settings>>,
    writer: Mutex<()>,
    endpoint: tokio::sync::Mutex<Option<EndpointGeneration>>,
}

impl Settings {
    /// Stage a complete owner-only file and flush its contents before changing the directory entry.
    fn stage(&self, root: &Path) -> Result<NamedTempFile> {
        let mut file = NamedTempFile::new_in(root)?;
        serde_json::to_writer(&mut file, self)?;
        file.as_file().sync_all()?;
        Ok(file)
    }
}

struct EndpointGeneration {
    live: Weak<SharedEndpoint>,
    drained: CancellationToken,
}

/// A single installation's persistent key, grants and address book.
#[derive(Clone)]
pub struct DeviceProfile(Arc<ProfileData>);

impl DeviceProfile {
    /// Open one process-owned profile without binding sockets. Handles in this
    /// process share live grants; another process must use a different data directory.
    pub fn open(root: &Path) -> Result<Self> {
        let result = (|| {
            static LIVE: OnceLock<Mutex<HashMap<PathBuf, Weak<ProfileData>>>> =
                OnceLock::new();
            let mut directory = fs::DirBuilder::new();
            directory.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory.mode(0o700);
            }
            directory.create(root)?;
            let canonical = root.canonicalize()?;
            let root = canonical.as_path();
            let mut live = LIVE
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            if let Some(profile) = live.get(root).and_then(Weak::upgrade) {
                return Ok(Self(profile));
            }
            live.retain(|_, profile| profile.strong_count() > 0);
            // Never unlink the lock file: replacing its inode would allow two
            // processes to acquire independent locks on the same logical profile.
            let mut options = OpenOptions::new();
            options.create(true).truncate(false).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let ownership = options.open(root.join("device.lock"))?;
            match ownership.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => anyhow::bail!(
                    "Desktop device profile is already open in another process"
                ),
                Err(fs::TryLockError::Error(error)) => {
                    return Err(error)
                        .context("Could not lock desktop device profile");
                }
            }
            let path = root.join("device.key");
            let key = match fs::read(&path) {
                Ok(bytes) => Self::decode_key(&bytes)?,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    // Publish a fully written owner-only tempfile. A racing creator
                    // reads the winner's key rather than replacing it or seeing a partial key.
                    let key = SecretKey::generate();
                    let mut file = NamedTempFile::new_in(root)?;
                    file.write_all(&key.to_bytes())?;
                    file.as_file().sync_all()?;
                    match file.persist_noclobber(&path) {
                        Ok(_) => key,
                        Err(error)
                            if error.error.kind()
                                == ErrorKind::AlreadyExists =>
                        {
                            Self::decode_key(&fs::read(&path)?)?
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            };
            // Also flush an existing entry on retry after an earlier directory sync failed.
            Self::sync_directory(root)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&path)?.permissions().mode();
                ensure!(
                    mode & 0o077 == 0,
                    "Device key must only be accessible by its owner"
                );
            }
            let settings = match fs::read(root.join("remote.json")) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .context("Invalid remote device settings")?,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    Settings::builder().build()
                }
                Err(error) => return Err(error.into()),
            };
            let profile = Arc::new(
                ProfileData::builder()
                    .root(root.to_owned())
                    ._ownership(ownership)
                    .key(key)
                    .settings(Mutex::new(Arc::new(settings)))
                    .writer(Mutex::new(()))
                    .endpoint(Default::default())
                    .build(),
            );
            live.insert(root.to_owned(), Arc::downgrade(&profile));
            Ok(Self(profile))
        })();
        result.inspect_err(|error| {
            tracing::warn!("Could not open the desktop device profile: {error}")
        })
    }

    /// Require the original raw key format; corrupt keys must never be regenerated silently.
    fn decode_key(bytes: &[u8]) -> Result<SecretKey> {
        Ok(SecretKey::from_bytes(
            bytes.try_into().context("Invalid device key length")?,
        ))
    }

    /// Make the rename of a key or grant record durable on filesystems with directory handles.
    fn sync_directory(root: &Path) -> Result<()> {
        #[cfg(unix)]
        File::open(root)?
            .sync_all()
            .context("Could not sync remote profile directory")?;
        #[cfg(not(unix))]
        let _ = root;
        Ok(())
    }

    /// Lock a settings snapshot; updates replace it only after a successful disk write.
    fn settings(&self) -> MutexGuard<'_, Arc<Settings>> {
        self.0
            .settings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Persist an unconditional update to the installation settings.
    fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<()> {
        self.update_while(
            |candidate| change(Arc::make_mut(candidate)),
            || Ok(()),
        )
    }

    /// Validate just before publication and again after directory sync before enabling a new grant.
    fn update_while(
        &self,
        change: impl FnOnce(&mut Arc<Settings>),
        valid: impl Fn() -> Result<()>,
    ) -> Result<()> {
        // Only writers queue behind filesystem I/O. Readers keep seeing the last published snapshot.
        let _writer = self.0.writer.lock().unwrap_or_else(|e| e.into_inner());
        let previous = Arc::clone(&self.settings());
        let mut candidate = Arc::clone(&previous);
        change(&mut candidate);
        if Arc::ptr_eq(&previous, &candidate) {
            return valid();
        }
        let result = (|| -> Result<()> {
            let file = candidate.stage(&self.0.root)?;
            valid()?;
            file.persist(self.0.root.join("remote.json"))?;
            if let Err(error) =
                Self::sync_directory(&self.0.root).and_then(|()| valid())
            {
                // Rename already succeeded. Restore the prior durable grants before admitting another peer.
                self.persist(&previous).context("Could not restore the previous remote profile after publication failed")?;
                return Err(error);
            }
            *self.settings() = candidate;
            Ok(())
        })();
        result.inspect_err(|error| {
            tracing::warn!("Could not persist remote device settings: {error}")
        })
    }

    /// Replace the disk snapshot and sync its parent after revocation or rollback.
    fn persist(&self, settings: &Settings) -> Result<()> {
        settings
            .stage(&self.0.root)?
            .persist(self.0.root.join("remote.json"))?;
        Self::sync_directory(&self.0.root)
    }

    /// Get this installation's authenticated public identifier.
    pub fn id(&self) -> String {
        self.0.key.public().to_string()
    }

    /// Get the advertised display name, using the operating system hostname by default.
    pub fn name(&self) -> String {
        self.settings()
            .name
            .clone()
            .or_else(|| {
                hostname::get()
                    .ok()
                    .and_then(|name| {
                        Peer::new(self.0.key.public(), &name.to_string_lossy())
                            .ok()
                    })
                    .map(|peer| peer.name)
            })
            .unwrap_or_else(|| "Desktop".to_owned())
    }

    /// Persist a user-provided display name for subsequent handshakes.
    pub fn set_name(&self, name: &str) -> Result<()> {
        let peer = Peer::new(self.0.key.public(), name)?;
        self.update(|settings| settings.name = Some(peer.name))
    }

    /// Returns the explicit opt-in for restoring the incoming listener on next launch.
    pub fn hosting_enabled(&self) -> bool {
        self.settings().hosting_enabled
    }

    /// Commits the startup preference before the desktop acknowledges the switch.
    pub fn set_hosting_enabled(&self, enabled: bool) -> Result<()> {
        self.update(|settings| settings.hosting_enabled = enabled)
    }

    /// Get controllers granted access to the local host.
    pub fn trusted_devices(&self) -> Vec<Peer> {
        self.settings().devices.values().cloned().collect()
    }

    /// Get previously paired remote hosts, excluding their private connection settings.
    pub fn known_hosts(&self) -> Vec<Peer> {
        self.settings()
            .hosts
            .values()
            .map(|host| Peer {
                id: host.addr.id.to_string(),
                name: host.name.clone(),
            })
            .collect()
    }

    /// Remove a grant while hosting is disabled; live hosts also close the connection via RemoteHost::revoke.
    pub fn revoke(&self, id: &str) -> Result<()> {
        // Remove the live grant even when storage is unavailable. A retry persists this same snapshot.
        let _writer = self.0.writer.lock().unwrap_or_else(|e| e.into_inner());
        let revoked = {
            let mut settings = self.settings();
            Arc::make_mut(&mut settings).devices.remove(id);
            Arc::clone(&settings)
        };
        self.persist(&revoked).inspect_err(|error| {
            tracing::warn!("Removed live device access but could not persist revocation: {error}");
        })
    }

    /// Look up a controller by its TLS-authenticated identity.
    pub(crate) fn trusted(&self, id: EndpointId) -> Option<Peer> {
        self.settings().devices.get(&id.to_string()).cloned()
    }

    /// Record explicit approval by the local device owner.
    pub(crate) fn trust_while(
        &self,
        peer: Peer,
        valid: impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.update_while(
            |settings| {
                Arc::make_mut(settings)
                    .devices
                    .insert(peer.id.clone(), peer);
            },
            valid,
        )
    }

    /// Retrieve the saved transport addresses for a paired host.
    pub(crate) fn host(&self, id: &str) -> Result<KnownHost> {
        self.settings()
            .hosts
            .get(id)
            .cloned()
            .context("Unknown remote host")
    }

    /// Save a host's authenticated display name and current reachable addresses.
    pub(crate) fn remember(&self, host: KnownHost) -> Result<()> {
        let id = host.addr.id.to_string();
        // The published record suffices when a reconnect already has these hints.
        if self.settings().hosts.get(&id) == Some(&host) {
            return Ok(());
        }
        self.update_while(
            |settings| {
                // Compare under the writer lock so concurrent route updates keep their ordering.
                if settings.hosts.get(&id) != Some(&host) {
                    Arc::make_mut(settings).hosts.insert(id, host);
                }
            },
            || Ok(()),
        )
    }

    /// Bind lazily and share one UDP endpoint between both simultaneous roles.
    pub(crate) async fn endpoint(
        &self,
        config: crate::NetworkConfig,
        hosting: bool,
    ) -> Result<Arc<SharedEndpoint>> {
        let mut slot = self.0.endpoint.lock().await;
        let port = self.settings().loopback_port;
        let endpoint = match slot
            .as_ref()
            .and_then(|generation| generation.live.upgrade())
        {
            Some(endpoint) => {
                ensure!(
                    endpoint.config == config,
                    "Disconnect all devices before changing relay settings"
                );
                endpoint
            }
            None => {
                // Serialize relay registrations and socket binds across generations.
                if let Some(previous) = slot.as_ref() {
                    previous.drained.cancelled().await;
                }
                let network = Arc::new(
                    config
                        .bind(self.clone(), &self.0.key, port.unwrap_or(0))
                        .await?,
                );
                *slot = Some(EndpointGeneration {
                    live: Arc::downgrade(&network),
                    drained: network.drained.clone(),
                });
                network
            }
        };
        // Only listeners need a stable loopback port across restarts. Outbound-only
        // controllers can use a fresh ephemeral port on every connection.
        if hosting && endpoint.config == crate::NetworkConfig::Loopback {
            let bound = endpoint
                .endpoint
                .bound_sockets()
                .first()
                .context("Could not determine loopback port")?
                .port();
            if port != Some(bound) {
                let profile = self.clone();
                tokio::task::spawn_blocking(move || {
                    profile
                        .update(|settings| settings.loopback_port = Some(bound))
                })
                .await??;
            }
        }
        Ok(endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A failed revocation write still removes live access, and a retry persists that removal.
    #[test]
    fn revocation_write_failure_denies_live_access_until_retry() -> Result<()> {
        let root = tempdir()?;
        let profile = DeviceProfile::open(root.path())?;
        let id = SecretKey::generate().public();
        profile.trust_while(Peer::new(id, "Controller")?, || Ok(()))?;
        let path = root.path().join("remote.json");
        let original = root.path().join("saved.json");
        fs::rename(&path, &original)?;
        fs::create_dir(&path)?;
        ensure!(profile.revoke(&id.to_string()).is_err());
        ensure!(
            profile.trusted(id).is_none(),
            "A failed revocation left the live grant enabled"
        );
        fs::remove_dir(&path)?;
        fs::rename(&original, &path)?;
        profile.revoke(&id.to_string())?;
        drop(profile);
        ensure!(DeviceProfile::open(root.path())?.trusted(id).is_none());
        Ok(())
    }

    /// Reconnecting with an unchanged record succeeds even when settings cannot be replaced.
    #[test]
    fn unchanged_host_does_not_require_a_profile_write() -> Result<()> {
        let root = tempdir()?;
        let profile = DeviceProfile::open(root.path())?;
        let original = KnownHost {
            addr: EndpointAddr::new(SecretKey::generate().public()),
            name: "Desktop".into(),
        };
        profile.remember(original.clone())?;
        let path = root.path().join("remote.json");
        fs::rename(&path, root.path().join("saved.json"))?;
        fs::create_dir(&path)?;
        profile.remember(original.clone())?;
        let renamed = KnownHost {
            name: "Updated".into(),
            ..original.clone()
        };
        ensure!(profile.remember(renamed.clone()).is_err());
        let rerouted = KnownHost {
            addr: original
                .addr
                .clone()
                .with_ip_addr("127.0.0.1:1234".parse()?),
            ..original.clone()
        };
        ensure!(profile.remember(rerouted.clone()).is_err());
        fs::remove_dir(&path)?;
        profile.remember(renamed)?;
        profile.remember(rerouted.clone())?;
        drop(profile);
        let saved = DeviceProfile::open(root.path())?
            .host(&original.addr.id.to_string())?;
        ensure!(saved.addr == rerouted.addr && saved.name == rerouted.name);
        Ok(())
    }

    /// Readers see the last published snapshot while a settings writer waits for durability.
    #[test]
    fn settings_reads_remain_available_during_a_write() -> Result<()> {
        use std::{sync::mpsc, thread, time::Duration};
        let root = tempdir()?;
        let profile = DeviceProfile::open(root.path())?;
        let id = SecretKey::generate().public();
        let known = KnownHost {
            addr: EndpointAddr::new(SecretKey::generate().public()),
            name: "Desktop".into(),
        };
        profile.remember(known.clone())?;
        let (started, writing) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::channel();
        let (observed, snapshot) = mpsc::channel();
        thread::scope(|scope| -> Result<()> {
            let profile_ref = &profile;
            let writer = scope.spawn(move || {
                let first = std::cell::Cell::new(true);
                profile_ref.trust_while(Peer::new(id, "Controller")?, || {
                    if first.replace(false) {
                        started.send(())?;
                        wait.recv()?;
                    }
                    Ok(())
                })
            });
            writing.recv_timeout(Duration::from_secs(5))?;
            scope.spawn(|| {
                // An unchanged reconnect reads the same published snapshot without queueing a write.
                observed
                    .send((profile.trusted_devices(), profile.remember(known)))
            });
            let before = snapshot.recv_timeout(Duration::from_secs(1));
            release.send(())?;
            writer.join().expect("grant writer")?;
            let (trusted, remembered) = before.context(
                "Snapshot or unchanged reconnect blocked behind persistence",
            )?;
            ensure!(trusted.is_empty());
            remembered?;
            Ok(())
        })?;
        ensure!(profile.trusted(id).is_some());
        Ok(())
    }

    /// Losing confirmation during directory sync restores both live and durable authorization.
    #[test]
    fn consent_must_survive_through_the_durability_boundary() -> Result<()> {
        use std::cell::Cell;
        let root = tempdir()?;
        let profile = DeviceProfile::open(root.path())?;
        let id = SecretKey::generate().public();
        let was_valid = Cell::new(true);
        let result = profile.trust_while(Peer::new(id, "Controller")?, || {
            ensure!(was_valid.replace(false), "Confirmation expired");
            Ok(())
        });
        ensure!(result.is_err());
        ensure!(profile.trusted(id).is_none());
        drop(profile);
        ensure!(DeviceProfile::open(root.path())?.trusted(id).is_none());
        Ok(())
    }
}
