//! Type-driven agent kernel and pi-compatible Turn state machine.

mod id;
mod model;
mod provider;
mod runtime;
mod sink;

pub use id::NanoidIdGenerator;
pub use model::{Model, ModelCatalog, ModelError, ModelFactory, ModelStream};
pub use protocol::{PendingMessages, QueueKind, QueuedMessage};
pub use provider::{ProviderModel, ProviderModelFactory};
pub use runtime::{
    AdmissionTicket, CheckpointPoint, CheckpointStage, ContextUsageEstimate,
    InsertionNotice, InsertionReceiver, Kernel, KernelError, KernelFactory,
    MailDelivery, MailPlacement, OwnedSessionRequest, PromptOutcome,
    PromptRequest, RunCheckpoints, RunSpan, TreeNavigationRequest, WhenBusy,
};
pub use sink::{ChannelEventSink, EventSink, SessionEventSinks, SinkError};
