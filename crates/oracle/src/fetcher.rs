//! Price fetcher trait and implementations

#[cfg(feature = "http-fetcher")]
use call_primitives::AssetId;
use crate::PricePair;

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
    client: reqwest::Client,
}

#[cfg(feature = "http-fetcher")]
impl HttpPriceFetcher {
    /// Create a new HttpPriceFetcher with configured endpoints.
    /// Each asset_id maps to an HTTP URL that returns a JSON object
    /// with a `"price"` field containing a numeric string or float.
    pub fn new(endpoints: std::collections::HashMap<AssetId, String>) -> Self {
        Self {
            endpoints,
            client: reqwest::Client::new(),
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

        // Blocking call — acceptable in the oracle context where we
        // already have a configurable delay window.
        let rt = tokio::runtime::Handle::try_current().ok()?;
        let future = async {
            let resp = self.client.get(url).send().await.ok()?;
            let body: serde_json::Value = resp.json().await.ok()?;
            // Support common formats: {"price": "123.45"}, {"lastPrice": "123.45"},
            // or a plain number
            let price_str = body.get("price")
                .or_else(|| body.get("lastPrice"))
                .or_else(|| body.get("last"))?
                .as_str()?;
            // Parse as f64 then convert to u128 (price in smallest units)
            price_str.parse::<f64>().ok().map(|f| f as u128)
        };

        tokio::task::block_in_place(|| rt.block_on(future))
    }

    fn sources(&self) -> Vec<String> {
        vec!["http".into()]
    }
}
