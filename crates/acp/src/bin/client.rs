//! Interactive ACP chat client. Starts the ACP agent in-process, sends prompts,
//! displays streaming responses with tool call progress, and handles approval prompts.
//!
//! Start with: `cargo run --bin claw`

use std::io::Write;
use std::sync::Arc;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo, Responder};
use kernel::Kernel;
use provider::factory::LlmFactory;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tools::ToolRegistry;

/// Convert a displayable error into an ACP internal error.
fn err(e: impl std::fmt::Display) -> agent_client_protocol::Error {
    agent_client_protocol::util::internal_error(e.to_string())
}

/// Run the interactive ACP client process.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = config::load()?;
    let llm_factory = Arc::new(LlmFactory::new(config.clone()));
    let mut tools = ToolRegistry::new();
    tools.register_builtins();
    let kernel = Arc::new(Kernel::new(llm_factory, config, Arc::new(tools)));
    let agent = Arc::new(acp::agent::ClawcodeAgent::new(kernel));

    // Use two one-way in-memory pipes so the interactive client always talks to
    // the exact agent implementation built in this process, not a stale binary.
    let (client_outgoing, agent_incoming) = tokio::io::duplex(64 * 1024);
    let (agent_outgoing, client_incoming) = tokio::io::duplex(64 * 1024);
    let client_io = ByteStreams::new(client_outgoing.compat_write(), client_incoming.compat());
    let agent_io = ByteStreams::new(agent_outgoing.compat_write(), agent_incoming.compat());

    let agent_task = tokio::spawn(async move {
        if let Err(e) = agent.serve(agent_io).await {
            eprintln!("in-process ACP agent failed: {e}");
        }
    });

    // Shared stdin reader to avoid races between main loop and approval handler
    let stdin_reader = Arc::new(Mutex::new(BufReader::new(tokio::io::stdin())));

    let stdin_approval = stdin_reader.clone();
    let stdin_main = stdin_reader;

    Client
        .builder()
        .on_receive_notification(
            |sn: SessionNotification, _cx: ConnectionTo<Agent>| async move {
                match &sn.update {
                    SessionUpdate::AgentMessageChunk(chunk) => {
                        if let ContentBlock::Text(t) = &chunk.content {
                            print!("{}", t.text);
                            let _ = std::io::stdout().flush();
                        }
                    }
                    SessionUpdate::AgentThoughtChunk(chunk) => {
                        if let ContentBlock::Text(t) = &chunk.content {
                            eprint!("{}", t.text);
                        }
                    }
                    SessionUpdate::ToolCall(tc) => {
                        eprintln!(
                            "\n  [{status}] {name}",
                            status = format!("{:?}", tc.status).to_lowercase(),
                            name = tc.title,
                        );
                    }
                    SessionUpdate::ToolCallUpdate(u) => {
                        if let Some(content) = &u.fields.content {
                            for c in content {
                                let agent_client_protocol::schema::ToolCallContent::Content(ct) = c
                                else {
                                    continue;
                                };
                                let ContentBlock::Text(t) = &ct.content else {
                                    continue;
                                };
                                eprintln!("  -> {}", t.text);
                            }
                        }
                    }
                    SessionUpdate::UsageUpdate(u) => {
                        eprintln!("\n  [tokens: {}]", u.used);
                    }
                    _ => {}
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            move |req: RequestPermissionRequest,
                  responder: Responder<RequestPermissionResponse>,
                  _cx| {
                let stdin = stdin_approval.clone();
                async move {
                    eprintln!("\n  == Approval Request ==");
                    eprintln!("  Tool: {}", req.tool_call.tool_call_id);
                    for opt in &req.options {
                        eprintln!("    [{}] {}", opt.option_id, opt.name);
                    }
                    eprint!("  Allow? [y/n]: ");
                    let _ = std::io::stdout().flush();

                    let mut input = String::new();
                    let mut reader = stdin.lock().await;
                    let allowed = match reader.read_line(&mut input).await {
                        Ok(_) => input.trim().to_lowercase().starts_with('y'),
                        Err(_) => false,
                    };
                    eprintln!("  [{}]", if allowed { "allowed" } else { "rejected" });

                    let outcome = if allowed {
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            "allow_once",
                        ))
                    } else {
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            "reject_once",
                        ))
                    };
                    responder.respond(RequestPermissionResponse::new(outcome))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(client_io, async move |conn: ConnectionTo<Agent>| {
            let init_resp: InitializeResponse = conn
                .send_request(InitializeRequest::new(
                    agent_client_protocol::schema::ProtocolVersion::V1,
                ))
                .block_task()
                .await?;
            if let Some(info) = &init_resp.agent_info {
                eprintln!("agent: {:?} v{:?}", info.title, info.version);
            }

            let cwd = std::env::current_dir().map_err(err)?;
            let session_resp: NewSessionResponse = conn
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;
            eprintln!("session: {}\n", session_resp.session_id);

            loop {
                eprint!("> ");
                let _ = std::io::stdout().flush();

                let mut input = String::new();
                let n = {
                    let mut reader = stdin_main.lock().await;
                    reader.read_line(&mut input).await.map_err(err)?
                };
                if n == 0 {
                    break;
                } // EOF
                let input = input.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                let prompt_req = PromptRequest::new(
                    session_resp.session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(&input))],
                );
                let prompt_resp: PromptResponse =
                    conn.send_request(prompt_req).block_task().await?;
                eprintln!(" [{:?}]", prompt_resp.stop_reason);
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("ACP error: {e}"))?;

    agent_task.abort();
    let _ = agent_task.await;
    Ok(())
}
