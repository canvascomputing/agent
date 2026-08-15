//! One HTTP call to an LLM API: where it goes, how long it may take, and what
//! a non-2xx answer means.
//!
//! Every provider owns one of these. The vendor code adds its own
//! authentication headers and its own reading of a 400 body; everything else
//! about the call is decided here, so no two vendors can disagree about it.

use std::time::Duration;

use serde_json::Value;

use super::environment;
use super::error::{ProviderError, ProviderResult};
use super::patterns;

pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Where a provider sends its requests, and the client that carries them.
pub(crate) struct Endpoint {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
    timeout: Duration,
}

impl Endpoint {
    pub(crate) fn new(api_key: impl Into<String>, base_url: &str) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            client: build_client(DEFAULT_REQUEST_TIMEOUT),
            timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    pub(crate) fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub(crate) fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = duration;
        self.client = build_client(self.timeout);
        self
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Read back what the builder set. Only the tests ask: a request reaches
    /// the URL through [`post`](Self::post), and the timeout lives in the
    /// client.
    #[cfg(test)]
    pub(crate) fn get_base_url(&self) -> &str {
        &self.base_url
    }

    #[cfg(test)]
    pub(crate) fn get_timeout(&self) -> Duration {
        self.timeout
    }

    /// A POST to `path` under this endpoint's base URL carrying `body` as JSON.
    /// The caller adds its own vendor's authentication headers before handing
    /// the request to [`send`](Self::send).
    pub(crate) fn post(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}{path}", self.base_url))
            .header("content-type", "application/json")
            .json(body)
    }

    /// Send a request built by [`post`](Self::post), mapping a transport
    /// failure and every non-2xx status to a typed [`ProviderError`].
    ///
    /// `classify` reads the bodies only this vendor words its own way. Anything
    /// it returns `None` for falls through: 401/403/404 map the same way for
    /// every vendor, 429/529 become [`ProviderError::RateLimited`], 5xx become
    /// retryable [`ProviderError::StatusUnclassified`], and other non-2xx codes
    /// become terminal [`ProviderError::StatusUnclassified`].
    pub(crate) async fn send(
        &self,
        request: reqwest::RequestBuilder,
        classify: fn(u16, &str) -> Option<ProviderError>,
    ) -> ProviderResult<reqwest::Response> {
        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::ConnectionFailed {
                message: error.to_string(),
            })?;
        map_http_errors(response, classify).await
    }
}

async fn map_http_errors(
    response: reqwest::Response,
    classify: fn(u16, &str) -> Option<ProviderError>,
) -> ProviderResult<reqwest::Response> {
    let status = response.status().as_u16();
    if status < 400 {
        return Ok(response);
    }
    let retry_delay = retry_delay_from_headers(&response);
    let body = response.text().await.unwrap_or_default();
    if let Some(error) = classify(status, &body) {
        return Err(error);
    }
    if let Some(error) = classify_common_status(status, &body) {
        return Err(error);
    }
    // Per-provider classifiers only recognise their own vendor's wording. When
    // an OpenAI-compatible proxy (LiteLLM, OpenRouter, …) wraps an upstream
    // overflow / rate-limit signal, the vendor classifier gives up. The shared
    // pattern bank catches those wrapped errors so the agent loop sees
    // ContextWindowExceeded / RateLimited instead of a terminal
    // StatusUnclassified.
    if let Some(error) = patterns::refine(status, &body, retry_delay) {
        return Err(error);
    }
    Err(fallback_http_error(status, body, retry_delay))
}

fn retry_delay_from_headers(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get("retry-after")?.to_str().ok()?;
    value.parse::<u64>().ok().map(Duration::from_secs)
}

