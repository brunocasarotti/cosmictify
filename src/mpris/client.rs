// SPDX-License-Identifier: MIT

use super::types::{track_id_from_url_or_path, PlaybackStatus, TrackSnapshot};
use mpris::{Player, PlayerFinder};
use std::time::Duration;

const PREFERRED_HINTS: &[&str] = &["spotify"];

/// Commands the UI can issue against the preferred MPRIS player.
#[derive(Debug, Clone)]
pub enum MprisCommand {
    PlayPause,
    Next,
    Previous,
    /// Absolute position 0.0 ..= 1.0 of track length.
    SeekFraction(f64),
    /// Volume 0.0 ..= 1.0
    SetVolume(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MprisFailureKind {
    Finder,
    Enumeration,
    Snapshot,
    Command,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MprisFailure {
    kind: MprisFailureKind,
}

impl MprisFailure {
    const fn new(kind: MprisFailureKind) -> Self {
        Self { kind }
    }

    pub fn worker() -> Self {
        Self::new(MprisFailureKind::Worker)
    }

    pub const fn kind(&self) -> MprisFailureKind {
        self.kind
    }
}

impl std::fmt::Display for MprisFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            MprisFailureKind::Finder => f.write_str("MPRIS connection failed"),
            MprisFailureKind::Enumeration => f.write_str("MPRIS player enumeration failed"),
            MprisFailureKind::Snapshot => f.write_str("MPRIS snapshot read failed"),
            MprisFailureKind::Command => f.write_str("MPRIS command failed"),
            MprisFailureKind::Worker => f.write_str("MPRIS worker failed"),
        }
    }
}

impl std::error::Error for MprisFailure {}

#[derive(Debug, Clone, PartialEq)]
pub enum MprisPollOutcome {
    Connected(Box<TrackSnapshot>),
    Unavailable,
    Failed(MprisFailure),
}

/// Poll current track snapshot. Spotify-only (this is Cosmictify).
pub fn fetch_snapshot() -> MprisPollOutcome {
    let finder = match PlayerFinder::new() {
        Ok(finder) => finder,
        Err(_) => {
            return MprisPollOutcome::Failed(MprisFailure::new(MprisFailureKind::Finder));
        }
    };

    let players = match finder.find_all() {
        Ok(players) => players,
        Err(_) => {
            return MprisPollOutcome::Failed(MprisFailure::new(MprisFailureKind::Enumeration));
        }
    };

    let Some(player) = pick_spotify(&players) else {
        return MprisPollOutcome::Unavailable;
    };

    match snapshot_from_player(player) {
        Ok(snapshot) => MprisPollOutcome::Connected(Box::new(snapshot)),
        Err(error) => MprisPollOutcome::Failed(error),
    }
}

pub fn apply_command(cmd: MprisCommand) -> Result<(), MprisFailure> {
    let finder = PlayerFinder::new().map_err(|_| MprisFailure::new(MprisFailureKind::Finder))?;
    let players = finder
        .find_all()
        .map_err(|_| MprisFailure::new(MprisFailureKind::Enumeration))?;
    let player =
        pick_spotify(&players).ok_or_else(|| MprisFailure::new(MprisFailureKind::Command))?;

    match cmd {
        MprisCommand::PlayPause => player
            .play_pause()
            .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?,
        MprisCommand::Next => player
            .next()
            .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?,
        MprisCommand::Previous => player
            .previous()
            .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?,
        MprisCommand::SetVolume(v) => {
            let v = v.clamp(0.0, 1.0);
            player
                .set_volume(v)
                .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?;
        }
        MprisCommand::SeekFraction(frac) => {
            let frac = frac.clamp(0.0, 1.0);
            let meta = player
                .get_metadata()
                .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?;
            let length = meta
                .length()
                .ok_or_else(|| MprisFailure::new(MprisFailureKind::Command))?;
            let target = Duration::from_secs_f64(length.as_secs_f64() * frac);
            if let Some(track_id) = meta.track_id() {
                if player.set_position(track_id, &target).is_err() {
                    let pos = player.get_position().unwrap_or(Duration::ZERO);
                    let delta = target.as_micros().saturating_sub(pos.as_micros()) as i64;
                    player
                        .seek(delta)
                        .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?;
                }
            } else {
                let pos = player.get_position().unwrap_or(Duration::ZERO);
                let delta = target.as_micros() as i64 - pos.as_micros() as i64;
                player
                    .seek(delta)
                    .map_err(|_| MprisFailure::new(MprisFailureKind::Command))?;
            }
        }
    }

    Ok(())
}

fn pick_spotify(players: &[Player]) -> Option<&Player> {
    players.iter().find(|p| {
        let bus = p.bus_name_trimmed().to_ascii_lowercase();
        let id = p.identity().to_ascii_lowercase();
        PREFERRED_HINTS
            .iter()
            .any(|hint| bus.contains(hint) || id.contains(hint))
    })
}

fn snapshot_from_player(player: &Player) -> Result<TrackSnapshot, MprisFailure> {
    let metadata = player
        .get_metadata()
        .map_err(|_| MprisFailure::new(MprisFailureKind::Snapshot))?;
    let status = match player.get_playback_status() {
        Ok(mpris::PlaybackStatus::Playing) => PlaybackStatus::Playing,
        Ok(mpris::PlaybackStatus::Paused) => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    };

    let title = metadata.title().map(sanitize_one_line).unwrap_or_default();
    let artist = metadata
        .artists()
        .map(|a| sanitize_one_line(&a.join(", ")))
        .unwrap_or_default();
    let album = metadata
        .album_name()
        .map(sanitize_one_line)
        .unwrap_or_default();
    let art_url = metadata.art_url().map(|s| s.to_string());
    let url = metadata.url().map(|s| s.to_string());

    let track_id = url
        .as_deref()
        .and_then(track_id_from_url_or_path)
        .or_else(|| {
            metadata
                .track_id()
                .map(|id| id.to_string())
                .and_then(|s| track_id_from_url_or_path(&s))
        });

    let length = metadata.length();
    let position = player.get_position().unwrap_or(Duration::ZERO);
    let volume = player.get_volume().unwrap_or(1.0).clamp(0.0, 1.0);

    let can_go_next = player.can_go_next().unwrap_or(false);
    let can_go_previous = player.can_go_previous().unwrap_or(false);
    let can_play = player.can_play().unwrap_or(false);
    let can_pause = player.can_pause().unwrap_or(false);
    let can_seek = player.can_seek().unwrap_or(false);
    let shuffle = player.get_shuffle().unwrap_or(false);

    Ok(TrackSnapshot {
        connected: true,
        identity: player.identity().to_string(),
        bus_name: player.bus_name_trimmed().to_string(),
        title,
        artist,
        album,
        status,
        art_url,
        track_id,
        url,
        length,
        position,
        volume,
        can_go_next,
        can_go_previous,
        can_play,
        can_pause,
        can_seek,
        shuffle,
    })
}

/// Collapse whitespace/newlines so panel text never wraps vertically.
fn sanitize_one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{MprisFailure, MprisFailureKind};

    #[test]
    fn failure_display_contains_only_allowlisted_text() {
        let failure = MprisFailure::new(MprisFailureKind::Command);

        assert_eq!(failure.to_string(), "MPRIS command failed");
    }
}
