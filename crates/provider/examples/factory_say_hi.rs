//! Uses the active configured model through upstream rig-core.

use provider::ProviderFactory;
use rig_core::completion::CompletionRequest;

/// Resolves authentication and performs one explicit example request.
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
    let response =
        model
            .call(configured.prepare(CompletionRequest::new(
                "Say hi in one short sentence.",
            )))
            .await?;
    println!("{}", response.text());
    Ok(())
}
