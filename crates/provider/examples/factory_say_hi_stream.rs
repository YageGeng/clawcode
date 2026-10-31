//! Streams the active configured model and checks its final provider response.

use futures::StreamExt;
use provider::ProviderFactory;
use rig_core::{
    completion::CompletionRequest,
    streaming::{Item, StreamEvent},
};

/// Prints only display deltas and verifies the complete final response.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = config::load()?;
    let active = config.current().active_model.clone();
    let (provider, model) =
        active.split_once('/').ok_or("invalid active model")?;
    let factory = ProviderFactory::new(config);
    let configured = factory.resolve(provider, model)?;
    let auth = configured.auth_context().await?;
    let model = configured
        .build_model(auth.as_ref())?
        .with_transport(configured.http_client());
    let mut stream = model
        .stream(configured.prepare(CompletionRequest::new(
            "Say hi in one short sentence.",
        )))?;
    while let Some(item) = stream.next().await {
        if let Item::Event(StreamEvent::Text { text, .. }) = item? {
            print!("{text}");
        }
    }
    stream.finish().await?;
    println!();
    Ok(())
}
