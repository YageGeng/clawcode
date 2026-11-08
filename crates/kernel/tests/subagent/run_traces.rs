//! Subagent traces: a foreground child continues its parent's trace, and a
//! background child starts a new trace that names its parent's.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use kernel::WhenBusy;
use protocol::{SessionId, StopReason};
use tokio::sync::Notify;

use super::background::{background_test, invoke};
use super::fixture::{Route, TestKernel, finished, request, text};
use super::foreground::delegate;
use super::spans::{line_with, run_fields};

/// Captures only this test's logs.
///
/// Tests in this binary run in parallel and the fixture's sequential ids repeat
/// across them, so a process-wide buffer could attribute another test's line.
/// `#[tokio::test]` runs every task on the test's thread, which a thread-local
/// subscriber therefore sees.
#[derive(Clone, Default)]
struct ThreadLogs(Arc<Mutex<Vec<u8>>>);

impl ThreadLogs {
    /// Installs a thread-local subscriber writing here; the guard keeps it active.
    fn install(&self) -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(self.clone())
                .finish(),
        )
    }

    /// Returns everything written so far.
    fn content(&self) -> String {
        String::from_utf8(self.0.lock().expect("log lock").clone())
            .expect("UTF-8 logs")
    }
}

/// One writer handle sharing the capture buffer.
struct ThreadLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for ThreadLogWriter {
    /// Appends one formatted line to the shared buffer.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
            .write(buffer)
    }

    /// The in-memory buffer has nothing to flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for ThreadLogs {
    type Writer = ThreadLogWriter;

    /// Creates a writer sharing this capture's buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        ThreadLogWriter(Arc::clone(&self.0))
    }
}

/// The text that marks a Run start line for one Session.
fn run_start(session: &SessionId) -> String {
    format!("started Kernel Run for session {session} ")
}

/// Returns the lines that start a Run for one Session, in log order.
fn run_starts(output: &str, session: &SessionId) -> Vec<String> {
    let needle = run_start(session);
    output
        .lines()
        .filter(|line| line.contains(&needle))
        .map(str::to_string)
        .collect()
}

/// Builds a parent that delegates the given tasks in one Tool batch.
fn delegating(tasks: &'static [&'static str]) -> TestKernel {
    TestKernel::new(move |r| {
        let input = r.messages.last().map(|m| text(&m.content));
        match input.as_deref() {
            Some("delegate") => Route::Events(
                tasks
                    .iter()
                    .enumerate()
                    .map(|(index, task)| {
                        delegate(task, &format!("call-{index}"))
                    })
                    .chain([finished(StopReason::ToolUse)])
                    .collect(),
            ),
            Some(task) if tasks.contains(&task) => {
                Route::answer("child answer")
            }
            _ => Route::answer("done"),
        }
    })
}

/// A foreground child Run continues the parent's trace under a Run of its own.
#[tokio::test]
async fn foreground_child_shares_the_parent_trace() {
    let logs = ThreadLogs::default();
    let _guard = logs.install();
    let test = delegating(&["child task"]);
    let owner = test.create("owner").await;
    test.kernel
        .submit(request(&owner, "delegate", WhenBusy::FollowUp))
        .await
        .expect("submit");
    let child = test.owned_children(&owner)[0].session_id.clone();

    let output = logs.content();
    let (parent_trace, parent_run) = run_fields(
        &line_with(&output, &run_start(&owner)).expect("parent start"),
    );
    let child_start =
        &line_with(&output, &run_start(&child)).expect("child start");
    let (child_trace, child_run) = run_fields(child_start);
    assert_eq!(child_trace, parent_trace, "{child_start}");
    assert_ne!(child_run, parent_run, "{child_start}");
    assert!(
        child_start.contains("as foreground subagent"),
        "{child_start}"
    );
    test.kernel.close_session(&owner).await.expect("close");
}

