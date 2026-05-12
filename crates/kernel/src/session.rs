//! Session lifecycle: channel-backed handles, background task, and event stream.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use protocol::{Event, KernelError, Op, SessionId, StopReason};
use provider::factory::ArcLlm;
use tokio::sync::{mpsc, watch};

use crate::context::ContextManager;
use crate::tool::ToolRegistry;
use crate::turn::{TurnContext, execute_turn};

/// Frontend handle for a live session.
#[derive(Clone)]
pub struct Thread {
    pub session_id: SessionId,
    /// Send operations to the background task.
    pub(crate) tx_op: mpsc::UnboundedSender<Op>,
    /// Shared sender for per-turn events.
    /// The background task and handle swap this for each prompt
    /// so every call gets a fresh connected receiver.
    pub(crate) tx_event: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<Event>>>,
    /// Signal cancellation.
    pub(crate) cancel_tx: watch::Sender<bool>,
}

impl Thread {
    /// Create a new event receiver for this prompt and wire it up.
    pub(crate) async fn take_rx(&self) -> mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut guard = self.tx_event.lock().await;
        *guard = tx;
        rx
    }
}

/// Runtime state owned by the background task of a single session.
pub(crate) struct Session {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub rx_op: mpsc::UnboundedReceiver<Op>,
    pub tx_event: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<Event>>>,
    #[allow(dead_code)]
    pub cancel_rx: watch::Receiver<bool>,
    pub context: Box<dyn ContextManager>,
    pub llm: ArcLlm,
    pub tools: Arc<ToolRegistry>,
}

/// Spawn the background task for a session and return the frontend handle.
pub(crate) fn spawn_thread(
    session_id: SessionId,
    cwd: PathBuf,
    llm: ArcLlm,
    tools: Arc<ToolRegistry>,
    context: Box<dyn ContextManager>,
) -> Thread {
    let (tx_op, rx_op) = mpsc::unbounded_channel();
    // Create a dummy connected pair so the shared tx_event is always valid
    let (initial_tx, _initial_rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);

    let tx_event = Arc::new(tokio::sync::Mutex::new(initial_tx));

    let runtime = Session {
        session_id: session_id.clone(),
        cwd,
        rx_op,
        tx_event: tx_event.clone(),
        cancel_rx,
        context,
        llm,
        tools,
    };

    tokio::spawn(run_loop(runtime));

    Thread {
        session_id,
        tx_op,
        tx_event,
        cancel_tx,
    }
}

// We need a placeholder — actually let me just drop the rx.
// Wait, the initial rx_event from the channel is never used... it can be dropped.

/// Background task: receive ops, execute turns, emit events.
async fn run_loop(mut rt: Session) {
    loop {
        let op = rt.rx_op.recv().await;
        match op {
            Some(Op::Prompt { text, .. }) => {
                let ctx = TurnContext::builder()
                    .session_id(rt.session_id.clone())
                    .llm(rt.llm.clone())
                    .tools(rt.tools.clone())
                    .cwd(rt.cwd.clone())
                    .build();

                let tx = { rt.tx_event.lock().await.clone() };

                if let Err(e) = execute_turn(&ctx, text, &mut rt.context, &tx).await {
                    let _ = tx.send(Event::TurnComplete {
                        session_id: rt.session_id.clone(),
                        stop_reason: StopReason::Error,
                    });
                    tracing::error!(
                        session_id = %rt.session_id,
                        error = %e,
                        "Turn execution failed"
                    );
                } else {
                    let _ = tx.send(Event::TurnComplete {
                        session_id: rt.session_id.clone(),
                        stop_reason: StopReason::EndTurn,
                    });
                }
            }
            Some(Op::Cancel { .. }) | Some(Op::CloseSession { .. }) | None => break,
            _ => {}
        }
    }
}

/// Build an [`EventStream`] from the session's event receiver and cancel watch.
///
/// The stream terminates when `TurnComplete` arrives or cancellation is signaled.
pub(crate) fn event_stream(
    mut rx_event: mpsc::UnboundedReceiver<Event>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Pin<Box<dyn Stream<Item = Result<Event, KernelError>> + Send + 'static>> {
    Box::pin(async_stream::stream! {
        loop {
            tokio::select! {
                event = rx_event.recv() => {
                    match event {
                        Some(e @ Event::TurnComplete { .. }) => {
                            yield Ok(e);
                            break;
                        }
                        Some(e) => yield Ok(e),
                        None => break,
                    }
                }
                _ = cancel_rx.changed() => {
                    yield Err(KernelError::Cancelled);
                    break;
                }
            }
        }
    })
}
