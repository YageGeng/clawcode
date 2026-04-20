#[allow(dead_code)]
mod common;

use std::error::Error;

use llm::{
    client::{ProviderClient, completion::CompletionClient},
    completion::{CompletionModel as _, Message},
    providers::{
        chatgpt::{self, ResponsesWebSocketSessionBuilder},
        openai::responses_api::{
            streaming::{ItemChunkKind, ResponseChunkKind},
            websocket::ResponsesWebSocketEvent,
        },
    },
};

/// Runs a ChatGPT websocket example with one warmup turn and one chained follow-up turn.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = chatgpt::Client::from_env();
    let model = client.completion_model(chatgpt::GPT_5_3_CODEX);
    let mut session = ResponsesWebSocketSessionBuilder::new(model.clone())
        .connect()
        .await?;

    let warmup_request = model
        .completion_request(Message::user(
            "You will answer a follow-up question about websocket mode.",
        ))
        .preamble("Be precise and concise.".to_string())
        .build();

    let warmup_id = session.warmup(warmup_request).await?;
    println!("warmup response id: {warmup_id}");

    let request = model
        .completion_request(Message::user(
            "Explain the benefit of websocket mode in one sentence.",
        ))
        .build();

    session.send(request).await?;

    loop {
        let event = session.next_event().await?;
        match event {
            ResponsesWebSocketEvent::Item(item) => {
                if let ItemChunkKind::OutputTextDelta(delta) = item.data {
                    print!("{}", delta.delta);
                }
            }
            ResponsesWebSocketEvent::Response(chunk) => {
                println!("\nresponse event: {:?}", chunk.kind);
                if matches!(
                    chunk.kind,
                    ResponseChunkKind::ResponseCompleted
                        | ResponseChunkKind::ResponseFailed
                        | ResponseChunkKind::ResponseIncomplete
                ) {
                    break;
                }
            }
            ResponsesWebSocketEvent::Done(done) => {
                println!("\nresponse.done id={:?}", done.response_id());
            }
            ResponsesWebSocketEvent::Error(error) => {
                return Err(error.to_string().into());
            }
        }
    }

    let chained_request = model
        .completion_request(Message::user(
            "Now restate that as three very short bullet points.",
        ))
        .build();
    let response = session.completion(chained_request).await?;

    println!(
        "chained response:\n{}",
        common::assistant_text(&response.choice)
    );
    println!("usage: {:?}", response.usage);
    session.close().await?;

    Ok(())
}
