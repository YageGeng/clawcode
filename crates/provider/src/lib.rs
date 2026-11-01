//! Configured upstream models with shared transport and authentication.

mod auth;
pub mod factory;

pub use factory::{
    ConfiguredModel, ProviderBuildError, ProviderConnectError, ProviderFactory,
};
