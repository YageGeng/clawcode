use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use protocol::{
    ContentBlock, McpArgumentInfo, McpAuthorizationRequest,
    McpAuthorizationResult, McpCapabilityCounts, McpCatalog,
    McpCatalogRevisions, McpCompletionRequest, McpCompletionResult,
    McpCompletionTarget, McpElicitationAction, McpElicitationMode,
    McpElicitationRequest, McpElicitationResult, McpFailureStage,
    McpHostRequest, McpHostResponse, McpIdentityError, McpOAuthState,
    McpOAuthStatus, McpPromptInfo, McpPromptRef, McpProtocolVersion,
    McpRequestContext, McpResourceInfo, McpResourceRef,
    McpResourceTemplateInfo, McpRoot, McpRootsRequest, McpRootsResult,
    McpSamplingRequest, McpSamplingResult, McpServerFailure, McpServerId,
    McpServerImplementation, McpServerState, McpServerStatus,
    McpSessionSnapshot, McpTaskState, McpTaskStatus, McpToolInfo, McpToolRef,
    McpTransportKind, Role, SessionId, StopReason, TimestampMs, TraceId,
    TurnId,
};

/// Exact MCP protocol versions remain explicit strings at every wire boundary.
#[test]
fn protocol_versions_use_exact_wire_values() {
    let legacy = serde_json::to_value(McpProtocolVersion::V2025_11_25)
        .expect("serialize legacy protocol");
    let modern = serde_json::to_value(McpProtocolVersion::V2026_07_28)
        .expect("serialize modern protocol");

    assert_eq!(legacy, "2025-11-25");
    assert_eq!(modern, "2026-07-28");
    assert_eq!(McpProtocolVersion::V2025_11_25.to_string(), "2025-11-25");
    assert_eq!(McpProtocolVersion::V2026_07_28.to_string(), "2026-07-28");
    assert_eq!(
        serde_json::from_value::<McpProtocolVersion>(legacy)
            .expect("deserialize legacy protocol"),
        McpProtocolVersion::V2025_11_25
    );
    assert_eq!(
        serde_json::from_value::<McpProtocolVersion>(modern)
            .expect("deserialize modern protocol"),
        McpProtocolVersion::V2026_07_28
    );
    serde_json::from_value::<McpProtocolVersion>(serde_json::json!(
        "2024-11-05"
    ))
    .expect_err("unknown MCP protocol must be rejected");
}

/// Current timestamps are created by the public scalar type and remain millisecond values.
#[test]
fn timestamp_ms_creates_current_unix_time() {
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_millis();
    let timestamp = TimestampMs::now();
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_millis();

    assert!(u128::from(timestamp.get()) >= before);
    assert!(u128::from(timestamp.get()) <= after);
}

/// Server identifiers enforce namespace-safe values and generate stable public Tool names.
#[test]
fn server_ids_are_namespace_safe() {
    for invalid in ["", "   ", "file system", "server/name", "server__nested"] {
        McpServerId::try_from(invalid).expect_err("invalid MCP server id");
    }

    let server_id =
        McpServerId::try_from("file-system_1").expect("valid server id");
    let reference = McpToolRef {
        server_id: server_id.clone(),
        remote_name: "read_file".to_string(),
    };

    assert_eq!(server_id.as_str(), "file-system_1");
    assert_eq!(server_id.as_ref(), "file-system_1");
    assert_eq!(server_id.to_string(), "file-system_1");
    assert_eq!(reference.public_name(), "mcp__file-system_1__read_file");
    assert_eq!(
        serde_json::to_value(server_id).expect("serialize server id"),
        "file-system_1"
    );
    let owned =
        McpServerId::try_from("owned".to_string()).expect("owned server id");
    assert_eq!(String::from(owned), "owned");
    assert_eq!(McpServerId::try_from("   "), Err(McpIdentityError::Empty));
}

