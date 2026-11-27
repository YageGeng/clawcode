//! The desktop app's Rust side: what `App.tsx` imports from `clawcode-backend`.
//! Their TypeScript declarations are generated from these types into `.nsh/types`.

mod apps;
mod connection;
mod diff;
mod editors;
mod fonts;
mod hosting;
mod runtime;
mod thread;

use std::time::Duration;

use futures::StreamExt;
use nsh_core::file_dialog::OpenDialog;
use nsh_core::prelude::*;

pub use connection::{RemoteConnection, RemotePairing};
pub use hosting::{
    HostingSnapshot, PairingInvitation, RemoteDevice, RemoteHosting,
};
pub use thread::{BackendOutput, BackendThread};

/// The crate's native module (`nsh.json` `"rust"`).
#[native_module("clawcode-backend")]
fn native(registry: &mut Registry) {
    registry
        .class::<Backend>()
        .class::<RemoteHosting>()
        .class::<RemoteConnection>()
        .class::<RemotePairing>()
        .module::<diff::SnapshotDiff>()
        .module::<editors::ExternalEditors>()
        .module::<Folders>()
        .module::<fonts::DesktopFonts>();
}

/// The system's folder chooser: TSX's own picker only picks files.
#[module]
pub struct Folders;

#[module]
impl Folders {
    /// Asks the user for a folder; resolves with its path, or null when
    /// cancelled; rejects when the system cannot show a chooser.
    pub async fn pick() -> Result<Option<String>, String> {
        let picked = pick_paths(OpenDialog::folders()).await?;
        Ok(picked
            .and_then(|paths| paths.into_iter().next())
            .map(|path| path.to_string_lossy().into_owned()))
    }
}

/// The clawcode backend running in this process: one ACP connection whose
/// JSON-RPC messages travel as JSON text frames.
#[class]
pub struct Backend {
    thread: BackendThread,
    output: Option<BackendOutput>,
}

impl From<(BackendThread, BackendOutput)> for Backend {
    /// Takes ownership of one running local application and its NSH frame stream.
    fn from((thread, output): (BackendThread, BackendOutput)) -> Self {
        Self {
            thread,
            output: Some(output),
        }
    }
}

#[class]
impl Backend {
    /// Starts the backend from the user's configuration; rejects with the
    /// reason when the configuration or the application fails.
    pub async fn start() -> Result<Backend, String> {
        let config = config::load().map_err(|error| {
            tracing::error!("desktop configuration failed to load: {}", error);
            error.to_string()
        })?;
        BackendThread::start(config).await.map(Backend::from)
    }

    /// Sends one ACP frame; throws once the backend has stopped.
    pub fn send(&self, frame: String) -> Result<(), String> {
        self.thread.send(frame)
    }

    /// Calls `listener` with every frame the backend writes, in order, until
    /// the connection ends; resolves with the error that ended it, or null.
    /// Only one call may serve the frames.
    pub async fn serve(
        this: Instance<Self>,
        #[args(frame)] listener: Callback<String>,
    ) -> Result<Option<String>, String> {
        let output =
            this.with_mut(|backend| backend.output.take()).ok_or_else(
                || "the backend's frames are already served".to_owned(),
            )?;
        let BackendOutput { mut frames, ended } = output;
        while let Some(frame) = frames.next().await {
            listener.post(frame);
        }
        ended.await.map_err(|error| {
            tracing::error!(
                "desktop backend exited without an end reason: {}",
                error
            );
            "desktop backend exited without reporting its drain".to_owned()
        })
    }

    /// Ends the connection and blocks until the backend has stopped or
    /// `timeout_ms` passed; whether it stopped. Only for the app's quit: it
    /// holds the engine thread while it waits.
    pub fn shutdown_now(&mut self, timeout_ms: u32) -> bool {
        self.thread
            .shutdown_blocking(Duration::from_millis(u64::from(timeout_ms)))
    }

    /// Ends the connection, shuts the application down and resolves once its
    /// runtime has drained, or rejects if blocking work outlived the shutdown deadline.
    pub async fn shutdown(this: Instance<Self>) -> Result<(), String> {
        let stopped = this.with_mut(|backend| backend.thread.shutdown());
        stopped.await
    }
}
