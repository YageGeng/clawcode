//! Internal protocol types for clawcode agent-core / frontend communication.
//!
//! Uses a Submission Queue (SQ) / Event Queue (EQ) pattern:
//! - The frontend sends [`Op`] submissions to the kernel.
//! - The kernel streams [`Event`]s back through an event channel.
//!
//! These types are designed so they can be bridged to ACP
//! (`agent-client-protocol`) for IDE-native UI rendering.

pub mod acp_conv;
pub mod agent;
pub mod config;
pub mod event;
pub mod kernel;
pub mod op;
pub mod permission;
pub mod plan;
pub mod session;
pub mod tool;

pub use agent::*;
pub use config::*;
pub use event::*;
pub use kernel::*;
pub use op::*;
pub use permission::*;
pub use plan::*;
pub use session::*;
pub use tool::*;
