use app::{ApplicationError, ApplicationFactory};
use config::{AppConfig, ConfigHandle, ExtensionsConfig};
use extension::{ExtensionError, ExtensionFactory};
use protocol::ExtensionId;

/// The default build catalogue creates only the production command guard.
#[test]
fn production_catalog_contains_command_guard() {
    let configured = ExtensionsConfig::default();
    let factory = extensions::compiled_extensions()
        .expect("compiled catalogue")
        .select(&configured.enabled)
        .expect("default selection");
    let modules = factory.create_modules().expect("session modules");

    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].descriptor().id.to_string(), "command-guard");
}

/// The production composition root rejects runtime code absent from the binary.
#[test]
fn application_rejects_an_extension_not_compiled_at_build_time() {
    let missing = ExtensionId::try_from("hook-examples")
        .expect("hook example extension id");
    let config = AppConfig {
        extensions: ExtensionsConfig {
            enabled: vec![missing.clone()],
        },
        ..AppConfig::default()
    };

    let error = match ApplicationFactory::new(ConfigHandle::from_config(config))
        .build()
    {
        Ok(_application) => panic!("uncompiled extension must fail startup"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ApplicationError::Extension(
            ExtensionError::ExtensionNotCompiled(extension_id)
        ) if extension_id == missing
    ));
}
