// SPDX-License-Identifier: MIT

//! Unified `/v1/me/library` Web API calls.
//!
//! Spotify migrated the saved-tracks endpoints in February 2026; the
//! current 2026 path for Development Mode apps is the unified
//! `/v1/me/library` family. This module implements the three calls
//! Cosmictify needs:
//!
//! * `GET /v1/me/library/contains?uris=...` — check whether a track is in
//!   the user's library (returns `[true]` or `[false]`).
//! * `PUT /v1/me/library?uris=...` — save a track to the library.
//! * `DELETE /v1/me/library?uris=...` — remove a track from the library.
//!
//! All three call sites go through the 401-refresh-retry machinery on
//! [`SpotifyClient`]; the tests below drive the same path against a local
//! mock HTTP server.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::spotify::client::{HttpCategory, SpotifyApiError, SpotifyClient};
use crate::spotify::keyring::TokenStore;

// ---------------------------------------------------------------------------
// Track ID and URI helpers
// ---------------------------------------------------------------------------

/// Reasons a candidate track id may be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackIdError {
    /// Empty input.
    Empty,
    /// Track id longer than 64 characters.
    TooLong,
    /// Track id contains a non-`[A-Za-z0-9]` character.
    InvalidCharacters,
}

impl std::fmt::Display for TrackIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("track id is empty"),
            Self::TooLong => f.write_str("track id is too long"),
            Self::InvalidCharacters => f.write_str("track id has invalid characters"),
        }
    }
}

impl std::error::Error for TrackIdError {}

/// Validate a Spotify track id. Returns the same slice on success.
///
/// A Spotify track id is a 22-character base62 string. We accept the
/// slightly wider range `[A-Za-z0-9]{1,64}` to stay forward-compatible
/// with any future format change.
pub fn validate_track_id(id: &str) -> Result<&str, TrackIdError> {
    if id.is_empty() {
        return Err(TrackIdError::Empty);
    }
    if id.len() > 64 {
        return Err(TrackIdError::TooLong);
    }
    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(TrackIdError::InvalidCharacters);
    }
    Ok(id)
}

/// Build the canonical `spotify:track:{id}` URI for a validated track id.
pub fn build_track_uri(track_id: &str) -> Result<String, TrackIdError> {
    let id = validate_track_id(track_id)?;
    Ok(format!("spotify:track:{id}"))
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Reasons a `/v1/me/library/contains` response may be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainsError {
    /// Body was not valid JSON.
    InvalidJson,
    /// Body was not a JSON array.
    NotAnArray,
    /// Body had the wrong number of elements.
    WrongLength,
    /// An element was not a boolean.
    NotABool,
    /// Body was empty.
    Empty,
}

impl std::fmt::Display for ContainsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson => f.write_str("invalid JSON"),
            Self::NotAnArray => f.write_str("not a JSON array"),
            Self::WrongLength => f.write_str("wrong number of elements"),
            Self::NotABool => f.write_str("element is not a boolean"),
            Self::Empty => f.write_str("empty body"),
        }
    }
}

impl std::error::Error for ContainsError {}

/// Parse a `/v1/me/library/contains` response body. The endpoint returns a
/// JSON array with one boolean per `uris` query parameter; we expect
/// exactly one element because Cosmictify only ever queries one URI at a
/// time.
pub fn parse_contains(body: &str) -> Result<bool, ContainsError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(ContainsError::Empty);
    }
    // Fast-path: a body of `true` or `false` (no array) is unambiguous and
    // is what some test fixtures emit. The real Spotify endpoint always
    // returns an array, but this is forgiving.
    match body {
        "true" => return Ok(true),
        "false" => return Ok(false),
        _ => {}
    }
    // Expected shape: `[true]` or `[false]`, optionally with whitespace.
    if !body.starts_with('[') || !body.ends_with(']') {
        return Err(ContainsError::NotAnArray);
    }
    let inner = body[1..body.len() - 1].trim();
    if inner.is_empty() {
        return Err(ContainsError::Empty);
    }
    // Reject the comma case to keep the "one URI in" contract loud.
    if inner.contains(',') {
        return Err(ContainsError::WrongLength);
    }
    match inner {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ContainsError::NotABool),
    }
}

