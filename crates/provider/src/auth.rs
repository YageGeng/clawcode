//! Upstream authentication assembly; credential format and refresh remain upstream-owned.

use rig_core::providers::chatgpt::auth::{
    AuthSource, Authenticator, DeviceCodeHandler, FileTokenStore,
};

use crate::factory::ConfiguredModel;

impl ConfiguredModel {
    /// Uses an independent upstream store and keeps device prompts off ACP stdout.
    pub(super) fn authenticator(
        config: &config::ProviderAuthConfig,
    ) -> Authenticator {
        let config::ProviderAuthConfig::Chatgpt { token_store_dir } = config;
        Authenticator::new(
            AuthSource::OAuth,
            FileTokenStore::new(token_store_dir),
            DeviceCodeHandler::new(|prompt| {
                // This is an interactive sign-in prompt, not a credential log event.
                eprintln!(
                    "ChatGPT sign-in: {} (code: {})",
                    prompt.verification_uri, prompt.user_code
                );
            }),
            true,
        )
    }
}