/// Tool, Prompt, Resource, and Template entries round-trip without losing routing references.
#[test]
fn catalog_round_trips_structured_references() {
    let server_id = McpServerId::try_from("catalog").expect("server id");
    let tool = McpToolInfo::builder()
        .reference(McpToolRef {
            server_id: server_id.clone(),
            remote_name: "search".to_string(),
        })
        .public_name("mcp__catalog__search".to_string())
        .description(Some("Search documents".to_string()))
        .input_schema(serde_json::json!({
            "type": "object",
            "properties": { "query": { "type": "string" } }
        }))
        .build();
    let prompt = McpPromptInfo::builder()
        .reference(McpPromptRef {
            server_id: server_id.clone(),
            remote_name: "review".to_string(),
        })
        .title(Some("Review".to_string()))
        .description(None)
        .arguments(vec![McpArgumentInfo {
            name: "path".to_string(),
            description: Some("Path to review".to_string()),
            required: true,
        }])
        .build();
    let resource = McpResourceInfo::builder()
        .reference(McpResourceRef {
            server_id: server_id.clone(),
            remote_uri: "file:///workspace/readme.md".to_string(),
        })
        .name("readme".to_string())
        .title(None)
        .description(Some("Workspace readme".to_string()))
        .mime_type(Some("text/markdown".to_string()))
        .size(Some(128))
        .build();
    let template = McpResourceTemplateInfo::builder()
        .server_id(server_id)
        .uri_template("file:///{path}".to_string())
        .name("file".to_string())
        .title(None)
        .description(None)
        .mime_type(Some("text/plain".to_string()))
        .build();
    let catalog = McpCatalog::builder()
        .tools(vec![tool])
        .prompts(vec![prompt])
        .resources(vec![resource])
        .resource_templates(vec![template])
        .build();

    assert_eq!(
        catalog.counts(),
        McpCapabilityCounts {
            tools: 1,
            prompts: 1,
            resources: 1,
            resource_templates: 1,
        }
    );
    let encoded = serde_json::to_value(&catalog).expect("serialize catalog");
    let decoded: McpCatalog =
        serde_json::from_value(encoded).expect("deserialize catalog");
    assert_eq!(decoded, catalog);
}

/// Server status snapshots preserve revisions, safe failures, OAuth state, and string timestamps.
#[test]
fn session_snapshot_preserves_complete_status() {
    let status = McpServerStatus::builder()
        .server_id(McpServerId::try_from("remote").expect("server id"))
        .protocol(McpProtocolVersion::V2026_07_28)
        .transport(McpTransportKind::StreamableHttp)
        .state(McpServerState::Degraded)
        .implementation(Some(McpServerImplementation {
            name: "reference-server".to_string(),
            version: "1.2.3".to_string(),
        }))
        .revisions(McpCatalogRevisions {
            tools: 5,
            prompts: 3,
            resources: 8,
            resource_templates: 2,
        })
        .counts(McpCapabilityCounts {
            tools: 4,
            prompts: 2,
            resources: 7,
            resource_templates: 1,
        })
        .failure(Some(McpServerFailure {
            stage: McpFailureStage::Synchronization,
            message: "resource refresh failed".to_string(),
            occurred_at_ms: TimestampMs::from(9_007_199_254_740_999),
        }))
        .oauth(
            McpOAuthStatus::builder()
                .state(McpOAuthState::RefreshRequired)
                .scopes(vec!["resources.read".to_string()])
                .expires_at_ms(Some(TimestampMs::from(123)))
                .authorization_url(None)
                .build(),
        )
        .updated_at_ms(TimestampMs::from(9_007_199_254_741_000))
        .build();
    let snapshot = McpSessionSnapshot {
        revision: 11,
        servers: vec![status],
        catalog: McpCatalog::default(),
    };

    let encoded = serde_json::to_value(&snapshot).expect("serialize snapshot");
    assert_eq!(encoded["servers"][0]["protocol"], "2026-07-28");
    assert_eq!(encoded["servers"][0]["transport"], "streamableHttp");
    assert_eq!(encoded["servers"][0]["state"], "degraded");
    assert_eq!(
        encoded["servers"][0]["failure"]["occurredAtMs"],
        "9007199254740999"
    );
    assert_eq!(encoded["servers"][0]["updatedAtMs"], "9007199254741000");
    assert_eq!(
        serde_json::from_value::<McpSessionSnapshot>(encoded)
            .expect("deserialize snapshot"),
        snapshot
    );
}

