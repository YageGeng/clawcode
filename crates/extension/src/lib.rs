//! Non-UI pi-compatible extension lifecycle.

mod contract;
mod pipeline;

pub use contract::{Extension, ExtensionError, ExtensionFactory};
pub use pipeline::{ExtensionPipeline, StaticExtensionFactory};
