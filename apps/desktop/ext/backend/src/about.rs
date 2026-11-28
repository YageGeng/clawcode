//! What the settings window's About page shows that only the native side knows.

use nsh_core::prelude::*;

/// The NSH runtime version and where this installation keeps its files.
#[derive(Debug, JsObject)]
pub struct AboutInfo {
    /// The version of the nsh crates this app was built with.
    pub nsh_version: String,
    /// The configuration file the backend reads, when one exists.
    pub config_file: Option<String>,
    /// The root of session logs, databases and credentials, when the configuration resolves one.
    pub data_dir: Option<String>,
}

/// The desktop bridge for the About page.
#[module]
pub struct AppDetails;

#[module]
impl AppDetails {
    /// Reads the NSH version and the configuration's paths; a configuration that cannot load leaves the paths out.
    pub fn about() -> AboutInfo {
        // A project-local `claw.toml` is found relative to the working directory; show where it really is.
        let config_file = config::config_path()
            .ok()
            .flatten()
            .map(|path| std::path::absolute(&path).unwrap_or(path));
        // A data root may be relative to the working directory too.
        let data_dir = config::load()
            .ok()
            .and_then(|handle| handle.current().session_persistence.data_root())
            .map(|path| std::path::absolute(&path).unwrap_or(path));
        AboutInfo {
            nsh_version: nsh_core::element::description::VERSION.to_owned(),
            config_file: config_file
                .map(|path| path.to_string_lossy().into_owned()),
            data_dir: data_dir.map(|path| path.to_string_lossy().into_owned()),
        }
    }
}