/// Completion requests retain typed routing targets and deterministic argument context.
#[test]
fn completion_uses_typed_targets() {
    let request = McpCompletionRequest::builder()
        .target(McpCompletionTarget::from(McpPromptRef {
            server_id: McpServerId::try_from("prompts").expect("server id"),
            remote_name: "review".to_string(),
        }))
        .argument_name("path".to_string())
        .argument_value("src/".to_string())
        .context(BTreeMap::from([(
            "language".to_string(),
            "rust".to_string(),
        )]))
        .build();
    let result = McpCompletionResult {
        values: vec!["src/lib.rs".to_string()],
        total: Some(1),
        has_more: false,
    };

    let encoded = serde_json::to_value(&request).expect("serialize completion");
    assert_eq!(encoded["target"]["type"], "prompt");
    assert_eq!(encoded["target"]["reference"]["remoteName"], "review");
    assert_eq!(
        serde_json::from_value::<McpCompletionRequest>(encoded)
            .expect("deserialize completion"),
        request
    );
    assert_eq!(
        serde_json::from_value::<McpCompletionResult>(
            serde_json::to_value(&result).expect("serialize completion result")
        )
        .expect("deserialize completion result"),
        result
    );
}

/// OAuth authorization and Task status types keep interactive state explicit.
#[test]
fn oauth_and_task_status_are_explicit() {
    let authorization = McpAuthorizationRequest::builder()
        .server_id(McpServerId::try_from("secure").expect("server id"))
        .session_id(SessionId::try_from("session-1").expect("session id"))
        .authorization_url("https://auth.example/authorize".to_string())
        .scopes(vec!["tools.call".to_string()])
        .build();
    let task = McpTaskStatus::builder()
        .task_id("task-1".to_string())
        .state(McpTaskState::InputRequired)
        .progress(Some(0.5))
        .message(Some("Choose a repository".to_string()))
        .updated_at_ms(TimestampMs::from(456))
        .build();

    let authorization_json =
        serde_json::to_value(authorization).expect("serialize authorization");
    assert_eq!(authorization_json["serverId"], "secure");
    assert_eq!(authorization_json["sessionId"], "session-1");
    let task_json = serde_json::to_value(task).expect("serialize task status");
    assert_eq!(task_json["state"], "inputRequired");
    assert_eq!(task_json["updatedAtMs"], "456");
    assert_eq!(
        McpOAuthStatus::default().state,
        McpOAuthState::NotConfigured
    );
}

/// Every lifecycle enum has a stable explicit wire value.
#[test]
fn lifecycle_enums_have_complete_wire_values() {
    let server_states = [
        (McpServerState::Disabled, "disabled"),
        (McpServerState::Starting, "starting"),
        (McpServerState::Negotiating, "negotiating"),
        (McpServerState::Discovering, "discovering"),
        (McpServerState::Ready, "ready"),
        (McpServerState::Degraded, "degraded"),
        (McpServerState::Failed, "failed"),
        (McpServerState::Stopping, "stopping"),
        (McpServerState::Stopped, "stopped"),
    ];
    let failure_stages = [
        (McpFailureStage::Transport, "transport"),
        (McpFailureStage::Negotiation, "negotiation"),
        (McpFailureStage::Discovery, "discovery"),
        (McpFailureStage::Synchronization, "synchronization"),
        (McpFailureStage::Authentication, "authentication"),
        (McpFailureStage::Invocation, "invocation"),
        (McpFailureStage::Shutdown, "shutdown"),
    ];
    let oauth_states = [
        (McpOAuthState::NotConfigured, "notConfigured"),
        (McpOAuthState::Unauthenticated, "unauthenticated"),
        (McpOAuthState::Authorizing, "authorizing"),
        (McpOAuthState::Ready, "ready"),
        (McpOAuthState::RefreshRequired, "refreshRequired"),
        (McpOAuthState::Failed, "failed"),
    ];
    let task_states = [
        (McpTaskState::Working, "working"),
        (McpTaskState::InputRequired, "inputRequired"),
        (McpTaskState::Completed, "completed"),
        (McpTaskState::Failed, "failed"),
        (McpTaskState::Cancelled, "cancelled"),
    ];

    for (state, expected) in server_states {
        assert_eq!(
            serde_json::to_value(state).expect("server state"),
            expected
        );
    }
    for (stage, expected) in failure_stages {
        assert_eq!(
            serde_json::to_value(stage).expect("failure stage"),
            expected
        );
    }
    for (state, expected) in oauth_states {
        assert_eq!(serde_json::to_value(state).expect("oauth state"), expected);
    }
    for (state, expected) in task_states {
        assert_eq!(serde_json::to_value(state).expect("task state"), expected);
    }
}