/// Concurrent foreground children share the parent's trace with distinct Runs.
#[tokio::test]
async fn concurrent_foreground_children_keep_distinct_runs() {
    let logs = ThreadLogs::default();
    let _guard = logs.install();
    let test = delegating(&["first task", "second task"]);
    let owner = test.create("owner").await;
    test.kernel
        .submit(request(&owner, "delegate", WhenBusy::FollowUp))
        .await
        .expect("submit");
    let children = test.owned_children(&owner);
    assert_eq!(children.len(), 2);

    let output = logs.content();
    let parent_trace = run_fields(
        &line_with(&output, &run_start(&owner)).expect("parent start"),
    )
    .0;
    let child_fields = children
        .iter()
        .map(|child| {
            run_fields(
                &line_with(&output, &run_start(&child.session_id))
                    .expect("child start"),
            )
        })
        .collect::<Vec<_>>();
    assert!(child_fields.iter().all(|(trace, _)| trace == &parent_trace));
    assert_ne!(child_fields[0].1, child_fields[1].1);
    test.kernel.close_session(&owner).await.expect("close");
}

/// A background child starts a new trace whose start line names the parent's.
#[tokio::test]
async fn background_child_starts_a_linked_trace() {
    let logs = ThreadLogs::default();
    let _guard = logs.install();
    let test = background_test(WhenBusy::default(), |_| {
        Route::answer("background answer")
    });
    let owner = test.create("owner").await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"tracer","message":"Trace me."}),
    )
    .await;
    let child = test.owned_children(&owner)[0].session_id.clone();
    test.wait_until(|| {
        test.user_texts(&owner)
            .iter()
            .any(|text| text.starts_with("[subagent tracer"))
    })
    .await;

    let output = logs.content();
    let parent_trace = run_fields(
        &line_with(&output, &run_start(&owner)).expect("parent start"),
    )
    .0
    .expect("parent trace");
    let child_start =
        &line_with(&output, &run_start(&child)).expect("child start");
    assert!(
        child_start.contains(&format!(
            "as background subagent linked to trace {parent_trace}"
        )),
        "{child_start}"
    );
    assert_ne!(
        run_fields(child_start).0,
        Some(parent_trace),
        "{child_start}"
    );
    test.kernel.close_session(&owner).await.expect("close");
}

/// A message to a busy background child joins its Run; the Reporter's line links the traces.
#[tokio::test]
async fn background_message_to_a_busy_child_is_linked_by_the_reporter() {
    let logs = ThreadLogs::default();
    let _guard = logs.install();
    let gate = Arc::new(Notify::new());
    let hold = Arc::clone(&gate);
    let test = background_test(WhenBusy::default(), move |input| {
        if input.starts_with("You are") {
            Route::Wait(Arc::clone(&hold), vec![finished(StopReason::EndTurn)])
        } else {
            Route::answer("noted")
        }
    });
    let owner = test.create("owner").await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"spawn","name":"tracer","message":"x"}),
    )
    .await;
    let child = test.owned_children(&owner)[0].session_id.clone();
    test.wait_until(|| {
        test.kernel
            .session_runtime(&child)
            .expect("runtime")
            .running
    })
    .await;
    invoke(
        &test,
        &owner,
        serde_json::json!({"action":"send","name":"tracer","message":"more"}),
    )
    .await;
    test.wait_until(|| {
        test.kernel
            .pending_messages(&child)
            .expect("queue")
            .steering
            .len()
            == 1
    })
    .await;
    gate.notify_one();
    test.wait_until(|| {
        test.user_texts(&owner)
            .iter()
            .any(|text| text.starts_with("[subagent "))
    })
    .await;

    let output = logs.content();
    assert_eq!(run_starts(&output, &child).len(), 1);
    let owner_traces = run_starts(&output, &owner)
        .iter()
        .filter_map(|line| run_fields(line).0)
        .collect::<Vec<_>>();
    let deliveries = output
        .lines()
        .filter(|line| {
            line.contains("started subagent delivery from trace")
                && line.ends_with(&format!("to session {child}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(deliveries.len(), 2, "{output}");
    for delivery in deliveries {
        assert!(
            owner_traces
                .iter()
                .any(|trace| delivery.contains(&format!("from trace {trace} "))),
            "{delivery}"
        );
    }
    test.kernel.close_session(&owner).await.expect("close");
}
