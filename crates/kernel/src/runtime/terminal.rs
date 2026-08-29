use super::*;

impl Kernel {
    /// Returns one non-consuming snapshot of retained terminals for a Session.
    pub fn terminals(
        &self,
        session_id: &SessionId,
    ) -> Result<protocol::TerminalListResult, KernelError> {
        self.session(session_id)?
            .terminals
            .list()
            .map_err(Into::into)
    }

    /// Subscribes a host observer to coalesced terminal snapshot revisions.
    pub fn subscribe_terminals(
        &self,
        session_id: &SessionId,
    ) -> Result<tokio::sync::watch::Receiver<u64>, KernelError> {
        Ok(self.session(session_id)?.terminals.subscribe())
    }

    /// Terminates one Session-owned terminal idempotently.
    pub async fn terminate_terminal(
        &self,
        session_id: &SessionId,
        terminal_id: protocol::TerminalId,
    ) -> Result<protocol::TerminalTerminateResult, KernelError> {
        let terminals = Arc::clone(&self.session(session_id)?.terminals);
        let terminated = terminals.terminate(terminal_id).await?;
        Ok(protocol::TerminalTerminateResult { terminated })
    }

    /// Terminates and removes every retained terminal for one live Session.
    pub async fn clean_terminals(
        &self,
        session_id: &SessionId,
    ) -> Result<protocol::TerminalCleanResult, KernelError> {
        let terminals = Arc::clone(&self.session(session_id)?.terminals);
        let cleaned = terminals
            .terminate_all(protocol::TerminalRemovalReason::Cleaned)
            .await?;
        Ok(protocol::TerminalCleanResult { cleaned })
    }

    /// Permanently shuts down every terminal manager without holding the Session map lock.
    pub async fn terminate_all_terminals(&self) -> Result<usize, KernelError> {
        let managers = self
            .sessions
            .read()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .values()
            .map(|session| Arc::clone(&session.terminals))
            .collect::<Vec<_>>();
        let mut cleaned = 0_usize;
        let mut first_error = None;
        for manager in managers {
            match manager
                .shutdown(protocol::TerminalRemovalReason::KernelShutdown)
                .await
            {
                Ok(count) => cleaned = cleaned.saturating_add(count),
                Err(error) if first_error.is_none() => {
                    first_error = Some(error)
                }
                Err(_error) => {}
            }
        }
        if let Some(error) = first_error {
            Err(error.into())
        } else {
            Ok(cleaned)
        }
    }
}
