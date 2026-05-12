//! Agent domain verification via DNS TXT and HTTP file checks.
//!
//! Enabled by the `domain-verify` feature. When disabled, all verification
//! functions return `Ok(false)` so the crate compiles without heavy deps.

/// Verify agent domain ownership by checking DNS TXT records.
///
/// Looks for TXT records on the domain and returns `true` if any record
/// contains `expected_token` as a substring.
#[cfg(feature = "domain-verify")]
pub fn verify_domain_dns_txt(domain: &str, expected_token: &str) -> Result<bool, String> {
    use hickory_resolver::{config::ResolverConfig, config::ResolverOpts, Resolver};

    let resolver = Resolver::new(ResolverConfig::default(), ResolverOpts::default())
        .map_err(|e| format!("dns resolver init failed: {e}"))?;

    let lookup = resolver
        .txt_lookup(domain)
        .map_err(|e| format!("dns txt lookup failed: {e}"))?;

    for record in lookup.iter() {
        let data = record.txt_data();
        for txt in data {
            if let Ok(s) = String::from_utf8(txt.to_vec()) {
                if s.contains(expected_token) {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

#[cfg(not(feature = "domain-verify"))]
pub fn verify_domain_dns_txt(_domain: &str, _expected_token: &str) -> Result<bool, String> {
    Ok(false)
}

/// Verify agent domain ownership by fetching a well-known HTTP file.
///
/// Fetches `{url}/.well-known/callchain-agent.txt` and returns `true` if the
/// response body contains `expected_token` as a substring.
#[cfg(feature = "domain-verify")]
pub fn verify_domain_http_file(url: &str, expected_token: &str) -> Result<bool, String> {
    let base = url.trim_end_matches('/');
    let verify_url = format!("{base}/.well-known/callchain-agent.txt");

    let response = ureq::get(&verify_url)
        .set("User-Agent", "Callchain-Agent-Verifier/1.0")
        .call()
        .map_err(|e| format!("http fetch failed: {e}"))?;

    let body = response
        .into_string()
        .map_err(|e| format!("http read body failed: {e}"))?;

    Ok(body.contains(expected_token))
}

#[cfg(not(feature = "domain-verify"))]
pub fn verify_domain_http_file(_url: &str, _expected_token: &str) -> Result<bool, String> {
    Ok(false)
}

/// Attempt both DNS TXT and HTTP verification.
/// Returns `true` if either method succeeds.
pub fn verify_domain(domain: &str, expected_token: &str) -> Result<bool, String> {
    // Try DNS first
    if verify_domain_dns_txt(domain, expected_token)? {
        return Ok(true);
    }

    // Fallback to HTTP
    verify_domain_http_file(domain, expected_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "domain-verify"))]
    fn test_verify_domain_no_feature_returns_false() {
        // When domain-verify is not enabled, both functions return false
        let result = verify_domain_dns_txt("example.com", "test");
        assert!(result.is_ok());
        assert!(!result.unwrap());

        let result = verify_domain_http_file("https://example.com", "test");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    #[cfg(not(feature = "domain-verify"))]
    fn test_verify_domain_wrapper_no_feature() {
        let result = verify_domain("example.com", "test");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    // ── Real network tests (ignored by default) ────────────────────────

    /// Verify that DNS TXT lookup works against a domain known to have TXT records.
    #[ignore = "network: requires live DNS"]
    #[test]
    #[cfg(feature = "domain-verify")]
    fn test_verify_domain_dns_txt_google_has_records() {
        // google.com reliably has SPF TXT records containing "google"
        let result = verify_domain_dns_txt("google.com", "google");
        assert!(result.is_ok(), "dns lookup failed: {:?}", result.err());
        assert!(
            result.unwrap(),
            "expected to find 'google' in google.com TXT records"
        );
    }

    /// Verify that DNS TXT lookup returns false when token is not present.
    #[ignore = "network: requires live DNS"]
    #[test]
    #[cfg(feature = "domain-verify")]
    fn test_verify_domain_dns_txt_no_match() {
        let result = verify_domain_dns_txt("google.com", "this-is-definitely-not-present-xyz");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    /// Verify HTTP fetch against example.com (no .well-known file, should 404).
    #[ignore = "network: requires live HTTP"]
    #[test]
    #[cfg(feature = "domain-verify")]
    fn test_verify_domain_http_file_not_found() {
        // example.com does not host /.well-known/callchain-agent.txt
        let result = verify_domain_http_file("https://example.com", "anything");
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    /// Verify HTTP fetch against a domain that hosts the verification file.
    #[ignore = "network: requires live HTTP"]
    #[test]
    #[cfg(feature = "domain-verify")]
    fn test_verify_domain_http_file_found() {
        // This test requires a real server hosting the file.
        // Replace with a known-verified domain before running.
        let result = verify_domain_http_file("https://example.com", "test");
        // We just assert it doesn't panic; result depends on external state
        assert!(result.is_ok());
    }

    /// DNS lookup on a non-existent domain should fail gracefully.
    #[ignore = "network: requires live DNS"]
    #[test]
    #[cfg(feature = "domain-verify")]
    fn test_verify_domain_dns_nonexistent_domain() {
        let result = verify_domain_dns_txt(
            "this-domain-definitely-does-not-exist-12345.invalid",
            "test",
        );
        assert!(
            result.is_err() || !result.unwrap(),
            "non-existent domain should not verify"
        );
    }
}
