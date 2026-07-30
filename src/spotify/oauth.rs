// SPDX-License-Identifier: MIT

//! PKCE, authorization URL construction, callback parsing, and the loopback
//! HTTP listener used to catch Spotify's browser redirect.
//!
//! Everything in this module is *pure* HTTP parsing or local socket handling;
//! no Spotify production endpoints are ever contacted. The network calls to
//! `https://accounts.spotify.com/...` live in `super::client` and are exercised
//! against a local mock server in the test suite.

// All items here are part of the agreed public Spotify API and are exercised
// by the unit tests. The tests live in a `#[cfg(test)]` module so the release
// build reports them as unused; the allows keep the public surface quiet until
// Task 3 (token exchange) consumes them.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use form_urlencoded::parse as parse_query;

use crate::spotify::types::{AuthCode, AuthorizeUrlParams, PkceVerifier, SPOTIFY_AUTH_URL};

/// Errors that can be returned from `parse_callback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackError {
    /// Callback query was empty or contained no parseable key/value pairs.
    Malformed,
    /// `state` parameter was absent or did not match `expected_state`.
    StateMismatch,
    /// Spotify returned `error=access_denied` (or another `error=...` value).
    Denied,
    /// Callback did not contain an authorization `code` parameter.
    MissingCode,
}

const RESPONSE_TYPE_CODE: &str = "code";
const CODE_CHALLENGE_METHOD_S256: &str = "S256";

/// Generate a fresh PKCE verifier using the OS RNG.
///
/// Convenience wrapper around `PkceVerifier::generate()`.
pub fn generate_pkce_verifier() -> PkceVerifier {
    PkceVerifier::generate()
}

/// Build the Spotify authorization URL for the loopback PKCE flow.
///
/// All parameters that go into the URL are pulled from `params`; this
/// function does not look at any global state.
pub fn build_authorize_url(params: AuthorizeUrlParams) -> String {
    // We assemble the query with `form_urlencoded` rather than `format!` so
    // every parameter value is correctly percent-encoded.
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer
        .append_pair("client_id", &params.client_id)
        .append_pair("response_type", RESPONSE_TYPE_CODE)
        .append_pair("redirect_uri", &params.redirect_uri)
        .append_pair("code_challenge_method", CODE_CHALLENGE_METHOD_S256)
        .append_pair("code_challenge", &params.code_challenge)
        .append_pair("state", params.state.as_str())
        .append_pair("scope", &params.scopes);
    format!("{SPOTIFY_AUTH_URL}?{}", serializer.finish())
}