/// Host requests retain Turn, Trace, timestamp, and typed interaction payloads.
#[test]
fn host_requests_round_trip_typed_context() {
    let context = McpRequestContext::builder()
        .server_id(McpServerId::try_from("host").expect("server id"))
        .session_id(SessionId::try_from("session-host").expect("session id"))
        .turn_id(TurnId::try_from("turn-host").expect("turn id"))
        .trace_id(TraceId::try_from("trace-host").expect("trace id"))
        .requested_at_ms(TimestampMs::from(9_007_199_254_740_999))
        .build();
    let sampling = McpHostRequest::Sampling(
        McpSamplingRequest::builder()
            .context(context.clone())
            .messages(Vec::new())
            .system_prompt(Some("Be concise".to_string()))
            .max_tokens(128)
            .temperature(Some(0.2))
            .build(),
    );
    let elicitation = McpHostRequest::Elicitation(McpElicitationRequest {
        request_id: "host:trace-host:request-1".to_string(),
        context: context.clone(),
        mode: McpElicitationMode::Form {
            message: "Choose a repository".to_string(),
            requested_schema: serde_json::json!({
                "type": "object",
                "properties": { "repository": { "type": "string" } }
            }),
        },
    });
    let roots = McpHostRequest::Roots(McpRootsRequest {
        context: context.clone(),
    });

    let sampling_json =
        serde_json::to_value(&sampling).expect("serialize sampling request");
    assert_eq!(sampling_json["type"], "sampling");
    assert_eq!(sampling_json["request"]["context"]["turnId"], "turn-host");
    assert_eq!(sampling_json["request"]["context"]["traceId"], "trace-host");
    assert_eq!(
        sampling_json["request"]["context"]["requestedAtMs"],
        "9007199254740999"
    );
    assert_eq!(
        serde_json::from_value::<McpHostRequest>(sampling_json)
            .expect("deserialize sampling request"),
        sampling
    );
    assert_eq!(
        serde_json::from_value::<McpHostRequest>(
            serde_json::to_value(&elicitation)
                .expect("serialize elicitation request")
        )
        .expect("deserialize elicitation request"),
        elicitation
    );
    assert_eq!(
        serde_json::from_value::<McpHostRequest>(
            serde_json::to_value(&roots).expect("serialize roots request")
        )
        .expect("deserialize roots request"),
        roots
    );
}

/// Host responses preserve Sampling content, Elicitation action, Roots, and authorization data.
#[test]
fn host_responses_are_typed_by_interaction() {
    let responses = [
        McpHostResponse::Sampling(
            McpSamplingResult::builder()
                .role(Role::Assistant)
                .blocks(vec![ContentBlock::Text {
                    text: "sampled".to_string(),
                }])
                .stop_reason(StopReason::EndTurn)
                .model("provider/model".to_string())
                .build(),
        ),
        McpHostResponse::Elicitation(McpElicitationResult {
            action: McpElicitationAction::Accept,
            content: Some(serde_json::json!({ "repository": "clawcode" })),
        }),
        McpHostResponse::Roots(McpRootsResult {
            roots: vec![McpRoot {
                uri: "file:///workspace".to_string(),
                name: Some("workspace".to_string()),
            }],
        }),
        McpHostResponse::Authorization(McpAuthorizationResult {
            response_uri: "http://localhost/callback?code=opaque".to_string(),
        }),
    ];

    for response in responses {
        let encoded =
            serde_json::to_value(&response).expect("serialize Host response");
        let decoded: McpHostResponse =
            serde_json::from_value(encoded).expect("deserialize Host response");
        assert_eq!(decoded, response);
    }

    let authorization = McpAuthorizationResult {
        response_uri: "http://localhost/callback?code=must-not-leak"
            .to_string(),
    };
    assert!(!format!("{authorization:?}").contains("must-not-leak"));
}
