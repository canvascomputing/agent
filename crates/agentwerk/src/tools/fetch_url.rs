//! Fetches a URL and returns its extracted text. Gives an agent access to external documentation the prompt cannot enumerate up front.

use super::tool::{Tool, ToolContext, ToolResult};
use crate::prompts::directives::{
    DirectiveStore, FETCH_URL_BODY_NOT_READ, FETCH_URL_CREDENTIALS_PRESENT, FETCH_URL_HOST_MISSING,
    FETCH_URL_HOST_NOT_RESOLVABLE, FETCH_URL_REDIRECT_LOCATION_MISSING, FETCH_URL_REQUEST_FAILED,
    FETCH_URL_RESPONSE_TOO_LARGE, FETCH_URL_SCHEME_MISSING, FETCH_URL_SCHEME_UNSUPPORTED,
    FETCH_URL_TOO_LONG, FETCH_URL_TOO_MANY_REDIRECTS,
};

const MAX_URL_LENGTH: usize = 2000;
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_LENGTH: usize = 100_000;
const FETCH_TIMEOUT_SECS: u64 = 60;
const MAX_REDIRECT_HOPS: usize = 10;

/// What the tool identifies itself as until [`FetchUrlTool::impersonate`]
/// changes that. The version comes from `Cargo.toml`, so a release moves it.
const DEFAULT_USER_AGENT: &str = concat!("agentwerk/", env!("CARGO_PKG_VERSION"));

/// The one browser [`FetchUrlTool::impersonate`] presents itself as. Pinned
/// rather than chosen, since the two values must agree on a version.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36";
const BROWSER_CLIENT_HINT: &str =
    r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#;
const BROWSER_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,\
    image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";

/// The HTTP/2 SETTINGS a browser opens a connection with. Together with
/// [`request_headers`] this is as far as `reqwest` reaches.
const BROWSER_STREAM_WINDOW: u32 = 6_291_456;
const BROWSER_CONNECTION_WINDOW: u32 = 15_728_640;
const BROWSER_MAX_FRAME_SIZE: u32 = 16_384;

/// Fetch a URL and return its content as text. Concurrent. HTML is converted
/// to plain text; HTTP is upgraded to HTTPS; cross-host redirects are
/// surfaced rather than followed.
///
/// # Examples
///
/// ```
/// use agentwerk::Agent;
/// use agentwerk::tools::FetchUrlTool;
///
/// Agent::new().tool(FetchUrlTool::new());
/// ```
#[derive(Clone, Default)]
pub struct FetchUrlTool {
    impersonate: bool,
}

impl FetchUrlTool {
    /// Create the tool. Requests carry agentwerk's own user agent until
    /// [`FetchUrlTool::impersonate`] changes that.
    pub fn new() -> Self {
        Self::default()
    }

    /// Send the headers and HTTP/2 settings a browser sends, reaching a site
    /// that refuses a client it cannot recognize as one.
    ///
    /// The TLS handshake is unchanged. rustls writes the ClientHello, so JA3
    /// and JA4 do not read as a browser, and a site reading those rather than
    /// the headers refuses the request either way. Cloudflare, DataDome,
    /// Akamai, and Kasada all read those.
    ///
    /// # Examples
    ///
    /// ```
    /// use agentwerk::Agent;
    /// use agentwerk::tools::FetchUrlTool;
    ///
    /// Agent::new().tool(FetchUrlTool::new().impersonate());
    /// ```
    pub fn impersonate(mut self) -> Self {
        self.impersonate = true;
        self
    }
}

#[derive(serde::Deserialize)]
pub struct FetchUrlArgs {
    url: String,
    #[serde(default = "default_max_length")]
    max_length: usize,
}

fn default_max_length() -> usize {
    DEFAULT_MAX_LENGTH
}

impl From<FetchUrlTool> for Tool {
    fn from(tool: FetchUrlTool) -> Tool {
        let impersonate = tool.impersonate;
        Tool::new("fetch_url")
            .description(include_str!("fetch_url.tool.md"))
            .schema(include_str!("fetch_url.schema.json"))
            .concurrent(true)
            .handler(move |args: FetchUrlArgs, ctx: ToolContext| async move {
                run(args, ctx, impersonate).await
            })
            .build()
    }
}

