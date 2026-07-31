// SPDX-License-Identifier: MIT

//! Spotify Web API client: token exchange/refresh, unified `/me/library`
//! endpoints, and the typed error surface that the UI consumes.
//!
//! All network calls go through a single `ureq` agent that is configured
//! without keep-alive for the test suite and with conservative timeouts in
//! production. The client never sends a Client Secret: token exchange and
//! refresh both rely on PKCE plus the public `client_id` only, per the
//! approved plan.
//!
//! Manual JSON parsing is used for the OAuth token response so the network
//! path does not depend on a derived `Deserialize` for [`TokenSet`]
//! (the keyring module still uses the derived `Serialize` for
//! persistence). The library endpoints use `serde_json` only on the
//! small `[bool]` contains response, which is trivial to round-trip.

use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

use crate::spotify::keyring::TokenStore;
use crate::spotify::types::{
    AuthCode, PkceVerifier, REDIRECT_URI, SPOTIFY_TOKEN_URL, TokenSet,
};

/// Default Spotify Web API base URL. Tests override this with
/// `with_api_base` so the client points at a local mock server.
pub const DEFAULT_API_BASE: &str = "https://api.spotify.com/v1";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Coarse category for an HTTP status that does not fit the more specific
/// [`SpotifyApiError`] variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpCategory {
    /// 4xx response that is not auth/forbidden/rate-limited.
    Client,
    /// 5xx response.
    Server,
}

/// Errors returned from the Web API client.
///
/// Variants are deliberately coarse so the UI can present a short, actionable
/// message without inspecting raw response bodies. None of the variants
/// carry user-specific data; inner strings are static category labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpotifyApiError {
    /// Generic HTTP failure outside the categories below. Returned for
    /// 4xx/5xx responses that are not 401, 403, or 429.
    Http { status: u16, category: HttpCategory },
    /// Refresh-specific failure: no refresh token on file, refresh itself
    /// failed, or a 401 still came back after a successful refresh. The
    /// inner string is a static category label.
    Refresh(&'static str),
    /// 403-style account/allowlist/Forbidden error. Spotify returns this
    /// for apps not on the allowlist, for accounts without Premium on a
    /// Development Mode app, and for quota/region restrictions.
    Allowlist(&'static str),
    /// 429 rate limit. `retry_after` is in seconds (0 if the response did
    /// not include a `Retry-After` header).
    RateLimited { retry_after: u64 },
    /// Network / I/O error before any HTTP response was received.
    Transport(&'static str),
    /// Response body could not be parsed into the expected shape.
    Malformed(&'static str),
}

impl std::fmt::Display for SpotifyApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { status, category } => {
                let cat = match category {
                    HttpCategory::Client => "client",
                    HttpCategory::Server => "server",
                };
                write!(f, "Spotify API HTTP {status} ({cat})")
            }
            Self::Refresh(tag) => write!(f, "Spotify token refresh failed: {tag}"),
            Self::Allowlist(tag) => write!(f, "Spotify account access error: {tag}"),
            Self::RateLimited { retry_after } => {
                if *retry_after == 0 {
                    f.write_str("Spotify rate limited")
                } else {
                    write!(f, "Spotify rate limited, retry after {retry_after}s")
                }
            }
            Self::Transport(tag) => write!(f, "Spotify transport error: {tag}"),
            Self::Malformed(tag) => write!(f, "Spotify response malformed: {tag}"),
        }
    }
}

impl std::error::Error for SpotifyApiError {}

/// Map a Spotify HTTP status into a category for [`SpotifyApiError::Http`].
fn categorize(status: u16) -> HttpCategory {
    if (500..600).contains(&status) {
        HttpCategory::Server
    } else {
        HttpCategory::Client
    }
}

// ---------------------------------------------------------------------------
// Manual JSON parser for the OAuth token response
// ---------------------------------------------------------------------------

