use crate::{
    http_client::sse::BoxedStream,
    wasm_compat::{BoxedFuture, WasmCompatSendStream},
};
use bytes::Bytes;
use http::HeaderName;
pub use http::{HeaderMap, HeaderValue, Method, Request, Response, Uri, request::Builder};
pub mod multipart;
pub mod retry;
pub mod sse;
pub use multipart::MultipartForm;
pub use reqwest::Client as ReqwestClient;
use snafu::{OptionExt, ResultExt, Snafu};
use std::pin::Pin;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("Invalid response: status={} message={}", status, message))]
    InvalidResponse {
        status: http::StatusCode,
        message: String,
    },
    #[snafu(display("Invalid code: status={}", status))]
    InvalidCode { status: http::StatusCode },
    #[snafu(display("Invalid content type: header={:?}", header))]
    InvalidContentType { header: http::header::HeaderValue },

    #[snafu(display("Reqwest error: stage={} source={}", stage, source))]
    Reqwest {
        source: reqwest::Error,
        stage: String,
    },
    #[cfg(feature = "reqwest-middleware")]
    #[snafu(display("Reqwest middleware error: stage={} source={}", stage, source))]
    ReqwestMiddleware {
        source: reqwest_middleware::Error,
        stage: String,
    },
    #[snafu(display("Http error: stage={} source={}", stage, source))]
    Http { source: http::Error, stage: String },

    #[snafu(display("Header error: stage={} source={}", stage, source))]
    Header {
        source: http::header::InvalidHeaderValue,
        stage: String,
    },
    #[snafu(display("No headers"))]
    NoHeaders,
    #[snafu(display("stream end"))]
    StreamEnded,
}

pub type Result<T> = std::result::Result<T, Error>;

pub type StreamingResponse = Response<BoxedStream>;
pub type LazyBytes = BoxedFuture<'static, Result<Bytes>>;
pub type LazyBody<T> = BoxedFuture<'static, Result<T>>;

#[derive(Debug, Clone, Copy)]
pub struct NoBody;

impl From<NoBody> for Bytes {
    fn from(_: NoBody) -> Self {
        Bytes::new()
    }
}

impl From<NoBody> for reqwest::Body {
    fn from(_: NoBody) -> Self {
        reqwest::Body::default()
    }
}

pub async fn text(response: Response<LazyBody<Vec<u8>>>) -> Result<String> {
    let text = response.into_body().await?;
    Ok(String::from(String::from_utf8_lossy(&text)))
}

pub fn make_auth_header(key: impl AsRef<str>) -> Result<(HeaderName, HeaderValue)> {
    Ok((
        http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", key.as_ref())).context(HeaderSnafu {
            stage: "build-auth-header",
        })?,
    ))
}

pub fn bearer_auth_header(headers: &mut HeaderMap, key: impl AsRef<str>) -> Result<()> {
    let (k, v) = make_auth_header(key)?;

    headers.insert(k, v);

    Ok(())
}

pub fn with_bearer_auth(mut req: Builder, auth: &str) -> Result<Builder> {
    bearer_auth_header(req.headers_mut().context(NoHeadersSnafu)?, auth)?;

    Ok(req)
}

/// A helper trait to make generic requests (both regular and SSE) possible.
pub trait HttpClientExt: Send + Sync {
    /// Send a HTTP request, get a response back (as bytes). Response must be able to be turned back into Bytes.
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes>,
        T: Send,
        U: From<Bytes>,
        U: Send + 'static;

    /// Send a HTTP request with a multipart body, get a response back (as bytes). Response must be able to be turned back into Bytes (although usually for the response, you will probably want to specify Bytes anyway).
    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes>,
        U: Send + 'static;

    /// Send a HTTP request, get a streamed response back (as a stream of [`bytes::Bytes`].)
    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send
    where
        T: Into<Bytes>;
}

