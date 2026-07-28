// SPDX-License-Identifier: MIT

use std::time::Duration;

/// Playback state mirrored from MPRIS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl PlaybackStatus {
    pub fn is_playing(self) -> bool {
        matches!(self, Self::Playing)
    }
}

/// Snapshot of the current Spotify/MPRIS track for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSnapshot {
    pub connected: bool,
    pub identity: String,
    pub bus_name: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub status: PlaybackStatus,
    pub art_url: Option<String>,
    /// Track id for Spotify Web API (`3bym…`), if parseable.
    pub track_id: Option<String>,
    /// Open URL (xesam:url).
    pub url: Option<String>,
    pub length: Option<Duration>,
    pub position: Duration,
    /// 0.0 ..= 1.0
    pub volume: f64,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
    pub shuffle: bool,
}

impl Default for TrackSnapshot {
    fn default() -> Self {
        Self {
            connected: false,
            identity: String::new(),
            bus_name: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            status: PlaybackStatus::Stopped,
            art_url: None,
            track_id: None,
            url: None,
            length: None,
            position: Duration::ZERO,
            volume: 1.0,
            can_go_next: false,
            can_go_previous: false,
            can_play: false,
            can_pause: false,
            can_seek: false,
            shuffle: false,
        }
    }
}

impl TrackSnapshot {
    /// Progress fraction 0.0 ..= 1.0, or 0 if unknown length.
    #[allow(dead_code)]
    pub fn progress_fraction(&self) -> f64 {
        let Some(len) = self.length.filter(|d| !d.is_zero()) else {
            return 0.0;
        };
        (self.position.as_secs_f64() / len.as_secs_f64()).clamp(0.0, 1.0)
    }

    #[allow(dead_code)]
    pub fn position_label(&self) -> String {
        format_duration(self.position)
    }

    pub fn length_label(&self) -> String {
        self.length
            .map(format_duration)
            .unwrap_or_else(|| "--:--".into())
    }

    pub fn display_line(&self) -> String {
        if !self.connected {
            return String::new();
        }
        if self.title.is_empty() {
            return self.identity.clone();
        }
        if self.artist.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.title, self.artist)
        }
    }
}

pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let m = total / 60;
    let s = total % 60;
    format!("{m}:{s:02}")
}

/// Extract a Spotify track id from open.spotify URL, `spotify:track:` URI, or MPRIS path.
pub fn track_id_from_url_or_path(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(rest) = s.strip_prefix("spotify:track:") {
        let id = rest.split(['?', '#']).next().unwrap_or(rest);
        return nonempty_id(id);
    }

    if let Some(idx) = s.find("open.spotify.com/track/") {
        let rest = &s[idx + "open.spotify.com/track/".len()..];
        let id = rest.split(['?', '#', '/']).next().unwrap_or(rest);
        return nonempty_id(id);
    }

    // /com/spotify/track/<id> or similar
    if let Some(idx) = s.rfind("/track/") {
        let rest = &s[idx + "/track/".len()..];
        let id = rest.split(['?', '#', '/']).next().unwrap_or(rest);
        return nonempty_id(id);
    }

    None
}

fn nonempty_id(id: &str) -> Option<String> {
    let id = id.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}
