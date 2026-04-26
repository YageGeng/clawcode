use serde::Deserialize;

use crate::{
    client::{
        self, BearerAuth, Capabilities, Capable, DebugExt, Nothing, Provider, ProviderBuilder,
        ProviderClient,
    },
    http_client::{self, HttpClientExt},
};

use super::completion::CompletionModel;

/// DeepSeek API base URL
pub const DEEPSEEK_API_BASE_URL: &str = "https://api.deepseek.com";

/// DeepSeek provider extension marker
#[derive(Debug, Default, Clone, Copy)]
pub struct DeepSeekExt;

/// DeepSeek provider extension builder
#[derive(Debug, Default, Clone, Copy)]
pub struct DeepSeekExtBuilder;

type DeepSeekApiKey = BearerAuth;

/// DeepSeek client type aliases
pub type Client<H = reqwest::Client> = client::Client<DeepSeekExt, H>;
pub type ClientBuilder<H = reqwest::Client> =
    client::ClientBuilder<DeepSeekExtBuilder, DeepSeekApiKey, H>;

impl Provider for DeepSeekExt {
    type Builder = DeepSeekExtBuilder;
    const VERIFY_PATH: &'static str = "/user/balance";
}

impl<H> Capabilities<H> for DeepSeekExt {
    type Completion = Capable<CompletionModel<H>>;
    type Embeddings = Nothing;
    type Transcription = Nothing;
    type ModelListing = Nothing;
}

impl DebugExt for DeepSeekExt {}

impl ProviderBuilder for DeepSeekExtBuilder {
    type Extension<H>
        = DeepSeekExt
    where
        H: HttpClientExt;
    type ApiKey = DeepSeekApiKey;

    const BASE_URL: &'static str = DEEPSEEK_API_BASE_URL;

    fn build<H>(
        _builder: &client::ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<Self::Extension<H>>
    where
        H: HttpClientExt,
    {
        Ok(DeepSeekExt)
    }
}

impl ProviderClient for Client {
    type Input = DeepSeekApiKey;

    /// Creates a client from the DEEPSEEK_API_KEY environment variable; panics if unset.
    fn from_env() -> Self {
        let api_key = std::env::var("DEEPSEEK_API_KEY")
            .expect("DEEPSEEK_API_KEY environment variable must be set");
        Self::builder().api_key(api_key).build().unwrap()
    }

    fn from_val(input: Self::Input) -> Self {
        Self::builder().api_key(input).build().unwrap()
    }
}

/// Error response returned by the DeepSeek API.
#[derive(Debug, Deserialize)]
pub struct ApiErrorResponse {
    pub(crate) message: String,
}

/// Wraps a successful or error response from the DeepSeek API.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ApiResponse<T> {
    Ok(T),
    Err(ApiErrorResponse),
}
