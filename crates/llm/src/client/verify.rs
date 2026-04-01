use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum VerifyError {
    #[snafu(display("invalid authentication"))]
    Auth,
    #[snafu(display("provider error: {message}"))]
    Provider { message: String },
    #[snafu(display("http error: stage={stage}, source={source}"))]
    VerifyHttp {
        source: crate::http_client::Error,
        stage: String,
    },
}

/// A provider client that can verify the configuration.
/// Clone is required for conversions between client types.
pub trait VerifyClient {
    /// Verify the configuration.
    fn verify(&self) -> impl Future<Output = Result<(), VerifyError>> + Send;
}
