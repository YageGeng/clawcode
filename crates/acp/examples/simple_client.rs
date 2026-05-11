//! An interactive ACP chat client that spawns `clawcode-acp` and talks over stdio.
//!
//! Start with: `cargo run --example simple_client`
//! Type your messages and press Enter. Empty line or Ctrl+D to quit.

use std::io::Write;
use std::process::Stdio;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

fn err(e: impl std::fmt::Display) -> agent_client_protocol::Error {
    agent_client_protocol::util::internal_error(e.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent_bin = std::env::var("CARGO_BIN_EXE_acp").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/debug/acp").to_string()
    });

    eprintln!("Spawning: {agent_bin}");

    let mut child = Command::new(&agent_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| err(format!("failed to spawn agent: {e}")))?;

    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    // ByteStreams::new(writer, reader)
    let agent_io = ByteStreams::new(stdin.compat_write(), stdout.compat());

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
                        eprintln!("\n{} ({:?})", tc.title, tc.status);
                    }
                    SessionUpdate::UsageUpdate(u) => {
                        eprintln!("\n[usage] {} tokens", u.used);
                    }
                    other => {
                        eprintln!("\n[debug] unhandled: {:?}", other);
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(agent_io, async |conn: ConnectionTo<Agent>| {
            // Step 1: Initialize
            eprintln!("-> initialize");
            let init_resp: InitializeResponse = conn
                .send_request(InitializeRequest::new(
                    agent_client_protocol::schema::ProtocolVersion::V1,
                ))
                .block_task()
                .await?;
            if let Some(info) = &init_resp.agent_info {
                eprintln!("<- agent: {:?} v{:?}", info.title, info.version);
            }

            // Step 2: New session
            eprintln!("-> new_session");
            let session_resp: NewSessionResponse = conn
                .send_request(NewSessionRequest::new("."))
                .block_task()
                .await?;
            eprintln!("<- session: {}", session_resp.session_id);
            eprintln!("Ready. Type a message and press Enter. Empty line to quit.\n");

            // Step 3: Interactive chat loop
            let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
            loop {
                eprint!("> ");
                let _ = std::io::stdout().flush();

                let line = lines.next_line().await.map_err(err)?;
                match line {
                    Some(input) if input.trim().is_empty() => {
                        eprintln!("Goodbye!");
                        break;
                    }
                    Some(input) => {
                        let prompt_req = PromptRequest::new(
                            session_resp.session_id.clone(),
                            vec![ContentBlock::Text(TextContent::new(&input))],
                        );
                        let prompt_resp: PromptResponse =
                            conn.send_request(prompt_req).block_task().await?;
                        eprintln!(" [stop: {:?}]", prompt_resp.stop_reason);
                    }
                    None => {
                        eprintln!("Goodbye!");
                        break;
                    }
                }
            }

            Ok(())
        })
        .await
        .map_err(|e| format!("ACP error: {e}"))?;

    let _ = child.wait().await;
    Ok(())
}
