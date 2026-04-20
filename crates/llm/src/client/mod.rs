use std::{fmt::Debug, marker::PhantomData, sync::Arc};

use bytes::Bytes;
use completion::CompletionClient;
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use verify::{ProviderSnafu, VerifyClient, VerifyError, VerifyHttpSnafu};

use crate::{
    completion::request::CompletionModel,
    http_client::{
        self, Builder, HttpClientExt, HttpSnafu, LazyBody, MultipartForm, make_auth_header,
    },
    usage::{GetTokenUsage, Usage},
};

pub mod completion;
pub mod verify;

/// Abstracts over the ability to instantiate a client, either via environment variables or some
/// `Self::Input`
pub trait ProviderClient {
    type Input;

    /// Create a client from the process's environment.
    /// Panics if an environment is improperly configured.
    fn from_env() -> Self;

    fn from_val(input: Self::Input) -> Self;
}

/// The final streaming response from a dynamic client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalCompletionResponse {
    pub usage: Option<Usage>,
}

impl GetTokenUsage for FinalCompletionResponse {
    fn token_usage(&self) -> Option<Usage> {
        self.usage
    }
}

/// A trait for API keys. This determines whether the key is inserted into a [Client]'s default
/// headers (in the `Some` case) or handled by a given provider extension (in the `None` case)
pub trait ApiKey: Sized {
    fn into_header(self) -> Option<http_client::Result<(HeaderName, HeaderValue)>> {
        None
    }
}

/// An API key which will be inserted into a `Client`'s default headers as a bearer auth token
pub struct BearerAuth(String);

impl ApiKey for BearerAuth {
    fn into_header(self) -> Option<http_client::Result<(HeaderName, HeaderValue)>> {
        Some(make_auth_header(self.0))
    }
}

impl<S> From<S> for BearerAuth
where
    S: Into<String>,
{
    fn from(value: S) -> Self {
        Self(value.into())
    }
}

/// A type containing nothing at all. For `Option`-like behavior on the type level, i.e. to describe
/// the lack of a capability or field (an API key, for instance)
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl ApiKey for Nothing {}

impl TryFrom<String> for Nothing {
    type Error = &'static str;

    fn try_from(_: String) -> Result<Self, Self::Error> {
        Err(
            "Tried to create a Nothing from a string - this should not happen, please file an issue",
        )
    }
}

#[derive(Clone)]
pub struct Client<Ext = Nothing, H = reqwest::Client> {
    base_url: Arc<str>,
    headers: Arc<HeaderMap>,
    http_client: H,
    ext: Ext,
}

pub trait DebugExt: Debug {
    fn fields(&self) -> impl Iterator<Item = (&'static str, &dyn Debug)> {
        std::iter::empty()
    }
}

impl<Ext, H> std::fmt::Debug for Client<Ext, H>
where
    Ext: DebugExt,
    H: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = &mut f.debug_struct("Client");

        d = d
            .field("base_url", &self.base_url)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .filter_map(|(k, v)| {
                        if k == http::header::AUTHORIZATION || k.as_str().contains("api-key") {
                            None
                        } else {
                            Some((k, v))
                        }
                    })
                    .collect::<Vec<(&HeaderName, &HeaderValue)>>(),
            )
            .field("http_client", &self.http_client);

        self.ext
            .fields()
            .fold(d, |d, (name, field)| d.field(name, field))
            .finish()
    }
}

pub enum Transport {
    Http,
    Sse,
    NdJson,
}

/// An API provider extension, this abstracts over extensions which may be used in conjunction with
/// the `Client<Ext, H>` struct to define the behavior of a provider with respect to networking,
/// auth, instantiating models
pub trait Provider: Sized {
    /// The builder type that constructs this provider extension.
    /// This associates extensions with their builders for type inference.
    type Builder: ProviderBuilder;

    const VERIFY_PATH: &'static str;