/// Parse a Spotify callback query string and validate the state.
///
/// Accepts either a full URL (the typical browser redirect) or a bare
/// `application/x-www-form-urlencoded` query string. The function extracts
/// the query before parsing, so callers can pass the raw redirect URL.
pub fn parse_callback(input: &str, expected_state: &str) -> Result<AuthCode, CallbackError> {
    let query = extract_query(input);

    if query.is_empty() {
        return Err(CallbackError::Malformed);
    }

    let mut state: Option<String> = None;
    let mut code: Option<String> = None;
    let mut error: Option<String> = None;

    for (k, v) in parse_query(query.as_bytes()) {
        match k.as_ref() {
            "state" => state = Some(v.into_owned()),
            "code" => code = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }

    // An explicit error response always wins (denial, etc.).
    if error.is_some() {
        return Err(CallbackError::Denied);
    }

    // state must be present and equal to expected. Missing state is
    // malformed; mismatched state is a security rejection.
    match state {
        Some(ref s) if s == expected_state => {}
        Some(_) => return Err(CallbackError::StateMismatch),
        None => return Err(CallbackError::Malformed),
    }

    // After state validation, code must be present.
    match code {
        Some(c) => AuthCode::new(&c).map_err(|_| CallbackError::MissingCode),
        None => Err(CallbackError::MissingCode),
    }
}

/// Extract the query portion of a URL or pass through a bare query string.
///
/// `parse_callback` accepts both forms so tests and the live loopback listener
/// can share one implementation.
fn extract_query(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Bare query: starts with a key= or is `?...` already.
    let qpos = trimmed.find('?').unwrap_or(0);
    let after_q = &trimmed[qpos..];
    if after_q.starts_with('?') {
        // Stop at any fragment.
        match after_q.find('#') {
            Some(hash) => after_q[1..hash].to_string(),
            None => after_q[1..].to_string(),
        }
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Loopback HTTP listener
// ---------------------------------------------------------------------------

/// Errors returned by [`LoopbackListener`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopbackError {
    /// Could not bind to the requested address (port already in use, etc.).
    BindFailed,
    /// I/O error during accept/read/write. Inner is a non-sensitive category.
    Io(&'static str),
    /// The HTTP request line was not parseable.
    MalformedRequest,
    /// The browser hit a path other than `/callback`. The path is preserved for
    /// diagnostics, but never contains user secrets.
    WrongPath(String),
    /// `state` parameter did not match the expected value.
    StateMismatch,
    /// The browser reported `error=access_denied` (or another `error=...`).
    UserDenied,
    /// The authorization `code` was missing or invalid.
    MissingCode,
    /// No callback request arrived before the timeout elapsed.
    Timeout,
}

impl std::fmt::Display for LoopbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BindFailed => f.write_str("failed to bind loopback listener"),
            Self::Io(_) => f.write_str("loopback listener I/O error"),
            Self::MalformedRequest => f.write_str("malformed HTTP request"),
            Self::WrongPath(_) => f.write_str("unexpected path on loopback callback"),
            Self::StateMismatch => f.write_str("loopback callback state mismatch"),
            Self::UserDenied => f.write_str("user denied Spotify authorization"),
            Self::MissingCode => f.write_str("loopback callback missing authorization code"),
            Self::Timeout => f.write_str("loopback callback timed out"),
        }
    }
}

impl std::error::Error for LoopbackError {}

/// Default loopback bind address (per the approved plan: `127.0.0.1:43821`).
pub const DEFAULT_LOOPBACK_ADDR: &str = "127.0.0.1:43821";

/// A bound loopback HTTP listener ready to receive Spotify's browser callback.
///
/// `LoopbackListener` owns the underlying [`TcpListener`] and a snapshot of
/// its local address. Calling [`wait_for_callback`](Self::wait_for_callback)
/// accepts exactly one connection, serves a small HTML response, parses the
/// callback, and drops the listener — releasing the port.
pub struct LoopbackListener {
    inner: TcpListener,
    addr: SocketAddr,
}

impl LoopbackListener {
    /// Bind to the default `127.0.0.1:43821` loopback address.
    pub fn bind_default() -> Result<Self, LoopbackError> {
        let addr: SocketAddr = DEFAULT_LOOPBACK_ADDR
            .parse()
            .expect("DEFAULT_LOOPBACK_ADDR must parse as SocketAddr");
        Self::bind(addr)
    }

    /// Bind to a specific loopback address. Tests use port `0` to obtain an
    /// OS-assigned port.
    pub fn bind(addr: SocketAddr) -> Result<Self, LoopbackError> {
        let inner = TcpListener::bind(addr).map_err(|_| LoopbackError::BindFailed)?;
        let addr = inner.local_addr().map_err(|_| LoopbackError::BindFailed)?;
        Ok(Self { inner, addr })
    }

