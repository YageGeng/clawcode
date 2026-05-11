//! Provider-agnostic chat message types.
//!
//! These types are defined in `protocol` and re-exported here
//! for backward compatibility.

pub use protocol::message::*;

/// A local trait to convert protocol messages into provider-specific message types.
/// This avoids the orphan rule problem with `TryFrom` / `TryInto` for foreign types.
pub trait TryIntoMany<T> {
    /// The error type returned when conversion fails.
    type Error;
    /// Consumes self and attempts to convert into a `Vec<T>` of provider messages.
    fn try_into_many(self) -> Result<Vec<T>, Self::Error>;
}

/// A local trait to convert protocol messages into provider-specific message vectors.
/// This avoids the orphan rule problem with `From` / `Into` for foreign types.
///
/// Unlike [`TryIntoMany`], this conversion is infallible — it always succeeds
/// in producing a `Vec<T>` of the target provider message type.
pub trait IntoMany<T> {
    /// Consumes self and converts into a `Vec<T>` of provider messages.
    fn into_many(self) -> Vec<T>;
}

// Re-add the From impl that depends on CompletionError (provider-only type)
use crate::completion::CompletionError;

impl From<MessageError> for CompletionError {
    fn from(error: MessageError) -> Self {
        CompletionError::RequestError(error.into())
    }
}