/// Parse a Spotify token response body into a [`TokenSet`]. `obtained_at`
/// is set to the current wall clock.
///
/// The parser is hand-rolled (no `serde_json::from_str`) per the approved
/// plan: the response is a small, well-known shape with at most five
/// fields, and we want the network path to be free of a derived
/// `Deserialize` so the redaction contract on [`TokenSet`] cannot be
/// bypassed by an accidentally-exposed field.
pub fn parse_token_response(body: &str) -> Result<TokenSet, SpotifyApiError> {
    let mut p = JsonParser::new(body);
    p.skip_ws();
    p.expect(b'{')?;
    p.skip_ws();

    let mut access_token: Option<String> = None;
    let mut token_type: Option<String> = None;
    let mut expires_in: Option<u64> = None;
    let mut refresh_token: Option<String> = None;
    let mut scope: Option<String> = None;

    if p.peek() == Some(b'}') {
        p.bump();
    } else {
        loop {
            p.skip_ws();
            let key = p.parse_string()?;
            p.skip_ws();
            p.expect(b':')?;
            match key.as_str() {
                "access_token" => access_token = Some(p.parse_string()?),
                "token_type" => token_type = Some(p.parse_string()?),
                "expires_in" => expires_in = Some(p.parse_u64()?),
                "refresh_token" => refresh_token = Some(p.parse_string()?),
                "scope" => scope = Some(p.parse_string()?),
                _ => p.skip_value()?,
            }
            p.skip_ws();
            match p.peek() {
                Some(b',') => {
                    p.bump();
                }
                Some(b'}') => {
                    p.bump();
                    break;
                }
                _ => return Err(SpotifyApiError::Malformed("expected_object_end")),
            }
        }
    }

    let access_token =
        access_token.ok_or(SpotifyApiError::Malformed("missing_access_token"))?;
    let token_type =
        token_type.ok_or(SpotifyApiError::Malformed("missing_token_type"))?;
    let expires_in =
        expires_in.ok_or(SpotifyApiError::Malformed("missing_expires_in"))?;

    Ok(TokenSet::new_now(
        access_token,
        token_type,
        expires_in,
        refresh_token,
        scope,
    ))
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn expect(&mut self, b: u8) -> Result<(), SpotifyApiError> {
        self.skip_ws();
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(SpotifyApiError::Malformed("expected_byte"))
        }
    }

    fn parse_string(&mut self) -> Result<String, SpotifyApiError> {
        self.skip_ws();
        if self.bump() != Some(b'"') {
            return Err(SpotifyApiError::Malformed("expected_string"));
        }
        let mut out = Vec::new();
        loop {
            match self.bump() {
                Some(b'"') => {
                    return String::from_utf8(out)
                        .map_err(|_| SpotifyApiError::Malformed("invalid_utf8_string"));
                }
                Some(b'\\') => match self.bump() {
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'/') => out.push(b'/'),
                    Some(b'n') => out.push(b'\n'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b'b') => out.push(0x08),
                    Some(b'f') => out.push(0x0C),
                    Some(b'u') => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let b = self
                                .bump()
                                .ok_or(SpotifyApiError::Malformed("eof_in_unicode_escape"))?;
                            let d = (b as char)
                                .to_digit(16)
                                .ok_or(SpotifyApiError::Malformed("invalid_unicode_escape"))?;
                            code = code * 16 + d;
                        }
                        if let Some(c) = char::from_u32(code) {
                            let mut buf = [0u8; 4];
                            let s = c.encode_utf8(&mut buf);
                            out.extend_from_slice(s.as_bytes());
                        } else {
                            return Err(SpotifyApiError::Malformed("invalid_unicode_codepoint"));
                        }
                    }
                    _ => return Err(SpotifyApiError::Malformed("invalid_escape")),
                },
                Some(b) if b < 0x20 => {
                    return Err(SpotifyApiError::Malformed("control_char_in_string"))
                }
                Some(b) => out.push(b),
                None => return Err(SpotifyApiError::Malformed("unterminated_string")),
            }
        }
    }

    fn parse_u64(&mut self) -> Result<u64, SpotifyApiError> {
        self.skip_ws();
        let start = self.pos;
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(SpotifyApiError::Malformed("expected_number"));
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| SpotifyApiError::Malformed("invalid_utf8"))?;
        s.parse::<u64>()
            .map_err(|_| SpotifyApiError::Malformed("u64_overflow"))
    }

    fn skip_value(&mut self) -> Result<(), SpotifyApiError> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => {
                self.parse_string()?;
                Ok(())
            }
            Some(b'{') => self.skip_object(),
            Some(b'[') => self.skip_array(),
            Some(b't') => self.expect_literal(b"true"),
            Some(b'f') => self.expect_literal(b"false"),
            Some(b'n') => self.expect_literal(b"null"),
            Some(b) if b.is_ascii_digit() || b == b'-' => {
                self.skip_number();
                Ok(())
            }
            _ => Err(SpotifyApiError::Malformed("unexpected_value")),
        }
    }

    fn skip_object(&mut self) -> Result<(), SpotifyApiError> {
        self.expect(b'{')?;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.skip_ws();
            self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_value()?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => return Err(SpotifyApiError::Malformed("expected_object_end")),
            }
        }
    }

    fn skip_array(&mut self) -> Result<(), SpotifyApiError> {
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.skip_value()?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => return Err(SpotifyApiError::Malformed("expected_array_end")),
            }
        }
    }

    fn skip_number(&mut self) {
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b) if b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
    }

    fn expect_literal(&mut self, lit: &[u8]) -> Result<(), SpotifyApiError> {
        if self.bytes[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(SpotifyApiError::Malformed("expected_literal"))
        }
    }
}

