use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use extension::{
    ContextPoint, ExtensionCommandContext, ExtensionCommandHandler,
    ExtensionContext, ExtensionError, ExtensionFactory, ExtensionHandler,
    ExtensionModule, ExtensionRegistrar, StaticExtensionFactory,
    TurnStartPoint,
};
use protocol::{
    ContextEvent, ContextResult, ExtensionCommandDefinition,
    ExtensionDescriptor, ExtensionFlagDefinition, ExtensionFlagKind,
    ExtensionId, StaticExtensionRegistration, ToolCall, ToolDefinition,
    ToolResult, TurnStartEvent,
};
use tools::{AgentTool, ToolError, ToolExecutionContext};

/// Context handler used to exercise typed registration.
struct ContextHandler;

#[async_trait]
impl ExtensionHandler<ContextPoint> for ContextHandler {
    /// Leaves the current model context unchanged.
    async fn handle(
        &self,
        _event: &ContextEvent,
        _context: &ExtensionContext,
    ) -> Result<ContextResult, ExtensionError> {
        Ok(ContextResult::default())
    }
}

/// Turn observer used to prove handlers for different points stay separate.
struct TurnHandler;

#[async_trait]
impl ExtensionHandler<TurnStartPoint> for TurnHandler {
    /// Observes a turn without returning a transformation.
    async fn handle(
        &self,
        _event: &TurnStartEvent,
        _context: &ExtensionContext,
    ) -> Result<(), ExtensionError> {
        Ok(())
    }
}

/// Module with deterministic typed handler registration.
struct TestModule {
    id: &'static str,
    version: String,
    context_handlers: usize,
}

impl ExtensionModule for TestModule {
    /// Returns stable module metadata used as every handler source.
    fn descriptor(&self) -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from(self.id).expect("extension id"),
            name: self.id.to_string(),
            version: self.version.clone(),
        }
    }

    /// Registers the configured number of context handlers and one turn observer.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        for _index in 0..self.context_handlers {
            registrar.on::<ContextPoint, _>(ContextHandler)?;
        }
        registrar.on::<TurnStartPoint, _>(TurnHandler)
    }
}

/// Typed registries preserve point separation, source identity, and module order.
#[test]
fn registrar_freezes_handlers_in_registration_order() {
    let mut registrar = ExtensionRegistrar::new();
    registrar
        .register_module(&TestModule {
            id: "first",
            version: "1".to_string(),
            context_handlers: 2,
        })
        .expect("register first module");
    registrar
        .register_module(&TestModule {
            id: "second",
            version: "1".to_string(),
            context_handlers: 1,
        })
        .expect("register second module");
    let registry = registrar.freeze();

    assert_eq!(registry.handler_count::<ContextPoint>(), 3);
    assert_eq!(registry.handler_count::<TurnStartPoint>(), 2);
    assert_eq!(
        registry.handler_sources::<ContextPoint>(),
        vec!["first", "first", "second"]
    );
}

/// Registration rejects duplicate extension identities and duplicate global flags.
#[test]
fn registrar_rejects_build_time_conflicts() {
    let mut registrar = ExtensionRegistrar::new();
    let module = TestModule {
        id: "audit",
        version: "1".to_string(),
        context_handlers: 1,
    };
    registrar
        .register_module(&module)
        .expect("first module registration");
    assert!(matches!(
        registrar.register_module(&module),
        Err(ExtensionError::DuplicateExtension(_))
    ));

    let flag = ExtensionFlagDefinition::builder()
        .name("trace".to_string())
        .kind(ExtensionFlagKind::Boolean)
        .build();
    registrar
        .register_flag(flag.clone())
        .expect("first flag registration");
    assert!(matches!(
        registrar.register_flag(flag),
        Err(ExtensionError::DuplicateFlag(_))
    ));
}

