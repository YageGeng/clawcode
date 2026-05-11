#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used,
        clippy::unreachable
    )
)]
//! Provider-focused LLM client crate.

#![allow(dead_code)]

extern crate self as rig;

pub mod client;
pub mod completion;
pub mod factory;
pub mod http_client;
pub(crate) mod json_utils;
pub mod markers;
pub mod model;
pub mod one_or_many;
pub mod prelude;
pub mod providers;

pub mod streaming;
pub mod wasm_compat;

// Re-export commonly used types and traits
pub use completion::message;
pub use one_or_many::{EmptyListError, OneOrMany};

pub mod telemetry;
