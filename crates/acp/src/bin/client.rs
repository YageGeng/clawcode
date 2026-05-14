//! Interactive ACP chat client. Starts the ACP agent in-process, sends prompts,
//! displays streaming responses with tool call progress, and handles approval prompts.
//!
//! Start with: `cargo run --bin claw`

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::*;
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo, Responder};
use clap::Parser;
use kernel::Kernel;
use provider::factory::LlmFactory;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tools::ToolRegistry;

/// Command-line options for the interactive claw client.
#[derive(Clone, Debug, Parser)]
#[command(name = "claw", version, about = "Interactive clawcode ACP client")]
struct Cli {
    /// List persisted sessions for the current working directory and exit.
    #[arg(long, conflicts_with = "resume")]
    list_sessions: bool,

    /// Resume a persisted session id instead of creating a new session.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,
}

/// Print session list results returned by the ACP agent.
fn print_sessions(response: ListSessionsResponse) {
    if response.sessions.is_empty() {
        println!("No sessions found for this working directory.");
        return;
    }

    for session in response.sessions {
        let updated = session.updated_at.as_deref().unwrap_or("-");
        let title = session.title.as_deref().unwrap_or("");
        println!(
            "{}\t{}\t{}\t{}",
            session.session_id,
            updated,
            session.cwd.display(),
            title
        );
    }
}

/// Request persisted sessions for `cwd` from the in-process ACP agent.
async fn list_sessions(
    conn: &ConnectionTo<Agent>,
    cwd: PathBuf,
) -> Result<(), agent_client_protocol::Error> {
    let response: ListSessionsResponse = conn
        .send_request(ListSessionsRequest::new().cwd(cwd))
        .block_task()
        .await?;
    print_sessions(response);
    Ok(())
}

/// Create a new session or load the requested persisted session.
async fn open_session(
    conn: &ConnectionTo<Agent>,
    cli: &Cli,
    cwd: PathBuf,
) -> Result<agent_client_protocol::schema::SessionId, agent_client_protocol::Error> {
    if let Some(session_id) = &cli.resume {
        let acp_session_id = agent_client_protocol::schema::SessionId::new(session_id.clone());
        let _: LoadSessionResponse = conn
            .send_request(LoadSessionRequest::new(acp_session_id.clone(), cwd))
            .block_task()
            .await?;
        eprintln!("resumed session: {acp_session_id}\n");
        return Ok(acp_session_id);
    }

    let session_resp: NewSessionResponse = conn
        .send_request(NewSessionRequest::new(cwd))
        .block_task()
        .await?;
    eprintln!("session: {}\n", session_resp.session_id);
    Ok(session_resp.session_id)
}

/// Run the interactive ACP client process.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = config::load()?;

    let tools = ToolRegistry::new();
    tools.register_builtins();

    let kernel = Kernel::new(
        Arc::new(LlmFactory::new(config.clone())),
        config,
        Arc::new(tools),
    );
    kernel.register_agent_tools();

    let agent = Arc::new(acp::agent::ClawcodeAgent::new(Arc::new(kernel)));

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
                        let status = format!("{:?}", tc.status).to_lowercase();
                        let args = tc
                            .raw_input
                            .as_ref()
                            .map(|v| format!(" {}", v))
                            .unwrap_or_default();
                        eprintln!("\n  [{status}] {name}{args}", name = tc.title,);
                    }
                    SessionUpdate::ToolCallUpdate(u) => {
                        if let Some(s) = &u.fields.status {
                            let s = format!("{:?}", s).to_lowercase();
                            eprintln!("  [{s}] {id}", id = u.tool_call_id);
                        }
                        if let Some(content) = &u.fields.content {
                            for c in content {
                                let agent_client_protocol::schema::ToolCallContent::Content(ct) = c
                                else {
                                    continue;
                                };
                                let ContentBlock::Text(t) = &ct.content else {
                                    continue;
                                };
                                eprint!("{}", t.text);
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
                    let title = req.tool_call.fields.title.as_deref().unwrap_or("?");
                    let desc = req
                        .tool_call
                        .fields
                        .content
                        .as_ref()
                        .and_then(|c| {
                            c.first().and_then(|cc| match cc {
                                agent_client_protocol::schema::ToolCallContent::Content(ct) => {
                                    match &ct.content {
                                        ContentBlock::Text(t) => Some(t.text.as_str()),
                                        _ => None,
                                    }
                                }
                                _ => None,
                            })
                        })
                        .unwrap_or("");
                    eprintln!("\n  ══ Approve: {title} ══");
                    if !desc.is_empty() {
                        eprintln!("  {desc}");
                    }
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

            let cwd =
                std::env::current_dir().map_err(agent_client_protocol::util::internal_error)?;
            if cli.list_sessions {
                list_sessions(&conn, cwd).await?;
                return Ok(());
            }

            let session_id = open_session(&conn, &cli, cwd).await?;

            loop {
                eprint!("> ");
                let _ = std::io::stdout().flush();

                let mut input = String::new();
                let n = {
                    let mut reader = stdin_main.lock().await;
                    reader
                        .read_line(&mut input)
                        .await
                        .map_err(agent_client_protocol::util::internal_error)?
                };
                if n == 0 {
                    break;
                } // EOF
                let input = input.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                let prompt_req = PromptRequest::new(
                    session_id.clone(),
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
