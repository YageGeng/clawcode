use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use extension::{
    CompiledExtension, ExtensionCatalog, ExtensionError, ExtensionFactory,
    ExtensionModule, ExtensionRegistrar,
};
use protocol::{
    ExtensionDescriptor, ExtensionFlagDefinition, ExtensionFlagKind,
    ExtensionId, ExtensionProviderRegistration, StaticExtensionRegistration,
};

/// Minimal module that exposes the descriptor supplied by each catalogue test.
struct TestModule(ExtensionDescriptor);

impl ExtensionModule for TestModule {
    /// Returns the module identity used to validate the compiled declaration.
    fn descriptor(&self) -> ExtensionDescriptor {
        self.0.clone()
    }

    /// Registers no handlers because these tests exercise catalogue composition.
    fn register(
        &self,
        _registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
}

/// Creates one validated identifier from a readable test literal.
fn id(value: &str) -> ExtensionId {
    ExtensionId::try_from(value).expect("extension id")
}

/// Creates one descriptor whose display metadata mirrors its identifier.
fn descriptor(value: &str) -> ExtensionDescriptor {
    ExtensionDescriptor {
        id: id(value),
        name: value.to_string(),
        version: "1".to_string(),
    }
}

/// Creates one compiled entry with matching static and runtime descriptors.
fn entry(value: &str) -> CompiledExtension {
    let declared = descriptor(value);
    let module_descriptor = declared.clone();
    CompiledExtension::new(
        declared.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![declared])
            .build(),
        Arc::new(move || {
            Ok(Arc::new(TestModule(module_descriptor.clone()))
                as Arc<dyn ExtensionModule>)
        }),
    )
}

/// Requested order controls module and hook priority independently of build order.
#[test]
fn catalog_preserves_requested_order() {
    let catalog = ExtensionCatalog::new(vec![entry("first"), entry("second")])
        .expect("compiled catalogue");
    let selected = catalog
        .select(&[id("second"), id("first")])
        .expect("select extensions");
    let modules = selected.create_modules().expect("create modules");
    let ids: Vec<_> = modules
        .iter()
        .map(|module| module.descriptor().id.to_string())
        .collect();

    assert_eq!(ids, vec!["second", "first"]);
}

/// Duplicate runtime entries fail instead of registering handlers twice.
#[test]
fn catalog_rejects_duplicate_requested_ids() {
    let catalog =
        ExtensionCatalog::new(vec![entry("first")]).expect("catalogue");

    assert!(matches!(
        catalog.select(&[id("first"), id("first")]),
        Err(ExtensionError::DuplicateConfiguredExtension(extension_id))
            if extension_id == id("first")
    ));
}

/// A runtime configuration cannot activate code absent from the binary.
#[test]
fn catalog_rejects_extensions_missing_from_the_binary() {
    let catalog =
        ExtensionCatalog::new(vec![entry("first")]).expect("catalogue");

    assert!(matches!(
        catalog.select(&[id("missing")]),
        Err(ExtensionError::ExtensionNotCompiled(extension_id))
            if extension_id == id("missing")
    ));
}

/// A generated declaration is validated when its session module is created.
#[test]
fn catalog_rejects_module_descriptor_mismatches_during_session_creation() {
    let declared = descriptor("declared");
    let actual = descriptor("actual");
    let entry = CompiledExtension::new(
        declared.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![declared])
            .build(),
        Arc::new(move || {
            Ok(Arc::new(TestModule(actual.clone()))
                as Arc<dyn ExtensionModule>)
        }),
    );
    let catalog = ExtensionCatalog::new(vec![entry]).expect("catalogue");
    let selected = catalog
        .select(&[id("declared")])
        .expect("select declared extension without constructing it");

    assert!(matches!(
        selected.create_modules(),
        Err(ExtensionError::DescriptorMismatch { .. })
    ));
}

/// Catalogue selection must not create and discard a session-owned module.
#[test]
fn catalog_constructs_each_module_only_for_a_session() {
    let constructions = Arc::new(AtomicUsize::new(0));
    let observed_constructions = Arc::clone(&constructions);
    let declared = descriptor("stateful");
    let module_descriptor = declared.clone();
    let entry = CompiledExtension::new(
        declared.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![declared])
            .build(),
        Arc::new(move || {
            observed_constructions.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(TestModule(module_descriptor.clone()))
                as Arc<dyn ExtensionModule>)
        }),
    );
    let catalog = ExtensionCatalog::new(vec![entry]).expect("catalogue");

    let selected = catalog
        .select(&[id("stateful")])
        .expect("select stateful extension");
    assert_eq!(constructions.load(Ordering::SeqCst), 0);

    selected.create_modules().expect("create session module");
    assert_eq!(constructions.load(Ordering::SeqCst), 1);
}