/// Statuses every vendor reports the same way. Runs after the vendor's own
/// `classify`, which therefore keeps the first word on any status.
fn classify_common_status(status: u16, body: &str) -> Option<ProviderError> {
    match status {
        401 => Some(ProviderError::AuthenticationFailed {
            message: body.into(),
        }),
        403 => Some(ProviderError::PermissionDenied {
            message: body.into(),
        }),
        404 => Some(ProviderError::ModelNotFound {
            message: body.into(),
        }),
        _ => None,
    }
}

fn fallback_http_error(status: u16, body: String, retry_delay: Option<Duration>) -> ProviderError {
    match status {
        429 | 529 => ProviderError::RateLimited {
            message: body,
            status,
            retry_delay,
        },
        _ => ProviderError::StatusUnclassified {
            status,
            message: body,
            retryable: matches!(status, 500 | 502 | 503 | 504),
            retry_delay,
        },
    }
}

/// Build the `reqwest::Client` every built-in provider uses. The whole-request
/// timeout makes a hung endpoint surface as a retryable `ConnectionFailed`
/// instead of blocking indefinitely. The 10-minute default mirrors Anthropic's
/// own reference agent (`claude-code`).
///
/// Trusts custom certificate authorities the way OpenSSL and curl do: when
/// `SSL_CERT_FILE` or `SSL_CERT_DIR` is set, the built-in root store is
/// replaced entirely by the certificates loaded from them.
fn build_client(timeout: Duration) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    let file = environment::env_opt("SSL_CERT_FILE");
    let dir = environment::env_opt("SSL_CERT_DIR");
    if file.is_some() || dir.is_some() {
        builder = builder.tls_built_in_root_certs(false);
        for cert in root_certificates_from(file.as_deref(), dir.as_deref()) {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder
        .build()
        .expect("reqwest::Client with timeout should build")
}

/// Load CA certificates from a PEM bundle file, a directory of PEM files, or
/// both. Panics if either path is unreadable, contains invalid PEM data, or if
/// the combined result is empty: with the built-in roots off, an empty trust
/// store would quietly reject every connection.
fn root_certificates_from(file: Option<&str>, dir: Option<&str>) -> Vec<reqwest::Certificate> {
    let mut certs = Vec::new();
    if let Some(path) = file {
        let pem = std::fs::read(path).unwrap_or_else(|e| panic!("SSL_CERT_FILE {path}: {e}"));
        certs.extend(
            reqwest::Certificate::from_pem_bundle(&pem)
                .unwrap_or_else(|e| panic!("SSL_CERT_FILE {path}: {e}")),
        );
    }
    if let Some(path) = dir {
        let entries =
            std::fs::read_dir(path).unwrap_or_else(|e| panic!("SSL_CERT_DIR {path}: {e}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|e| panic!("SSL_CERT_DIR {path}: {e}"));
            let Ok(pem) = std::fs::read(entry.path()) else {
                continue;
            };
            // c-rehash directories mix certs with symlinks/hashes; skip what doesn't parse
            if let Ok(found) = reqwest::Certificate::from_pem_bundle(&pem) {
                certs.extend(found);
            }
        }
    }
    assert!(
        !certs.is_empty(),
        "SSL_CERT_FILE/SSL_CERT_DIR set but no certificates loaded: every connection would be rejected"
    );
    certs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_request_timeout_is_ten_minutes() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(600));
    }

    #[test]
    fn a_new_endpoint_starts_at_the_default_timeout() {
        let endpoint = Endpoint::new("key", "https://example.test");
        assert_eq!(endpoint.get_timeout(), DEFAULT_REQUEST_TIMEOUT);
    }

    #[test]
    fn the_timeout_builder_stores_its_duration() {
        let endpoint =
            Endpoint::new("key", "https://example.test").timeout(Duration::from_secs(42));
        assert_eq!(endpoint.get_timeout(), Duration::from_secs(42));
    }

    #[test]
    fn the_base_url_builder_replaces_the_one_given_to_new() {
        let endpoint =
            Endpoint::new("key", "https://example.test").base_url("http://localhost:4000");
        assert_eq!(endpoint.base_url, "http://localhost:4000");
    }

    #[test]
    fn maps_http_401_to_authentication_failed() {
        assert!(matches!(
            classify_common_status(401, "boom"),
            Some(ProviderError::AuthenticationFailed { .. })
        ));
    }

    #[test]
    fn maps_http_403_to_permission_denied() {
        assert!(matches!(
            classify_common_status(403, "boom"),
            Some(ProviderError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn maps_http_404_to_model_not_found() {
        assert!(matches!(
            classify_common_status(404, "boom"),
            Some(ProviderError::ModelNotFound { .. })
        ));
    }

    #[test]
    fn a_common_status_keeps_the_body_as_its_message() {
        let Some(ProviderError::AuthenticationFailed { message }) =
            classify_common_status(401, "your api key is revoked")
        else {
            panic!("expected AuthenticationFailed");
        };
        assert_eq!(message, "your api key is revoked");
    }

    #[test]
    fn other_statuses_fall_through() {
        assert!(classify_common_status(400, "boom").is_none());
        assert!(classify_common_status(500, "boom").is_none());
    }

    #[test]
    fn a_429_falls_back_to_rate_limited() {
        assert!(matches!(
            fallback_http_error(429, "slow down".into(), None),
            ProviderError::RateLimited { .. }
        ));
    }

    #[test]
    fn a_503_falls_back_to_a_retryable_unclassified_status() {
        assert!(matches!(
            fallback_http_error(503, "upstream down".into(), None),
            ProviderError::StatusUnclassified {
                retryable: true,
                ..
            }
        ));
    }

    #[test]
    fn an_unrecognised_4xx_falls_back_to_a_terminal_unclassified_status() {
        assert!(matches!(
            fallback_http_error(418, "teapot".into(), None),
            ProviderError::StatusUnclassified {
                retryable: false,
                ..
            }
        ));
    }

    /// Generates a throwaway self-signed cert at `cert_path` via the `openssl` CLI.
    /// Returns `false` when `openssl` isn't on `PATH`, so callers can skip
    /// gracefully instead of failing on a machine without it installed.
    fn generate_self_signed_cert(cert_path: &std::path::Path) -> bool {
        let key_path = cert_path.with_extension("key");
        let output = match std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=agentwerk-test",
                "-keyout",
                key_path.to_str().unwrap(),
                "-out",
                cert_path.to_str().unwrap(),
            ])
            .output()
        {
            Ok(output) => output,
            Err(_) => return false,
        };
        std::fs::remove_file(&key_path).ok();
        assert!(
            output.status.success(),
            "openssl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    fn root_certificates_from_pem_bundle_loads_all_certs_in_file() {
        let path = std::env::temp_dir().join("agentwerk-test-ssl-cert-file.pem");
        if !generate_self_signed_cert(&path) {
            eprintln!("skipping: openssl not found on PATH");
            return;
        }
        let certs = root_certificates_from(Some(path.to_str().unwrap()), None);
        assert_eq!(certs.len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn root_certificates_from_dir_skips_files_that_are_not_certificates() {
        let dir = std::env::temp_dir().join("agentwerk-test-ssl-cert-dir");
        std::fs::create_dir_all(&dir).unwrap();
        if !generate_self_signed_cert(&dir.join("cert.pem")) {
            eprintln!("skipping: openssl not found on PATH");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        std::fs::write(dir.join("not-a-cert.txt"), "not pem data").unwrap();
        let certs = root_certificates_from(None, Some(dir.to_str().unwrap()));
        assert_eq!(certs.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[should_panic(expected = "no certificates loaded")]
    fn root_certificates_from_panics_when_nothing_loads() {
        let dir = std::env::temp_dir().join("agentwerk-test-ssl-cert-dir-empty");
        std::fs::create_dir_all(&dir).unwrap();
        root_certificates_from(None, Some(dir.to_str().unwrap()));
    }
}
