//! Shared scripting for agent-tool tests: owners and children call tools by
//! naming them in their input, children answer through a per-test route.

use std::sync::Arc;

use protocol::{ModelRequest, SessionId, SessionSummary};
use tokio::sync::Notify;

use super::fixture::{
    KernelPolicy, Route, TestKernel, calls, finished, text, tool_result,
};

/// Reads the last message text of one provider request, skipping the hidden
/// roster and command-session notes the Kernel states before a request.
pub fn last(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|message| {
            !matches!(&message.content, protocol::MessageContent::Extension { extension }
                if !extension.display)
        })
        .map(|message| text(&message.content))
        .unwrap_or_default()
}

/// A provider answer held until the test releases `gate`.
pub fn held(gate: &Arc<Notify>, answer: &str) -> Route {
    Route::Wait(
        Arc::clone(gate),
        vec![
            protocol::ModelStreamEvent::TextDelta(answer.into()),
            finished(protocol::StopReason::EndTurn),
        ],
    )
}

/// Turns `call:<json [[id, tool, args], ...]>` anywhere in `input` into tool calls.
fn scripted_calls(input: &str) -> Option<Route> {
    let json = input.split_once("call:")?.1;
    let specs: Vec<(String, String, serde_json::Value)> =
        serde_json::from_str(json).expect("call specs");
    let specs = specs
        .iter()
        .map(|(id, tool, args)| (id.as_str(), tool.as_str(), args.clone()))
        .collect::<Vec<_>>();
    Some(calls(&specs))
}

/// Builds a Kernel whose owners and children run scripted tool calls; any
/// other child task is answered by `child`, any other owner input by "owner answer".
pub fn agents(
    policy: KernelPolicy,
    child: impl Fn(&ModelRequest) -> Route + Send + Sync + 'static,
) -> TestKernel {
    TestKernel::with_policy(
        move |request| {
            let input = last(request);
            if let Some(route) = scripted_calls(&input) {
                route
            } else if input.starts_with("Message Type: new_task") {
                child(request)
            } else {
                Route::answer("owner answer")
            }
        },
        policy,
    )
}

/// Runs one prompt to completion and returns its Run.
pub async fn prompt(
    test: &TestKernel,
    session: &SessionId,
    input: &str,
) -> protocol::RunResult {
    test.start(session, input)
        .await
        .expect("join")
        .expect("run")
}

/// Runs scripted tool calls from `session` and returns each call's result text.
pub async fn call(
    test: &TestKernel,
    session: &SessionId,
    specs: serde_json::Value,
) -> Vec<String> {
    let ids = specs
        .as_array()
        .expect("specs")
        .iter()
        .map(|spec| spec[0].as_str().expect("id").to_string())
        .collect::<Vec<_>>();
    prompt(test, session, &format!("call:{specs}")).await;
    ids.iter()
        .map(|id| tool_result(test, session, id).0)
        .collect()
}

/// Spawns children from `owner`; `specs` is `[[id, args], ...]`.
pub async fn spawn(
    test: &TestKernel,
    owner: &SessionId,
    specs: serde_json::Value,
) -> Vec<String> {
    let calls = specs
        .as_array()
        .expect("specs")
        .iter()
        .map(|spec| {
            serde_json::json!([spec[0], protocol::SPAWN_AGENT_TOOL, spec[1]])
        })
        .collect::<Vec<_>>();
    call(test, owner, serde_json::Value::Array(calls)).await
}

/// The spawned child of `owner` named `name`.
pub fn child(
    test: &TestKernel,
    owner: &SessionId,
    name: &str,
) -> SessionSummary {
    test.owned_children(owner)
        .into_iter()
        .find(|summary| {
            summary
                .ownership
                .as_ref()
                .is_some_and(|edge| edge.name.as_ref() == name)
        })
        .expect("child")
}