    fn build_uri(&self, base_url: &str, path: &str, _transport: Transport) -> String {
        // Some providers (like Azure) have a blank base URL to allow users to input their own endpoints.
        let base_url = if base_url.is_empty() {
            base_url.to_string()
        } else {
            base_url.to_string() + "/"
        };

        base_url.to_string() + path.trim_start_matches('/')
    }

    fn with_custom(&self, req: http_client::Builder) -> http_client::Result<http_client::Builder> {
        Ok(req)
    }
}

/// A wrapper type providing runtime checks on a provider's capabilities via the [Capability] trait
pub struct Capable<M>(PhantomData<M>);

pub trait Capability {
    const CAPABLE: bool;
}

impl<M> Capability for Capable<M> {
    const CAPABLE: bool = true;
}

impl Capability for Nothing {
    const CAPABLE: bool = false;
}

/// The capabilities of a given provider, i.e. embeddings, audio transcriptions, text completion
pub trait Capabilities<H = reqwest::Client> {
    type Completion: Capability;
    type Embeddings: Capability;
    type Transcription: Capability;
    type ModelListing: Capability;
    // #[cfg(feature = "image")]
    // type ImageGeneration: Capability;
    // #[cfg(feature = "audio")]
    // type AudioGeneration: Capability;
}

/// An API provider extension *builder*, this abstracts over provider-specific builders which are
/// able to configure and produce a given provider's extension type
///
/// See [Provider]
pub trait ProviderBuilder: Sized + Default + Clone {
    type Extension<H>: Provider
    where
        H: HttpClientExt;
    type ApiKey: ApiKey;

    const BASE_URL: &'static str;

    /// Build the provider extension from the client builder configuration.
    fn build<H>(
        builder: &ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<Self::Extension<H>>
    where
        H: HttpClientExt;

    /// This method can be used to customize the fields of `builder` before it is used to create
    /// a client. For example, adding default headers
    fn finish<H>(
        &self,
        builder: ClientBuilder<Self, Self::ApiKey, H>,
    ) -> http_client::Result<ClientBuilder<Self, Self::ApiKey, H>> {
        Ok(builder)
    }
}

impl<Ext> Client<Ext, reqwest::Client>
where
    Ext: Provider,
    Ext::Builder: ProviderBuilder<Extension<reqwest::Client> = Ext> + Default,
{
    pub fn new(
        api_key: impl Into<<Ext::Builder as ProviderBuilder>::ApiKey>,
    ) -> http_client::Result<Self> {
        Self::builder().api_key(api_key).build()
    }
}

impl<Ext, H> Client<Ext, H> {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn ext(&self) -> &Ext {
        &self.ext
    }

    /// Returns the shared HTTP backend so provider adapters can build derived clients.
    pub fn http_client(&self) -> &H {
        &self.http_client
    }

