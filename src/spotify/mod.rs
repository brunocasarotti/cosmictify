// SPDX-License-Identifier: MIT

//! Spotify OAuth and Web API integration.
//!
//! Task 1: pure types and PKCE foundation.
//! Task 2: Linux Secret Service credential persistence.
//! Task 3: loopback listener + token exchange/refresh + unified library API.
//! No UI effects live in this module yet.

mod client;
mod keyring;
mod library;
mod oauth;
mod types;

// Re-exports form the agreed public Spotify API surface used by Task 5 (UI
// integration). They are exercised by the unit tests but the release build
// sees them as unused.
#[allow(unused_imports, dead_code)]
pub use oauth::{build_authorize_url, generate_pkce_verifier, parse_callback, CallbackError};
#[allow(unused_imports, dead_code)]
pub use oauth::{
    callback_response_html, parse_http_request, serve_one_callback, CallbackRequest,
    LoopbackError, LoopbackListener, DEFAULT_LOOPBACK_ADDR,
};
#[allow(unused_imports, dead_code)]
pub use types::{
    validate_client_id, AuthCode, AuthCodeError, AuthorizeUrlParams, AuthorizeUrlParamsBuilder,
    ClientIdError, OAuthState, OAuthStateError, PkceError, PkceVerifier, TokenSet, REDIRECT_URI,
    REFRESH_SAFETY_MARGIN, REQUIRED_SCOPES, SPOTIFY_AUTH_URL, SPOTIFY_TOKEN_URL,
};
#[allow(unused_imports, dead_code)]
pub use keyring::{
    InMemoryTokenStore, KeyringError, SecretServiceTokenStore, TokenStore, ACCOUNT_NAME,
    SERVICE_NAME,
};
#[allow(unused_imports, dead_code)]
pub use client::{
    parse_token_response, HttpCategory, SpotifyApiError, SpotifyClient, DEFAULT_API_BASE,
};
#[allow(unused_imports, dead_code)]
pub use library::{
    build_track_uri, parse_contains, validate_track_id, ContainsError, TrackIdError,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // --- RFC 7636 §4.1: PKCE verifier alphabet/length -------------------------

    #[test]
    fn generated_verifier_uses_unreserved_alphabet_and_valid_length() {
        for _ in 0..32 {
            let v = generate_pkce_verifier();
            let s = v.as_str();
            assert!(
                s.len() >= 43 && s.len() <= 128,
                "verifier length out of range: {}",
                s.len()
            );
            for b in s.bytes() {
                let unreserved = b.is_ascii_alphanumeric()
                    || matches!(b, b'-' | b'.' | b'_' | b'~');
                assert!(unreserved, "verifier byte {:#x} not in RFC 7636 alphabet", b);
            }
        }
    }

    #[test]
    fn pkce_verifier_new_rejects_short_input() {
        assert!(matches!(
            PkceVerifier::new("too-short"),
            Err(PkceError::InvalidLength)
        ));
    }

    #[test]
    fn pkce_verifier_new_rejects_long_input() {
        let s = "a".repeat(129);
        assert!(matches!(
            PkceVerifier::new(&s),
            Err(PkceError::InvalidLength)
        ));
    }

    #[test]
    fn pkce_verifier_new_rejects_disallowed_alphabet() {
        // length OK, but contains space and '+'
        let s = "abcd abcd+abcd abcd abcd abcd abcd abcd abcd";
        assert!(matches!(
            PkceVerifier::new(s),
            Err(PkceError::InvalidAlphabet)
        ));
    }

    #[test]
    fn pkce_verifier_new_accepts_min_max_length() {
        assert!(PkceVerifier::new(&"a".repeat(43)).is_ok());
        assert!(PkceVerifier::new(&"a".repeat(128)).is_ok());
    }

    // --- RFC 7636 Appendix B: published S256 test vector ----------------------

    #[test]
    fn s256_challenge_matches_rfc7636_appendix_b() {
        // From RFC 7636 Appendix B.
        let verifier = PkceVerifier::new(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        )
        .expect("verifier must validate");
        assert_eq!(
            verifier.challenge_s256(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    // --- Authorization URL ---------------------------------------------------

    #[test]
    fn authorize_url_contains_required_components() {
        let params = AuthorizeUrlParams::builder()
            .client_id("e117d2b248334356b28cdf56be6eba18")
            .state(OAuthState::new("abcdefghijklmnop").unwrap())
            .code_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
            .build();
        let url = build_authorize_url(params);
        assert!(
            url.starts_with(SPOTIFY_AUTH_URL),
            "url must start with auth endpoint, got {url}"
        );

        let parsed: std::collections::HashMap<String, String> =
            url::Url::parse(&url)
                .expect("url must be parseable")
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();

        assert_eq!(
            parsed.get("client_id").map(String::as_str),
            Some("e117d2b248334356b28cdf56be6eba18")
        );
        assert_eq!(parsed.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(parsed.get("redirect_uri").map(String::as_str), Some(REDIRECT_URI));
        assert_eq!(
            parsed.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            parsed.get("code_challenge").map(String::as_str),
            Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        );
        assert_eq!(parsed.get("state").map(String::as_str), Some("abcdefghijklmnop"));
        assert_eq!(
            parsed.get("scope").map(String::as_str),
            Some(REQUIRED_SCOPES)
        );
    }

    // --- Callback parser -----------------------------------------------------

    #[test]
    fn callback_parser_accepts_matching_state_and_code() {
        let query = "code=AuthCodeXYZ&state=opaque-state";
        let parsed = parse_callback(query, "opaque-state").expect("must accept");
        assert_eq!(parsed.as_str(), "AuthCodeXYZ");
    }

    #[test]
    fn callback_parser_rejects_state_mismatch() {
        let query = "code=AuthCodeXYZ&state=other";
        assert!(matches!(
            parse_callback(query, "expected"),
            Err(CallbackError::StateMismatch)
        ));
    }

    #[test]
    fn callback_parser_rejects_missing_state() {
        let query = "code=AuthCodeXYZ";
        assert!(matches!(
            parse_callback(query, "expected"),
            Err(CallbackError::Malformed)
        ));
    }

    #[test]
    fn callback_parser_rejects_denied() {
        let query = "error=access_denied&state=expected";
        assert!(matches!(
            parse_callback(query, "expected"),
            Err(CallbackError::Denied)
        ));
    }

    #[test]
    fn callback_parser_rejects_missing_code() {
        let query = "state=expected";
        assert!(matches!(
            parse_callback(query, "expected"),
            Err(CallbackError::MissingCode)
        ));
    }

    #[test]
    fn callback_parser_rejects_malformed_query() {
        // Empty query is not a valid callback.
        assert!(matches!(
            parse_callback("", "expected"),
            Err(CallbackError::Malformed)
        ));
        // Both error and code is contradictory: error wins → Denied.
        let query = "error=access_denied&state=expected&code=xyz";
        assert!(matches!(
            parse_callback(query, "expected"),
            Err(CallbackError::Denied)
        ));
    }

    // --- Client ID validation ------------------------------------------------

    #[test]
    fn validate_client_id_accepts_thirty_two_hex_chars() {
        assert!(validate_client_id("e117d2b248334356b28cdf56be6eba18").is_ok());
        assert!(validate_client_id("00000000000000000000000000000000").is_ok());
        assert!(validate_client_id("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").is_ok());
    }

    #[test]
    fn validate_client_id_rejects_wrong_length() {
        assert!(matches!(
            validate_client_id("abc123"),
            Err(ClientIdError::InvalidLength)
        ));
        assert!(matches!(
            validate_client_id(&"a".repeat(33)),
            Err(ClientIdError::InvalidLength)
        ));
    }

    #[test]
    fn validate_client_id_rejects_non_hex() {
        assert!(matches!(
            validate_client_id("g117d2b248334356b28cdf56be6eba18"),
            Err(ClientIdError::InvalidFormat)
        ));
    }

    // --- Token expiry safety margin ------------------------------------------

    #[test]
    fn token_refresh_due_at_subtracts_safety_margin() {
        let obtained = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let tokens = TokenSet {
            access_token: "a".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
            refresh_token: Some("r".into()),
            scope: Some(REQUIRED_SCOPES.into()),
            obtained_at: obtained,
        };
        let due = tokens.refresh_due_at();
        let expected = obtained + Duration::from_secs(3600) - REFRESH_SAFETY_MARGIN;
        assert_eq!(due, expected);
    }

    #[test]
    fn token_refresh_due_is_strictly_before_expiry() {
        let tokens = TokenSet {
            access_token: "a".into(),
            token_type: "Bearer".into(),
            expires_in: 60,
            refresh_token: Some("r".into()),
            scope: None,
            obtained_at: SystemTime::now(),
        };
        let due = tokens.refresh_due_at();
        let expires = tokens.expires_at();
        assert!(
            due < expires,
            "refresh-due must precede expiry (due={:?}, expires={:?})",
            due,
            expires
        );
    }

    #[test]
    fn token_set_debug_redacts_access_and_refresh_tokens() {
        // Use distinctive token strings that the assertion can search for.
        let tokens = TokenSet {
            access_token: "ACCESS-secret-needle".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
            refresh_token: Some("REFRESH-secret-needle".into()),
            scope: Some(REQUIRED_SCOPES.into()),
            obtained_at: UNIX_EPOCH,
        };
        let dbg = format!("{tokens:?}");

        assert!(
            !dbg.contains("ACCESS-secret-needle"),
            "access_token leaked in Debug: {dbg}"
        );
        assert!(
            !dbg.contains("REFRESH-secret-needle"),
            "refresh_token leaked in Debug: {dbg}"
        );
        // The redaction marker must be present so log consumers know the
        // field exists.
        assert!(
            dbg.contains("<redacted>"),
            "Debug should mark redacted fields: {dbg}"
        );
        // Non-sensitive fields must still be visible.
        assert!(dbg.contains("Bearer"));
        assert!(dbg.contains("3600"));
    }
}