impl HttpClientExt for reqwest::Client {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes>,
        U: From<Bytes> + Send,
    {
        let (parts, body) = req.into_parts();
        let req = self
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body.into());

        async move {
            let response = req.send().await.context(ReqwestSnafu { stage: "send" })?;
            if !response.status().is_success() {
                let status = response.status();

                let text = response.text().await.context(ReqwestSnafu {
                    stage: "send-error-text",
                })?;

                return Err(InvalidResponseSnafu {
                    status,
                    message: text,
                }
                .build());
            }

            let mut res = Response::builder().status(response.status());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let body: LazyBody<U> = Box::pin(async {
                let bytes = response
                    .bytes()
                    .await
                    .context(ReqwestSnafu { stage: "bytes" })?;

                let body = U::from(bytes);
                Ok(body)
            });

            res.body(body).context(HttpSnafu { stage: "body" })
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes>,
        U: Send + 'static,
    {
        let (parts, body) = req.into_parts();
        let body = reqwest::multipart::Form::from(body);

        let req = self
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .multipart(body);

        async move {
            let response = req.send().await.context(ReqwestSnafu {
                stage: "send-multipart",
            })?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.context(ReqwestSnafu {
                    stage: "send-multipart-error-text",
                })?;

                return Err(InvalidResponseSnafu {
                    status,
                    message: text,
                }
                .build());
            }

            let mut res = Response::builder().status(response.status());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let body: LazyBody<U> = Box::pin(async {
                let bytes = response.bytes().await.context(ReqwestSnafu {
                    stage: "multipart-bytes",
                })?;

                let body = U::from(bytes);
                Ok(body)
            });

            res.body(body).context(HttpSnafu {
                stage: "multipart-body",
            })
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send
    where
        T: Into<Bytes>,
    {
        let (parts, body) = req.into_parts();

        let req = self
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body.into())
            .build()
            .context(ReqwestSnafu {
                stage: "streaming-request-build",
            })
            .unwrap();

        let client = self.clone();

        async move {
            let response: reqwest::Response = client.execute(req).await.context(ReqwestSnafu {
                stage: "send-streaming",
            })?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.context(ReqwestSnafu {
                    stage: "send-streaming-error-text",
                })?;

                return Err(InvalidResponseSnafu {
                    status,
                    message: text,
                }
                .build());
            }

            let mut res = Response::builder()
                .status(response.status())
                .version(response.version());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            use futures::StreamExt;

            let mapped_stream: Pin<Box<dyn WasmCompatSendStream<InnerItem = Result<Bytes>>>> =
                Box::pin(response.bytes_stream().map(|chunk| {
                    chunk.context(ReqwestSnafu {
                        stage: "streaming-chunk",
                    })
                }));

            res.body(mapped_stream).context(HttpSnafu {
                stage: "streaming-body",
            })
        }
    }
}

#[cfg(feature = "reqwest-middleware")]
#[cfg_attr(docsrs, doc(cfg(feature = "reqwest-middleware")))]
impl HttpClientExt for reqwest_middleware::ClientWithMiddleware {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes>,
    {
        let (parts, body) = req.into_parts();
        let req = self
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body.into());