/// Global flags from selected extensions must remain uniquely addressable.
#[test]
fn catalog_rejects_duplicate_static_flags() {
    let flag = ExtensionFlagDefinition::builder()
        .name("trace".to_string())
        .kind(ExtensionFlagKind::Boolean)
        .build();
    let entries = ["first", "second"]
        .into_iter()
        .map(|value| {
            let declared = descriptor(value);
            let module_descriptor = declared.clone();
            CompiledExtension::new(
                declared.clone(),
                StaticExtensionRegistration::builder()
                    .extensions(vec![declared])
                    .flags(vec![flag.clone()])
                    .build(),
                Arc::new(move || {
                    Ok(Arc::new(TestModule(module_descriptor.clone()))
                        as Arc<dyn ExtensionModule>)
                }),
            )
        })
        .collect();
    let catalog = ExtensionCatalog::new(entries).expect("catalogue");

    assert!(matches!(
        catalog.select(&[id("first"), id("second")]),
        Err(ExtensionError::DuplicateFlag(name)) if name == "trace"
    ));
}

/// Duplicate flags inside one extension are rejected before catalogue merging.
#[test]
fn catalog_rejects_duplicate_static_flags_within_one_extension() {
    let declared = descriptor("first");
    let module_descriptor = declared.clone();
    let flag = ExtensionFlagDefinition::builder()
        .name("trace".to_string())
        .kind(ExtensionFlagKind::Boolean)
        .build();
    let entry = CompiledExtension::new(
        declared.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![declared])
            .flags(vec![flag.clone(), flag])
            .build(),
        Arc::new(move || {
            Ok(Arc::new(TestModule(module_descriptor.clone()))
                as Arc<dyn ExtensionModule>)
        }),
    );
    let catalog = ExtensionCatalog::new(vec![entry]).expect("catalogue");

    assert!(matches!(
        catalog.select(&[id("first")]),
        Err(ExtensionError::DuplicateFlag(name)) if name == "trace"
    ));
}

/// Provider overlays cannot ambiguously redefine the same provider name.
#[test]
fn catalog_rejects_duplicate_static_providers() {
    let provider = ExtensionProviderRegistration {
        name: "custom".to_string(),
        config: serde_json::json!({ "base_url": "https://example.invalid" }),
    };
    let entries = ["first", "second"]
        .into_iter()
        .map(|value| {
            let declared = descriptor(value);
            let module_descriptor = declared.clone();
            CompiledExtension::new(
                declared.clone(),
                StaticExtensionRegistration::builder()
                    .extensions(vec![declared])
                    .providers(vec![provider.clone()])
                    .build(),
                Arc::new(move || {
                    Ok(Arc::new(TestModule(module_descriptor.clone()))
                        as Arc<dyn ExtensionModule>)
                }),
            )
        })
        .collect();
    let catalog = ExtensionCatalog::new(entries).expect("catalogue");

    assert!(matches!(
        catalog.select(&[id("first"), id("second")]),
        Err(ExtensionError::DuplicateProvider(name)) if name == "custom"
    ));
}

/// Duplicate providers inside one extension are rejected before catalogue merging.
#[test]
fn catalog_rejects_duplicate_static_providers_within_one_extension() {
    let declared = descriptor("first");
    let module_descriptor = declared.clone();
    let provider = ExtensionProviderRegistration {
        name: "custom".to_string(),
        config: serde_json::json!({ "base_url": "https://example.invalid" }),
    };
    let entry = CompiledExtension::new(
        declared.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![declared])
            .providers(vec![provider.clone(), provider])
            .build(),
        Arc::new(move || {
            Ok(Arc::new(TestModule(module_descriptor.clone()))
                as Arc<dyn ExtensionModule>)
        }),
    );
    let catalog = ExtensionCatalog::new(vec![entry]).expect("catalogue");

    assert!(matches!(
        catalog.select(&[id("first")]),
        Err(ExtensionError::DuplicateProvider(name)) if name == "custom"
    ));
}
