use std::sync::Arc;

use super::super::{Kernel, KernelError, Session};
use protocol::{RunId, TurnId};

impl Kernel {
    /// Captures immutable Session/Run/Turn data for fresh middleware on every HTTP attempt.
    pub(in crate::runtime) fn completion_middleware(
        &self,
        session: &Arc<Session>,
        run_id: &RunId,
        turn_id: &TurnId,
    ) -> Result<extension::ProviderMiddleware, KernelError> {
        let context =
            self.extension_context(session, Some(run_id), Some(turn_id))?;
        Ok(session
            .extensions
            .runtime_ref()
            .provider_middleware(context))
    }
}
