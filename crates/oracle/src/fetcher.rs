//! Price fetcher trait and implementations

use crate::PricePair;

/// Parse a decimal string (e.g. `"123.456789"`) into a fixed-point `u128`
/// with `precision` fractional digits, without using `f64`.
fn parse_decimal_to_u128(s: &str, precision: u32) -> Option<u128> {
    let mut parts = s.split('.');
    let int_part = parts.next()?;
    let frac_part = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return None; // More than one dot
    }

    let int_val = int_part.parse::<u128>().ok()?;
    let mut frac_val: u128 = 0;
    let mut digits = 0;
    for ch in frac_part.chars() {
        if !ch.is_ascii_digit() {
            return None;
        }
        if digits < precision {
            frac_val = frac_val * 10 + (ch as u8 - b'0') as u128;
            digits += 1;
        }
    }
    // Pad remaining precision digits with zeros
    while digits < precision {
        frac_val *= 10;
        digits += 1;
    }

    let scale = 10u128.checked_pow(precision)?;
    int_val.checked_mul(scale)?.checked_add(frac_val)
}
#[cfg(feature = "http-fetcher")]
use call_primitives::AssetId;

/// Trait for fetching prices from external data sources.
/// Validators implement this to provide real-time price data
/// for oracle submissions.
pub trait PriceFetcher: Send + Sync {
    /// Fetch the current price for a pair. Returns price in smallest units.
    fn fetch_price(&self, pair: PricePair) -> Option<u128>;
    /// Data source names this fetcher uses (e.g., ["binance", "coinbase"])
    fn sources(&self) -> Vec<String>;
}

/// No-op price fetcher — used in devnet or when no external API is configured.
pub struct NoOpPriceFetcher;

impl PriceFetcher for NoOpPriceFetcher {
    fn fetch_price(&self, _pair: PricePair) -> Option<u128> {
        None
    }

    fn sources(&self) -> Vec<String> {
        Vec::new()
    }
}

/// HTTP-based price fetcher for production use.
/// Maps asset IDs to API endpoint URLs and queries them on demand.
///
/// Requires the `http-fetcher` feature flag.
#[cfg(feature = "http-fetcher")]
pub struct HttpPriceFetcher {
    endpoints: std::collections::HashMap<AssetId, String>,
    client: reqwest::blocking::Client,
}

#[cfg(feature = "http-fetcher")]
impl HttpPriceFetcher {
    /// Create a new HttpPriceFetcher with configured endpoints.
    /// Each asset_id maps to an HTTP URL that returns a JSON object
    /// with a `"price"` field containing a numeric string or float.
    pub fn new(endpoints: std::collections::HashMap<AssetId, String>) -> Self {
        Self {
            endpoints,
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Add an endpoint for a specific asset.
    pub fn with_endpoint(mut self, asset_id: AssetId, url: String) -> Self {
        self.endpoints.insert(asset_id, url);
        self
    }
}

#[cfg(feature = "http-fetcher")]
impl PriceFetcher for HttpPriceFetcher {
    fn fetch_price(&self, pair: PricePair) -> Option<u128> {
        let url = self.endpoints.get(&pair.base)?;

        let resp = self.client.get(url).send().ok()?;
        let body: serde_json::Value = resp.json().ok()?;
        // Support common formats: {"price": "123.45"}, {"lastPrice": "123.45"},
        // {"price": 123.45}, or a plain number.
        let price_node = body
            .get("price")
            .or_else(|| body.get("lastPrice"))
            .or_else(|| body.get("last"))?;
        let price_str = match price_node {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Number(n) => Some(n.as_str()),
            _ => None,
        }?;
        // Parse decimal string without losing precision via f64.
        // Assumes 6-decimal fixed-point output for the chain.
        parse_decimal_to_u128(price_str, 6)
    }

    fn sources(&self) -> Vec<String> {
        vec!["http".into()]
    }
}