/// One extension cannot register the same command name twice.
#[test]
fn registrar_rejects_duplicate_commands_within_one_extension() {
    struct CommandModule;
    struct CommandHandler;

    #[async_trait]
    impl ExtensionCommandHandler for CommandHandler {
        /// Accepts command input without performing host actions.
        async fn handle(
            &self,
            _arguments: &str,
            _parameters: &serde_json::Value,
            _context: &ExtensionCommandContext,
        ) -> Result<(), ExtensionError> {
            Ok(())
        }
    }

    impl ExtensionModule for CommandModule {
        /// Returns the command module descriptor.
        fn descriptor(&self) -> ExtensionDescriptor {
            ExtensionDescriptor {
                id: ExtensionId::try_from("commands").expect("extension id"),
                name: "Commands".to_string(),
                version: "1".to_string(),
            }
        }

        /// Attempts to register the same command definition twice.
        fn register(
            &self,
            registrar: &mut ExtensionRegistrar,
        ) -> Result<(), ExtensionError> {
            let command = ExtensionCommandDefinition {
                name: "inspect".to_string(),
                description: None,
            };
            registrar.register_command(command.clone(), CommandHandler)?;
            registrar.register_command(command, CommandHandler)
        }
    }

    let mut registrar = ExtensionRegistrar::new();
    assert!(matches!(
        registrar.register_module(&CommandModule),
        Err(ExtensionError::DuplicateCommand { .. })
    ));
}

/// Static factories construct new module objects for every session runtime.
#[test]
fn static_factory_creates_fresh_modules_per_session() {
    let serial = Arc::new(AtomicUsize::new(0));
    let module_serial = Arc::clone(&serial);
    let factory = StaticExtensionFactory::new(
        StaticExtensionRegistration::default(),
        vec![Arc::new(move || {
            let version = module_serial.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(TestModule {
                id: "fresh",
                version: version.to_string(),
                context_handlers: 1,
            }) as Arc<dyn ExtensionModule>)
        })],
    );

    let first = factory.create_modules().expect("first session modules");
    let second = factory.create_modules().expect("second session modules");
    assert_eq!(first[0].descriptor().version, "0");
    assert_eq!(second[0].descriptor().version, "1");
}

/// A module that fails after writing candidate state must leave no registrations behind.
#[test]
fn registrar_rolls_back_every_registration_from_a_failed_module() {
    struct FailingModule;

    impl ExtensionModule for FailingModule {
        /// Reuses the identity later registered successfully.
        fn descriptor(&self) -> ExtensionDescriptor {
            ExtensionDescriptor {
                id: ExtensionId::try_from("transactional")
                    .expect("extension id"),
                name: "Transactional".to_string(),
                version: "1".to_string(),
            }
        }

        /// Writes one handler before returning the expected registration error.
        fn register(
            &self,
            registrar: &mut ExtensionRegistrar,
        ) -> Result<(), ExtensionError> {
            registrar.on::<ContextPoint, _>(ContextHandler)?;
            Err(ExtensionError::Factory("expected failure".to_string()))
        }
    }

    let mut registrar = ExtensionRegistrar::new();
    assert!(registrar.register_module(&FailingModule).is_err());
    registrar
        .register_module(&TestModule {
            id: "transactional",
            version: "2".to_string(),
            context_handlers: 1,
        })
        .expect("retry registration");

    let registry = registrar.freeze();
    assert_eq!(registry.handler_count::<ContextPoint>(), 1);
    assert_eq!(
        registry.handler_sources::<ContextPoint>(),
        vec!["transactional"]
    );
}

/// Minimal tool used to observe duplicate-name registration order.
struct NamedTool(&'static str);

#[async_trait]
impl AgentTool for NamedTool {
    /// Exposes the configured stable tool name.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.0.to_string(),
            description: self.0.to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    /// Is never executed by this registration-only test.
    async fn execute(
        &self,
        call: ToolCall,
        _context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::Execution {
            tool: call.name,
            message: "not executed".to_string(),
        })
    }
}

/// Static extension tools use deterministic first-registration-wins semantics.
#[test]
fn registrar_keeps_the_first_extension_tool_with_each_name() {
    struct ToolModule {
        id: &'static str,
        description: &'static str,
    }

    impl ExtensionModule for ToolModule {
        /// Returns the configured owner identity.
        fn descriptor(&self) -> ExtensionDescriptor {
            ExtensionDescriptor {
                id: ExtensionId::try_from(self.id).expect("extension id"),
                name: self.id.to_string(),
                version: "1".to_string(),
            }
        }

        /// Registers one colliding tool definition.
        fn register(
            &self,
            registrar: &mut ExtensionRegistrar,
        ) -> Result<(), ExtensionError> {
            registrar.register_tool(Arc::new(NamedTool(self.description)))
        }
    }

    let mut registrar = ExtensionRegistrar::new();
    registrar
        .register_module(&ToolModule {
            id: "first",
            description: "shared",
        })
        .expect("first tool");
    registrar
        .register_module(&ToolModule {
            id: "second",
            description: "shared",
        })
        .expect("second tool");

    let tools = registrar.freeze().tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].0.as_str(), "first");
}
