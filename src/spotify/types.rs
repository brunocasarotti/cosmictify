// SPDX-License-Identifier: MIT

//! Pure Spotify types: endpoints, scopes, PKCE verifier, auth code, OAuth state,
//! and `TokenSet`. Nothing in this file performs network, keyring, or UI work.

// Items in this module that are not yet exercised by Task 1 tests but are part
// of the agreed public surface for later tasks (Task 3 token exchange, Task 5
// UI integration). Re-evaluate after Task 5 lands.
#![allow(dead_code)]

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Spotify authorization endpoint.
pub const SPOTIFY_AUTH_URL: &str = "https://accounts.spotify.com/authorize";
/// Spotify token endpoint.
pub const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// Loopback redirect URI registered in the user's Spotify Developer app.
pub const REDIRECT_URI: &str = "http://127.0.0.1:43821/callback";
/// Scopes Cosmictify needs to check and toggle the user's library.
pub const REQUIRED_SCOPES: &str = "user-library-read user-library-modify";
/// Refresh the access token this far before its reported expiry.
pub const REFRESH_SAFETY_MARGIN: Duration = Duration::from_secs(60);
/// Length of a Spotify Client ID (32 lowercase hex chars).
pub const CLIENT_ID_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Client ID validation
// ---------------------------------------------------------------------------

/// Reasons a candidate Client ID string may be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientIdError {
    /// Wrong number of characters (expected exactly 32).
    InvalidLength,
    /// Length is fine but a character is not `[0-9a-f]`.
    InvalidFormat,
}

/// Validate a Spotify Client ID. Returns the trimmed slice on success.
pub fn validate_client_id(s: &str) -> Result<&str, ClientIdError> {
    if s.len() != CLIENT_ID_LEN {
        return Err(ClientIdError::InvalidLength);
    }
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ClientIdError::InvalidFormat);
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// PKCE verifier (RFC 7636 §4.1)
// ---------------------------------------------------------------------------

/// Reasons a PKCE verifier may be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkceError {
    /// Length is outside the allowed `[43, 128]` byte range.
    InvalidLength,
    /// A byte is outside the RFC 7636 unreserved-character alphabet.
    InvalidAlphabet,
}

/// PKCE code verifier (RFC 7636 §4.1).
///
/// The verifier is a high-entropy string of unreserved characters
/// `[A-Z][a-z][0-9]-._~` between 43 and 128 bytes. Spotify's Authorization
/// Code flow must be paired with a matching `code_challenge` derived from it.
#[derive(Clone, Debug)]
pub struct PkceVerifier(String);

impl PkceVerifier {
    /// Build a verifier from a candidate string. Validates alphabet and length.
    pub fn new(s: &str) -> Result<Self, PkceError> {
        let len = s.len();
        if !(43..=128).contains(&len) {
            return Err(PkceError::InvalidLength);
        }
        if !s.bytes().all(is_unreserved) {
            return Err(PkceError::InvalidAlphabet);
        }
        Ok(Self(s.to_string()))
    }

    /// Generate a fresh 64-byte verifier using the OS RNG.
    ///
    /// 64 bytes of entropy base64url-encoded (no padding) produces an
    /// 86-character verifier — comfortably inside the `[43, 128]` RFC range.
    pub fn generate() -> Self {
        let mut buf = [0u8; 64];
        OsRng.fill_bytes(&mut buf);
        let encoded = URL_SAFE_NO_PAD.encode(buf);
        // The encoded form is always 86 chars of the unreserved alphabet.
        Self(encoded)
    }

    /// Compute the S256 challenge (RFC 7636 §4.2).
    ///
    /// Returns `BASE64URL(SHA256(verifier))` without padding.
    pub fn challenge_s256(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }

    /// Raw verifier string. Send this to Spotify in the token exchange.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

// ---------------------------------------------------------------------------
// Authorization code
// ---------------------------------------------------------------------------

/// Reasons an authorization code may be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthCodeError {
    /// Empty input.
    Empty,
    /// Input is implausibly long; Spotify never returns codes this big.
    TooLong,
    /// Input contains a byte outside the visible ASCII printable range.
    InvalidEncoding,
}

/// Spotify authorization code returned on the callback URL.
#[derive(Clone, Debug)]
pub struct AuthCode(String);

impl AuthCode {
    /// Build an auth code from a candidate string.
    pub fn new(s: &str) -> Result<Self, AuthCodeError> {
        if s.is_empty() {
            return Err(AuthCodeError::Empty);
        }
        if s.len() > 512 {
            return Err(AuthCodeError::TooLong);
        }
        if !s.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(AuthCodeError::InvalidEncoding);
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// OAuth state (RFC 6749 §10.12)
// ---------------------------------------------------------------------------

/// Reasons an OAuth state value may be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthStateError {
    /// Length is outside `[8, 128]`.
    InvalidLength,
    /// A byte is outside the unreserved-character alphabet.
    InvalidAlphabet,
}

/// Opaque state value used to bind the callback to the originating request.
#[derive(Clone, Debug)]
pub struct OAuthState(String);

impl OAuthState {
    /// Build a state from a candidate string. Validates length and alphabet.
    pub fn new(s: &str) -> Result<Self, OAuthStateError> {
        if !(8..=128).contains(&s.len()) {
            return Err(OAuthStateError::InvalidLength);
        }
        if !s.bytes().all(is_unreserved) {
            return Err(OAuthStateError::InvalidAlphabet);
        }
        Ok(Self(s.to_string()))
    }

