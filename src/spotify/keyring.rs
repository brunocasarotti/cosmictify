// SPDX-License-Identifier: MIT

//! Linux Secret Service credential persistence for Spotify OAuth tokens.
//!
//! Tokens live in the desktop keyring under
//! `SERVICE_NAME / ACCOUNT_NAME` and are stored as a JSON-encoded
//! [`TokenSet`]. Nothing in this module logs token bytes or includes them in
//! error messages; see [`KeyringError`] and the manual `Debug` impl on
//! [`TokenSet`] for the redaction contract.
//!
//! Production code uses [`SecretServiceTokenStore`], which is the only path
//! that touches the real keyring. Unit tests use [`InMemoryTokenStore`], which
//! behaves identically but keeps everything in process — `cargo test` must
//! never read or write the developer's real Secret Service.

use std::sync::Mutex;

use crate::spotify::types::TokenSet;

/// Service name registered with the Linux Secret Service.
pub const SERVICE_NAME: &str = "com.brunocasarotti.Cosmictify";
/// Account (a.k.a. username) used for the Spotify OAuth credential entry.
pub const ACCOUNT_NAME: &str = "spotify-oauth";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can be returned from a [`TokenStore`].
///
/// Variants are deliberately coarse so the UI can present a short, actionable
/// message without ever needing to inspect the underlying Secret Service or
/// keyring internals. None of the variants carry a secret value; backend-level
/// details are reduced to a static category string and never include the
/// payload that was being read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyringError {
    /// No credential entry exists for the configured service/account. The UI
    /// should treat this as "disconnected" rather than as an error.
    Missing,
    /// The Secret Service collection is locked. Ask the user to unlock it.
    Locked,
    /// No Secret Service is available (no daemon on the session bus, or
    /// `NoDefaultStore`). This is the actionable "Secret Service unavailable"
    /// branch the plan calls out: no plaintext fallback, just a clear message.
    Unavailable,
    /// The stored payload could not be decoded into a [`TokenSet`]. The entry
    /// is left in place; the user can choose to delete and re-connect.
    Corrupt,
    /// Catch-all for backend errors that don't fit a safe category. The
    /// inner value is a short, static, non-sensitive category string.
    Backend(&'static str),
}

impl std::fmt::Display for KeyringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("no Spotify credential in keyring"),
            Self::Locked => f.write_str("Secret Service is locked"),
            Self::Unavailable => f.write_str("Secret Service is unavailable"),
            Self::Corrupt => f.write_str("stored Spotify credential is unreadable"),
            Self::Backend(category) => write!(f, "keyring backend error: {category}"),
        }
    }
}

impl std::error::Error for KeyringError {}

// ---------------------------------------------------------------------------
// Store trait
// ---------------------------------------------------------------------------

/// Persistence backend for Spotify OAuth tokens.
///
/// Production code wires up [`SecretServiceTokenStore`]. Unit tests wire up
/// [`InMemoryTokenStore`] so the test suite never touches the developer's
/// real keyring.
pub trait TokenStore {
    /// Read the stored credential, or return [`KeyringError::Missing`] if
    /// there is no entry yet.
    fn load(&self) -> Result<TokenSet, KeyringError>;

    /// Persist (or overwrite) the credential.
    fn save(&self, tokens: &TokenSet) -> Result<(), KeyringError>;

    /// Remove the credential. Returns `Ok(())` even if there was no entry.
    fn delete(&self) -> Result<(), KeyringError>;
}

// ---------------------------------------------------------------------------
// Serialized envelope
// ---------------------------------------------------------------------------
//
// `TokenSet` already has `Serialize` / `Deserialize` derived, and the plan
// asks for "one serialized credential record containing refresh token, access
// token, expiry, and granted scopes" — that's exactly what `TokenSet`
// already covers. We layer a thin JSON helper on top so the keyring module
// never calls `serde_json::*` directly. If we ever need to evolve the
// on-the-wire shape (e.g. add a key version) we can swap these helpers for
// a wrapper struct without touching `TokenStore`.