    pub fn with_ext<NewExt>(self, new_ext: NewExt) -> Client<NewExt, H> {
        Client {
            base_url: self.base_url,
            headers: self.headers,
            http_client: self.http_client,
            ext: new_ext,
        }
    }
}

impl<Ext, H> HttpClientExt for Client<Ext, H>
where
    H: HttpClientExt + 'static,
    Ext: Send + Sync + 'static,
{
    fn send<T, U>(
        &self,
        mut req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes>,
        U: Send + 'static,
    {
        req.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );

        self.http_client.send(req)
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes>,
        U: Send + 'static,
    {
        self.http_client.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        mut req: Request<T>,
    ) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + Send
    where
        T: Into<Bytes>,
    {
        req.headers_mut().insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );

        self.http_client.send_streaming(req)
    }
}

impl<Ext> Client<Ext, reqwest::Client>
where
    Ext: Provider,
    Ext::Builder: ProviderBuilder<Extension<reqwest::Client> = Ext> + Default,
{
    pub fn builder() -> ClientBuilder<Ext::Builder, NeedsApiKey, reqwest::Client> {
        ClientBuilder {
            api_key: NeedsApiKey,
            headers: Default::default(),
            base_url: <Ext::Builder as ProviderBuilder>::BASE_URL.into(),
            http_client: None,
            ext: Default::default(),
        }
    }
}

impl<Ext, H> Client<Ext, H>
where
    Ext: Provider,
{
    pub fn post<S>(&self, path: S) -> http_client::Result<Builder>
    where
        S: AsRef<str>,
    {
        let uri = self
            .ext
            .build_uri(&self.base_url, path.as_ref(), Transport::Http);

        let mut req = Request::post(uri);

        if let Some(hs) = req.headers_mut() {
            hs.extend(self.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        self.ext.with_custom(req)
    }

    pub fn post_sse<S>(&self, path: S) -> http_client::Result<Builder>
    where
        S: AsRef<str>,
    {
        let uri = self
            .ext
            .build_uri(&self.base_url, path.as_ref(), Transport::Sse);

        let mut req = Request::post(uri);

        if let Some(hs) = req.headers_mut() {
            hs.extend(self.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        self.ext.with_custom(req)
    }

    pub fn get_sse<S>(&self, path: S) -> http_client::Result<Builder>
    where
        S: AsRef<str>,
    {
        let uri = self
            .ext
            .build_uri(&self.base_url, path.as_ref(), Transport::Sse);

        let mut req = Request::get(uri);

        if let Some(hs) = req.headers_mut() {
            hs.extend(self.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        self.ext.with_custom(req)
    }

    pub fn get<S>(&self, path: S) -> http_client::Result<Builder>
    where
        S: AsRef<str>,
    {
        let uri = self
            .ext
            .build_uri(&self.base_url, path.as_ref(), Transport::Http);

        let mut req = Request::get(uri);

        if let Some(hs) = req.headers_mut() {
            hs.extend(self.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        self.ext.with_custom(req)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NeedsApiKey;

// ApiKey is generic because Anthropic uses custom auth header, local models like Ollama use none
#[derive(Clone)]
pub struct ClientBuilder<Ext, ApiKey = NeedsApiKey, H = reqwest::Client> {
    base_url: String,
    api_key: ApiKey,
    headers: HeaderMap,
    http_client: Option<H>,
    ext: Ext,
}

impl<ExtBuilder, H> Default for ClientBuilder<ExtBuilder, NeedsApiKey, H>
where
    H: Default,
    ExtBuilder: ProviderBuilder + Default,
{
    fn default() -> Self {
        Self {
            api_key: NeedsApiKey,
            headers: Default::default(),
            base_url: ExtBuilder::BASE_URL.into(),
            http_client: None,
            ext: Default::default(),
        }
    }
}

impl<Ext, H> ClientBuilder<Ext, NeedsApiKey, H> {
    /// Set the API key for this client. This *must* be done before the `build` method can be
    /// called
    pub fn api_key<ApiKey>(self, api_key: impl Into<ApiKey>) -> ClientBuilder<Ext, ApiKey, H> {
        ClientBuilder {
            api_key: api_key.into(),
            base_url: self.base_url,
            headers: self.headers,
            http_client: self.http_client,
            ext: self.ext,
        }
    }
}

impl<Ext, ApiKey, H> ClientBuilder<Ext, ApiKey, H>
where
    Ext: Clone,
{
    /// Owned map over the ext field
    #[allow(dead_code)]
    pub(crate) fn over_ext<F, NewExt>(self, f: F) -> ClientBuilder<NewExt, ApiKey, H>
    where
        F: FnOnce(Ext) -> NewExt,
    {
        let ClientBuilder {
            base_url,
            api_key,
            headers,
            http_client,
            ext,
        } = self;

        let new_ext = f(ext.clone());

        ClientBuilder {
            base_url,
            api_key,
            headers,
            http_client,
            ext: new_ext,
        }
    }

    /// Set the base URL for this client
    pub fn base_url<S>(self, base_url: S) -> Self
    where
        S: AsRef<str>,
    {
        Self {
            base_url: base_url.as_ref().to_string(),
            ..self
        }
    }

    /// Set the HTTP backend used in this client
    pub fn http_client<U>(self, http_client: U) -> ClientBuilder<Ext, ApiKey, U> {
        ClientBuilder {
            http_client: Some(http_client),
            base_url: self.base_url,
            api_key: self.api_key,
            headers: self.headers,
            ext: self.ext,
        }
    }

    /// Set the HTTP headers used in this client
    pub fn http_headers(self, headers: HeaderMap) -> Self {
        Self { headers, ..self }
    }

    #[allow(dead_code)]
    pub(crate) fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    #[allow(dead_code)]
    pub(crate) fn ext_mut(&mut self) -> &mut Ext {
        &mut self.ext
    }
}

impl<Ext, ApiKey, H> ClientBuilder<Ext, ApiKey, H> {
    #[allow(dead_code)]
    pub(crate) fn get_api_key(&self) -> &ApiKey {
        &self.api_key
    }
}

impl<Ext, Key, H> ClientBuilder<Ext, Key, H> {
    pub fn ext(&self) -> &Ext {
        &self.ext
    }
}

impl<ExtBuilder, Key, H> ClientBuilder<ExtBuilder, Key, H>
where
    ExtBuilder: ProviderBuilder<ApiKey = Key>,
    Key: ApiKey,
    H: Default + HttpClientExt,
{
    pub fn build(mut self) -> http_client::Result<Client<ExtBuilder::Extension<H>, H>> {
        let ext_builder = self.ext.clone();

        self = ext_builder.finish(self)?;
        let ext = ExtBuilder::build(&self)?;

        let ClientBuilder {
            http_client,
            base_url,
            mut headers,
            api_key,
            ..
        } = self;

        if let Some((k, v)) = api_key.into_header().transpose()? {
            headers.insert(k, v);
        }

        let http_client = http_client.unwrap_or_default();

        Ok(Client {
            http_client,
            base_url: Arc::from(base_url.as_str()),
            headers: Arc::new(headers),
            ext,
        })
    }
}

impl<M, Ext, H> CompletionClient for Client<Ext, H>
where
    Ext: Capabilities<H, Completion = Capable<M>>,
    M: CompletionModel<Client = Self>,
{
    type CompletionModel = M;

    fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel {
        M::make(self, model)
    }
}

impl<Ext, H> VerifyClient for Client<Ext, H>
where
    H: HttpClientExt,
    Ext: DebugExt + Provider + Sync,
{
    async fn verify(&self) -> Result<(), VerifyError> {
        use http::StatusCode;

        let req = self
            .get(Ext::VERIFY_PATH)
            .context(VerifyHttpSnafu {
                stage: "verify-get",
            })?
            .body(http_client::NoBody)
            .context(HttpSnafu {
                stage: "verfig-body",
            })
            .context(VerifyHttpSnafu {
                stage: "verify-body",
            })?;

        let response = self.http_client.send(req).await.unwrap();

        match response.status() {
            StatusCode::OK => Ok(()),
            StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => Err(VerifyError::Auth),
            StatusCode::INTERNAL_SERVER_ERROR => {
                let text = http_client::text(response)
                    .await
                    .context(VerifyHttpSnafu { stage: "500-text" })?;
                Err(ProviderSnafu { message: text }.build())
            }
            status if status.as_u16() == 529 => {
                let text = http_client::text(response)
                    .await
                    .context(VerifyHttpSnafu { stage: "529-text" })?;
                Err(ProviderSnafu { message: text }.build())
            }
            _ => {
                let status = response.status();

                if status.is_success() {
                    Ok(())
                } else {
                    response
                        .into_body()
                        .await
                        .map(|_| ())
                        .context(VerifyHttpSnafu {
                            stage: "verify-body-text",
                        })
                }
            }
        }
    }
}