    /// Generate a fresh state using the OS RNG.
    pub fn generate() -> Self {
        let mut buf = [0u8; 16];
        OsRng.fill_bytes(&mut buf);
        let encoded = URL_SAFE_NO_PAD.encode(buf);
        // 22 chars of unreserved alphabet — well inside the [8, 128] window.
        Self(encoded)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Token set
// ---------------------------------------------------------------------------

/// Spotify token response (RFC 6749 §5.1 + Spotify extensions).
///
/// `refresh_token` is `Option` because subsequent refreshes do not always
/// return a new one — Spotify reuses the original refresh token until the user
/// revokes access.
///
/// `obtained_at` is filled in by the caller (with the local wall clock) when
/// the response is received. We do not store this on the wire, but it is
/// needed to compute when proactive refresh should fire.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub token_type: String,
    /// Lifetime of the access token in seconds.
    pub expires_in: u64,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default = "epoch")]
    pub obtained_at: SystemTime,
}

/// Marker that replaces token values when [`TokenSet`] is formatted for
/// debugging or logging. This is the centerpiece of the "no token values
/// in Debug/errors" contract documented in the approved plan.
#[derive(Debug)]
struct Redacted(&'static str);

impl fmt::Display for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never serialize access_token or refresh_token. We still want the
        // shape of the struct and the non-sensitive metadata (token type,
        // expires_in, scope, obtained_at) to be visible in logs.
        f.debug_struct("TokenSet")
            .field("access_token", &Redacted("<redacted>"))
            .field("token_type", &self.token_type)
            .field("expires_in", &self.expires_in)
            .field("refresh_token", &Redacted("<redacted>"))
            .field("scope", &self.scope)
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

impl TokenSet {
    /// Build a token set with `obtained_at` set to the current wall clock.
    pub fn new_now(
        access_token: impl Into<String>,
        token_type: impl Into<String>,
        expires_in: u64,
        refresh_token: Option<String>,
        scope: Option<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            token_type: token_type.into(),
            expires_in,
            refresh_token,
            scope,
            obtained_at: SystemTime::now(),
        }
    }

    /// Wall-clock instant when the access token expires.
    pub fn expires_at(&self) -> SystemTime {
        self.obtained_at + Duration::from_secs(self.expires_in)
    }

    /// Wall-clock instant at which a refresh should be triggered.
    ///
    /// Always strictly before `expires_at()` so a token request can complete
    /// without the caller racing the expiry boundary.
    pub fn refresh_due_at(&self) -> SystemTime {
        self.expires_at()
            .checked_sub(REFRESH_SAFETY_MARGIN)
            .unwrap_or(self.obtained_at)
    }

    /// True when a refresh token is present and the token can be refreshed.
    pub fn has_refresh_token(&self) -> bool {
        self.refresh_token
            .as_ref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }
}

fn epoch() -> SystemTime {
    UNIX_EPOCH
}

// ---------------------------------------------------------------------------
// Authorize URL parameters
// ---------------------------------------------------------------------------

/// Builder for the parameters that go into `build_authorize_url`.
#[derive(Clone, Debug)]
pub struct AuthorizeUrlParams {
    pub client_id: String,
    pub state: OAuthState,
    pub code_challenge: String,
    pub scopes: String,
    pub redirect_uri: String,
}

impl AuthorizeUrlParams {
    /// Begin building parameters with sensible defaults for the loopback flow.
    pub fn builder() -> AuthorizeUrlParamsBuilder {
        AuthorizeUrlParamsBuilder::default()
    }
}

/// Convenience builder for `AuthorizeUrlParams`.
#[derive(Default)]
pub struct AuthorizeUrlParamsBuilder {
    client_id: Option<String>,
    state: Option<OAuthState>,
    code_challenge: Option<String>,
    scopes: Option<String>,
    redirect_uri: Option<String>,
}

impl AuthorizeUrlParamsBuilder {
    pub fn client_id(mut self, v: impl Into<String>) -> Self {
        self.client_id = Some(v.into());
        self
    }
    pub fn state(mut self, v: OAuthState) -> Self {
        self.state = Some(v);
        self
    }
    pub fn code_challenge(mut self, v: impl Into<String>) -> Self {
        self.code_challenge = Some(v.into());
        self
    }
    /// Override the default scopes. Default is [`REQUIRED_SCOPES`].
    pub fn scopes(mut self, v: impl Into<String>) -> Self {
        self.scopes = Some(v.into());
        self
    }
    /// Override the default redirect URI. Default is [`REDIRECT_URI`].
    pub fn redirect_uri(mut self, v: impl Into<String>) -> Self {
        self.redirect_uri = Some(v.into());
        self
    }
    pub fn build(self) -> AuthorizeUrlParams {
        AuthorizeUrlParams {
            client_id: self
                .client_id
                .expect("AuthorizeUrlParamsBuilder: client_id required"),
            state: self
                .state
                .expect("AuthorizeUrlParamsBuilder: state required"),
            code_challenge: self
                .code_challenge
                .expect("AuthorizeUrlParamsBuilder: code_challenge required"),
            scopes: self.scopes.unwrap_or_else(|| REQUIRED_SCOPES.to_string()),
            redirect_uri: self
                .redirect_uri
                .unwrap_or_else(|| REDIRECT_URI.to_string()),
        }
    }
}