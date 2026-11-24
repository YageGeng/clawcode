//! Native desktop client built on gpui-kit, talking to the in-process ACP backend.

// Loads `locales/app.yml`; `rust_i18n::t!` anywhere in the crate reads from it.
rust_i18n::i18n!("locales", fallback = "en");

pub mod actions;
pub mod app;
pub mod assets;
pub mod backend;
pub mod branch;
pub mod catalog;
pub mod file;
pub mod i18n;
pub mod projection;
pub mod session;
pub mod state;
pub mod theme;
pub mod trajectory;
pub mod transcript;
pub mod ui;
pub mod utils;
pub mod workspace;
