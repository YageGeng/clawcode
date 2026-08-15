use std::sync::Arc;

use async_trait::async_trait;
use extension::{
    CompiledExtension, ExtensionContext, ExtensionError, ExtensionHandler,
    ExtensionModule, ExtensionRegistrar, ToolCallPoint, UserBashPoint,
};
use protocol::{
    ExtensionDescriptor, ExtensionId, StaticExtensionRegistration, ToolBlock,
    ToolCallEvent, ToolCallResult, UserBashDisposition, UserBashEvent,
    UserBashResult,
};

/// Stable identifier shared by build configuration and runtime metadata.
pub const EXTENSION_ID: &str = "command-guard";

const REJECTED_MESSAGE: &str = "destructive command rejected by command guard";

/// Extension module that installs destructive-command policy hooks.
struct CommandGuard;

impl CommandGuard {
    /// Builds the stable descriptor used by static and per-session registration.
    fn extension_descriptor() -> ExtensionDescriptor {
        ExtensionDescriptor {
            id: ExtensionId::try_from(EXTENSION_ID)
                .expect("command guard extension id is valid"),
            name: "Command guard".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl ExtensionModule for CommandGuard {
    /// Returns metadata that must match the generated compiled declaration.
    fn descriptor(&self) -> ExtensionDescriptor {
        Self::extension_descriptor()
    }

    /// Registers policy before either server-side bash execution path.
    fn register(
        &self,
        registrar: &mut ExtensionRegistrar,
    ) -> Result<(), ExtensionError> {
        registrar.on::<ToolCallPoint, _>(CommandPolicy)?;
        registrar.on::<UserBashPoint, _>(CommandPolicy)
    }
}

/// Shared policy applied to model-authored and user-authored shell commands.
struct CommandPolicy;

impl CommandPolicy {
    /// Detects direct recursive and forced rm invocations in shell segments.
    fn rejects(command: &str) -> bool {
        ShellCommandLine::from(command)
            .invocations
            .iter()
            .any(CommandInvocation::is_recursive_forced_remove)
    }
}

/// Parsed direct commands separated outside shell quotes.
struct ShellCommandLine {
    invocations: Vec<CommandInvocation>,
}

impl From<&str> for ShellCommandLine {
    /// Splits shell control segments before applying shlex word parsing.
    fn from(command: &str) -> Self {
        let mut segments = Vec::new();
        let mut current = String::new();
        let mut quote = None;
        let mut escaped = false;
        for character in command.chars() {
            if escaped {
                current.push(character);
                escaped = false;
                continue;
            }
            if character == '\\' {
                current.push(character);
                escaped = true;
                continue;
            }
            if let Some(active_quote) = quote {
                current.push(character);
                if character == active_quote {
                    quote = None;
                }
                continue;
            }
            if matches!(character, '\'' | '"') {
                quote = Some(character);
                current.push(character);
                continue;
            }
            if matches!(character, ';' | '|' | '&' | '\n') {
                if !current.trim().is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
                continue;
            }
            current.push(character);
        }
        if !current.trim().is_empty() {
            segments.push(current);
        }
        Self {
            invocations: segments
                .into_iter()
                .filter_map(|segment| {
                    CommandInvocation::try_from(segment.as_str()).ok()
                })
                .collect(),
        }
    }
}

/// One directly executable shell command and its parsed arguments.
struct CommandInvocation {
    executable: String,
    arguments: Vec<String>,
}

impl TryFrom<&str> for CommandInvocation {
    type Error = ();

    /// Parses one shell segment without evaluating expansions or substitutions.
    fn try_from(segment: &str) -> Result<Self, Self::Error> {
        let mut words = shlex::split(segment).ok_or(())?.into_iter();
        let executable = words.next().ok_or(())?;
        Ok(Self {
            executable,
            arguments: words.collect(),
        })
    }
}

impl CommandInvocation {
    /// Returns whether this direct invocation combines recursive and force rm flags.
    fn is_recursive_forced_remove(&self) -> bool {
        if std::path::Path::new(&self.executable)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            != Some("rm")
        {
            return false;
        }
        let mut recursive = false;
        let mut forced = false;
        for argument in &self.arguments {
            if argument == "--" {
                break;
            }
            if argument == "--recursive" {
                recursive = true;
            } else if argument == "--force" {
                forced = true;
            } else if let Some(flags) = argument.strip_prefix('-') {
                recursive |=
                    flags.chars().any(|flag| matches!(flag, 'r' | 'R'));
                forced |= flags.contains('f');
            }
        }
        recursive && forced
    }
}

#[async_trait]
impl ExtensionHandler<ToolCallPoint> for CommandPolicy {
    /// Blocks destructive built-in bash calls before validation and execution.
    async fn handle(
        &self,
        event: &ToolCallEvent,
        _context: &ExtensionContext,
    ) -> Result<ToolCallResult, ExtensionError> {
        let command = event
            .call
            .arguments
            .get("command")
            .and_then(serde_json::Value::as_str);
        if event.call.name == "bash" && command.is_some_and(Self::rejects) {
            return Ok(ToolCallResult::Block(
                ToolBlock::builder()
                    .reason(REJECTED_MESSAGE.to_string())
                    .terminate(false)
                    .build(),
            ));
        }
        Ok(ToolCallResult::Continue)
    }
}

#[async_trait]
impl ExtensionHandler<UserBashPoint> for CommandPolicy {
    /// Returns a policy result so destructive user bash never starts a process.
    async fn handle(
        &self,
        event: &UserBashEvent,
        _context: &ExtensionContext,
    ) -> Result<Option<UserBashResult>, ExtensionError> {
        Ok(Self::rejects(&event.command).then(|| {
            UserBashResult::builder()
                .disposition(UserBashDisposition::Blocked {
                    reason: REJECTED_MESSAGE.to_string(),
                })
                .output(REJECTED_MESSAGE.to_string())
                .cancelled(false)
                .truncated(false)
                .build()
        }))
    }
}

/// Describes the module and creates fresh policy instances for each session.
pub(crate) fn definition() -> CompiledExtension {
    let descriptor = CommandGuard::extension_descriptor();
    CompiledExtension::new(
        descriptor.clone(),
        StaticExtensionRegistration::builder()
            .extensions(vec![descriptor])
            .build(),
        Arc::new(|| Ok(Arc::new(CommandGuard) as Arc<dyn ExtensionModule>)),
    )
}
