//! Provider-agnostic completion and chat abstractions.
//!
//! This module contains the low-level request and response types used by provider
//! implementations, plus the high-level traits most callers use through
//! the `Prompt`, `Chat`, `TypedPrompt`, `Completion`, and `CompletionModel` traits:
//!
//! - [`Prompt`] sends one user prompt and returns assistant text.
//! - [`Chat`] sends a prompt with existing history and returns assistant text.
//! - [`TypedPrompt`] requests structured output and deserializes it into a Rust type.
//! - [`Completion`] exposes a request builder for call-site overrides.
//! - [`CompletionModel`] is the provider-facing trait implemented by completion models.
//!
//! `CompletionRequest` is Rig's canonical request representation. Provider modules
//! translate it into provider-specific request bodies and convert responses back into
//! [`CompletionResponse`].
//!
pub mod message;
pub mod request;

pub use message::{AssistantContent, Message, MessageError};
pub use request::*;