// ---------------------------------------------------------------------------
// Spotify client
// ---------------------------------------------------------------------------

/// HTTP client wrapping a `ureq::Agent` with the token-exchange and
/// refresh helpers, the typed error surface, and the 401 → refresh → retry
/// logic required by Task 3.
///
/// `SpotifyClient` is generic over the [`TokenStore`] so the production
/// build wires up [`SecretServiceTokenStore`](crate::spotify::keyring::SecretServiceTokenStore)
/// and the unit tests wire up [`InMemoryTokenStore`](crate::spotify::keyring::InMemoryTokenStore).
pub struct SpotifyClient<S: TokenStore> {
    client_id: String,
    tokens: Mutex<Option<TokenSet>>,
    store: S,
    http: ureq::Agent,
    token_url: String,
    api_base: String,
    redirect_uri: String,
}

impl<S: TokenStore> SpotifyClient<S> {
    /// Build a client with the production Spotify endpoints and no current
    /// token. The next library call (or explicit `refresh`) will fail with
    /// `SpotifyApiError::Refresh("no_token")` until a token is set.
    pub fn new(client_id: String, store: S) -> Self {
        Self {
            client_id,
            tokens: Mutex::new(None),
            store,
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(15))
                .build(),
            token_url: SPOTIFY_TOKEN_URL.to_string(),
            api_base: DEFAULT_API_BASE.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
        }
    }

    /// Override the OAuth token endpoint. Tests point this at a local mock.
    #[cfg(test)]
    pub fn with_token_url(mut self, url: String) -> Self {
        self.token_url = url;
        self
    }

    /// Override the Web API base URL. Tests point this at a local mock.
    #[cfg(test)]
    pub fn with_api_base(mut self, base: String) -> Self {
        self.api_base = base;
        self
    }

    /// Pre-populate the client with a token set (e.g. loaded from the
    /// keyring during startup).
    pub fn with_tokens(mut self, tokens: TokenSet) -> Self {
        self.tokens = Mutex::new(Some(tokens));
        self
    }

    /// Return a clone of the current token set, if any.
    pub fn current_tokens(&self) -> Option<TokenSet> {
        self.tokens.lock().expect("tokens lock poisoned").clone()
    }

    /// Replace the in-memory token set and persist the new value via the
    /// configured [`TokenStore`]. UI code calls this after a successful
    /// exchange or refresh.
    pub fn set_tokens(&self, tokens: TokenSet) -> Result<(), SpotifyApiError> {
        {
            let mut guard = self.tokens.lock().expect("tokens lock poisoned");
            *guard = Some(tokens.clone());
        }
        self.store.save(&tokens).map_err(|_| {
            // Keyring errors collapse into a static refresh category so
            // token values are never reflected back to the caller.
            SpotifyApiError::Refresh("keyring_save_failed")
        })
    }

    /// True when the current token is within [`crate::spotify::types::REFRESH_SAFETY_MARGIN`]
    /// of expiry (or has no expiry at all). UI code calls this before each
    /// library call to decide whether a proactive refresh should fire.
    #[cfg(test)]
    pub fn needs_refresh(&self) -> bool {
        let Some(t) = self.current_tokens() else {
            return true;
        };
        let now = std::time::SystemTime::now();
        now >= t.refresh_due_at()
    }

    /// Exchange an authorization `code` (plus its PKCE `verifier`) for a
    /// fresh token set, persist it, and return it.
    ///
    /// The request is form-encoded with `grant_type=authorization_code`,
    /// the registered `redirect_uri`, the `client_id`, and the
    /// `code_verifier`. No Client Secret is ever sent.
    pub fn exchange_code(
        &self,
        code: &AuthCode,
        verifier: &PkceVerifier,
    ) -> Result<TokenSet, SpotifyApiError> {
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("code", code.as_str())
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("client_id", &self.client_id)
            .append_pair("code_verifier", verifier.as_str())
            .finish();
        let response = self
            .http
            .post(&self.token_url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&body);
        let body = collect_token_body(response)?;
        let tokens = parse_token_response(&body)?;
        self.set_tokens(tokens.clone())?;
        Ok(tokens)
    }

    /// Refresh the access token using the supplied refresh token. The
    /// request is form-encoded with `grant_type=refresh_token`, the
    /// `client_id`, and the refresh token. No Client Secret is sent.
    ///
    /// Spotify does not always return a new `refresh_token` on a refresh
    /// response; when it omits one, this method preserves the old value
    /// in the returned [`TokenSet`] so the caller can persist it without
    /// losing the ability to refresh again.
    pub fn refresh_with(&self, refresh_token: &str) -> Result<TokenSet, SpotifyApiError> {
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("refresh_token", refresh_token)
            .append_pair("client_id", &self.client_id)
            .finish();
        let response = self
            .http
            .post(&self.token_url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(&body);
        let body = collect_token_body(response)?;
        let mut tokens = parse_token_response(&body)?;
        // Preserve the original refresh token if Spotify omitted one.
        if tokens.refresh_token.as_deref().map(str::is_empty).unwrap_or(true) {
            tokens.refresh_token = Some(refresh_token.to_string());
        }
        self.set_tokens(tokens.clone())?;
        Ok(tokens)
    }

    /// Refresh using the refresh token currently held in memory. Returns
    /// `SpotifyApiError::Refresh("no_token")` if no tokens are loaded.
    pub fn refresh(&self) -> Result<TokenSet, SpotifyApiError> {
        let current = self
            .current_tokens()
            .ok_or(SpotifyApiError::Refresh("no_token"))?;
        let rt = current
            .refresh_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(SpotifyApiError::Refresh("no_refresh_token"))?;
        self.refresh_with(rt)
    }

    /// Send an authenticated request to the Web API. On 401 the client
    /// refreshes once and retries the request. A second 401 — or any
    /// refresh failure — surfaces as [`SpotifyApiError::Refresh`].
    pub(crate) fn send_authed<F>(&self, build: F) -> Result<ureq::Response, SpotifyApiError>
    where
        F: Fn(&str) -> ureq::Request,
    {
        let tokens = self
            .current_tokens()
            .ok_or(SpotifyApiError::Refresh("no_token"))?;
        let access = tokens.access_token.clone();

        match build(&access).call() {
            Ok(r) => Ok(r),
            Err(ureq::Error::Status(401, _)) => {
                // Refresh and retry exactly once. A second 401 is the
                // "reauth required" case called out in the plan.
                let new_tokens = self.refresh()?;
                let new_access = new_tokens.access_token.clone();
                match build(&new_access).call() {
                    Ok(r) if r.status() == 401 => {
                        Err(SpotifyApiError::Refresh("reauth_required"))
                    }
                    Ok(r) => Ok(r),
                    Err(e) => Err(map_ureq_error(e)),
                }
            }
            Err(e) => Err(map_ureq_error(e)),
        }
    }

    /// Borrow the configured Web API base (for tests and for library
    /// helpers that need to construct absolute URLs).
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Borrow the configured Client ID.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Borrow a reference to the token store.
    pub fn store(&self) -> &S {
        &self.store
    }
}

