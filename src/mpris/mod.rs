// SPDX-License-Identifier: MIT

//! MPRIS client focused on Spotify (with fallback to any active player).

mod client;
mod types;

pub use client::{
    apply_command, fetch_snapshot, MprisCommand, MprisFailure, MprisFailureKind, MprisPollOutcome,
};
pub use types::{format_duration, TrackSnapshot};

#[cfg(test)]
mod tests {
    use super::types::track_id_from_url_or_path;

    #[test]
    fn parses_open_spotify_url() {
        assert_eq!(
            track_id_from_url_or_path("https://open.spotify.com/track/3bymDcsSDlZo6WkhFpkVAx"),
            Some("3bymDcsSDlZo6WkhFpkVAx".into())
        );
    }

    #[test]
    fn parses_spotify_uri() {
        assert_eq!(
            track_id_from_url_or_path("spotify:track:3bymDcsSDlZo6WkhFpkVAx"),
            Some("3bymDcsSDlZo6WkhFpkVAx".into())
        );
    }

    #[test]
    fn parses_mpris_track_path() {
        assert_eq!(
            track_id_from_url_or_path("/com/spotify/track/3bymDcsSDlZo6WkhFpkVAx"),
            Some("3bymDcsSDlZo6WkhFpkVAx".into())
        );
    }

    #[test]
    fn fetch_snapshot_when_spotify_running() {
        let super::MprisPollOutcome::Connected(snap) = super::fetch_snapshot() else {
            // Soft check: Spotify and the session bus are optional in tests.
            return;
        };
        // Soft check: if Spotify isn't up, connected=false is OK.
        assert!(snap.connected);
        assert!(
            snap.bus_name.to_ascii_lowercase().contains("spotify")
                || snap.identity.to_ascii_lowercase().contains("spotify"),
            "expected spotify player, got bus={} identity={}",
            snap.bus_name,
            snap.identity
        );
        assert!(!snap.title.is_empty() || !snap.artist.is_empty());
        assert!(snap.track_id.is_some(), "expected parseable track id");
    }
}