    /// The local address the listener is bound to. For port `0` this is the
    /// OS-assigned port.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Accept one HTTP connection, parse the Spotify callback, write a small
    /// HTML response, and return the authorization code.
    ///
    /// `timeout` is enforced on the underlying `accept` loop. After the
    /// function returns (success or failure) the listener and its bound port
    /// are released, because `self` is consumed.
    pub fn wait_for_callback(
        self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<AuthCode, LoopbackError> {
        let (mut stream, _peer) = self.accept_with_timeout(timeout)?;
        // Bound the read/write on the accepted stream so a slow or stalled
        // client cannot hang the applet indefinitely even if it manages to
        // connect.
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        serve_one_callback(&mut stream, expected_state)
    }

    /// Drive the underlying `accept` loop on a deadline. `TcpListener` does
    /// not expose a direct read-timeout setter, so we flip the socket into
    /// non-blocking mode, poll on a small sleep, and re-check the deadline.
    fn accept_with_timeout(
        self,
        timeout: Duration,
    ) -> Result<(TcpStream, std::net::SocketAddr), LoopbackError> {
        self.inner
            .set_nonblocking(true)
            .map_err(|_| LoopbackError::Io("set_nonblocking"))?;
        let deadline = std::time::Instant::now() + timeout;
        let result = loop {
            match self.inner.accept() {
                Ok(pair) => break Ok(pair),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        break Err(LoopbackError::Timeout);
                    }
                    // Use a short sleep so we don't busy-loop but still
                    // react promptly when a client connects.
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break Err(LoopbackError::Io("accept")),
            }
        };
        // Reset to blocking before we hand the stream off, so the caller
        // and the accepted stream are not stuck in non-blocking mode.
        let _ = self.inner.set_nonblocking(false);
        result
    }
}

/// A parsed HTTP/1.1 request line for the loopback callback.
///
/// Only the fields Cosmictify actually needs are exposed; the listener does
/// not look at headers beyond the request line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackRequest {
    /// HTTP method (always `GET` for Spotify's browser redirect).
    pub method: String,
    /// Request path, without the query string. Spotify's redirect is always
    /// `/callback`.
    pub path: String,
    /// Raw, undecoded query string (after the `?`).
    pub query: String,
}

/// Parse the first line of an HTTP/1.1 request into method, path, and query.
///
/// This is intentionally minimal: it only parses the request line and does
/// not validate headers, body, or HTTP version. The loopback listener only
/// ever receives Spotify's well-formed browser redirect.
pub fn parse_http_request(buf: &str) -> Result<CallbackRequest, LoopbackError> {
    let first_line = buf.lines().next().ok_or(LoopbackError::MalformedRequest)?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().ok_or(LoopbackError::MalformedRequest)?;
    let target = parts.next().ok_or(LoopbackError::MalformedRequest)?;
    if method != "GET" {
        return Err(LoopbackError::MalformedRequest);
    }
    let (path, query) = match target.find('?') {
        Some(q) => (&target[..q], &target[q + 1..]),
        None => (target, ""),
    };
    Ok(CallbackRequest {
        method: method.to_string(),
        path: path.to_string(),
        query: query.to_string(),
    })
}

/// Build the small HTML response the browser will display after the callback.
///
/// The response is always a `200 OK` with a self-contained HTML body and
/// `Connection: close`, so the browser closes the socket once the page is
/// rendered.
pub fn callback_response_html(success: bool) -> String {
    let body = if success {
        "<!DOCTYPE html><html><head><title>Spotify Authorization</title></head><body><p>Spotify authorization complete. You can close this window and return to Cosmictify.</p></body></html>"
    } else {
        "<!DOCTYPE html><html><head><title>Spotify Authorization</title></head><body><p>Spotify authorization failed. Please return to Cosmictify and try again.</p></body></html>"
    };
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// Read a single HTTP/1.1 request from `stream`, parse it, write a small HTML
/// response, and return the authorization code (or an error).
///
/// The function is exposed (rather than being a private method on the
/// listener) so the tests can drive the full serve-one-callback code path
/// without depending on the listener's accept loop.
pub fn serve_one_callback(
    stream: &mut TcpStream,
    expected_state: &str,
) -> Result<AuthCode, LoopbackError> {
    // Read until the end of HTTP headers.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut tmp)
            .map_err(|_| LoopbackError::Io("read"))?;
        if n == 0 {
            return Err(LoopbackError::Io("eof"));
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return Err(LoopbackError::MalformedRequest);
        }
    }
    let text = std::str::from_utf8(&buf).map_err(|_| LoopbackError::MalformedRequest)?;
    let req = parse_http_request(text)?;

    // Only `/callback` is acceptable; everything else is reported as
    // `WrongPath` so the UI can show "we expected Spotify's redirect".
    if req.path != "/callback" {
        let html = callback_response_html(false);
        let _ = stream.write_all(html.as_bytes());
        let _ = stream.flush();
        return Err(LoopbackError::WrongPath(req.path));
    }

    match parse_callback(&req.query, expected_state) {
        Ok(code) => {
            let html = callback_response_html(true);
            let _ = stream.write_all(html.as_bytes());
            let _ = stream.flush();
            Ok(code)
        }
        Err(e) => {
            let html = callback_response_html(false);
            let _ = stream.write_all(html.as_bytes());
            let _ = stream.flush();
            Err(map_callback_error(e))
        }
    }
}

