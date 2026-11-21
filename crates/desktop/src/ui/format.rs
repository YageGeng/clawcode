//! Localized display text for data-layer values: labels, durations, and one-line text.
//!
//! The data modules (`projection`, `transcript`, `catalog`) stay free of gpui
//! and i18n; their user-facing wording lives here as inherent impls.

use std::borrow::Cow;

use gpui_kit::SharedString;

use crate::catalog::RelativeAge;
use crate::projection::{CommandSource, ToolCategory};
use crate::transcript::RunSummary;

impl ToolCategory {
    /// Localized action verb such as `Read` or `读取`.
    pub fn verb(self) -> SharedString {
        match self {
            ToolCategory::Read => rust_i18n::t!("Tool.read"),
            ToolCategory::Edit => rust_i18n::t!("Tool.edit"),
            ToolCategory::Delete => rust_i18n::t!("Tool.delete"),
            ToolCategory::Move => rust_i18n::t!("Tool.move"),
            ToolCategory::Search => rust_i18n::t!("Tool.search"),
            ToolCategory::Execute => rust_i18n::t!("Tool.execute"),
            ToolCategory::Fetch => rust_i18n::t!("Tool.fetch"),
            ToolCategory::Think => rust_i18n::t!("Tool.think"),
            ToolCategory::Other => rust_i18n::t!("Tool.other"),
        }
        .into_owned()
        .into()
    }
}

/// A duration in milliseconds, shown as `14s`, `1m 28s`, or `1h 2m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elapsed(pub u64);

impl Elapsed {
    /// Localized label with the two most significant units.
    pub fn label(self) -> SharedString {
        let seconds = self.0 / 1000;
        match seconds {
            0..60 => rust_i18n::t!("Elapsed.seconds", s = seconds),
            60..3600 => rust_i18n::t!(
                "Elapsed.minutes",
                m = seconds / 60,
                s = seconds % 60
            ),
            _ => rust_i18n::t!(
                "Elapsed.hours",
                h = seconds / 3600,
                m = seconds % 3600 / 60
            ),
        }
        .into_owned()
        .into()
    }
}

impl RunSummary {
    /// `Worked 1m 28s · Read 3 · Edit 2`; the time is omitted when unknown.
    pub fn label(&self) -> SharedString {
        let mut label = match self.elapsed_ms {
            Some(ms) => rust_i18n::t!(
                "Transcript.worked",
                elapsed = Elapsed(ms).label().as_ref()
            )
            .into_owned(),
            None => rust_i18n::t!("Transcript.worked_plain").into_owned(),
        };
        for (category, count) in &self.counts {
            label.push_str(&format!(" · {} {count}", category.verb()));
        }
        label.into()
    }
}

/// One display line of text that may span several: the first non-empty line,
/// trimmed, followed by ` …` when more non-empty text comes after it.
///
/// Rows of fixed height (tool targets, user-run commands, titles) would
/// otherwise lay out the extra lines and clip them.
pub fn single_line(text: &str) -> std::borrow::Cow<'_, str> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    match (lines.next(), lines.next()) {
        (None, _) => std::borrow::Cow::Borrowed(""),
        (Some(first), None) => std::borrow::Cow::Borrowed(first),
        (Some(first), Some(_)) => {
            std::borrow::Cow::Owned(format!("{first} \u{2026}"))
        }
    }
}

impl RelativeAge {
    /// Short localized label such as `3d` or `3 天`.
    pub fn label(self) -> SharedString {
        match self {
            RelativeAge::Now => rust_i18n::t!("Age.now"),
            RelativeAge::Minutes(count) => {
                rust_i18n::t!("Age.minutes", count = count)
            }
            RelativeAge::Hours(count) => {
                rust_i18n::t!("Age.hours", count = count)
            }
            RelativeAge::Days(count) => {
                rust_i18n::t!("Age.days", count = count)
            }
            RelativeAge::Weeks(count) => {
                rust_i18n::t!("Age.weeks", count = count)
            }
        }
        .into_owned()
        .into()
    }
}

/// A token count shown compactly: `999`, `12.4k`, `1.2M`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenCount(pub u64);

impl TokenCount {
    /// Compact label; decimals are truncated, not rounded, the same in every locale.
    pub fn label(self) -> SharedString {
        let n = self.0;
        match n {
            0..1_000 => n.to_string(),
            1_000..1_000_000 => {
                let tenths = n / 100;
                format!("{}.{}k", tenths / 10, tenths % 10)
            }
            _ => {
                let tenths = n / 100_000;
                format!("{}.{}M", tenths / 10, tenths % 10)
            }
        }
        .into()
    }
}

impl crate::transcript::RunUsage {
    /// `Model · in 12.4k · out 1.8k · cache 71% · 1m 28s`; unknown parts are left out.
    ///
    /// `model_name` is the display name of the reply's model; without one the
    /// model id is shown.
    pub fn label(&self, model_name: Option<&str>) -> SharedString {
        let mut parts: Vec<String> = Vec::new();
        if let Some(name) = model_name
            .map(str::to_owned)
            .or_else(|| self.model.as_ref().map(|(_, model)| model.clone()))
        {
            parts.push(name);
        }
        if let Some(input) = self.usage.input {
            parts.push(
                rust_i18n::t!(
                    "Transcript.usage_in",
                    count = TokenCount(input).label().as_ref()
                )
                .into_owned(),
            );
        }
        if let Some(output) = self.usage.output {
            parts.push(
                rust_i18n::t!(
                    "Transcript.usage_out",
                    count = TokenCount(output).label().as_ref()
                )
                .into_owned(),
            );
        }
        if let Some(permille) = self.usage.cache_hit_permille() {
            parts.push(
                rust_i18n::t!(
                    "Transcript.usage_cache",
                    percent = permille / 10
                )
                .into_owned(),
            );
        }
        if let Some(ms) = self.elapsed_ms {
            parts.push(Elapsed(ms).label().to_string());
        }
        parts.join(" · ").into()
    }
}

impl CommandSource {
    /// Localized name of where a slash command comes from.
    pub fn label(self) -> Cow<'static, str> {
        match self {
            CommandSource::Builtin => rust_i18n::t!("Command.source_builtin"),
            CommandSource::Extension => {
                rust_i18n::t!("Command.source_extension")
            }
            CommandSource::Skill => rust_i18n::t!("Command.source_skill"),
            CommandSource::Template => rust_i18n::t!("Command.source_template"),
        }
    }
}
