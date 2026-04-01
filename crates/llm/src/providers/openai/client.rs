use serde::Deserialize;

use crate::{
    client::{self, BearerAuth, Provider, ProviderBuilder},
    http_client::{self, HttpClientExt},
};

// ================================================================
// Main OpenAI Client
// ================================================================
const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";

// ================================================================
// OpenAI Responses API Extension
// ================================================================
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAIResponsesExt;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAIResponsesExtBuilder;

// ================================================================
// OpenAI Completions API Extension
// ================================================================
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAICompletionsExt;

#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAICompletionsExtBuilder;

type OpenAIApiKey = BearerAuth;

// Responses API client (default)
pub type Client<H = reqwest::Client> = client::Client<OpenAIResponsesExt, H>;
pub type ClientBuilder<H = reqwest::Client> =
    client::ClientBuilder<OpenAIResponsesExtBuilder, OpenAIApiKey, H>;

// Completions API client
pub type CompletionsClient<H = reqwest::Client> = client::Client<OpenAICompletionsExt, H>;
pub type CompletionsClientBuilder<H = reqwest::Client> =
    client::ClientBuilder<OpenAICompletionsExtBuilder, OpenAIApiKey, H>;

impl Provider for OpenAIResponsesExt {
    type Builder = OpenAIResponsesExtBuilder;
    const VERIFY_PATH: &'static str = "/models";
}

impl Provider for OpenAICompletionsExt {
    type Builder = OpenAICompletionsExtBuilder;
    const VERIFY_PATH: &'static str = "/models";
}

impl ProviderBuilder for OpenAIResponsesExtBuilder {
    type Extension<H>
        = OpenAIResponsesExt
    where
        H: HttpClientExt;
    type ApiKey = OpenAIApiKey;

    const BASE_URL: &'static str = OPENAI_API_BASE_URL;

    fn build<H>(
        _builder: &client::ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<Self::Extension<H>>
    where
        H: HttpClientExt,
    {
        Ok(OpenAIResponsesExt)
    }
}

impl ProviderBuilder for OpenAICompletionsExtBuilder {
    type Extension<H>
        = OpenAICompletionsExt
    where
        H: HttpClientExt;
    type ApiKey = OpenAIApiKey;

    const BASE_URL: &'static str = OPENAI_API_BASE_URL;

    fn build<H>(
        _builder: &client::ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<Self::Extension<H>>
    where
        H: HttpClientExt,
    {
        Ok(OpenAICompletionsExt)
    }
}

#[derive(Debug, Deserialize)]
pub struct ApiErrorResponse {
    pub(crate) message: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ApiResponse<T> {
    Ok(T),
    Err(ApiErrorResponse),
}