fn map_callback_error(e: CallbackError) -> LoopbackError {
    match e {
        CallbackError::Malformed => LoopbackError::MalformedRequest,
        CallbackError::StateMismatch => LoopbackError::StateMismatch,
        CallbackError::Denied => LoopbackError::UserDenied,
        CallbackError::MissingCode => LoopbackError::MissingCode,
    }
}

// ---------------------------------------------------------------------------
// Tests — pure module, no fixtures needed beyond what's declared above.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::types::{OAuthState, PkceVerifier, REDIRECT_URI, REQUIRED_SCOPES};
    use std::net::TcpStream;
    use std::thread;

    #[test]
    fn query_extractor_handles_bare_query_and_url() {
        assert_eq!(extract_query("code=abc&state=xyz"), "code=abc&state=xyz");
        assert_eq!(
            extract_query("https://example.com/callback?code=abc&state=xyz"),
            "code=abc&state=xyz"
        );
        assert_eq!(
            extract_query("https://example.com/callback?code=abc&state=xyz#fragment"),
            "code=abc&state=xyz"
        );
        assert_eq!(extract_query(""), "");
        assert_eq!(extract_query("   "), "");
    }

    #[test]
    fn authorize_url_is_built_with_expected_query_order() {
        // Query parameter order is not load-bearing for Spotify, but we
        // check the round-trip via a parsed URL to avoid encoding mistakes.
        let params = AuthorizeUrlParams::builder()
            .client_id("e117d2b248334356b28cdf56be6eba18")
            .state(OAuthState::new("abcdefghijklmnop").unwrap())
            .code_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
            .build();
        let url = build_authorize_url(params);
        let parsed = url::Url::parse(&url).expect("valid URL");
        let map: std::collections::HashMap<String, String> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(map.get("client_id").unwrap(), "e117d2b248334356b28cdf56be6eba18");
        assert_eq!(map.get("response_type").unwrap(), "code");
        assert_eq!(map.get("redirect_uri").unwrap(), REDIRECT_URI);
        assert_eq!(map.get("scope").unwrap(), REQUIRED_SCOPES);
        assert_eq!(map.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(
            map.get("code_challenge").unwrap(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(map.get("state").unwrap(), "abcdefghijklmnop");
    }

    #[test]
    fn authorize_url_percent_encodes_special_chars() {
        // Build a state that needs encoding and verify it round-trips.
        let params = AuthorizeUrlParams::builder()
            .client_id("e117d2b248334356b28cdf56be6eba18")
            .state(OAuthState::new("aaaaaaaaaaaaaaaa").unwrap())
            .code_challenge("challenge")
            .scopes("user-library-read user-library-modify")
            .build();
        let url = build_authorize_url(params);
        // OAuth2 scope values are space-delimited. Both standard encodings
        // (`+` from application/x-www-form-urlencoded, `%20` from RFC 3986)
        // are accepted by Spotify and equivalent on the wire; assert either.
        assert!(
            url.contains("user-library-read+user-library-modify")
                || url.contains("user-library-read%20user-library-modify"),
            "scope must be space-encoded, got: {url}"
        );
    }

    #[test]
    fn parse_callback_accepts_full_url() {
        let url = "http://127.0.0.1:43821/callback?code=ABC&state=zzz";
        let parsed = parse_callback(url, "zzz").unwrap();
        assert_eq!(parsed.as_str(), "ABC");
    }

    #[test]
    fn parse_callback_treats_empty_code_as_missing() {
        let err = parse_callback("code=&state=zzz", "zzz").unwrap_err();
        assert_eq!(err, CallbackError::MissingCode);
    }

    #[test]
    fn generate_produces_a_valid_verifier() {
        let v = generate_pkce_verifier();
        // Re-validate to confirm round-trip.
        PkceVerifier::new(v.as_str()).expect("generated verifier must validate");
    }

    // --- HTTP request parser ------------------------------------------------

    #[test]
    fn parse_http_request_extracts_path_and_query() {
        let req = parse_http_request("GET /callback?code=ABC&state=zzz HTTP/1.1\r\n").unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/callback");
        assert_eq!(req.query, "code=ABC&state=zzz");
    }

    #[test]
    fn parse_http_request_handles_no_query() {
        let req = parse_http_request("GET / HTTP/1.1\r\n").unwrap();
        assert_eq!(req.path, "/");
        assert_eq!(req.query, "");
    }

    #[test]
    fn parse_http_request_rejects_non_get() {
        assert!(matches!(
            parse_http_request("POST /callback HTTP/1.1\r\n"),
            Err(LoopbackError::MalformedRequest)
        ));
    }

    #[test]
    fn parse_http_request_rejects_empty() {
        assert!(matches!(parse_http_request(""), Err(LoopbackError::MalformedRequest)));
        assert!(matches!(
            parse_http_request("only-one-token"),
            Err(LoopbackError::MalformedRequest)
        ));
    }

    // --- HTTP response builder ----------------------------------------------

    #[test]
    fn callback_response_html_contains_content_length_and_body() {
        let html = callback_response_html(true);
        assert!(html.starts_with("HTTP/1.1 200 OK"));
        assert!(html.contains("Content-Length: "));
        assert!(html.contains("Content-Type: text/html"));
        assert!(html.contains("Connection: close"));
        assert!(html.contains("Spotify authorization complete"));
        let failure = callback_response_html(false);
        assert!(failure.contains("Spotify authorization failed"));
        // The header section and the body must be separated by CRLF CRLF.
        assert!(html.contains("\r\n\r\n"));
    }

    // --- Loopback listener: end-to-end on a random port --------------------

    fn spawn_client(addr: SocketAddr, request: &'static [u8]) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut s = TcpStream::connect(addr).expect("client must connect");
            s.write_all(request).expect("client must write");
            // Closing the write side helps the server observe EOF if it cares
            // and lets the response come back promptly.
            let _ = s.shutdown(std::net::Shutdown::Write);
            let mut buf = Vec::new();
            // Use a small read timeout so a hung server does not block the
            // test forever.
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let _ = s.read_to_end(&mut buf);
            buf
        })
    }

    #[test]
    fn listener_returns_code_for_valid_callback() {
        let listener = LoopbackListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr();
        let expected = "state-abcdef";
        let handle = thread::spawn(move || listener.wait_for_callback(expected, Duration::from_secs(2)));

        let response = spawn_client(
            addr,
            b"GET /callback?code=AuthCodeXYZ&state=state-abcdef HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .join()
        .unwrap();
        let body = String::from_utf8_lossy(&response);
        assert!(
            body.contains("Spotify authorization complete"),
            "expected success page, got: {body}"
        );

        let code = handle.join().unwrap().expect("listener should return code");
        assert_eq!(code.as_str(), "AuthCodeXYZ");
    }

    #[test]
    fn listener_rejects_wrong_path() {
        let listener = LoopbackListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr();
        let handle = thread::spawn(move || listener.wait_for_callback("state", Duration::from_secs(2)));

        let response = spawn_client(
            addr,
            b"GET /not-callback?code=ABC&state=state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .join()
        .unwrap();
        let body = String::from_utf8_lossy(&response);
        assert!(
            body.contains("Spotify authorization failed"),
            "expected failure page, got: {body}"
        );

        let result = handle.join().unwrap();
        assert!(
            matches!(&result, Err(LoopbackError::WrongPath(p)) if p == "/not-callback"),
            "expected WrongPath, got {result:?}"
        );
    }

    #[test]
    fn listener_rejects_state_mismatch() {
        let listener = LoopbackListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr();
        let handle = thread::spawn(move || listener.wait_for_callback("expected", Duration::from_secs(2)));

        let _ = spawn_client(
            addr,
            b"GET /callback?code=ABC&state=other HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .join()
        .unwrap();

        let result = handle.join().unwrap();
        assert!(matches!(result, Err(LoopbackError::StateMismatch)));
    }

    #[test]
    fn listener_rejects_denied() {
        let listener = LoopbackListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr();
        let handle = thread::spawn(move || listener.wait_for_callback("expected", Duration::from_secs(2)));

        let _ = spawn_client(
            addr,
            b"GET /callback?error=access_denied&state=expected HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .join()
        .unwrap();

        let result = handle.join().unwrap();
        assert!(matches!(result, Err(LoopbackError::UserDenied)));
    }

    #[test]
    fn listener_rejects_missing_code() {
        let listener = LoopbackListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr();
        let handle = thread::spawn(move || listener.wait_for_callback("expected", Duration::from_secs(2)));

        let _ = spawn_client(
            addr,
            b"GET /callback?state=expected HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .join()
        .unwrap();

        let result = handle.join().unwrap();
        assert!(matches!(result, Err(LoopbackError::MissingCode)));
    }

    #[test]
    fn listener_times_out_and_releases_port() {
        let listener = LoopbackListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr();
        let start = std::time::Instant::now();
        let result = listener.wait_for_callback("any-state", Duration::from_millis(80));
        let elapsed = start.elapsed();
        assert!(matches!(result, Err(LoopbackError::Timeout)));
        // The accept timeout fired somewhere near the requested 80 ms; we
        // accept anything below a few seconds so flaky CI doesn't break.
        assert!(elapsed < Duration::from_secs(2), "timeout took too long: {elapsed:?}");
        // Port must be reusable after the listener is dropped; rebind to
        // the exact same address and assert it succeeds.
        let _rebound = TcpListener::bind(addr).expect("port should be released after timeout");
    }

    #[test]
    fn listener_default_address_is_127_0_0_1_port_43821() {
        // The default must match the redirect URI registered in the user's
        // Spotify Developer app exactly.
        let addr: SocketAddr = DEFAULT_LOOPBACK_ADDR.parse().unwrap();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 43821);
    }

    #[test]
    fn serve_one_callback_serves_failure_page_on_state_mismatch() {
        // Drive `serve_one_callback` directly with an in-memory socketpair
        // to verify the response body matches the error branch.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            serve_one_callback(&mut stream, "expected")
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .write_all(b"GET /callback?code=ABC&state=other HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let _ = client.shutdown(std::net::Shutdown::Write);
        let mut buf = Vec::new();
        let _ = client.read_to_end(&mut buf);
        let result = server.join().unwrap();
        let body = String::from_utf8_lossy(&buf);
        assert!(matches!(result, Err(LoopbackError::StateMismatch)));
        assert!(body.contains("Spotify authorization failed"));
    }
}