async fn run(args: FetchUrlArgs, ctx: ToolContext, impersonate: bool) -> ToolResult {
    let FetchUrlArgs { url, max_length } = args;

    let validated_url = match validate_url(&url, &ctx.directives) {
        Ok(u) => u,
        Err(msg) => return ToolResult::error(msg),
    };

    let text = match fetch_url(&validated_url, impersonate, &ctx.directives).await {
        Ok(text) => text,
        Err(msg) => return ToolResult::error(msg),
    };
    if let FetchedContent::Redirect {
        original_url,
        redirect_url,
        status,
    } = &text
    {
        let msg = format!(
            "REDIRECT DETECTED: The URL redirects to a different host.\n\n\
             Original URL: {original_url}\n\
             Redirect URL: {redirect_url}\n\
             Status: {status}\n\n\
             To fetch the content, make a new fetch_url request with the redirect URL."
        );
        return ToolResult::success(msg);
    }
    let FetchedContent::Page {
        body,
        status,
        content_type,
        bytes,
    } = text
    else {
        unreachable!()
    };

    let output = format_output(&url, &body, status, &content_type, bytes, max_length);
    ToolResult::success(output)
}

// Fetching

enum FetchedContent {
    Page {
        body: String,
        status: u16,
        content_type: String,
        bytes: usize,
    },
    Redirect {
        original_url: String,
        redirect_url: String,
        status: u16,
    },
}

async fn fetch_url(
    url: &str,
    impersonate: bool,
    directives: &DirectiveStore,
) -> std::result::Result<FetchedContent, String> {
    // Manual redirect handling prevents open-redirect exploitation across domains.
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none());
    if impersonate {
        builder = builder
            .http2_initial_stream_window_size(BROWSER_STREAM_WINDOW)
            .http2_initial_connection_window_size(BROWSER_CONNECTION_WINDOW)
            .http2_max_frame_size(BROWSER_MAX_FRAME_SIZE);
    }
    let client = builder.build().map_err(|e| e.to_string())?;

    let response = follow_safe_redirects(&client, url, impersonate, directives).await?;
    if let FollowResult::CrossDomain {
        original_url,
        redirect_url,
        status,
    } = response
    {
        return Ok(FetchedContent::Redirect {
            original_url,
            redirect_url,
            status,
        });
    }
    let FollowResult::Ok(response) = response else {
        unreachable!()
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let bytes = response
        .bytes()
        .await
        .map_err(|e| directives.render(FETCH_URL_BODY_NOT_READ, &[("error", &e.to_string())]))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(directives.render(
            FETCH_URL_RESPONSE_TOO_LARGE,
            &[
                ("bytes", &bytes.len().to_string()),
                ("limit", &MAX_RESPONSE_BYTES.to_string()),
            ],
        ));
    }

    let raw_text = String::from_utf8_lossy(&bytes);
    let body = if content_type.contains("text/html") {
        strip_html(&raw_text)
    } else {
        raw_text.into_owned()
    };

    Ok(FetchedContent::Page {
        body,
        status,
        content_type,
        bytes: bytes.len(),
    })
}

fn format_output(
    url: &str,
    body: &str,
    status: u16,
    content_type: &str,
    bytes: usize,
    max_length: usize,
) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "URL: {url}\nStatus: {status}\nContent-Type: {content_type}\nSize: {bytes} bytes\n\n",
    ));

    let remaining = max_length.saturating_sub(output.len());
    if body.len() > remaining {
        // Slicing the raw byte index panics when it lands inside a multi-byte
        // character, which any non-ASCII page reaches sooner or later.
        let mut cut = remaining;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        output.push_str(&body[..cut]);
        output.push_str("\n\n[Content truncated...]");
    } else {
        output.push_str(body);
    }

    output
}

// Request headers

