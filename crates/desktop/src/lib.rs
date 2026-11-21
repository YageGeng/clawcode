//! Native desktop client built on gpui-kit, talking to the in-process ACP backend.

// Loads `locales/app.yml`; `rust_i18n::t!` anywhere in the crate reads from it.
rust_i18n::i18n!("locales", fallback = "en");

pub mod actions;
pub mod app;
pub mod assets;
pub mod backend;
pub mod catalog;
pub mod file;
pub mod i18n;
pub mod projection;
pub mod session;
pub mod state;
pub mod theme;
pub mod transcript;
pub mod ui;
pub mod workspace;

/// The current Unix time in milliseconds; `0` when the clock reads before the
/// epoch, `u64::MAX` past the year 584 million.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