fn map_ureq_error(e: ureq::Error) -> SpotifyApiError {
    match e {
        ureq::Error::Status(401, _) => {
            // The outer 401 case is handled by `send_authed`; reaching this
            // branch means the post-refresh retry also 401'd, so this is
            // a reauth-required condition.
            SpotifyApiError::Refresh("reauth_required")
        }
        ureq::Error::Status(403, _) => SpotifyApiError::Allowlist("forbidden"),
        ureq::Error::Status(429, r) => {
            let retry_after = parse_retry_after(r.header("Retry-After"));
            SpotifyApiError::RateLimited { retry_after }
        }
        ureq::Error::Status(s, _) => SpotifyApiError::Http {
            status: s,
            category: categorize(s),
        },
        ureq::Error::Transport(_) => SpotifyApiError::Transport("request_failed"),
    }
}

/// Read the body of a `ureq::Response` from the OAuth token endpoint,
/// mapping HTTP status errors to the typed [`SpotifyApiError`] surface.
/// The token endpoint never returns a successful empty body, so we treat
/// non-2xx as errors here rather than handing the JSON back to the parser.
fn collect_token_body(
    result: Result<ureq::Response, ureq::Error>,
) -> Result<String, SpotifyApiError> {
    match result {
        Ok(r) => {
            let status = r.status();
            let mut buf = String::new();
            r.into_reader()
                .read_to_string(&mut buf)
                .map_err(|_| SpotifyApiError::Malformed("body_read_failed"))?;
            if !(200..300).contains(&status) {
                return Err(SpotifyApiError::Http {
                    status,
                    category: categorize(status),
                });
            }
            Ok(buf)
        }
        Err(e) => Err(map_ureq_error(e)),
    }
}

