//! Upstream token accounting with a versioned, read-compatible storage boundary.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

pub use rig_core::completion::Usage;

use crate::message::{decimal_u64, optional_decimal_u64};

/// Owns upstream accounting without turning missing counters into reported zero.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModelUsage(Usage);

impl From<Usage> for ModelUsage {
    /// Retains the provider's inclusive counts, optional values, and cost.
    fn from(usage: Usage) -> Self {
        Self(usage)
    }
}

impl AsRef<Usage> for ModelUsage {
    /// Borrows the upstream accounting contract for consumers and projections.
    fn as_ref(&self) -> &Usage {
        &self.0
    }
}

impl std::ops::AddAssign<&Self> for ModelUsage {
    /// Delegates partial-counter and cost accumulation to the upstream owner.
    fn add_assign(&mut self, other: &Self) {
        self.0 += other.0;
    }
}

/// The version tag distinguishes inclusive upstream counts from historical usage.
#[derive(Serialize, Deserialize)]
struct UsageRecord {
    format: String,
    usage: Usage,
}

/// Historical counters used zero for absence and mixed cached-input conventions.
/// Only serde constructs this compatibility record; the builder derive follows the workspace rule.
#[derive(Deserialize, typed_builder::TypedBuilder)]
struct LegacyUsage {
    // Validate the legacy field on input, but do not trust its ambiguous cache accounting.
    #[serde(rename = "input_tokens", with = "decimal_u64")]
    _input_tokens: u64,
    #[serde(with = "decimal_u64")]
    output_tokens: u64,
    #[serde(with = "decimal_u64")]
    total_tokens: u64,
    #[serde(with = "decimal_u64")]
    cache_read_tokens: u64,
    #[serde(with = "decimal_u64")]
    cache_write_tokens: u64,
    #[serde(default, with = "optional_decimal_u64")]
    #[builder(default)]
    reasoning_tokens: Option<u64>,
}

impl From<LegacyUsage> for ModelUsage {
    /// Recovers only counts whose meaning is provable without guessing a provider.
    fn from(legacy: LegacyUsage) -> Self {
        let LegacyUsage {
            _input_tokens: _,
            output_tokens,
            total_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        } = legacy;
        let output = (output_tokens > 0).then_some(output_tokens);
        let mut total = (total_tokens > 0).then_some(total_tokens);
        let mut input = total
            .zip(output)
            .and_then(|(total, output)| total.checked_sub(output));
        let invalid = total.is_some_and(|total| total < output_tokens)
            || input.is_some_and(|input| {
                cache_read_tokens
                    .checked_add(cache_write_tokens)
                    .is_none_or(|cache| cache > input)
            });
        if invalid {
            input = None;
            total = None;
        }
        Usage::new()
            .input_tokens(input)
            .output_tokens(output)
            .total_tokens(total)
            .cached_input_tokens(
                (cache_read_tokens > 0).then_some(cache_read_tokens),
            )
            .cache_creation_input_tokens(
                (cache_write_tokens > 0).then_some(cache_write_tokens),
            )
            .reasoning_tokens(reasoning_tokens)
            .into()
    }
}

impl Serialize for ModelUsage {
    /// Writes an explicit format tag while retaining upstream field meanings.
    fn serialize<S: Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        UsageRecord {
            format: "rig-core-v1".to_owned(),
            usage: self.0,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ModelUsage {
    /// Reads old records in memory without rewriting files or guessing future formats.
    fn deserialize<D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(format) = value.get("format") {
            if format.as_str() != Some("rig-core-v1") {
                return Err(D::Error::custom(format!(
                    "unsupported usage format: {format}"
                )));
            }
            serde_json::from_value::<UsageRecord>(value)
                .map(|record| Self(record.usage))
                .map_err(D::Error::custom)
        } else {
            serde_json::from_value::<LegacyUsage>(value)
                .map(Self::from)
                .map_err(D::Error::custom)
        }
    }
}