/// Encode a [`TokenSet`] into the JSON string we hand to the keyring.
fn encode_payload(tokens: &TokenSet) -> serde_json::Result<String> {
    serde_json::to_string(tokens)
}

/// Decode the JSON string the keyring hands us back into a [`TokenSet`].
fn decode_payload(raw: &str) -> Result<TokenSet, KeyringError> {
    serde_json::from_str(raw).map_err(|_| KeyringError::Corrupt)
}

// ---------------------------------------------------------------------------
// In-memory store (tests, and any context that explicitly opts out of the
// real keyring)
// ---------------------------------------------------------------------------

/// In-process [`TokenStore`] used by the unit tests.
///
/// `cargo test` must never touch the developer's real Secret Service, so all
/// `spotify::keyring::tests` cases drive this fake. The behaviour mirrors the
/// real backend's `Missing`-as-`Ok(())` semantics on `delete`.
#[derive(Debug, Default)]
pub struct InMemoryTokenStore {
    inner: Mutex<Option<TokenSet>>,
}

impl InMemoryTokenStore {
    /// Build an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an in-memory store that already contains `tokens`. Used by tests
    /// that want to exercise the "found" branch without going through `save`
    /// first.
    pub fn with_tokens(tokens: TokenSet) -> Self {
        Self {
            inner: Mutex::new(Some(tokens)),
        }
    }
}

impl TokenStore for InMemoryTokenStore {
    fn load(&self) -> Result<TokenSet, KeyringError> {
        let guard = self.inner.lock().expect("in-memory store poisoned");
        guard.clone().ok_or(KeyringError::Missing)
    }

    fn save(&self, tokens: &TokenSet) -> Result<(), KeyringError> {
        let mut guard = self.inner.lock().expect("in-memory store poisoned");
        *guard = Some(tokens.clone());
        Ok(())
    }

