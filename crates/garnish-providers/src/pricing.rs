//! Simple price table: USD per million tokens, bundled defaults with an
//! optional user override file at <data_dir>/prices.json (same shape).
//! Unknown model -> None; the cost ledger stores tokens with usd = NULL
//! rather than guessing.

use crate::model::Usage;

const BUNDLED: &str = include_str!("../fixtures/prices.json");

fn table(override_file: Option<&std::path::Path>) -> serde_json::Value {
    let mut prices: serde_json::Value = serde_json::from_str(BUNDLED).expect("bundled prices.json invalid");
    if let Some(path) = override_file {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(serde_json::Value::Object(user)) = serde_json::from_str(&text) {
                for (k, v) in user {
                    prices[k] = v;
                }
            }
        }
    }
    prices
}

/// Cost in USD for a usage record, if the model is priced. Cache-read tokens
/// are billed at the model's cache rate when present, else the input rate.
pub fn cost_usd(model: &str, usage: &Usage) -> Option<f64> {
    cost_usd_at(model, usage, None)
}

/// Like `cost_usd`, with a user override file (typically
/// `<data_dir>/prices.json`) merged over the bundled table.
pub fn cost_usd_at(model: &str, usage: &Usage, override_file: Option<&std::path::Path>) -> Option<f64> {
    let prices = table(override_file);
    let entry = prices.get(model)?;
    let per_m_in = entry["in"].as_f64()?;
    let per_m_out = entry["out"].as_f64()?;
    let per_m_cache = entry["cache_read"].as_f64().unwrap_or(per_m_in);
    let fresh_in = usage.input_tokens.saturating_sub(usage.cache_read_tokens);
    Some(
        (fresh_in as f64 * per_m_in
            + usage.cache_read_tokens as f64 * per_m_cache
            + usage.output_tokens as f64 * per_m_out)
            / 1_000_000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_known_model_and_rejects_unknown() {
        let usage = Usage { input_tokens: 1_000_000, output_tokens: 1_000_000, cache_read_tokens: 0 };
        let c = cost_usd("claude-sonnet-5", &usage).unwrap();
        assert!(c > 0.0);
        assert!(cost_usd("model-that-does-not-exist", &usage).is_none());
    }

    #[test]
    fn cache_read_is_cheaper() {
        let fresh = Usage { input_tokens: 1_000_000, output_tokens: 0, cache_read_tokens: 0 };
        let cached = Usage { input_tokens: 1_000_000, output_tokens: 0, cache_read_tokens: 1_000_000 };
        assert!(cost_usd("claude-sonnet-5", &cached).unwrap() < cost_usd("claude-sonnet-5", &fresh).unwrap());
    }
}