/// The headers one hop sends. `first_hop` picks the `Sec-Fetch-Site` a browser
/// would report: nothing yet for a request the address bar made, the same
/// origin once a redirect has moved it.
///
/// No `Accept-Encoding` while impersonating, even though a browser sends one:
/// `reqwest` is built here without `gzip`, `brotli`, and `zstd`, so a
/// compressed body would reach [`strip_html`] as binary.
fn request_headers(impersonate: bool, first_hop: bool) -> Vec<(&'static str, &'static str)> {
    if !impersonate {
        return vec![
            ("Accept", "text/markdown, text/html, */*"),
            ("User-Agent", DEFAULT_USER_AGENT),
        ];
    }
    vec![
        ("User-Agent", BROWSER_USER_AGENT),
        ("Accept", BROWSER_ACCEPT),
        ("Accept-Language", "en-US,en;q=0.9"),
        ("sec-ch-ua", BROWSER_CLIENT_HINT),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"macOS\""),
        ("Sec-Fetch-Dest", "document"),
        ("Sec-Fetch-Mode", "navigate"),
        (
            "Sec-Fetch-Site",
            if first_hop { "none" } else { "same-origin" },
        ),
        ("Sec-Fetch-User", "?1"),
        ("Upgrade-Insecure-Requests", "1"),
    ]
}

// Redirect safety

enum FollowResult {
    Ok(reqwest::Response),
    CrossDomain {
        original_url: String,
        redirect_url: String,
        status: u16,
    },
}

/// Follows same-host redirects (including www. add/remove) up to MAX_REDIRECT_HOPS.
/// Cross-domain redirects are surfaced instead of followed.
async fn follow_safe_redirects(
    client: &reqwest::Client,
    url: &str,
    impersonate: bool,
    directives: &DirectiveStore,
) -> std::result::Result<FollowResult, String> {
    let mut current_url = url.to_string();

    for hop in 0..MAX_REDIRECT_HOPS {
        let mut request = client.get(&current_url);
        for (name, value) in request_headers(impersonate, hop == 0) {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|e| {
            directives.render(FETCH_URL_REQUEST_FAILED, &[("error", &e.to_string())])
        })?;

        let status = response.status().as_u16();
        if !is_redirect(status) {
            return Ok(FollowResult::Ok(response));
        }

        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| directives.render(FETCH_URL_REDIRECT_LOCATION_MISSING, &[]))?;

        let redirect_url = resolve_redirect_location(&current_url, location);

        if is_same_origin(&current_url, &redirect_url) {
            current_url = redirect_url;
        } else {
            return Ok(FollowResult::CrossDomain {
                original_url: url.to_string(),
                redirect_url,
                status,
            });
        }
    }

    Err(directives.render(
        FETCH_URL_TOO_MANY_REDIRECTS,
        &[("limit", &MAX_REDIRECT_HOPS.to_string())],
    ))
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 307 | 308)
}

/// Allows redirects that keep the same scheme, port, and host (ignoring www. prefix).
/// Rejects cross-domain redirects and targets with embedded credentials.
fn is_same_origin(original_url: &str, redirect_url: &str) -> bool {
    let Some(orig) = parse_origin(original_url) else {
        return false;
    };
    let Some(redir) = parse_origin(redirect_url) else {
        return false;
    };

    orig.scheme == redir.scheme && orig.port == redir.port && orig.bare_host() == redir.bare_host()
}

struct UrlOrigin {
    scheme: String,
    host: String,
    port: String,
}

impl UrlOrigin {
    fn bare_host(&self) -> &str {
        self.host.strip_prefix("www.").unwrap_or(&self.host)
    }
}

fn parse_origin(url: &str) -> Option<UrlOrigin> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or(rest);

    if authority.contains('@') {
        return None;
    }

    let (host, port) = authority
        .split_once(':')
        .map(|(h, p)| (h.to_string(), p.to_string()))
        .unwrap_or_else(|| (authority.to_string(), String::new()));

    Some(UrlOrigin {
        scheme: scheme.to_string(),
        host,
        port,
    })
}

