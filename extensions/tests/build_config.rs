use std::path::{Path, PathBuf};

#[path = "../build/config.rs"]
mod build_config;

use build_config::{BuildConfigError, BuildConfiguration};

/// Isolated filesystem used to exercise the real build-code generator.
struct BuildFixture {
    root: tempfile::TempDir,
    config_path: PathBuf,
    available_root: PathBuf,
}

impl BuildFixture {
    /// Creates available extension source files under one temporary root.
    fn new(sources: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().expect("temporary build root");
        let config_path = root.path().join("claw.toml");
        let available_root = root.path().join("available");
        std::fs::create_dir_all(&available_root)
            .expect("create available root");
        for (relative, source) in sources {
            let path = available_root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create source parent");
            }
            std::fs::write(path, source).expect("write extension source");
        }
        Self {
            root,
            config_path,
            available_root,
        }
    }

    /// Writes the complete TOML consumed by the build configuration.
    fn write_config(&self, source: &str) {
        std::fs::write(&self.config_path, source).expect("write config");
    }

    /// Runs generation and returns the emitted Rust source.
    fn render(&self) -> String {
        let output = self.root.path().join("compiled_extensions.rs");
        let configuration = BuildConfiguration::from_path(
            &self.config_path,
            &self.available_root,
        )
        .expect("parse build configuration");
        assert_eq!(configuration.config_path(), self.config_path);
        assert_eq!(configuration.available_root(), self.available_root);
        configuration.render(&output).expect("render catalogue");
        std::fs::read_to_string(output).expect("read generated source")
    }

    /// Returns the configuration path for direct error assertions.
    fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Returns the available-source root for direct error assertions.
    fn available_root(&self) -> &Path {
        &self.available_root
    }
}

/// Generated module and factory order follows the TOML declaration exactly.
#[test]
fn build_config_generates_only_enabled_modules_in_order() {
    let fixture = BuildFixture::new(&[
        ("command_guard.rs", "pub fn definition() {}"),
        ("hook_examples/mod.rs", "pub fn definition() {}"),
    ]);
    fixture.write_config(
        "[extensions]\nenabled = [\"hook-examples\", \"command-guard\"]\n",
    );

    let generated = fixture.render();
    let hook_position =
        generated.find("mod hook_examples").expect("hook module");
    let guard_position =
        generated.find("mod command_guard").expect("guard module");

    assert!(hook_position < guard_position);
    assert_eq!(generated.matches("::definition()").count(), 2);
}

/// A configuration without an extension section still compiles the safe default.
#[test]
fn build_config_defaults_to_command_guard() {
    let fixture =
        BuildFixture::new(&[("command_guard.rs", "pub fn definition() {}")]);
    fixture.write_config("active_model = \"provider/model\"\n");

    let generated = fixture.render();

    assert!(generated.contains("mod command_guard"));
    assert_eq!(generated.matches("::definition()").count(), 1);
}

/// Runtime-only configuration fields cannot couple extension code generation to AppConfig.
#[test]
fn build_config_ignores_unrelated_application_fields() {
    let fixture =
        BuildFixture::new(&[("command_guard.rs", "pub fn definition() {}")]);
    fixture.write_config(
        "providers = \"validated only at runtime\"\n\n[extensions]\nenabled = [\"command-guard\"]\n",
    );

    let generated = fixture.render();

    assert!(generated.contains("mod command_guard"));
}

/// An explicit empty list emits a valid catalogue without extension modules.
#[test]
fn build_config_allows_an_empty_catalogue() {
    let fixture = BuildFixture::new(&[]);
    fixture.write_config("[extensions]\nenabled = []\n");

    let generated = fixture.render();

    assert!(!generated.contains("#[path ="));
    assert!(generated.contains("ExtensionCatalog::new(Vec::new())"));
}

/// Duplicate IDs fail the build before duplicate module declarations are emitted.
#[test]
fn build_config_rejects_duplicate_extensions() {
    let fixture =
        BuildFixture::new(&[("command_guard.rs", "pub fn definition() {}")]);
    fixture.write_config(
        "[extensions]\nenabled = [\"command-guard\", \"command-guard\"]\n",
    );

    assert!(matches!(
        BuildConfiguration::from_path(
            fixture.config_path(),
            fixture.available_root(),
        ),
        Err(BuildConfigError::DuplicateExtension(extension_id))
            if extension_id == "command-guard"
    ));
}

/// IDs that cannot map to one local Rust module are rejected before path access.
#[test]
fn build_config_rejects_unsafe_extension_ids() {
    let fixture = BuildFixture::new(&[]);
    fixture.write_config("[extensions]\nenabled = [\"../escape\"]\n");

    assert!(matches!(
        BuildConfiguration::from_path(
            fixture.config_path(),
            fixture.available_root(),
        ),
        Err(BuildConfigError::InvalidExtensionId(extension_id))
            if extension_id == "../escape"
    ));
}

/// Rust keywords are rejected before the generator emits an invalid module declaration.
#[test]
fn build_config_rejects_rust_keyword_extension_ids() {
    let fixture = BuildFixture::new(&[("type.rs", "pub fn definition() {}")]);
    fixture.write_config("[extensions]\nenabled = [\"type\"]\n");

    assert!(matches!(
        BuildConfiguration::from_path(
            fixture.config_path(),
            fixture.available_root(),
        ),
        Err(BuildConfigError::InvalidExtensionId(extension_id))
            if extension_id == "type"
    ));
}

/// Configuring an unknown extension fails instead of emitting uncompilable Rust.
#[test]
fn build_config_rejects_missing_extension_sources() {
    let fixture = BuildFixture::new(&[]);
    fixture.write_config("[extensions]\nenabled = [\"missing\"]\n");

    assert!(matches!(
        BuildConfiguration::from_path(
            fixture.config_path(),
            fixture.available_root(),
        ),
        Err(BuildConfigError::MissingExtensionSource(extension_id))
            if extension_id == "missing"
    ));
}
