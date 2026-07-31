// SPDX-License-Identifier: MIT

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};

#[derive(Debug, Clone, Default, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    pub demo: String,
    /// Optional Spotify Client ID from the user's personal Spotify Developer app.
    /// 32-character hex string. When empty, the Web API like button is unavailable.
    pub spotify_client_id: String,
}
