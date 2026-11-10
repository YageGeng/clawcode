//! Wire shapes of the unified exec tool results and process lifecycle events.

use protocol::{
    AgentEventPayload, ExecExit, ToolCallId, ToolResultDetails, TurnId,
};

/// Exec results are tagged like every other tool detail and keep snake_case fields.
#[test]
fn exec_details_serialize_with_type_tag() {
    let details = ToolResultDetails::Exec {
        process_id: Some(48213),
        exit_code: None,
        signal: None,
        chunk_id: "3fa2c1".to_string(),
        wall_time_ms: 10_002,
        original_token_count: 12,
        full_output_path: None,
        terminal_call_id: ToolCallId::try_from("call-1").expect("call id"),
    };

    let json = serde_json::to_value(&details).expect("serialize details");

    assert_eq!(json["type"], "exec");
    assert_eq!(json["process_id"], 48213);
    assert_eq!(json["terminal_call_id"], "call-1");
    let back: ToolResultDetails =
        serde_json::from_value(json).expect("deserialize details");
    assert_eq!(back, details);
}

/// A background exit event round-trips and an omitted `lost` means a real exit.
#[test]
fn exec_exited_event_round_trips() {
    let exit = ExecExit::builder()
        .tool_call_id(ToolCallId::try_from("call-1").expect("call id"))
        .turn_id(TurnId::try_from("turn-1").expect("turn id"))
        .process_id(48213)
        .exit_code(Some(0))
        .output("done\n".to_string())
        .wall_time_ms(1_500)
        .build();
    let payload = AgentEventPayload::ExecExited { exit: exit.clone() };

    let mut json = serde_json::to_value(&payload).expect("serialize event");

    assert_eq!(json["event"], "exec_exited");
    assert_eq!(json["exit"]["process_id"], 48213);
    let back: AgentEventPayload =
        serde_json::from_value(json.clone()).expect("deserialize event");
    assert_eq!(back, payload);
    json["exit"]
        .as_object_mut()
        .expect("exit object")
        .remove("lost");
    let without_lost: AgentEventPayload =
        serde_json::from_value(json).expect("deserialize without lost");
    assert_eq!(without_lost, AgentEventPayload::ExecExited { exit });
}

/// The exec extension methods parse back from their names; only listing is a query.
#[test]
fn exec_methods_parse_and_classify() {
    use protocol::AcpExtensionMethod;
    for (method, name, query) in [
        (
            AcpExtensionMethod::ExecList,
            "_clawcode/session/exec/list",
            true,
        ),
        (
            AcpExtensionMethod::ExecTerminate,
            "_clawcode/session/exec/terminate",
            false,
        ),
        (
            AcpExtensionMethod::ExecClean,
            "_clawcode/session/exec/clean",
            false,
        ),
    ] {
        assert_eq!(method.as_str(), name);
        assert_eq!(AcpExtensionMethod::parse(name), Some(method));
        assert_eq!(method.is_query(), query, "{name}");
    }
}