/// Parse a `Retry-After` header into a number of seconds. Accepts either
/// a bare integer (delta-seconds) or an HTTP-date; the date form falls
/// back to 0 because the test suite never uses it.
fn parse_retry_after(header: Option<&str>) -> u64 {
    let Some(value) = header else { return 0 };
    if let Some(seconds) = value.trim().parse::<u64>().ok() {
        return seconds;
    }
    // HTTP-date: try to parse, otherwise report 0 so the UI can still
    // display a generic "rate limited" message.
    0
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Test-only HTTP mock used by the unit tests. The mock binds a loopback
/// port, accepts connections in a worker thread, and invokes a caller-
/// supplied handler for each request.
///
/// `cargo test` must never talk to production Spotify, so all network
/// code paths in the spotify module are exercised against `MockHttp`.
#[cfg(test)]
pub(crate) mod testing {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    /// A single scripted HTTP response.
    #[derive(Debug, Clone)]
    pub struct MockResponse {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: String,
    }

    impl MockResponse {
        /// Build a JSON response with the given status and body. The
        /// response is sent with `Connection: close` so the server
        /// shuts down the socket after the body, preventing keep-alive
        /// from leaking across tests.
        pub fn json(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                headers: vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("Connection".to_string(), "close".to_string()),
                ],
                body: body.into(),
            }
        }

        /// Build an empty 200/204 response (used by save/remove).
        pub fn empty(status: u16) -> Self {
            Self {
                status,
                headers: vec![
                    ("Content-Length".to_string(), "0".to_string()),
                    ("Connection".to_string(), "close".to_string()),
                ],
                body: String::new(),
            }
        }

        /// Build a response with a custom header (e.g. `Retry-After`).
        pub fn with_header(mut self, name: &str, value: &str) -> Self {
            self.headers
                .push((name.to_string(), value.to_string()));
            self
        }
    }

    /// State shared between the test thread and the worker.
    struct Inner {
        captured: Mutex<Vec<String>>,
        stop: AtomicBool,
    }

    /// Multi-request mock HTTP server. Bind on a random loopback port,
    /// serve queued responses in order, and let the test inspect every
    /// request that arrived.
    pub struct MockHttp {
        addr: SocketAddr,
        inner: Arc<Inner>,
        handle: Option<JoinHandle<()>>,
    }

    impl MockHttp {
        /// Start a new mock server with the given handler closure. The
        /// handler receives the raw request string and returns the
        /// response to send back.
        pub fn start<F>(handler: F) -> Self
        where
            F: Fn(&str) -> MockResponse + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock listener");
            let addr = listener.local_addr().expect("mock listener addr");
            let inner = Arc::new(Inner {
                captured: Mutex::new(Vec::new()),
                stop: AtomicBool::new(false),
            });
            let inner_clone = Arc::clone(&inner);
            let handle = thread::spawn(move || {
                listener
                    .set_nonblocking(true)
                    .expect("set nonblocking on mock listener");
                loop {
                    if inner_clone.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // Read the request (synchronously on the stream).
                            let _ = stream.set_nonblocking(false);
                            let request = match read_http_request(&mut stream) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            inner_clone
                                .captured
                                .lock()
                                .expect("mock captured lock")
                                .push(request.clone());
                            let response = handler(&request);
                            let _ = write_http_response(&mut stream, &response);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                }
            });
            Self {
                addr,
                inner,
                handle: Some(handle),
            }
        }

        /// Address the mock is bound to.
        pub fn addr(&self) -> SocketAddr {
            self.addr
        }

        /// All request strings the mock has seen, in order.
        pub fn requests(&self) -> Vec<String> {
            self.inner.captured.lock().expect("mock captured lock").clone()
        }
    }

    impl Drop for MockHttp {
        fn drop(&mut self) {
            self.inner.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
        let mut buf = Vec::with_capacity(1024);
        let mut tmp = [0u8; 1024];
        loop {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if buf.len() > 64 * 1024 {
                break;
            }
        }
        // Best-effort body read using Content-Length so POSTs are captured.
        let text = String::from_utf8_lossy(&buf).into_owned();
        if let Some(idx) = text.find("Content-Length:") {
            let after = &text[idx + "Content-Length:".len()..];
            let line = after.lines().next().unwrap_or("");
            if let Ok(len) = line.trim().parse::<usize>() {
                // Did we already read past the headers?
                let header_end = text.find("\r\n\r\n").map(|p| p + 4).unwrap_or(buf.len());
                let already = buf.len().saturating_sub(header_end);
                let mut need = len.saturating_sub(already);
                while need > 0 {
                    let n = stream.read(&mut tmp)?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    need = need.saturating_sub(n);
                }
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn write_http_response(stream: &mut TcpStream, response: &MockResponse) -> std::io::Result<()> {
        let reason = default_status_text(response.status);
        let mut out = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
            response.status,
            reason,
            response.body.len()
        );
        for (k, v) in &response.headers {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(&response.body);
        stream.write_all(out.as_bytes())?;
        stream.flush()?;
        let _ = stream.shutdown(std::net::Shutdown::Both);
        Ok(())
    }

    fn default_status_text(status: u16) -> &'static str {
        match status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            _ => "Status",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::testing::{MockHttp, MockResponse};
    use super::*;
    use crate::spotify::keyring::InMemoryTokenStore;
    use crate::spotify::types::{AuthCode, PkceVerifier, REQUIRED_SCOPES};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_code() -> AuthCode {
        AuthCode::new("AuthCodeXYZ").unwrap()
    }

    fn sample_verifier() -> PkceVerifier {
        PkceVerifier::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk").unwrap()
    }

    // --- Manual token JSON parser -------------------------------------------

    #[test]
    fn parse_token_response_reads_all_fields() {
        let body = r#"{"access_token":"abc","token_type":"Bearer","expires_in":3600,"refresh_token":"def","scope":"user-library-read user-library-modify"}"#;
        let t = parse_token_response(body).unwrap();
        assert_eq!(t.access_token, "abc");
        assert_eq!(t.token_type, "Bearer");
        assert_eq!(t.expires_in, 3600);
        assert_eq!(t.refresh_token.as_deref(), Some("def"));
        assert_eq!(t.scope.as_deref(), Some(REQUIRED_SCOPES));
    }

    #[test]
    fn parse_token_response_treats_refresh_and_scope_as_optional() {
        let body = r#"{"access_token":"abc","token_type":"Bearer","expires_in":60}"#;
        let t = parse_token_response(body).unwrap();
        assert!(t.refresh_token.is_none());
        assert!(t.scope.is_none());
    }

    #[test]
    fn parse_token_response_ignores_unknown_fields() {
        let body = r#"{"access_token":"abc","token_type":"Bearer","expires_in":60,"extra":"foo","nested":{"a":1}}"#;
        let t = parse_token_response(body).unwrap();
        assert_eq!(t.access_token, "abc");
    }

    #[test]
    fn parse_token_response_rejects_missing_required_field() {
        let cases = [
            (r#"{"token_type":"Bearer","expires_in":60}"#, "missing_access_token"),
            (r#"{"access_token":"abc","expires_in":60}"#, "missing_token_type"),
            (r#"{"access_token":"abc","token_type":"Bearer"}"#, "missing_expires_in"),
            (r#"{}"#, "missing_access_token"),
        ];
        for (body, want) in cases {
            let err = parse_token_response(body).unwrap_err();
            match err {
                SpotifyApiError::Malformed(tag) => assert_eq!(tag, want, "body={body}"),
                other => panic!("expected Malformed({want}) for {body}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_token_response_rejects_malformed_input() {
        assert!(matches!(
            parse_token_response("not json"),
            Err(SpotifyApiError::Malformed(_))
        ));
        assert!(matches!(
            parse_token_response(r#"[1,2,3]"#),
            Err(SpotifyApiError::Malformed(_))
        ));
        // Wrong type for access_token: we expect a string and we get a number.
        assert!(matches!(
            parse_token_response(r#"{"access_token":123,"token_type":"Bearer","expires_in":60}"#),
            Err(SpotifyApiError::Malformed(_))
        ));
    }

    #[test]
    fn parse_token_response_handles_whitespace_and_unicode() {
        let body = r#" { "access_token" : "abc" , "token_type" : "Bearer" , "expires_in" : 1 , "refresh_token" : "déjà" } "#;
        let t = parse_token_response(body).unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("déjà"));
    }

    // --- Token exchange -----------------------------------------------------

    fn setup_mock_with_handler<F>(handler: F) -> (SpotifyClient<InMemoryTokenStore>, MockHttp)
    where
        F: Fn(&str) -> MockResponse + Send + 'static,
    {
        let mock = MockHttp::start(handler);
        let token_url = format!("http://{}/api/token", mock.addr());
        let api_base = format!("http://{}", mock.addr());
        let store = InMemoryTokenStore::new();
        let client = SpotifyClient::new(
            "e117d2b248334356b28cdf56be6eba18".to_string(),
            store,
        )
        .with_token_url(token_url)
        .with_api_base(api_base);
        (client, mock)
    }

    #[test]
    fn exchange_code_sends_form_encoded_request_without_client_secret() {
        let (client, mock) = setup_mock_with_handler(|req| {
            // Must NOT contain a `client_secret` form field.
            assert!(!req.contains("client_secret"), "client_secret leaked: {req}");
            assert!(req.contains("grant_type=authorization_code"), "{req}");
            assert!(req.contains("client_id=e117d2b248334356b28cdf56be6eba18"), "{req}");
            assert!(req.contains("code=AuthCodeXYZ"), "{req}");
            assert!(req.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A43821%2Fcallback"), "{req}");
            assert!(req.contains("code_verifier=dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "{req}");
            assert!(req.contains("Content-Type: application/x-www-form-urlencoded"), "{req}");
            MockResponse::json(
                200,
                r#"{"access_token":"NEW_ACCESS","token_type":"Bearer","expires_in":3600,"refresh_token":"NEW_REFRESH","scope":"user-library-read user-library-modify"}"#,
            )
        });

        let tokens = client
            .exchange_code(&sample_code(), &sample_verifier())
            .expect("exchange should succeed");
        assert_eq!(tokens.access_token, "NEW_ACCESS");
        assert_eq!(tokens.refresh_token.as_deref(), Some("NEW_REFRESH"));
        // Tokens are persisted to the store.
        let loaded = client.current_tokens().unwrap();
        assert_eq!(loaded.access_token, "NEW_ACCESS");
        drop(mock);
    }

    #[test]
    fn refresh_uses_refresh_token_grant_and_preserves_old_refresh_when_omitted() {
        let (client, mock) = setup_mock_with_handler(|req| {
            assert!(req.contains("grant_type=refresh_token"), "{req}");
            assert!(
                !req.contains("client_secret"),
                "client_secret leaked in refresh: {req}"
            );
            assert!(
                req.contains("client_id=e117d2b248334356b28cdf56be6eba18"),
                "{req}"
            );
            assert!(req.contains("refresh_token=OLD_REFRESH"), "{req}");
            // Spotify omits refresh_token here, so the client should preserve
            // the old one in the returned TokenSet.
            MockResponse::json(
                200,
                r#"{"access_token":"ROTATED","token_type":"Bearer","expires_in":3600}"#,
            )
        });

        let initial = TokenSet::new_now(
            "OLD",
            "Bearer",
            3600,
            Some("OLD_REFRESH".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        client.set_tokens(initial).unwrap();
        let tokens = client.refresh().expect("refresh should succeed");
        assert_eq!(tokens.access_token, "ROTATED");
        assert_eq!(
            tokens.refresh_token.as_deref(),
            Some("OLD_REFRESH"),
            "missing refresh_token must not overwrite the stored one"
        );
        drop(mock);
    }

    #[test]
    fn refresh_uses_new_refresh_token_when_spotify_returns_one() {
        let (client, mock) = setup_mock_with_handler(|_req| {
            MockResponse::json(
                200,
                r#"{"access_token":"ROTATED","token_type":"Bearer","expires_in":3600,"refresh_token":"NEW_REFRESH"}"#,
            )
        });
        let initial = TokenSet::new_now(
            "OLD",
            "Bearer",
            3600,
            Some("OLD_REFRESH".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        client.set_tokens(initial).unwrap();
        let tokens = client.refresh().expect("refresh should succeed");
        assert_eq!(tokens.refresh_token.as_deref(), Some("NEW_REFRESH"));
        drop(mock);
    }

    #[test]
    fn refresh_without_stored_token_returns_refresh_error() {
        let (client, _mock) = setup_mock_with_handler(|_req| {
            // The mock should never be hit in this test.
            panic!("mock should not be called when no tokens are loaded");
        });
        let err = client.refresh().unwrap_err();
        assert_eq!(err, SpotifyApiError::Refresh("no_token"));
    }

    #[test]
    fn refresh_without_refresh_token_returns_refresh_error() {
        let (client, _mock) = setup_mock_with_handler(|_req| {
            panic!("mock should not be called without a refresh token");
        });
        let initial = TokenSet::new_now("OLD", "Bearer", 3600, None, None);
        client.set_tokens(initial).unwrap();
        let err = client.refresh().unwrap_err();
        assert_eq!(err, SpotifyApiError::Refresh("no_refresh_token"));
    }

    #[test]
    fn exchange_code_propagates_403_as_allowlist() {
        let (client, mock) = setup_mock_with_handler(|_req| {
            MockResponse::json(403, r#"{"error":"forbidden"}"#)
        });
        let err = client
            .exchange_code(&sample_code(), &sample_verifier())
            .unwrap_err();
        assert_eq!(err, SpotifyApiError::Allowlist("forbidden"));
        drop(mock);
    }

    #[test]
    fn exchange_code_propagates_429_with_retry_after() {
        let (client, mock) = setup_mock_with_handler(|_req| {
            MockResponse::json(429, r#"{"error":"rate_limited"}"#)
                .with_header("Retry-After", "7")
        });
        let err = client
            .exchange_code(&sample_code(), &sample_verifier())
            .unwrap_err();
        assert_eq!(err, SpotifyApiError::RateLimited { retry_after: 7 });
        drop(mock);
    }

    // --- 401 retry ---------------------------------------------------------

    #[test]
    fn library_call_401_triggers_one_refresh_and_retry() {
        // The mock is shared between the library endpoint and the token
        // endpoint: the first library call returns 401, the refresh call
        // returns new tokens, the second library call returns the real
        // answer.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let lib_count = Arc::new(AtomicUsize::new(0));
        let lib_count_clone = Arc::clone(&lib_count);
        let (client, mock) = setup_mock_with_handler(move |req| {
            if req.contains("/me/library/contains") {
                let n = lib_count_clone.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    MockResponse::json(401, r#"{"error":"unauthorized"}"#)
                } else {
                    MockResponse::json(200, "[true]")
                }
            } else if req.contains("/api/token") {
                MockResponse::json(
                    200,
                    r#"{"access_token":"NEW","token_type":"Bearer","expires_in":3600,"refresh_token":"REFRESHED"}"#,
                )
            } else {
                MockResponse::json(500, "")
            }
        });

        let initial = TokenSet::new_now(
            "STALE",
            "Bearer",
            3600,
            Some("REFRESH_ME".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        client.set_tokens(initial).unwrap();

        // The first call 401s, then we refresh, then we retry.
        let contains = client.library_contains("abc123").unwrap();
        assert!(contains, "expected contains=true after retry");
        let requests = mock.requests();
        // We should have observed: library(401) -> token(refresh) -> library(200).
        let library_calls = requests
            .iter()
            .filter(|r| r.contains("/me/library/contains"))
            .count();
        let refresh_calls = requests
            .iter()
            .filter(|r| r.contains("/api/token") && r.contains("grant_type=refresh_token"))
            .count();
        assert_eq!(library_calls, 2, "expected one 401 + one retry");
        assert_eq!(refresh_calls, 1, "expected exactly one refresh");
        assert_eq!(
            lib_count.load(Ordering::SeqCst),
            2,
            "expected two library calls"
        );
        drop(mock);
    }

    #[test]
    fn library_call_401_after_successful_refresh_returns_reauth_required() {
        // Even after a successful refresh, if the retry 401s the client
        // surfaces `Refresh("reauth_required")` so the UI can move the
        // session into "reconnect required" state.
        let (client, mock) = setup_mock_with_handler(|req| {
            if req.contains("/me/library/contains") {
                MockResponse::json(401, r#"{"error":"unauthorized"}"#)
            } else if req.contains("/api/token") {
                MockResponse::json(
                    200,
                    r#"{"access_token":"NEW","token_type":"Bearer","expires_in":3600,"refresh_token":"REFRESHED"}"#,
                )
            } else {
                MockResponse::json(500, "")
            }
        });
        let initial = TokenSet::new_now(
            "STALE",
            "Bearer",
            3600,
            Some("REFRESH_ME".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        client.set_tokens(initial).unwrap();
        let err = client.library_contains("abc123").unwrap_err();
        assert_eq!(err, SpotifyApiError::Refresh("reauth_required"));
        drop(mock);
    }

    // --- needs_refresh / current_tokens ------------------------------------

    #[test]
    fn needs_refresh_is_true_without_tokens() {
        let (client, _mock) = setup_mock_with_handler(|_| MockResponse::json(200, "[]"));
        assert!(client.needs_refresh());
    }

    #[test]
    fn needs_refresh_is_false_with_long_lived_tokens() {
        let (client, _mock) = setup_mock_with_handler(|_| MockResponse::json(200, "[]"));
        let tokens = TokenSet::new_now(
            "a",
            "Bearer",
            3600,
            Some("r".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        client.set_tokens(tokens).unwrap();
        assert!(!client.needs_refresh());
    }

    #[test]
    fn needs_refresh_is_true_for_already_expired_tokens() {
        let (client, _mock) = setup_mock_with_handler(|_| MockResponse::json(200, "[]"));
        let mut tokens = TokenSet::new_now("a", "Bearer", 1, Some("r".to_string()), None);
        // Force the token to be "old" by rewinding obtained_at.
        tokens.obtained_at = UNIX_EPOCH;
        client.set_tokens(tokens).unwrap();
        assert!(client.needs_refresh());
    }

    // --- Categorize helper -------------------------------------------------

    #[test]
    fn categorize_maps_status_into_client_or_server() {
        assert_eq!(categorize(400), HttpCategory::Client);
        assert_eq!(categorize(404), HttpCategory::Client);
        assert_eq!(categorize(499), HttpCategory::Client);
        assert_eq!(categorize(500), HttpCategory::Server);
        assert_eq!(categorize(503), HttpCategory::Server);
    }

    // --- Roundtrip for parse_token_response with realistic timing ----------

    #[test]
    fn parse_token_response_sets_obtained_at_to_current_time() {
        let before = SystemTime::now();
        let tokens = parse_token_response(
            r#"{"access_token":"a","token_type":"Bearer","expires_in":60}"#,
        )
        .unwrap();
        let after = SystemTime::now();
        assert!(tokens.obtained_at >= before && tokens.obtained_at <= after);
    }
}
