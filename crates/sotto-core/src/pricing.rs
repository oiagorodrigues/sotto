//! Dated price table (`config/pricing.toml`) and cost computation from token usage.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::providers::Usage;

const BUNDLED: &str = include_str!("../../../config/pricing.toml");

/// Model id used for any Ollama model; local inference has no per-token price.
pub const OLLAMA_PRICE_KEY: &str = "ollama";

/// File name of the user's price overrides in the app data directory.
pub const OVERRIDE_FILE: &str = "pricing.toml";

#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    #[error("cannot read pricing file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid pricing file: {0}")]
    Parse(#[from] toml::de::Error),
}

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Streaming transcription is billed by the minute of audio, not by token.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionPrice {
    pub usd_per_minute: f64,
    /// When the provider's page listed this price.
    #[serde(default)]
    pub as_of: String,
}

/// `as_of` and `models` may be left out of an override file, which only lists the models it
/// changes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceTable {
    #[serde(default)]
    pub as_of: String,
    #[serde(default)]
    pub models: HashMap<String, ModelPrice>,
    #[serde(default)]
    pub transcription: HashMap<String, TranscriptionPrice>,
}

impl PriceTable {
    pub fn parse(text: &str) -> Result<Self, PricingError> {
        Ok(toml::from_str(text)?)
    }

    pub fn load(path: &Path) -> Result<Self, PricingError> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    /// The table shipped in `config/pricing.toml`, embedded at build time.
    pub fn bundled() -> Result<Self, PricingError> {
        Self::parse(BUNDLED)
    }

    /// Reads an override file; a missing file is `Ok(None)`.
    pub fn load_override(path: &Path) -> Result<Option<Self>, PricingError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Entries in `other` replace this table's entries for the same model.
    pub fn merge(&mut self, other: Self) {
        self.models.extend(other.models);
        self.transcription.extend(other.transcription);
    }

    /// Cost in millionths of a dollar of `audio_ms` milliseconds of audio sent to a
    /// transcription model. Unknown models cost 0.
    #[must_use]
    pub fn transcription_cost_micro_usd(&self, model: &str, audio_ms: u64) -> u64 {
        self.transcription.get(model).map_or(0, |p| {
            // usd/min * 1e6 micro-usd/usd / 60_000 ms/min
            (audio_ms as f64 * p.usd_per_minute * 1_000_000.0 / 60_000.0).round() as u64
        })
    }

    /// Whether usage of `model` can be costed: both token prices must be above zero. A zero or
    /// missing price would make the spend cap count nothing.
    #[must_use]
    pub fn is_priced(&self, model: &str) -> bool {
        self.price(model)
            .is_some_and(|p| p.input > 0.0 && p.output > 0.0)
    }

    #[must_use]
    pub fn price(&self, model: &str) -> Option<&ModelPrice> {
        self.models.get(model)
    }

    /// Cost in millionths of a dollar. One USD-per-million-tokens price is exactly one
    /// micro-USD per token, so no unit conversion is needed. Unknown models cost 0.
    #[must_use]
    pub fn cost_micro_usd(&self, model: &str, usage: &Usage) -> u64 {
        let Some(p) = self.price(model) else {
            return 0;
        };
        let total = usage.input_tokens as f64 * p.input
            + usage.output_tokens as f64 * p.output
            + usage.cache_read_input_tokens as f64 * p.cache_read
            + usage.cache_creation_input_tokens as f64 * p.cache_write;
        total.round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(i: u64, o: u64, r: u64, w: u64) -> Usage {
        Usage {
            input_tokens: i,
            output_tokens: o,
            cache_read_input_tokens: r,
            cache_creation_input_tokens: w,
        }
    }

    #[test]
    fn bundled_table_parses_with_expected_prices() {
        let t = PriceTable::bundled().unwrap();
        assert_eq!(t.as_of, "2026-10-06");
        assert_eq!(t.price("claude-haiku-5-5").unwrap().output, 0.50);
        assert_eq!(t.price("claude-opus-5-5").unwrap().cache_read, 0.20);
        assert_eq!(t.price(OLLAMA_PRICE_KEY).unwrap().input, 0.0);
    }

    #[test]
    fn computes_micro_usd() {
        let t = PriceTable::bundled().unwrap();
        // 1M input at $2 + 1M output at $10 on Sonnet = $12 = 12_000_000 micro-USD.
        assert_eq!(
            t.cost_micro_usd("claude-sonnet-5-5", &usage(1_000_000, 1_000_000, 0, 0)),
            12_000_000
        );
        // 1,000 input + 200 output + 5,000 cache read on Haiku: 100 + 100 + 50.
        assert_eq!(
            t.cost_micro_usd("claude-haiku-5-5", &usage(1_000, 200, 5_000, 0)),
            250
        );
    }

    #[test]
    fn unknown_model_costs_zero() {
        let t = PriceTable::bundled().unwrap();
        assert_eq!(t.cost_micro_usd("nope", &usage(10, 10, 10, 10)), 0);
    }

    #[test]
    fn openai_has_no_invented_price_and_overrides_merge() {
        let mut t = PriceTable::bundled().unwrap();
        assert!(t.is_priced("claude-haiku-5-5"));
        for m in ["gpt-4o", "gpt-5", "gpt-5-mini", "nope", OLLAMA_PRICE_KEY] {
            assert!(!t.is_priced(m), "{m}");
        }
        let o = PriceTable::parse(
            "[models.\"gpt-5-mini\"]\ninput = 0.25\noutput = 2.0\ncache_read = 0.025\ncache_write = 0.0\n\
             [models.\"claude-haiku-5-5\"]\ninput = 0.2\noutput = 1.0\ncache_read = 0.02\ncache_write = 0.25\n",
        )
        .unwrap();
        t.merge(o);
        assert!(t.is_priced("gpt-5-mini"));
        assert_eq!(t.price("claude-haiku-5-5").unwrap().input, 0.2);
        assert_eq!(t.price("claude-sonnet-5-5").unwrap().input, 2.0);
        assert_eq!(t.as_of, "2026-10-06");
    }

    #[test]
    fn override_file_is_optional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(OVERRIDE_FILE);
        assert_eq!(PriceTable::load_override(&path).unwrap(), None);
        std::fs::write(&path, "[models.x]\ninput = 1.0\n").unwrap();
        assert!(PriceTable::load_override(&path).is_err());
    }

    #[test]
    fn transcription_is_priced_per_minute_of_audio() {
        let t = PriceTable::bundled().unwrap();
        let p = t.transcription.get("gpt-live-transcribe").unwrap();
        assert_eq!(p.usd_per_minute, 0.017);
        assert!(!p.as_of.is_empty());
        // One minute is 17,000 micro-USD; 30 s is half.
        assert_eq!(
            t.transcription_cost_micro_usd("gpt-live-transcribe", 60_000),
            17_000
        );
        assert_eq!(
            t.transcription_cost_micro_usd("gpt-live-transcribe", 30_000),
            8_500
        );
        assert_eq!(t.transcription_cost_micro_usd("nope", 60_000), 0);
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(PriceTable::parse("as_of = \"x\"\nbogus = 1\n[models]").is_err());
    }
}