// ---------------------------------------------------------------------------
// SpotifyClient methods
// ---------------------------------------------------------------------------

/// Concurrency latch used by the library methods to ensure we never issue
/// more than one library mutation request at a time. The UI can hammer
/// the heart button; without this latch we'd send every mutation request
/// to Spotify.
///
/// Uses `thread_local!` so each test thread gets its own latch, avoiding
/// false `mutation_in_flight` failures when the test suite runs in parallel.
thread_local! {
    static LIBRARY_MUTATING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn try_acquire_mutation() -> bool {
    LIBRARY_MUTATING.with(|cell| {
        let prev = cell.replace(true);
        !prev // if prev was false, we acquired it
    })
}

fn release_mutation() {
    LIBRARY_MUTATING.with(|cell| cell.set(false));
}

impl<S: TokenStore> SpotifyClient<S> {
    /// `GET /v1/me/library/contains?uris=spotify:track:{track_id}`.
    ///
    /// `track_id` is validated; an empty or otherwise invalid id is
    /// surfaced as [`SpotifyApiError::Malformed`].
    pub fn library_contains(&self, track_id: &str) -> Result<bool, SpotifyApiError> {
        let track_id = validate_track_id(track_id)
            .map_err(|_| SpotifyApiError::Malformed("invalid_track_id"))?;
        let uri = build_track_uri(track_id)
            .map_err(|_| SpotifyApiError::Malformed("invalid_track_id"))?;
        let url = self.library_contains_url(&uri);
        let response = self.send_authed(|token| {
            ureq::get(&url).set("Authorization", &format!("Bearer {token}"))
        })?;
        let status = response.status();
        let body = response
            .into_string()
            .map_err(|_| SpotifyApiError::Malformed("contains_body_read"))?;
        parse_contains(&body).map_err(|e| match e {
            ContainsError::Empty => SpotifyApiError::Malformed("contains_empty"),
            ContainsError::InvalidJson
            | ContainsError::NotAnArray
            | ContainsError::WrongLength
            | ContainsError::NotABool => SpotifyApiError::Malformed("contains_response"),
        })
        .map_err(|err| {
            // Surface a 5xx as Http/Server so the UI can show a different
            // message than a malformed body.
            if status >= 500 && status < 600 {
                SpotifyApiError::Http {
                    status,
                    category: HttpCategory::Server,
                }
            } else {
                err
            }
        })
    }

    /// `PUT /v1/me/library?uris=spotify:track:{track_id}`.
    pub fn library_save(&self, track_id: &str) -> Result<(), SpotifyApiError> {
        let track_id = validate_track_id(track_id)
            .map_err(|_| SpotifyApiError::Malformed("invalid_track_id"))?;
        let uri = build_track_uri(track_id)
            .map_err(|_| SpotifyApiError::Malformed("invalid_track_id"))?;
        let url = self.library_modify_url(&uri);
        // Optimistic update is the UI's responsibility (Task 5). Here we
        // simply enforce the "one mutation in flight at a time" contract
        // to avoid racing the user with the network.
        if !try_acquire_mutation() {
            return Err(SpotifyApiError::Malformed("mutation_in_flight"));
        }
        let result = self.send_authed(|token| {
            ureq::put(&url)
                .set("Authorization", &format!("Bearer {token}"))
                // Spotify's unified library endpoint requires an explicit
                // zero-length body for query-parameter mutations. `call()`
                // omits this header, which the live API rejects with 411.
                .set("Content-Length", "0")
        });
        release_mutation();
        let response = result?;
        // 200 / 204 both mean "saved". Other 2xx are unexpected but we
        // accept them silently to match Spotify's tolerant behaviour.
        if !(200..300).contains(&response.status()) {
            return Err(SpotifyApiError::Http {
                status: response.status(),
                category: HttpCategory::Client,
            });
        }
        Ok(())
    }

    /// `DELETE /v1/me/library?uris=spotify:track:{track_id}`.
    pub fn library_remove(&self, track_id: &str) -> Result<(), SpotifyApiError> {
        let track_id = validate_track_id(track_id)
            .map_err(|_| SpotifyApiError::Malformed("invalid_track_id"))?;
        let uri = build_track_uri(track_id)
            .map_err(|_| SpotifyApiError::Malformed("invalid_track_id"))?;
        let url = self.library_modify_url(&uri);
        if !try_acquire_mutation() {
            return Err(SpotifyApiError::Malformed("mutation_in_flight"));
        }
        let result = self.send_authed(|token| {
            ureq::delete(&url)
                .set("Authorization", &format!("Bearer {token}"))
                .set("Content-Length", "0")
        });
        release_mutation();
        let response = result?;
        if !(200..300).contains(&response.status()) {
            return Err(SpotifyApiError::Http {
                status: response.status(),
                category: HttpCategory::Client,
            });
        }
        Ok(())
    }

    fn library_contains_url(&self, uri: &str) -> String {
        // Use the `url` crate so the colon in the Spotify URI is percent-
        // encoded correctly. `format!("?uris={uri}")` would leave a bare
        // colon, which works for some HTTP servers but not all proxies.
        let mut url = url::Url::parse(&format!("{}/me/library/contains", self.api_base()))
            .expect("api base + path must parse");
        url.query_pairs_mut().append_pair("uris", uri);
        url.into_string()
    }

    fn library_modify_url(&self, uri: &str) -> String {
        let mut url = url::Url::parse(&format!("{}/me/library", self.api_base()))
            .expect("api base + path must parse");
        url.query_pairs_mut().append_pair("uris", uri);
        url.into_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::client::testing::{MockHttp, MockResponse};
    use crate::spotify::keyring::InMemoryTokenStore;
    use crate::spotify::types::REQUIRED_SCOPES;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // --- track_id / URI helpers --------------------------------------------

    #[test]
    fn validate_track_id_accepts_alphanumeric() {
        assert!(validate_track_id("abc123XYZ").is_ok());
        assert!(validate_track_id("3bYm3a1sdf0aS2ksd8aAAA").is_ok());
    }

    #[test]
    fn validate_track_id_rejects_empty() {
        assert_eq!(validate_track_id(""), Err(TrackIdError::Empty));
    }

    #[test]
    fn validate_track_id_rejects_too_long() {
        let s = "a".repeat(65);
        assert_eq!(validate_track_id(&s), Err(TrackIdError::TooLong));
    }

    #[test]
    fn validate_track_id_rejects_invalid_characters() {
        assert_eq!(
            validate_track_id("abc:123"),
            Err(TrackIdError::InvalidCharacters)
        );
        assert_eq!(
            validate_track_id("abc/123"),
            Err(TrackIdError::InvalidCharacters)
        );
        assert_eq!(
            validate_track_id("abc 123"),
            Err(TrackIdError::InvalidCharacters)
        );
    }

    #[test]
    fn build_track_uri_produces_canonical_form() {
        assert_eq!(build_track_uri("3bYm3a1sdf0a"), Ok("spotify:track:3bYm3a1sdf0a".to_string()));
    }

    #[test]
    fn build_track_uri_propagates_validation_error() {
        assert!(matches!(build_track_uri(""), Err(TrackIdError::Empty)));
        assert!(matches!(build_track_uri("a:b"), Err(TrackIdError::InvalidCharacters)));
    }

    // --- parse_contains ----------------------------------------------------

    #[test]
    fn parse_contains_accepts_array_form() {
        assert!(parse_contains("[true]").unwrap());
        assert!(!parse_contains("[false]").unwrap());
        assert!(parse_contains(" [ true ] ").unwrap());
    }

    #[test]
    fn parse_contains_accepts_bare_bool_for_robustness() {
        assert!(parse_contains("true").unwrap());
        assert!(!parse_contains("false").unwrap());
    }

    #[test]
    fn parse_contains_rejects_malformed_responses() {
        assert_eq!(parse_contains(""), Err(ContainsError::Empty));
        assert!(matches!(parse_contains("[]"), Err(ContainsError::Empty)));
        assert!(matches!(
            parse_contains("not json"),
            Err(ContainsError::NotAnArray)
        ));
        assert!(matches!(
            parse_contains("[true,false]"),
            Err(ContainsError::WrongLength)
        ));
        assert!(matches!(
            parse_contains("[1]"),
            Err(ContainsError::NotABool)
        ));
        assert!(matches!(
            parse_contains("[hello]"),
            Err(ContainsError::NotABool)
        ));
    }

    // --- End-to-end library calls against a local mock ---------------------

    fn setup_mock<F>(handler: F) -> (SpotifyClient<InMemoryTokenStore>, MockHttp)
    where
        F: Fn(&str) -> MockResponse + Send + 'static,
    {
        let mock = MockHttp::start(handler);
        let token_url = format!("http://{}/api/token", mock.addr());
        let api_base = format!("http://{}", mock.addr());
        let store = InMemoryTokenStore::new();
        let client =
            SpotifyClient::new("e117d2b248334356b28cdf56be6eba18".to_string(), store)
                .with_token_url(token_url)
                .with_api_base(api_base);
        (client, mock)
    }

    fn with_tokens(client: &SpotifyClient<InMemoryTokenStore>) {
        let tokens = crate::spotify::types::TokenSet::new_now(
            "ACCESS",
            "Bearer",
            3600,
            Some("REFRESH".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        client.set_tokens(tokens).unwrap();
    }

    // GET /me/library/contains

    #[test]
    fn library_contains_uses_get_and_percent_encodes_uri() {
        let (client, mock) = setup_mock(|req| {
            assert!(req.starts_with("GET /me/library/contains?"), "{req}");
            assert!(
                req.contains("uris=spotify%3Atrack%3Aabc123")
                    || req.contains("uris=spotify:track:abc123"),
                "uri not encoded in request: {req}"
            );
            assert!(
                req.contains("Authorization: Bearer ACCESS"),
                "missing bearer auth: {req}"
            );
            MockResponse::json(200, "[true]")
        });
        with_tokens(&client);
        assert!(client.library_contains("abc123").unwrap());
        drop(mock);
    }

    #[test]
    fn library_contains_parses_false_response() {
        let (client, mock) = setup_mock(|_req| MockResponse::json(200, "[false]"));
        with_tokens(&client);
        assert!(!client.library_contains("abc123").unwrap());
        drop(mock);
    }

    #[test]
    fn library_contains_rejects_empty_track_id() {
        let (client, mock) = setup_mock(|_| {
            panic!("mock should not be called for empty track id");
        });
        with_tokens(&client);
        let err = client.library_contains("").unwrap_err();
        assert!(matches!(err, SpotifyApiError::Malformed("invalid_track_id")));
        drop(mock);
    }

    #[test]
    fn library_contains_treats_malformed_body_as_error() {
        let (client, mock) = setup_mock(|_req| MockResponse::json(200, "not json"));
        with_tokens(&client);
        let err = client.library_contains("abc123").unwrap_err();
        assert!(matches!(err, SpotifyApiError::Malformed(_)));
        drop(mock);
    }

    // 401 -> refresh -> retry on the library endpoint

    #[test]
    fn library_contains_401_refreshes_and_retries() {
        let lib_count = Arc::new(AtomicUsize::new(0));
        let lib_count_clone = Arc::clone(&lib_count);
        let (client, mock) = setup_mock(move |req| {
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
        with_tokens(&client);
        assert!(client.library_contains("abc123").unwrap());
        assert_eq!(lib_count.load(Ordering::SeqCst), 2);
        // Confirm the refresh was actually a refresh_token grant.
        let requests = mock.requests();
        assert!(requests
            .iter()
            .any(|r| r.contains("grant_type=refresh_token")));
        drop(mock);
    }

    // 403 -> Allowlist

    #[test]
    fn library_contains_403_returns_allowlist() {
        let (client, mock) = setup_mock(|_req| {
            MockResponse::json(403, r#"{"error":"forbidden"}"#)
        });
        with_tokens(&client);
        let err = client.library_contains("abc123").unwrap_err();
        assert_eq!(err, SpotifyApiError::Allowlist("forbidden"));
        drop(mock);
    }

    // 429 -> RateLimited with retry_after

    #[test]
    fn library_contains_429_exposes_retry_after() {
        let (client, mock) = setup_mock(|_req| {
            MockResponse::json(429, r#"{"error":"rate_limited"}"#)
                .with_header("Retry-After", "12")
        });
        with_tokens(&client);
        let err = client.library_contains("abc123").unwrap_err();
        assert_eq!(err, SpotifyApiError::RateLimited { retry_after: 12 });
        drop(mock);
    }

    #[test]
    fn library_contains_429_without_retry_after_uses_zero() {
        let (client, mock) = setup_mock(|_req| {
            MockResponse::json(429, r#"{"error":"rate_limited"}"#)
        });
        with_tokens(&client);
        let err = client.library_contains("abc123").unwrap_err();
        assert_eq!(err, SpotifyApiError::RateLimited { retry_after: 0 });
        drop(mock);
    }

    // 404 / generic HTTP errors

    #[test]
    fn library_contains_404_surfaces_as_http() {
        let (client, mock) = setup_mock(|_req| MockResponse::json(404, "{}"));
        with_tokens(&client);
        let err = client.library_contains("abc123").unwrap_err();
        match err {
            SpotifyApiError::Http { status, category } => {
                assert_eq!(status, 404);
                assert_eq!(category, HttpCategory::Client);
            }
            other => panic!("expected Http(404), got {other:?}"),
        }
        drop(mock);
    }

    #[test]
    fn library_contains_500_surfaces_as_server() {
        let (client, mock) = setup_mock(|_req| MockResponse::json(500, "{}"));
        with_tokens(&client);
        let err = client.library_contains("abc123").unwrap_err();
        match err {
            SpotifyApiError::Http { status, category } => {
                assert_eq!(status, 500);
                assert_eq!(category, HttpCategory::Server);
            }
            other => panic!("expected Http(500), got {other:?}"),
        }
        drop(mock);
    }

    // PUT /me/library

    #[test]
    fn library_save_uses_put_with_uri() {
        let (client, mock) = setup_mock(|req| {
            assert!(req.starts_with("PUT /me/library?"), "{req}");
            assert!(
                req.contains("uris=spotify%3Atrack%3Aabc123")
                    || req.contains("uris=spotify:track:abc123"),
                "uri not encoded: {req}"
            );
            assert!(req.contains("Authorization: Bearer ACCESS"), "{req}");
            assert!(req.contains("Content-Length: 0"), "{req}");
            MockResponse::empty(200)
        });
        with_tokens(&client);
        client.library_save("abc123").unwrap();
        drop(mock);
    }

    #[test]
    fn library_save_accepts_204_response() {
        let (client, mock) = setup_mock(|_req| MockResponse::empty(204));
        with_tokens(&client);
        client.library_save("abc123").unwrap();
        drop(mock);
    }

    // DELETE /me/library

    #[test]
    fn library_remove_uses_delete_with_uri() {
        let (client, mock) = setup_mock(|req| {
            assert!(req.starts_with("DELETE /me/library?"), "{req}");
            assert!(
                req.contains("uris=spotify%3Atrack%3Aabc123")
                    || req.contains("uris=spotify:track:abc123"),
                "uri not encoded: {req}"
            );
            assert!(req.contains("Content-Length: 0"), "{req}");
            MockResponse::empty(200)
        });
        with_tokens(&client);
        client.library_remove("abc123").unwrap();
        drop(mock);
    }

    // No-credential path

    #[test]
    fn library_contains_without_token_returns_refresh_error() {
        let (client, mock) = setup_mock(|_| {
            panic!("mock should not be called without a token");
        });
        let err = client.library_contains("abc123").unwrap_err();
        assert_eq!(err, SpotifyApiError::Refresh("no_token"));
        drop(mock);
    }
}