        async move {
            let response = req
                .send()
                .await
                .context(ReqwestMiddlewareSnafu { stage: "send" })?;

            if !response.status().is_success() {
                let status = response.status();

                let text = response.text().await.context(ReqwestSnafu {
                    stage: "send-error-text",
                })?;

                return Err(InvalidResponseSnafu {
                    status,
                    message: text,
                }
                .build());
            }

            let mut res = Response::builder().status(response.status());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let body: LazyBody<U> = Box::pin(async {
                let bytes = response
                    .bytes()
                    .await
                    .context(ReqwestSnafu { stage: "bytes" })?;

                let body = U::from(bytes);
                Ok(body)
            });

            res.body(body).context(HttpSnafu { stage: "send-body" })
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes>,
        U: Send + 'static,
    {
        let (parts, body) = req.into_parts();
        let body = reqwest::multipart::Form::from(body);

        let req = self
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .multipart(body);

        async move {
            let response = req.send().await.context(ReqwestMiddlewareSnafu {
                stage: "send-multipart",
            })?;

            if !response.status().is_success() {
                let status = response.status();

                let text = response.text().await.context(ReqwestSnafu {
                    stage: "send-error-text",
                })?;

                return Err(InvalidResponseSnafu {
                    status,
                    message: text,
                }
                .build());
            }

            let mut res = Response::builder().status(response.status());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            let body: LazyBody<U> = Box::pin(async {
                let bytes = response.bytes().await.context(ReqwestSnafu {
                    stage: "multipart-bytes",
                })?;

                let body = U::from(bytes);
                Ok(body)
            });

            res.body(body).context(HttpSnafu {
                stage: "multipart-body",
            })
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send
    where
        T: Into<Bytes>,
    {
        let (parts, body) = req.into_parts();

        let req = self
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body.into())
            .build()
            .context(ReqwestSnafu {
                stage: "streaming-request-build",
            })
            .unwrap();

        let client = self.clone();

        async move {
            let response: reqwest::Response =
                client.execute(req).await.context(ReqwestMiddlewareSnafu {
                    stage: "send-streaming",
                })?;
            if !response.status().is_success() {
                let status = response.status();

                let text = response.text().await.context(ReqwestSnafu {
                    stage: "send-error-text",
                })?;

                return Err(InvalidResponseSnafu {
                    status,
                    message: text,
                }
                .build());
            }

            let mut res = Response::builder()
                .status(response.status())
                .version(response.version());

            if let Some(hs) = res.headers_mut() {
                *hs = response.headers().clone();
            }

            use futures::StreamExt;

            let mapped_stream: Pin<Box<dyn WasmCompatSendStream<InnerItem = Result<Bytes>>>> =
                Box::pin(response.bytes_stream().map(|chunk| {
                    chunk.context(ReqwestSnafu {
                        stage: "streaming-chunk",
                    })
                }));

            res.body(mapped_stream).context(HttpSnafu {
                stage: "streaming-body",
            })
        }
    }
}

/// Test utilities for mocking HTTP clients.
#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use bytes::Bytes;

    /// A mock HTTP client that returns pre-built SSE bytes from `send_streaming`.
    ///
    /// `send` and `send_multipart` always return `NOT_IMPLEMENTED`.
    #[derive(Clone)]
    pub struct MockStreamingClient {
        pub sse_bytes: Bytes,
    }

    impl HttpClientExt for MockStreamingClient {
        fn send<T, U>(
            &self,
            _req: Request<T>,
        ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + 'static
        where
            T: Into<Bytes>,
            U: From<Bytes>,
            U: 'static,
        {
            std::future::ready(Err(InvalidResponseSnafu {
                status: http::StatusCode::NOT_IMPLEMENTED,
                message: "Not implemented",
            }
            .build()))
        }

        fn send_multipart<U>(
            &self,
            _req: Request<MultipartForm>,
        ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + 'static
        where
            U: From<Bytes>,
            U: 'static,
        {
            std::future::ready(Err(InvalidResponseSnafu {
                status: http::StatusCode::NOT_IMPLEMENTED,
                message: "Not implemented",
            }
            .build()))
        }

        fn send_streaming<T>(
            &self,
            _req: Request<T>,
        ) -> impl Future<Output = Result<StreamingResponse>>
        where
            T: Into<Bytes>,
        {
            let sse_bytes = self.sse_bytes.clone();
            async move {
                let byte_stream = futures::stream::iter(vec![Ok::<Bytes, Error>(sse_bytes)]);
                let boxed_stream: sse::BoxedStream = Box::pin(byte_stream);

                Response::builder()
                    .status(http::StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "text/event-stream")
                    .body(boxed_stream)
                    .context(HttpSnafu {
                        stage: "streaming-body",
                    })
            }
        }
    }
}