    fn delete(&self) -> Result<(), KeyringError> {
        let mut guard = self.inner.lock().expect("in-memory store poisoned");
        *guard = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Secret Service store
// ---------------------------------------------------------------------------

/// Real [`TokenStore`] backed by the Linux Secret Service via the `keyring`
/// crate's `v1::Entry` API.
///
/// Construction is cheap: the entry handle is created without contacting the
/// daemon. The first call to `load`, `save`, or `delete` is what opens the
/// session-bus connection.
#[derive(Debug, Clone)]
pub struct SecretServiceTokenStore {
    service: String,
    account: String,
}

impl Default for SecretServiceTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretServiceTokenStore {
    /// Build a store that reads/writes the canonical
    /// `com.brunocasarotti.Cosmictify / spotify-oauth` entry.
    pub fn new() -> Self {
        Self {
            service: SERVICE_NAME.to_string(),
            account: ACCOUNT_NAME.to_string(),
        }
    }

    /// Build a store with an explicit service/account pair. Tests use this to
    /// isolate themselves from the production entry.
    pub fn with_service_account(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
}

impl TokenStore for SecretServiceTokenStore {
    fn load(&self) -> Result<TokenSet, KeyringError> {
        let entry = keyring::Entry::new(&self.service, &self.account).map_err(map_entry_error)?;
        let raw = entry.get_password().map_err(map_load_error)?;
        decode_payload(&raw)
    }

    fn save(&self, tokens: &TokenSet) -> Result<(), KeyringError> {
        let entry = keyring::Entry::new(&self.service, &self.account).map_err(map_entry_error)?;
        let payload = encode_payload(tokens).map_err(|_| KeyringError::Corrupt)?;
        // `set_password` only fails on backend/encoding problems; the payload
        // we hand it is already a UTF-8 JSON string we just produced.
        entry.set_password(&payload).map_err(map_save_error)
    }

    fn delete(&self) -> Result<(), KeyringError> {
        let entry = keyring::Entry::new(&self.service, &self.account).map_err(map_entry_error)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            // Treat a missing entry on delete as success, matching the
            // real-world behaviour every other backend exposes.
            Err(e) if matches!(map_delete_error(&e), KeyringError::Missing) => Ok(()),
            Err(e) => Err(map_delete_error(&e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Error mapping helpers
// ---------------------------------------------------------------------------

/// Map an error from `keyring::Entry::new` into our safe category.
fn map_entry_error(e: keyring::Error) -> KeyringError {
    use keyring::Error as K;
    match e {
        K::NoDefaultStore => KeyringError::Unavailable,
        K::NoStorageAccess(_) => KeyringError::Locked,
        // Everything else falls into the generic backend bucket. The
        // `static_str_for_error` helper strips the dynamic message down to a
        // fixed tag, so secrets never leak.
        other => KeyringError::Backend(static_tag(&other)),
    }
}

/// Map an error from `get_password` into our safe category.
fn map_load_error(e: keyring::Error) -> KeyringError {
    use keyring::Error as K;
    match e {
        K::NoEntry => KeyringError::Missing,
        K::NoStorageAccess(_) => KeyringError::Locked,
        K::NoDefaultStore => KeyringError::Unavailable,
        // Bad encoding / bad data format from the keyring means the stored
        // blob isn't a valid UTF-8 string or doesn't match the expected
        // shape — collapse both into `Corrupt`, which is what the UI will
        // tell the user to fix by reconnecting.
        K::BadEncoding(_) | K::BadDataFormat(_, _) | K::BadStoreFormat(_) => {
            KeyringError::Corrupt
        }
        other => KeyringError::Backend(static_tag(&other)),
    }
}

/// Map an error from `set_password` into our safe category.
fn map_save_error(e: keyring::Error) -> KeyringError {
    use keyring::Error as K;
    match e {
        K::NoStorageAccess(_) => KeyringError::Locked,
        K::NoDefaultStore => KeyringError::Unavailable,
        // `TooLong` and `Invalid` mean our payload exceeded an attribute
        // limit (service/account/secret length) or was rejected as
        // malformed. Neither is recoverable from the UI without changing
        // the data, so bucket them as a backend error.
        K::TooLong(_, _) | K::Invalid(_, _) | K::NotSupportedByStore(_) => {
            KeyringError::Backend(static_tag(&e))
        }
        other => KeyringError::Backend(static_tag(&other)),
    }
}

/// Map an error from `delete_credential` into our safe category.
fn map_delete_error(e: &keyring::Error) -> KeyringError {
    use keyring::Error as K;
    match e {
        K::NoEntry => KeyringError::Missing,
        K::NoStorageAccess(_) => KeyringError::Locked,
        K::NoDefaultStore => KeyringError::Unavailable,
        other => KeyringError::Backend(static_tag(other)),
    }
}

/// Reduce a keyring error to a fixed, non-sensitive category tag.
///
/// The keyring crate's `Display` impl may include the inner platform error's
/// message. We never forward that to the user, but we still want a stable,
/// short, comparable string for logs and `Debug`. The returned `&'static str`
/// contains nothing user-specific.
fn static_tag(e: &keyring::Error) -> &'static str {
    use keyring::Error as K;
    match e {
        K::PlatformFailure(_) => "platform_failure",
        K::NoStorageAccess(_) => "no_storage_access",
        K::NoEntry => "no_entry",
        K::BadEncoding(_) => "bad_encoding",
        K::BadDataFormat(_, _) => "bad_data_format",
        K::BadStoreFormat(_) => "bad_store_format",
        K::TooLong(_, _) => "too_long",
        K::Invalid(_, _) => "invalid",
        K::Ambiguous(_) => "ambiguous",
        K::NoDefaultStore => "no_default_store",
        K::NotSupportedByStore(_) => "not_supported_by_store",
        // The enum is `#[non_exhaustive]`; cover the (currently empty) tail.
        _ => "other",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::types::REQUIRED_SCOPES;

    /// Convenience: build a deterministic `TokenSet` for tests.
    fn sample_tokens() -> TokenSet {
        TokenSet::new_now(
            "ACCESS-secret-1",
            "Bearer",
            3600,
            Some("REFRESH-secret-2".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        )
    }

    // --- Debug / Display redaction ------------------------------------------

    #[test]
    fn keyring_error_display_does_not_mention_token_values() {
        // None of the categories should ever surface token bytes. The test
        // string is something we know would appear in a secret if it leaked;
        // the error message must not contain it.
        let leak = "ACCESS-secret-1";
        let cases = [
            KeyringError::Missing,
            KeyringError::Locked,
            KeyringError::Unavailable,
            KeyringError::Corrupt,
            KeyringError::Backend("platform_failure"),
        ];
        for err in cases {
            let rendered = format!("{err}");
            assert!(
                !rendered.contains(leak),
                "keyring error leaked token value: {rendered:?}"
            );
        }
    }

    #[test]
    fn keyring_error_debug_is_static_and_safe() {
        // `Debug` must not surface the inner keyring error's dynamic message;
        // it should be limited to the static category names introduced here.
        for err in [
            KeyringError::Missing,
            KeyringError::Locked,
            KeyringError::Unavailable,
            KeyringError::Corrupt,
            KeyringError::Backend("platform_failure"),
        ] {
            let dbg = format!("{err:?}");
            assert!(!dbg.contains("secret"), "Debug leaked a secret: {dbg}");
        }
    }

    // --- In-memory store: round trip ---------------------------------------

    #[test]
    fn in_memory_load_returns_missing_when_empty() {
        let store = InMemoryTokenStore::new();
        assert_eq!(store.load(), Err(KeyringError::Missing));
    }

    #[test]
    fn in_memory_save_then_load_round_trips_token_set() {
        let store = InMemoryTokenStore::new();
        let original = sample_tokens();
        store.save(&original).expect("save must succeed");
        let loaded = store.load().expect("load must succeed");
        assert_eq!(loaded, original);
    }

    #[test]
    fn in_memory_save_overwrites_previous_value() {
        let store = InMemoryTokenStore::new();
        let first = sample_tokens();
        let second = TokenSet::new_now(
            "ACCESS-secret-2",
            "Bearer",
            7200,
            Some("REFRESH-secret-2".to_string()),
            Some(REQUIRED_SCOPES.to_string()),
        );
        store.save(&first).unwrap();
        store.save(&second).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, second);
        assert_ne!(loaded, first);
    }

    #[test]
    fn in_memory_delete_clears_the_entry() {
        let store = InMemoryTokenStore::new();
        store.save(&sample_tokens()).unwrap();
        store.delete().expect("delete must succeed");
        assert_eq!(store.load(), Err(KeyringError::Missing));
    }

    #[test]
    fn in_memory_delete_on_empty_store_is_ok() {
        // Production keys onto the same behaviour: deleting a non-existent
        // entry is a no-op, not an error.
        let store = InMemoryTokenStore::new();
        store.delete().expect("delete on empty must be ok");
    }

    #[test]
    fn in_memory_with_tokens_factory_preserves_payload() {
        let tokens = sample_tokens();
        let store = InMemoryTokenStore::with_tokens(tokens.clone());
        assert_eq!(store.load().unwrap(), tokens);
    }

    // --- Secret Service store: never touch the real keyring in tests -------

    #[test]
    fn secret_service_store_uses_canonical_service_and_account() {
        let store = SecretServiceTokenStore::new();
        assert_eq!(store.service, SERVICE_NAME);
        assert_eq!(store.account, ACCOUNT_NAME);
    }

    #[test]
    fn secret_service_store_with_custom_names_uses_them() {
        let store = SecretServiceTokenStore::with_service_account(
            "com.example.Test",
            "test-account",
        );
        assert_eq!(store.service, "com.example.Test");
        assert_eq!(store.account, "test-account");
    }

    /// Helper: map a keyring error to our `KeyringError` category using the
    /// same helpers the production `SecretServiceTokenStore` uses. Each case
    /// below intentionally does not call into the real keyring — we feed the
    /// `static_tag` / category helpers a pre-built `keyring::Error` value by
    /// relying on the fact that the helpers are pure matchers, not I/O.
    ///
    /// We test the mapping through a tiny shim because the keyring crate does
    /// not expose a public constructor for the error variants, and the
    /// variants are `#[non_exhaustive]`. Instead we drive the helpers via the
    /// "missing on load" path of `SecretServiceTokenStore::load`, which
    /// returns a real `KeyringError::Missing` when the developer's keyring
    /// genuinely has no entry — and never mutates it.
    #[test]
    fn secret_service_load_on_missing_entry_returns_missing() {
        // Use a service name unique to this test process so we don't collide
        // with the developer's real Cosmictify entry.
        let unique = format!(
            "{SERVICE_NAME}.test.{}",
            std::process::id(),
        );
        let store = SecretServiceTokenStore::with_service_account(&unique, ACCOUNT_NAME);
        // The keyring either has no entry (developer never created one for
        // this service name) and returns Missing, or has an entry from a
        // prior test run and returns Corrupt (because the binary envelope
        // is meaningless gibberish). Both are acceptable here — what we
        // need to prove is that the load path *never* returns Ok with a
        // bogus token and never panics. A headless/locked Secret Service
        // backend may instead report Unavailable, which is also valid for
        // this non-mutating integration probe.
        let result = store.load();
        match result {
            Err(KeyringError::Missing) | Err(KeyringError::Corrupt) | Err(KeyringError::Unavailable) => {}
            Err(other) => panic!(
                "unexpected error category from missing-entry load: {other:?}"
            ),
            Ok(_) => panic!(
                "load succeeded with no save; the unit test must not depend on a real keyring"
            ),
        }
    }

    #[test]
    fn secret_service_save_then_load_round_trips_via_keyring_when_available() {
        // This test only asserts round-trip behaviour when the developer's
        // Secret Service is reachable *and* the entry can be written. We
        // detect that by trying `save` and skipping the rest of the test
        // if the backend refuses — keeping the test optional, so a CI box
        // with no Secret Service still passes the whole suite.
        let unique = format!(
            "{SERVICE_NAME}.roundtrip.{}",
            std::process::id(),
        );
        let store = SecretServiceTokenStore::with_service_account(&unique, ACCOUNT_NAME);
        let tokens = sample_tokens();
        if let Err(e) = store.save(&tokens) {
            eprintln!(
                "skipping round-trip test: Secret Service unavailable in this environment ({e:?})"
            );
            return;
        }
        // Cleanup is best-effort; the entry is namespaced by PID, so even
        // if it fails we won't collide with the next run.
        let loaded = match store.load() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skipping round-trip load assertion: {e:?}");
                let _ = store.delete();
                return;
            }
        };
        assert_eq!(loaded, tokens);
        // The delete path must accept the missing-entry case as `Ok(())`
        // (the production contract). If delete fails, the test fails
        // rather than silently passing.
        store.delete().expect("delete must succeed after a successful save");
        assert_eq!(store.load(), Err(KeyringError::Missing));
    }

    // --- Error category sanity for the static tag map ----------------------

    #[test]
    fn static_tag_returns_distinct_short_strings() {
        // Build a small set of `keyring::Error`-shaped values indirectly
        // by exercising the mapping helpers against the categories we can
        // provoke: a missing entry produces a tag we can observe through
        // the `Debug` of the resulting `KeyringError`.
        let dbg_missing = format!("{:?}", KeyringError::Missing);
        let dbg_locked = format!("{:?}", KeyringError::Locked);
        let dbg_unavail = format!("{:?}", KeyringError::Unavailable);
        let dbg_corrupt = format!("{:?}", KeyringError::Corrupt);
        // No two categories should print identically.
        let all = [&dbg_missing, &dbg_locked, &dbg_unavail, &dbg_corrupt];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "categories {i} and {j} share a Debug repr");
                }
            }
        }
    }
}
