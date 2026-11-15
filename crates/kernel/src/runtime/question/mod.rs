//! Codex `request_user_input_async`: questions the model asks without waiting.

mod dismiss;
mod open;
mod tool;

pub(in crate::runtime) use open::QuestionLedger;

use std::sync::Arc;

use crate::runtime::{Kernel, KernelError, Session};

/// Registers `request_user_input_async` as the kernel's own tool for one Session.
///
/// Children lose it through `ROOT_ONLY_TOOLS`, like `spawn_agent`.
pub(in crate::runtime) fn register(
    runtime: &Arc<Session>,
    kernel: &Kernel,
) -> Result<(), KernelError> {
    runtime.tools.register_kernel(Arc::new(
        tool::RequestUserInputAsyncTool {
            id_generator: Arc::clone(&kernel.id_generator),
        },
    ))?;
    Ok(())
}
