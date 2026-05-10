//! Provider integrations kept in this crate.
//!
//! The crate now only carries the chat-completion providers that are still in
//! active use.
//!
//! # Example
//! ```no_run
//! use provider::client::{CompletionClient, ProviderClient};
//! use provider::providers::openai;
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let openai = openai::Client::from_env()?;
//! let _model = openai.completion_model(openai::GPT_5_2);
//! # Ok(())
//! # }
//! ```
pub mod anthropic;
pub mod chatgpt;
pub mod deepseek;
pub(crate) mod internal;
pub mod minimax;
pub mod moonshot;
pub mod openai;
pub mod xiaomimimo;
