use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;

pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait WasmCompatSendStream:
    Stream<Item = Result<Bytes, crate::http_client::Error>> + Send
{
    type InnerItem: Send;
}

impl<T> WasmCompatSendStream for T
where
    T: Stream<Item = Result<Bytes, crate::http_client::Error>> + Send,
{
    type InnerItem = Result<Bytes, crate::http_client::Error>;
}
