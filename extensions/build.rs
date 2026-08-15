use std::env;
use std::path::PathBuf;

use protocol::{ConfigOverridePath, ProductIdentity};

#[path = "build/config.rs"]
mod build_config;

use build_config::BuildConfiguration;

/// Generates the extension modules selected by the parameterized build-time TOML.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir.parent().ok_or_else(|| {
        std::io::Error::other(format!(
            "extensions crate manifest has no workspace parent: {}",
            manifest_dir.display()
        ))
    })?;
    let config_path =
        if let Some(path) = env::var_os(ProductIdentity::CONFIG_PATH_ENV) {
            PathBuf::from(ConfigOverridePath::try_from(PathBuf::from(path))?)
        } else {
            workspace_root.join(ProductIdentity::CONFIG_FILE_NAME)
        };
    let available_root = manifest_dir.join("src/available");
    let output_path =
        PathBuf::from(env::var("OUT_DIR")?).join("compiled_extensions.rs");
    let configuration =
        BuildConfiguration::from_path(config_path, available_root)?;

    println!(
        "cargo:rerun-if-env-changed={}",
        ProductIdentity::CONFIG_PATH_ENV
    );
    println!(
        "cargo:rerun-if-changed={}",
        configuration.config_path().display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        configuration.available_root().display()
    );
    configuration.render(output_path)?;
    Ok(())
}