/// Resolve a possibly-relative Location header against the request URL.
fn resolve_redirect_location(base_url: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }

    let Some(scheme_end) = base_url.find("://") else {
        return location.to_string();
    };
    let origin_end = base_url[scheme_end + 3..]
        .find('/')
        .map_or(base_url.len(), |j| scheme_end + 3 + j);

    if location.starts_with('/') {
        format!("{}{location}", &base_url[..origin_end])
    } else {
        let dir_end = base_url.rfind('/').unwrap_or(origin_end);
        format!("{}/{location}", &base_url[..dir_end])
    }
}

// URL validation

fn validate_url(url: &str, directives: &DirectiveStore) -> std::result::Result<String, String> {
    if url.len() > MAX_URL_LENGTH {
        return Err(directives.render(
            FETCH_URL_TOO_LONG,
            &[
                ("length", &url.len().to_string()),
                ("limit", &MAX_URL_LENGTH.to_string()),
            ],
        ));
    }

    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| directives.render(FETCH_URL_SCHEME_MISSING, &[]))?;
    if !matches!(scheme, "http" | "https") {
        return Err(directives.render(FETCH_URL_SCHEME_UNSUPPORTED, &[("scheme", scheme)]));
    }

    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.contains('@') {
        return Err(directives.render(FETCH_URL_CREDENTIALS_PRESENT, &[]));
    }

    let host = authority.split(':').next().unwrap_or(authority);
    if host.is_empty() {
        return Err(directives.render(FETCH_URL_HOST_MISSING, &[]));
    }
    if host.split('.').count() < 2 {
        return Err(directives.render(FETCH_URL_HOST_NOT_RESOLVABLE, &[("host", host)]));
    }

    if scheme == "http" {
        return Ok(format!("https://{rest}"));
    }
    Ok(url.to_string())
}

// HTML-to-text

fn strip_html(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_invisible_block = false;
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '<' {
            let lookahead: String = chars.clone().take(10).collect();
            let lower = lookahead.to_lowercase();

            if lower.starts_with("script") || lower.starts_with("style") {
                in_invisible_block = true;
            } else if lower.starts_with("/script") || lower.starts_with("/style") {
                in_invisible_block = false;
            }
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if in_tag || in_invisible_block {
            continue;
        }
        if ch == '&' {
            text.push_str(&decode_html_entity(&mut chars));
            continue;
        }
        text.push(ch);
    }

    collapse_whitespace(&text)
}

fn decode_html_entity(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut name = String::new();
    for _ in 0..10 {
        match chars.peek() {
            Some(&';') => {
                chars.next();
                return resolve_named_entity(&name);
            }
            Some(&c) if c.is_alphanumeric() || c == '#' => {
                name.push(c);
                chars.next();
            }
            _ => break,
        }
    }
    format!("&{name}")
}

fn resolve_named_entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        "nbsp" => " ".into(),
        s if s.starts_with("#x") || s.starts_with("#X") => decode_numeric_entity(&s[2..], 16, name),
        s if s.starts_with('#') => decode_numeric_entity(&s[1..], 10, name),
        _ => format!("&{name};"),
    }
}

fn decode_numeric_entity(digits: &str, radix: u32, original: &str) -> String {
    u32::from_str_radix(digits, radix)
        .ok()
        .and_then(char::from_u32)
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("&{original};"))
}

fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut consecutive_blanks = 0;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks <= 1 {
                result.push('\n');
            }
        } else {
            consecutive_blanks = 0;
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(trimmed);
        }
    }

    result.trim().to_string()
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn header<'a>(headers: &'a [(&'static str, &'static str)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
    }

    #[test]
    fn every_example_the_schema_shows_deserializes_into_the_arguments() {
        let document = Tool::from(FetchUrlTool::new())
            .get_input_schema()
            .get_raw_schema()
            .clone();
        for example in document["examples"].as_array().expect("examples") {
            serde_json::from_value::<FetchUrlArgs>(example.clone())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
    }

    // URL validation

    #[test]
    fn validate_url_valid_https() {
        let result = validate_url_for_test("https://example.com/page");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://example.com/page");
    }

    #[test]
    fn validate_url_upgrades_http() {
        let result = validate_url_for_test("http://example.com/page");
        assert!(result.is_ok());
        assert!(result.unwrap().starts_with("https://"));
    }

    #[test]
    fn validate_url_accepts_port() {
        let result = validate_url_for_test("https://example.com:8080/page");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://example.com:8080/page");
    }

    #[test]
    fn validate_url_accepts_query_and_fragment() {
        assert!(validate_url_for_test("https://example.com/page?q=1&b=2#section").is_ok());
    }

    #[test]
    fn validate_url_rejects_no_host() {
        assert!(validate_url_for_test("https://").is_err());
    }

    #[test]
    fn validate_url_rejects_empty_host() {
        let err = validate_url_for_test("https:///path").unwrap_err();
        assert!(err.contains("names no host"));
    }

    #[test]
    fn validate_url_rejects_single_label_host() {
        let err = validate_url_for_test("https://localhost/page").unwrap_err();
        assert!(err.contains("publicly resolvable"));
    }

    #[test]
    fn validate_url_rejects_credentials() {
        let err = validate_url_for_test("https://user:pass@example.com").unwrap_err();
        assert!(err.contains("credentials"));
    }

    #[test]
    fn validate_url_rejects_too_long() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LENGTH));
        let err = validate_url_for_test(&long).unwrap_err();
        assert!(err.contains("over the 2000 character limit"));
    }

    #[test]
    fn validate_url_rejects_ftp() {
        let err = validate_url_for_test("ftp://example.com/file").unwrap_err();
        assert!(err.contains("Scheme `ftp` cannot be fetched"));
    }

    // Output

    #[test]
    fn truncation_cuts_before_a_multi_byte_character_rather_than_inside_it() {
        let body = format!("{}ä tail", "a".repeat(50));
        let output = format_output("https://example.com", &body, 200, "text/html", 99, 120);
        assert!(output.ends_with("[Content truncated...]"));
        assert!(!output.contains('ä'));
    }

    // Request headers

    #[test]
    fn the_default_sends_the_agentwerk_user_agent_carrying_the_crate_version() {
        let headers = request_headers(false, true);
        let agent = header(&headers, "User-Agent").expect("User-Agent");
        assert_eq!(agent, format!("agentwerk/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn impersonate_sends_a_browser_user_agent_and_client_hint() {
        let headers = request_headers(true, true);
        assert_eq!(header(&headers, "User-Agent"), Some(BROWSER_USER_AGENT));
        assert_eq!(header(&headers, "sec-ch-ua"), Some(BROWSER_CLIENT_HINT));
    }

    #[test]
    fn impersonate_reports_no_fetch_site_on_the_first_hop() {
        let headers = request_headers(true, true);
        assert_eq!(header(&headers, "Sec-Fetch-Site"), Some("none"));
    }

    #[test]
    fn impersonate_reports_the_same_origin_after_a_redirect() {
        let headers = request_headers(true, false);
        assert_eq!(header(&headers, "Sec-Fetch-Site"), Some("same-origin"));
    }

    #[test]
    fn impersonate_advertises_no_encoding_the_tool_cannot_decode() {
        let headers = request_headers(true, true);
        assert_eq!(header(&headers, "Accept-Encoding"), None);
    }

    // Redirect safety

    #[test]
    fn redirect_same_host_permitted() {
        assert!(is_same_origin(
            "https://example.com/a",
            "https://example.com/b"
        ));
    }

    #[test]
    fn redirect_www_add_permitted() {
        assert!(is_same_origin(
            "https://example.com/a",
            "https://www.example.com/b"
        ));
    }

    #[test]
    fn redirect_www_remove_permitted() {
        assert!(is_same_origin(
            "https://www.example.com/a",
            "https://example.com/b"
        ));
    }

    #[test]
    fn redirect_path_change_permitted() {
        assert!(is_same_origin(
            "https://example.com/old/path",
            "https://example.com/new/path"
        ));
    }

    #[test]
    fn redirect_query_change_permitted() {
        assert!(is_same_origin(
            "https://example.com/page",
            "https://example.com/page?redirected=true"
        ));
    }

    #[test]
    fn redirect_cross_domain_rejected() {
        assert!(!is_same_origin(
            "https://example.com/a",
            "https://evil.com/b"
        ));
    }

    #[test]
    fn redirect_subdomain_rejected() {
        assert!(!is_same_origin(
            "https://sub.example.com/a",
            "https://example.com/b"
        ));
    }

    #[test]
    fn redirect_protocol_change_rejected() {
        assert!(!is_same_origin(
            "https://example.com/a",
            "http://example.com/b"
        ));
    }

    #[test]
    fn redirect_port_change_rejected() {
        assert!(!is_same_origin(
            "https://example.com:443/a",
            "https://example.com:8080/b"
        ));
    }

    #[test]
    fn redirect_with_credentials_rejected() {
        assert!(!is_same_origin(
            "https://example.com/a",
            "https://user:pass@example.com/b"
        ));
    }

    // Redirect resolution

    #[test]
    fn resolve_absolute_redirect() {
        assert_eq!(
            resolve_redirect_location("https://example.com/a", "https://other.com/b"),
            "https://other.com/b"
        );
    }

    #[test]
    fn resolve_relative_redirect_absolute_path() {
        assert_eq!(
            resolve_redirect_location("https://example.com/old/page", "/new/page"),
            "https://example.com/new/page"
        );
    }

    #[test]
    fn resolve_relative_redirect_relative_path() {
        assert_eq!(
            resolve_redirect_location("https://example.com/old/page", "other"),
            "https://example.com/old/other"
        );
    }

    #[test]
    fn resolve_redirect_preserves_query() {
        assert_eq!(
            resolve_redirect_location("https://example.com/old", "/new?q=1&b=2"),
            "https://example.com/new?q=1&b=2"
        );
    }

    #[test]
    fn resolve_redirect_no_path_in_base() {
        assert_eq!(
            resolve_redirect_location("https://example.com", "/page"),
            "https://example.com/page"
        );
    }

    // HTML stripping

    #[test]
    fn strip_html_basic() {
        let text = strip_html("<html><body><h1>Hello</h1><p>World</p></body></html>");
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("<"));
    }

    #[test]
    fn strip_html_entities() {
        assert_eq!(
            strip_html("Tom &amp; Jerry &lt;3 &gt; &quot;cats&quot;"),
            "Tom & Jerry <3 > \"cats\""
        );
    }

    #[test]
    fn strip_html_numeric_entities() {
        assert_eq!(strip_html("&#65;&#x42;"), "AB");
    }

    #[test]
    fn strip_html_removes_script() {
        let text = strip_html("<p>Before</p><script>alert('xss')</script><p>After</p>");
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn strip_html_removes_style() {
        let text = strip_html("<style>body { color: red; }</style><p>Content</p>");
        assert!(text.contains("Content"));
        assert!(!text.contains("color"));
    }

    #[test]
    fn strip_html_self_closing_tags() {
        let text = strip_html("before<br/><img src='x'/>after");
        assert!(text.contains("before"));
        assert!(text.contains("after"));
        assert!(!text.contains("img"));
    }

    #[test]
    fn strip_html_nested_tags() {
        assert_eq!(
            strip_html("<div><p><b>deep text</b></p></div>"),
            "deep text"
        );
    }

    #[test]
    fn strip_html_attributes_stripped() {
        assert_eq!(
            strip_html(r#"<a href="https://example.com" class="link">click here</a>"#),
            "click here"
        );
    }

    #[test]
    fn strip_html_preserves_plain_text() {
        let text = "no tags here, just plain text";
        assert_eq!(strip_html(text), text);
    }

    #[test]
    fn collapse_whitespace_limits_blank_lines() {
        assert_eq!(
            collapse_whitespace("line1\n\n\n\n\nline2"),
            "line1\n\nline2"
        );
    }
}

#[cfg(test)]
fn validate_url_for_test(url: &str) -> std::result::Result<String, String> {
    validate_url(url, &DirectiveStore::default())
}
