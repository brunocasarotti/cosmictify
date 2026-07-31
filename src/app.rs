// SPDX-License-Identifier: MIT

use crate::config::Config;
use crate::marquee::Marquee;
use crate::mpris::{
    self, format_duration, MprisCommand, MprisFailure, MprisFailureKind, MprisPollOutcome,
    TrackSnapshot,
};
use crate::spotify::{
    self, KeyringError, LoopbackError, OAuthState, SecretServiceTokenStore, SpotifyApiError,
    SpotifyClient, TokenStore,
};
use crate::{art, fl};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::mouse::ScrollDelta;
use cosmic::iced::platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::widget::image::Handle;
use cosmic::iced::widget::text::Wrapping;
use cosmic::iced::window::Id;
use cosmic::iced::{self, event, time, window, Length, Limits, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, space};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(750);
/// Fast tick for smooth marquee + progress bar.
const TICK_INTERVAL: Duration = Duration::from_millis(33);
const POPUP_ART: u16 = 128;
const SPOTIFY_GREEN: [f32; 4] = [0.114, 0.725, 0.329, 1.0];
const PANEL_PROGRESS_WIDTH: f32 = crate::marquee::VIEWPORT_WIDTH;

#[derive(Debug, Clone, PartialEq)]
struct PanelDiagnosticSnapshot {
    output_name: String,
    panel_type: String,
    anchor: String,
    configured_size: String,
    suggested_bounds: Option<(f32, f32)>,
    scale_factor: f32,
    suggested_icon_size: (u16, u16),
    suggested_padding: (u16, u16),
}

impl PanelDiagnosticSnapshot {
    fn capture(core: &cosmic::Core) -> Self {
        Self {
            output_name: core.applet.output_name.clone(),
            panel_type: core.applet.panel_type.to_string(),
            anchor: format!("{:?}", core.applet.anchor),
            configured_size: format!("{:?}", core.applet.size),
            suggested_bounds: core
                .applet
                .suggested_bounds
                .map(|bounds| (bounds.width, bounds.height)),
            scale_factor: core.scale_factor(),
            suggested_icon_size: core.applet.suggested_size(true),
            suggested_padding: core.applet.suggested_padding(true),
        }
    }

    #[cfg(test)]
    fn for_test(
        output_name: &str,
        scale_factor: f32,
        suggested_bounds: Option<(f32, f32)>,
    ) -> Self {
        Self {
            output_name: output_name.to_string(),
            panel_type: "Panel".to_string(),
            anchor: "Top".to_string(),
            configured_size: "PanelSize(S)".to_string(),
            suggested_bounds,
            scale_factor,
            suggested_icon_size: (16, 16),
            suggested_padding: (4, 4),
        }
    }
}

fn diagnostics_changed(
    previous: &PanelDiagnosticSnapshot,
    current: &PanelDiagnosticSnapshot,
) -> bool {
    previous != current
}

fn should_log_resize(previous: Option<(f32, f32)>, current: (f32, f32)) -> bool {
    previous != Some(current)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum WindowDiagnosticEvent {
    Opened { width: f32, height: f32 },
    Closed,
    Resized { width: f32, height: f32 },
    Rescaled(f32),
    CloseRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MprisDiagnosticState {
    Connected,
    Unavailable,
    Failed(MprisFailureKind),
}

fn mpris_transition_changed(
    previous: Option<MprisDiagnosticState>,
    current: MprisDiagnosticState,
) -> bool {
    previous != Some(current)
}

/// Authentication state with the Spotify Web API.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthState {
    /// No Client ID configured yet.
    Unconfigured,
    /// Client ID is set but no OAuth tokens have been exchanged.
    Disconnected,
    /// OAuth flow is in progress (browser open, waiting for callback).
    Connecting,
    /// Tokens are valid and the API is usable.
    Connected,
    /// Refresh failed — user needs to re-authorize.
    ReconnectRequired,
}

/// Like state for the current track.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LikeState {
    /// No track ID available, or user not authenticated.
    Unavailable,
    /// Checking the library for this track.
    Loading,
    /// Track is saved in the user's library.
    Saved,
    /// Track is not saved in the user's library.
    Unsaved,
    /// Save or remove is in progress.
    Mutating,
    /// The last operation failed.
    Error,
}

pub struct AppModel {
    core: cosmic::Core,
    /// Cosmic config handle for reading/writing app settings.
    cosmic_config: Option<cosmic_config::Config>,
    popup: Option<Id>,
    /// When true, the popup shows the expandable Spotify setup section.
    spotify_settings_open: bool,
    config: Config,
    track: TrackSnapshot,
    position_sampled_at: Instant,
    album_art: Option<Handle>,
    current_art_url: Option<String>,
    volume_override: Option<(f64, Instant)>,
    marquee: Marquee,
    frame: u64,
    spotify_auth: AuthState,
    spotify_like: LikeState,
    /// Safe, localized category for the last Spotify Web API error.
    spotify_error: Option<String>,
    spotify_like_track: Option<String>,
    spotify_client: Option<SpotifyClient<SecretServiceTokenStore>>,
    /// Buffer for the Client ID text input field.
    spotify_client_id_input: String,
    panel_diagnostics: Option<PanelDiagnosticSnapshot>,
    window_sizes: HashMap<Id, (f32, f32)>,
    mpris_diagnostic_state: Option<MprisDiagnosticState>,
}

impl Default for AppModel {
    fn default() -> Self {
        Self {
            core: cosmic::Core::default(),
            cosmic_config: None,
            popup: None,
            spotify_settings_open: false,
            config: Config::default(),
            track: TrackSnapshot::default(),
            position_sampled_at: Instant::now(),
            album_art: None,
            current_art_url: None,
            volume_override: None,
            marquee: Marquee::default(),
            frame: 0,
            spotify_auth: AuthState::Unconfigured,
            spotify_like: LikeState::Unavailable,
            spotify_error: None,
            spotify_like_track: None,
            spotify_client: None,
            spotify_client_id_input: String::new(),
            panel_diagnostics: None,
            window_sizes: HashMap::new(),
            mpris_diagnostic_state: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    /// Expand/collapse the Spotify setup section inside the popup.
    ToggleSpotifySettings,
    WindowDiagnostic {
        id: Id,
        event: WindowDiagnosticEvent,
    },
    UpdateConfig(Config),
    MprisUpdate(MprisPollOutcome),
    PollMpris,
    Tick,
    PlayPause,
    Next,
    Previous,
    Seek(f64),
    Volume(f64),
    OpenInSpotify,
    ArtLoaded {
        url: String,
        result: Result<Handle, art::ArtLoadError>,
    },
    CommandDone {
        command: MprisCommand,
        result: Result<(), MprisFailure>,
    },
    Scroll(ScrollDelta),
    /// Spotify Web API: begin OAuth PKCE flow (also persists Client ID from the field).
    SpotifyConnect,
    /// Spotify Web API: disconnect and clear credentials.
    SpotifyDisconnect,
    /// Spotify Web API: PKCE callback completed (authorization code or error).
    SpotifyCallbackDone(Result<(), SpotifyApiError>),
    /// Spotify Web API: toggle the like state for the current track.
    SpotifyLikeToggle,
    /// Spotify Web API: result of a library contains check for one track.
    SpotifyLikeCheckResult {
        track_id: String,
        result: Result<bool, SpotifyApiError>,
    },
    /// Spotify Web API: result of a library save/remove. `saved` is the
    /// confirmed target state, retained so we do not immediately overwrite a
    /// successful mutation with an eventually-consistent follow-up read.
    SpotifyLikeDone {
        track_id: String,
        saved: bool,
        result: Result<(), SpotifyApiError>,
    },
    /// Spotify Web API: text input for Client ID field.
    SpotifyClientIdInput(String),
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "com.brunocasarotti.Cosmictify";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let cosmic_cfg = match cosmic_config::Config::new(Self::APP_ID, Config::VERSION) {
            Ok(context) => Some(context),
            Err(_) => {
                tracing::warn!("cosmic-config context unavailable; using defaults");
                None
            }
        };
        let config = cosmic_cfg
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((errors, config)) => {
                    tracing::warn!(
                        error_count = errors.len(),
                        "cosmic-config contained invalid entries; using recovered values"
                    );
                    config
                }
            })
            .unwrap_or_default();

        let panel_diagnostics = PanelDiagnosticSnapshot::capture(&core);
        let mut app = AppModel {
            core,
            cosmic_config: cosmic_cfg,
            config: config.clone(),
            spotify_client_id_input: config.spotify_client_id.clone(),
            panel_diagnostics: Some(panel_diagnostics),
            ..Default::default()
        };
        app.init_spotify();
        app.log_applet_initialized();

        (
            app,
            Task::perform(poll_mpris(), |snap| {
                cosmic::Action::App(Message::MprisUpdate(snap))
            }),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let _ = self.frame;

        let inner = if self.track.connected {
            self.panel_playing()
        } else {
            self.panel_offline()
        };

        // Match official applets (e.g. time): force vertical size to icon+padding,
        // let autosize grow width only. Do NOT lock height with auto_height(false) —
        // that crushed the marquee on XS panels.
        let (icon_w, icon_h) = self.core.applet.suggested_size(true);
        let (_h_pad_sym, v_pad) = self.core.applet.suggested_padding(true);
        let (h_pad, _) = self.core.applet.suggested_padding(true);
        let force_h = f32::from(icon_h.saturating_add(v_pad.saturating_mul(2)));

        let body = widget::row::with_capacity(2)
            .align_y(Vertical::Center)
            .push(inner)
            .push(
                widget::container(space::vertical())
                    .height(Length::Fixed(force_h))
                    .width(Length::Fixed(0.0)),
            );

        let button = widget::button::custom(body)
            .padding([0, h_pad])
            .class(cosmic::theme::Button::AppletIcon);

        let interactive = widget::mouse_area(button)
            .on_press(Message::TogglePopup)
            .on_middle_press(Message::PlayPause)
            .on_scroll(Message::Scroll);

        // Panel already supplies height via suggested_bounds inside autosize_window.
        let _ = icon_w;
        self.core.applet.autosize_window(interactive).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        self.view_popup()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch(vec![
            time::every(POLL_INTERVAL).map(|_| Message::PollMpris),
            time::every(TICK_INTERVAL).map(|_| Message::Tick),
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
            event::listen_with(|event, _, id| {
                let iced::Event::Window(window_event) = event else {
                    return None;
                };
                let event = match window_event {
                    window::Event::Opened { size, .. } => WindowDiagnosticEvent::Opened {
                        width: size.width,
                        height: size.height,
                    },
                    window::Event::Closed => WindowDiagnosticEvent::Closed,
                    window::Event::Resized(size) => WindowDiagnosticEvent::Resized {
                        width: size.width,
                        height: size.height,
                    },
                    window::Event::Rescaled(factor) => WindowDiagnosticEvent::Rescaled(factor),
                    window::Event::CloseRequested => WindowDiagnosticEvent::CloseRequested,
                    _ => return None,
                };
                Some(Message::WindowDiagnostic { id, event })
            }),
        ])
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    tracing::info!(popup_id = ?p, "popup close requested");
                    self.spotify_settings_open = false;
                    destroy_popup(p)
                } else {
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    tracing::info!(
                        popup_id = ?new_id,
                        parent_id = ?self.core.main_window_id(),
                        min_width = 320.0,
                        max_width = 380.0,
                        min_height = 180.0,
                        max_height = 640.0,
                        "popup open requested"
                    );
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    // Tall enough for the expandable Spotify setup section.
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(380.0)
                        .min_width(320.0)
                        .min_height(180.0)
                        .max_height(640.0);
                    get_popup(popup_settings)
                };
            }
            Message::PopupClosed(id) => {
                if is_popup_id(self.popup, id) {
                    tracing::info!(popup_id = ?id, "popup closed");
                    self.popup = None;
                    self.spotify_settings_open = false;
                }
            }
            Message::ToggleSpotifySettings => {
                self.spotify_settings_open = !self.spotify_settings_open;
                tracing::debug!(
                    expanded = self.spotify_settings_open,
                    popup_id = ?self.popup,
                    "Spotify setup section toggled"
                );
            }
            Message::WindowDiagnostic { id, event } => {
                let role = self.surface_role(id);
                match event {
                    WindowDiagnosticEvent::Opened { width, height } => {
                        self.window_sizes.insert(id, (width, height));
                        tracing::info!(window_id = ?id, role, width, height, "window opened");
                    }
                    WindowDiagnosticEvent::Closed => {
                        self.window_sizes.remove(&id);
                        tracing::info!(window_id = ?id, role, "window closed");
                    }
                    WindowDiagnosticEvent::Resized { width, height } => {
                        let previous = self.window_sizes.get(&id).copied();
                        if should_log_resize(previous, (width, height)) {
                            self.window_sizes.insert(id, (width, height));
                            tracing::debug!(window_id = ?id, role, width, height, "window resized");
                        }
                    }
                    WindowDiagnosticEvent::Rescaled(factor) => {
                        tracing::info!(window_id = ?id, role, factor, "window scale factor changed");
                    }
                    WindowDiagnosticEvent::CloseRequested => {
                        tracing::debug!(window_id = ?id, role, "window close requested by compositor");
                    }
                }
            }
            Message::PollMpris => {
                self.log_panel_diagnostics_if_changed();
                return Task::perform(poll_mpris(), |snap| {
                    cosmic::Action::App(Message::MprisUpdate(snap))
                });
            }
            Message::MprisUpdate(snap) => {
                return self.apply_mpris_outcome(snap);
            }
            Message::Tick => {
                // Time-based marquee/progress need a state bump to repaint.
                self.frame = self.frame.wrapping_add(1);
            }
            Message::PlayPause => {
                return run_command(MprisCommand::PlayPause);
            }
            Message::Next => {
                return run_command(MprisCommand::Next);
            }
            Message::Previous => {
                return run_command(MprisCommand::Previous);
            }
            Message::Scroll(delta) => {
                let y = match delta {
                    ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => y,
                };
                if y > 0.0 {
                    return run_command(MprisCommand::Next);
                } else if y < 0.0 {
                    return run_command(MprisCommand::Previous);
                }
            }
            Message::Seek(frac) => {
                if let Some(len) = self.track.length {
                    self.track.position =
                        Duration::from_secs_f64(len.as_secs_f64() * frac.clamp(0.0, 1.0));
                    self.position_sampled_at = Instant::now();
                }
                return run_command(MprisCommand::SeekFraction(frac));
            }
            Message::Volume(v) => {
                let v = v.clamp(0.0, 1.0);
                self.track.volume = v;
                self.volume_override = Some((v, Instant::now()));
                return run_command(MprisCommand::SetVolume(v));
            }
            Message::OpenInSpotify => {
                let result = if let Some(url) = self.track.url.clone() {
                    std::process::Command::new("xdg-open").arg(url).spawn()
                } else {
                    std::process::Command::new("xdg-open")
                        .arg("spotify:")
                        .spawn()
                };
                match result {
                    Ok(_) => tracing::debug!("Spotify URI opener spawned"),
                    Err(error) => tracing::warn!(%error, "failed to open Spotify URI"),
                }
            }
            Message::ArtLoaded { url, result } => {
                if self.current_art_url.as_deref() == Some(url.as_str()) {
                    match result {
                        Ok(handle) => {
                            self.album_art = Some(handle);
                            tracing::debug!("album artwork loaded");
                        }
                        Err(error) => {
                            self.album_art = None;
                            tracing::debug!(%error, "album artwork load failed");
                        }
                    }
                } else {
                    tracing::debug!("stale album artwork result ignored");
                }
            }
            Message::CommandDone { command, result } => {
                match result {
                    Ok(()) => tracing::debug!(?command, "MPRIS command completed"),
                    Err(error) => tracing::warn!(?command, %error, "MPRIS command failed"),
                }
                return Task::perform(poll_mpris(), |snap| {
                    cosmic::Action::App(Message::MprisUpdate(snap))
                });
            }
            // --- Spotify Web API ---
            Message::UpdateConfig(config) => {
                let client_id_changed = self.config.spotify_client_id != config.spotify_client_id;
                self.config = config;
                self.spotify_client_id_input = self.config.spotify_client_id.clone();
                if client_id_changed {
                    tracing::info!("Spotify configuration changed");
                    self.init_spotify();
                }
            }
            Message::SpotifyClientIdInput(s) => {
                self.spotify_client_id_input = s;
            }
            Message::SpotifyConnect => {
                // Persist Client ID from the field (if valid), then start OAuth.
                // One button covers both "save" and "connect".
                if !self.client_id_input_is_valid() {
                    return Task::none();
                }
                self.save_client_id_from_input();
                let Some(client) = self.spotify_client.as_ref() else {
                    return Task::none();
                };
                let client_id = client.client_id().to_owned();
                self.spotify_auth = AuthState::Connecting;
                tracing::info!("Spotify OAuth flow started");
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || run_oauth_flow(&client_id))
                            .await
                            .unwrap_or(Err(SpotifyApiError::Transport("oauth_task_failed")))
                    },
                    |result| cosmic::Action::App(Message::SpotifyCallbackDone(result)),
                );
            }
            Message::SpotifyCallbackDone(result) => match result {
                Ok(()) => {
                    tracing::info!("Spotify OAuth flow completed");
                    self.spotify_auth = AuthState::Connected;
                    return self.start_like_check();
                }
                Err(error) => {
                    tracing::warn!(%error, "Spotify OAuth flow failed");
                    self.spotify_auth = AuthState::ReconnectRequired;
                }
            },
            Message::SpotifyDisconnect => {
                if let Some(client) = self.spotify_client.as_ref() {
                    match client.store().delete() {
                        Ok(()) => tracing::info!("Spotify credentials deleted"),
                        Err(error) => {
                            tracing::warn!(%error, "failed to delete Spotify credentials");
                        }
                    }
                }
                self.spotify_auth = if self.spotify_client.is_some() {
                    AuthState::Disconnected
                } else {
                    AuthState::Unconfigured
                };
                self.spotify_like = LikeState::Unavailable;
                self.spotify_like_track = None;
            }
            Message::SpotifyLikeCheckResult { track_id, result } => {
                if !result_matches_current_track(self.track.track_id.as_deref(), &track_id) {
                    tracing::debug!("stale Spotify library check ignored");
                    return Task::none();
                }
                match result {
                    Ok(saved) => {
                        tracing::debug!(saved, "Spotify library check completed");
                        self.spotify_error = None;
                        self.spotify_like = if saved {
                            LikeState::Saved
                        } else {
                            LikeState::Unsaved
                        };
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Spotify library check failed");
                        self.spotify_error = Some(spotify_error_message(&error));
                        self.spotify_like = LikeState::Error;
                    }
                }
            }
            Message::SpotifyLikeToggle => {
                let Some(track_id) = self.track.track_id.clone() else {
                    return Task::none();
                };
                let Some(client) = self.spotify_client.as_ref() else {
                    return Task::none();
                };
                let should_save = self.spotify_like != LikeState::Saved;
                let client_id = client.client_id().to_owned();
                let result_track_id = track_id.clone();
                self.spotify_like = LikeState::Mutating;
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let store = SecretServiceTokenStore::new();
                            let tokens = store
                                .load()
                                .map_err(|_| SpotifyApiError::Refresh("keyring_load_failed"))?;
                            let client = SpotifyClient::new(client_id, store).with_tokens(tokens);
                            if should_save {
                                client.library_save(&track_id)
                            } else {
                                client.library_remove(&track_id)
                            }
                        })
                        .await
                        .unwrap_or(Err(SpotifyApiError::Transport("library_task_failed")))
                    },
                    move |result| {
                        cosmic::Action::App(Message::SpotifyLikeDone {
                            track_id: result_track_id,
                            saved: should_save,
                            result,
                        })
                    },
                );
            }
            Message::SpotifyLikeDone {
                track_id,
                saved,
                result,
            } => {
                if !result_matches_current_track(self.track.track_id.as_deref(), &track_id) {
                    tracing::debug!("stale Spotify library mutation ignored");
                    return Task::none();
                }
                match result {
                    Ok(()) => {
                        tracing::debug!(saved, "Spotify library mutation completed");
                        self.spotify_error = None;
                        self.spotify_like = if saved {
                            LikeState::Saved
                        } else {
                            LikeState::Unsaved
                        };
                    }
                    Err(error) => {
                        tracing::warn!(saved, %error, "Spotify library mutation failed");
                        self.spotify_error = Some(spotify_error_message(&error));
                        self.spotify_like = LikeState::Error;
                    }
                }
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl AppModel {
    fn log_applet_initialized(&self) {
        if let Some(snapshot) = self.panel_diagnostics.as_ref() {
            self.log_panel_snapshot("applet model initialized", snapshot);
        }
    }

    fn log_panel_diagnostics_if_changed(&mut self) {
        let current = PanelDiagnosticSnapshot::capture(&self.core);
        let changed = self
            .panel_diagnostics
            .as_ref()
            .is_none_or(|previous| diagnostics_changed(previous, &current));

        if changed {
            self.log_panel_snapshot("panel context changed", &current);
            self.panel_diagnostics = Some(current);
        }
    }

    fn log_panel_snapshot(&self, message: &'static str, snapshot: &PanelDiagnosticSnapshot) {
        tracing::info!(
            pid = std::process::id(),
            version = env!("CARGO_PKG_VERSION"),
            main_window_id = ?self.core.main_window_id(),
            output = %snapshot.output_name,
            panel_type = %snapshot.panel_type,
            anchor = %snapshot.anchor,
            configured_size = %snapshot.configured_size,
            suggested_bounds = ?snapshot.suggested_bounds,
            scale_factor = snapshot.scale_factor,
            suggested_icon_size = ?snapshot.suggested_icon_size,
            suggested_padding = ?snapshot.suggested_padding,
            "{message}"
        );
    }

    fn surface_role(&self, id: Id) -> &'static str {
        if self.core.main_window_id() == Some(id) {
            "main"
        } else if self.popup == Some(id) {
            "popup"
        } else {
            "unknown"
        }
    }

    fn client_id_input_is_valid(&self) -> bool {
        let id = self.spotify_client_id_input.trim();
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Persist the Client ID field into cosmic-config and re-init the API client.
    /// Clears keyring tokens when the Client ID changes (tokens are app-bound).
    fn save_client_id_from_input(&mut self) {
        let id = self.spotify_client_id_input.trim().to_string();
        let credential_delete_error = if id != self.config.spotify_client_id {
            self.spotify_client
                .as_ref()
                .and_then(|client| client.store().delete().err())
        } else {
            None
        };
        if let Some(error) = credential_delete_error {
            tracing::warn!(%error, "failed to delete credentials for previous Spotify configuration");
        }
        if let Some(ctx) = &self.cosmic_config {
            let mut new_config = self.config.clone();
            new_config.spotify_client_id = id.clone();
            match new_config.write_entry(ctx) {
                Ok(()) => tracing::info!("Spotify configuration saved"),
                Err(_) => tracing::warn!("failed to write Spotify configuration"),
            }
        } else {
            tracing::warn!(
                "Spotify configuration was not persisted because cosmic-config is unavailable"
            );
        }
        self.config.spotify_client_id = id;
        self.init_spotify();
    }

    /// Initialize or reinitialize the Spotify client from the current config.
    fn init_spotify(&mut self) {
        let client_id = self.config.spotify_client_id.trim().to_string();
        if client_id.is_empty() {
            self.spotify_auth = AuthState::Unconfigured;
            self.spotify_client = None;
            self.spotify_like = LikeState::Unavailable;
            return;
        }
        if spotify::validate_client_id(&client_id).is_err() {
            tracing::warn!("configured Spotify Client ID is invalid");
            self.spotify_auth = AuthState::Unconfigured;
            self.spotify_client = None;
            self.spotify_like = LikeState::Unavailable;
            return;
        }

        let store = SecretServiceTokenStore::new();
        match store.load() {
            Ok(tokens) => {
                tracing::info!("Spotify credentials loaded from Secret Service");
                self.spotify_client =
                    Some(SpotifyClient::new(client_id, store).with_tokens(tokens));
                self.spotify_auth = AuthState::Connected;
            }
            Err(KeyringError::Missing) => {
                tracing::debug!("no Spotify credentials found in Secret Service");
                self.spotify_client = Some(SpotifyClient::new(client_id, store));
                self.spotify_auth = AuthState::Disconnected;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to load Spotify credentials from Secret Service");
                self.spotify_client = Some(SpotifyClient::new(client_id, store));
                self.spotify_auth = AuthState::Disconnected;
            }
        }
        self.spotify_like = LikeState::Unavailable;
    }

    fn estimated_position(&self) -> Duration {
        let base = self.track.position;
        if self.track.status.is_playing() {
            base + self.position_sampled_at.elapsed()
        } else {
            base
        }
    }

    fn estimated_progress(&self) -> f64 {
        let Some(len) = self.track.length.filter(|d| !d.is_zero()) else {
            return 0.0;
        };
        (self.estimated_position().as_secs_f64() / len.as_secs_f64()).clamp(0.0, 1.0)
    }

    fn apply_mpris_outcome(&mut self, outcome: MprisPollOutcome) -> Task<cosmic::Action<Message>> {
        let state = match &outcome {
            MprisPollOutcome::Connected(_) => MprisDiagnosticState::Connected,
            MprisPollOutcome::Unavailable => MprisDiagnosticState::Unavailable,
            MprisPollOutcome::Failed(error) => MprisDiagnosticState::Failed(error.kind()),
        };

        if mpris_transition_changed(self.mpris_diagnostic_state, state) {
            match &outcome {
                MprisPollOutcome::Connected(_) => {
                    tracing::info!("Spotify MPRIS connected");
                }
                MprisPollOutcome::Unavailable => {
                    tracing::info!("Spotify MPRIS unavailable");
                }
                MprisPollOutcome::Failed(error) => {
                    tracing::warn!(kind = ?error.kind(), %error, "MPRIS polling failed");
                }
            }
            self.mpris_diagnostic_state = Some(state);
        }

        match outcome {
            MprisPollOutcome::Connected(snapshot) => self.apply_snapshot(*snapshot),
            MprisPollOutcome::Unavailable => self.apply_snapshot(TrackSnapshot::default()),
            MprisPollOutcome::Failed(_) => Task::none(),
        }
    }

    fn apply_snapshot(&mut self, mut snap: TrackSnapshot) -> Task<cosmic::Action<Message>> {
        if let Some((v, at)) = self.volume_override {
            if at.elapsed() < Duration::from_millis(800) {
                snap.volume = v;
            } else {
                self.volume_override = None;
            }
        }

        let art_changed = match (&self.current_art_url, &snap.art_url) {
            (None, Some(_)) => true,
            (Some(old), Some(new)) => old != new,
            (Some(_), None) => {
                self.album_art = None;
                self.current_art_url = None;
                false
            }
            (None, None) => false,
        };

        let position_jump = snap
            .position
            .as_millis()
            .abs_diff(self.track.position.as_millis())
            > 1500
            || snap.track_id != self.track.track_id
            || snap.title != self.track.title;

        if position_jump || !self.track.connected {
            self.position_sampled_at = Instant::now();
        } else if snap.status.is_playing() && self.track.status.is_playing() {
            let estimated = self.estimated_position();
            let drift = snap.position.as_millis().abs_diff(estimated.as_millis());
            if drift > 2000 {
                self.position_sampled_at = Instant::now();
            } else {
                // Re-base sample clock so estimated matches MPRIS without a visible jump.
                self.position_sampled_at = Instant::now();
            }
        } else {
            self.position_sampled_at = Instant::now();
        }

        let track_changed = self.track.track_id != snap.track_id;
        self.track = snap;

        if track_changed {
            self.spotify_error = None;
            self.spotify_like = LikeState::Unavailable;
            self.spotify_like_track = None;
        }

        if self.track.connected {
            self.marquee.set_text(self.track.display_line());
        } else {
            self.marquee.clear();
        }

        // Start the library lookup before loading album art. Both tasks are
        // asynchronous, but `Task::perform` returns immediately; returning
        // from the artwork branch first used to skip this lookup permanently
        // for the first snapshot of every track.
        if track_changed && self.spotify_auth == AuthState::Connected {
            return self.start_like_check();
        }

        if art_changed {
            if let Some(url) = self.track.art_url.clone() {
                self.current_art_url = Some(url.clone());
                return Task::perform(art::load_art(url.clone()), move |result| {
                    cosmic::Action::App(Message::ArtLoaded { url, result })
                });
            }
        }

        Task::none()
    }

    /// Query the saved-library state for the currently playing Spotify track.
    /// The task uses a new client populated from the Secret Service token so it
    /// never moves the app model's non-cloneable HTTP client onto a worker.
    fn start_like_check(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(track_id) = self.track.track_id.clone() else {
            self.spotify_like = LikeState::Unavailable;
            self.spotify_like_track = None;
            return Task::none();
        };
        let Some(client) = self.spotify_client.as_ref() else {
            self.spotify_like = LikeState::Unavailable;
            return Task::none();
        };

        let client_id = client.client_id().to_owned();
        let result_track_id = track_id.clone();
        self.spotify_like = LikeState::Loading;
        self.spotify_like_track = Some(track_id.clone());
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let store = SecretServiceTokenStore::new();
                    let tokens = store
                        .load()
                        .map_err(|_| SpotifyApiError::Refresh("keyring_load_failed"))?;
                    SpotifyClient::new(client_id, store)
                        .with_tokens(tokens)
                        .library_contains(&track_id)
                })
                .await
                .unwrap_or(Err(SpotifyApiError::Transport("library_task_failed")))
            },
            move |result| {
                cosmic::Action::App(Message::SpotifyLikeCheckResult {
                    track_id: result_track_id,
                    result,
                })
            },
        )
    }

    fn panel_offline(&self) -> Element<'_, Message> {
        // Non-symbolic size is larger (XS: 24 vs symbolic 16).
        let (w, _) = self.core.applet.suggested_size(false);
        widget::icon::from_name("multimedia-player-symbolic")
            .size(w)
            .symbolic(true)
            .icon()
            .into()
    }

    /// Popup content: player controls + gear footer; optional expandable setup.
    fn view_popup(&self) -> Element<'_, Message> {
        let mut col = widget::column::with_capacity(5)
            .spacing(8)
            .padding(12)
            .width(Length::Fixed(340.0));

        if self.track.connected {
            col = col.push(self.popup_playing());
        } else {
            col = col
                .push(widget::text::title4(fl!("offline")))
                .push(widget::text::body(fl!("nothing-playing")));

            if self.spotify_auth == AuthState::Unconfigured && !self.spotify_settings_open {
                col = col.push(widget::text::caption(fl!("spotify-settings-hint")));
            }

            col = col.push(self.popup_settings_footer(false));
        }

        if self.spotify_settings_open {
            col = col.push(self.spotify_setup_section());
        }

        self.core.applet.popup_container(col).into()
    }

    /// Footer row: optional Open in Spotify on the left, gear on the right.
    /// Gear toggles the expandable Spotify setup section below.
    fn popup_settings_footer(&self, show_open_spotify: bool) -> Element<'_, Message> {
        let gear_icon = if self.spotify_settings_open {
            "go-up-symbolic"
        } else {
            "preferences-system-symbolic"
        };

        let gear = widget::button::icon(widget::icon::from_name(gear_icon).size(16))
            .on_press(Message::ToggleSpotifySettings)
            .tooltip(fl!("spotify-settings-open"));

        let mut row = widget::row::with_capacity(3)
            .spacing(8)
            .align_y(Vertical::Center);

        if show_open_spotify {
            row = row.push(
                widget::button::standard(fl!("open-spotify")).on_press(Message::OpenInSpotify),
            );
        }

        row.push(space::horizontal()).push(gear).into()
    }

    fn panel_playing(&self) -> Element<'_, Message> {
        // Album art: use non-symbolic applet size (XS=24, S=32, …) so cover isn't tiny.
        let (art_px, _) = self.core.applet.suggested_size(false);
        let art_size = f32::from(art_px);

        let art: Element<'_, Message> = if let Some(handle) = &self.album_art {
            widget::container(
                widget::image(handle.clone())
                    .width(Length::Fixed(art_size))
                    .height(Length::Fixed(art_size)),
            )
            .width(Length::Fixed(art_size))
            .height(Length::Fixed(art_size))
            .into()
        } else {
            widget::icon::from_name("multimedia-player-symbolic")
                .size(art_px)
                .symbolic(true)
                .icon()
                .into()
        };

        let marquee = self.marquee.view(|s| {
            self.core
                .applet
                .text(s.to_owned())
                .wrapping(Wrapping::None)
                .into()
        });

        // Thin progress under the marquee (still one visual "line" next to the cover).
        let progress = self.estimated_progress();
        let bar = progress_bar(progress, PANEL_PROGRESS_WIDTH, 2.0);

        let text_col = widget::column::with_capacity(2)
            .spacing(4)
            .push(marquee)
            .push(bar);

        widget::row::with_capacity(2)
            .spacing(8)
            .align_y(Vertical::Center)
            .push(art)
            .push(text_col)
            .into()
    }

    fn popup_playing(&self) -> Element<'_, Message> {
        let art: Element<'_, Message> = if let Some(handle) = &self.album_art {
            widget::image(handle.clone())
                .width(Length::Fixed(f32::from(POPUP_ART)))
                .height(Length::Fixed(f32::from(POPUP_ART)))
                .into()
        } else {
            widget::container(
                widget::icon::from_name("multimedia-player-symbolic")
                    .size(64)
                    .icon(),
            )
            .width(Length::Fixed(f32::from(POPUP_ART)))
            .height(Length::Fixed(f32::from(POPUP_ART)))
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .into()
        };

        let title_text = if self.track.title.is_empty() {
            "\u{2014}"
        } else {
            self.track.title.as_str()
        };
        let artist_text = if self.track.artist.is_empty() {
            "\u{2014}"
        } else {
            self.track.artist.as_str()
        };
        let album_text = self.track.album.as_str();

        let meta: Element<'_, Message> = widget::column::with_capacity(3)
            .spacing(4)
            .push(widget::text::title4(title_text))
            .push(widget::text::body(artist_text))
            .push(widget::text::caption(album_text))
            .width(Length::Fill)
            .into();

        let header: Element<'_, Message> = widget::row::with_capacity(2)
            .spacing(12)
            .align_y(Vertical::Center)
            .push(art)
            .push(meta)
            .into();

        let pos = self.estimated_position();
        let pos_label = format_duration(pos);
        let len_label = self.track.length_label();
        let progress = self.estimated_progress();

        let seek = widget::slider(0.0..=1.0, progress, Message::Seek).step(0.001);

        let time_row: Element<'_, Message> = widget::row::with_capacity(3)
            .push(widget::text::caption(pos_label))
            .push(space::horizontal())
            .push(widget::text::caption(len_label))
            .into();

        let play_icon = if self.track.status.is_playing() {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        };

        let controls: Element<'_, Message> = widget::row::with_capacity(5)
            .spacing(8)
            .align_y(Vertical::Center)
            .push(space::horizontal())
            .push(
                widget::button::icon(widget::icon::from_name("media-skip-backward-symbolic"))
                    .on_press_maybe(self.track.can_go_previous.then_some(Message::Previous)),
            )
            .push(
                widget::button::icon(widget::icon::from_name(play_icon))
                    .on_press(Message::PlayPause),
            )
            .push(
                widget::button::icon(widget::icon::from_name("media-skip-forward-symbolic"))
                    .on_press_maybe(self.track.can_go_next.then_some(Message::Next)),
            )
            .push(space::horizontal())
            .into();

        let volume: Element<'_, Message> = widget::row::with_capacity(2)
            .spacing(8)
            .align_y(Vertical::Center)
            .push(
                widget::icon::from_name("audio-volume-medium-symbolic")
                    .size(16)
                    .icon(),
            )
            .push(
                widget::slider(0.0..=1.0, self.track.volume, Message::Volume)
                    .step(0.01)
                    .width(Length::Fill),
            )
            .into();

        // --- Like button ---
        let like_btn: Option<Element<'_, Message>> = self.like_button();

        let mut player = widget::column::with_capacity(7)
            .spacing(10)
            .push(header)
            .push(time_row)
            .push(seek)
            .push(controls)
            .push(volume);

        if let Some(btn) = like_btn {
            player = player.push(btn);
        }

        if let Some(error) = &self.spotify_error {
            player = player.push(widget::text::caption(error.clone()));
        }

        player.push(self.popup_settings_footer(true)).into()
    }

    /// Build the expandable Spotify setup section (shown under the gear).
    fn spotify_setup_section(&self) -> Element<'_, Message> {
        let mut section = widget::column::with_capacity(6).spacing(6);

        let show_howto = matches!(
            self.spotify_auth,
            AuthState::Unconfigured | AuthState::Disconnected
        );
        if show_howto {
            section = section
                .push(widget::text::title4(fl!("spotify-setup")))
                .push(widget::text::caption(fl!("spotify-howto-1")))
                .push(widget::text::caption(fl!("spotify-howto-2")))
                .push(widget::text::caption(fl!("spotify-howto-3")))
                .push(widget::text::caption(fl!("spotify-howto-4")))
                .push(widget::text::caption(fl!("spotify-redirect-uri-label")))
                .push(widget::text::caption("http://127.0.0.1:43821/callback"))
                .push(widget::text::caption(fl!("spotify-howto-5")))
                .push(widget::text::caption(fl!("spotify-howto-6")))
                .push(space::vertical().height(Length::Fixed(8.0)));
        }

        // Client ID field + single Connect action for setup/disconnected states.
        if matches!(
            self.spotify_auth,
            AuthState::Unconfigured | AuthState::Disconnected
        ) {
            // Placeholder-only: no separate label (avoids duplicated "Spotify Client ID").
            let input = widget::text_input(
                fl!("spotify-client-id-placeholder"),
                &self.spotify_client_id_input,
            )
            .on_input(Message::SpotifyClientIdInput);

            section = section.push(input).push(
                widget::button::suggested(fl!("connect-spotify")).on_press_maybe(
                    self.client_id_input_is_valid()
                        .then_some(Message::SpotifyConnect),
                ),
            );
        }

        if self.spotify_auth == AuthState::Connecting {
            section = section
                .push(space::vertical().height(Length::Fixed(8.0)))
                .push(widget::text::body(fl!("spotify-connecting")));
        }

        if self.spotify_auth == AuthState::ReconnectRequired {
            section = section
                .push(space::vertical().height(Length::Fixed(8.0)))
                .push(
                    widget::button::suggested(fl!("spotify-reconnect"))
                        .on_press(Message::SpotifyConnect),
                );
        }

        if self.spotify_auth == AuthState::Connected {
            section = section
                .push(widget::text::body(fl!("spotify-connected")))
                .push(
                    widget::button::standard(fl!("spotify-disconnect"))
                        .on_press(Message::SpotifyDisconnect),
                );
        }

        section.into()
    }

    /// Build the like button for the current track, if applicable.
    fn like_button(&self) -> Option<Element<'_, Message>> {
        if self.spotify_auth != AuthState::Connected {
            return None;
        }
        let has_track_id = self.track.track_id.is_some();
        if !has_track_id {
            return None;
        }

        let (heart_text, enabled) = match self.spotify_like {
            LikeState::Saved => ("\u{2665}", true),
            LikeState::Unsaved => ("\u{2661}", true),
            LikeState::Loading | LikeState::Mutating => ("…", false),
            LikeState::Error => ("⚠", true),
            LikeState::Unavailable => return None,
        };

        let heart = widget::text::body(heart_text).size(20);
        let btn = if enabled {
            widget::button::custom(heart).on_press(Message::SpotifyLikeToggle)
        } else {
            widget::button::custom(heart)
        };

        Some(btn.into())
    }
}

/// True only if an asynchronous library result still belongs to the track
/// currently displayed by the popup.
fn result_matches_current_track(current_track_id: Option<&str>, result_track_id: &str) -> bool {
    current_track_id == Some(result_track_id)
}

fn is_popup_id(popup: Option<Id>, closed: Id) -> bool {
    popup == Some(closed)
}

fn progress_bar<'a>(fraction: f64, width: f32, height: f32) -> Element<'a, Message> {
    let fraction = fraction.clamp(0.0, 1.0) as f32;
    let filled = (width * fraction).max(if fraction > 0.0 { 1.0 } else { 0.0 });

    let fill = widget::container(space::horizontal())
        .width(Length::Fixed(filled))
        .height(Length::Fixed(height))
        .style(move |_theme| {
            let mut style = widget::container::Style::default();
            style.background = Some(cosmic::iced::Background::Color(cosmic::iced::Color::from(
                SPOTIFY_GREEN,
            )));
            style.border.radius = height.into();
            style
        });

    widget::container(
        widget::row::with_capacity(2)
            .push(fill)
            .push(space::horizontal()),
    )
    .width(Length::Fixed(width))
    .height(Length::Fixed(height))
    .class(cosmic::style::Container::Background)
    .into()
}

async fn poll_mpris() -> MprisPollOutcome {
    match tokio::task::spawn_blocking(mpris::fetch_snapshot).await {
        Ok(outcome) => outcome,
        Err(_) => MprisPollOutcome::Failed(MprisFailure::worker()),
    }
}

fn run_command(cmd: MprisCommand) -> Task<cosmic::Action<Message>> {
    let completed_command = cmd.clone();
    Task::perform(
        async move {
            match tokio::task::spawn_blocking(move || mpris::apply_command(cmd)).await {
                Ok(result) => result,
                Err(_) => Err(MprisFailure::worker()),
            }
        },
        move |result| {
            cosmic::Action::App(Message::CommandDone {
                command: completed_command,
                result,
            })
        },
    )
}

/// Convert a token-safe API error into a localized user-facing category.
fn spotify_error_message(error: &SpotifyApiError) -> String {
    match error {
        SpotifyApiError::Refresh(_) => fl!("spotify-error-auth"),
        SpotifyApiError::Allowlist(_) => fl!("spotify-error-allowlist"),
        SpotifyApiError::RateLimited { .. } => fl!("spotify-error-rate-limited"),
        SpotifyApiError::Transport(_) => fl!("spotify-error-network"),
        SpotifyApiError::Http { status, .. } => {
            format!("{} (HTTP {status})", fl!("spotify-error-api"))
        }
        SpotifyApiError::Malformed(_) => fl!("spotify-error-api"),
    }
}

/// Run the OAuth PKCE authorization flow: bind loopback listener, open browser,
/// wait for callback, exchange code for tokens, and persist via keyring.
fn run_oauth_flow(client_id: &str) -> Result<(), SpotifyApiError> {
    let verifier = spotify::generate_pkce_verifier();
    let state = OAuthState::generate();
    let challenge = verifier.challenge_s256();

    let params = spotify::AuthorizeUrlParams::builder()
        .client_id(client_id)
        .state(state.clone())
        .code_challenge(challenge)
        .build();
    let url = spotify::build_authorize_url(params);

    // Bind listener before opening the browser.
    let listener = spotify::LoopbackListener::bind(
        spotify::DEFAULT_LOOPBACK_ADDR
            .parse()
            .map_err(|_| SpotifyApiError::Transport("bind_failed"))?,
    )
    .map_err(|_| SpotifyApiError::Transport("bind_failed"))?;

    // Open the authorization URL in the system browser.
    std::process::Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .map_err(|_| SpotifyApiError::Transport("browser_open_failed"))?;

    // Wait for the callback (60-second timeout).
    let auth_code = listener
        .wait_for_callback(state.as_str(), Duration::from_secs(60))
        .map_err(|e| match e {
            LoopbackError::Timeout => SpotifyApiError::Transport("auth_timeout"),
            LoopbackError::UserDenied => SpotifyApiError::Allowlist("denied"),
            LoopbackError::StateMismatch => SpotifyApiError::Transport("state_mismatch"),
            _ => SpotifyApiError::Transport("callback_error"),
        })?;

    // Exchange the authorization code for tokens.
    let store = SecretServiceTokenStore::new();
    let client = SpotifyClient::new(client_id.to_string(), store);
    let tokens = client.exchange_code(&auth_code, &verifier)?;

    // Tokens are persisted by `exchange_code` via `set_tokens`.
    let _ = tokens;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        diagnostics_changed, is_popup_id, mpris_transition_changed, result_matches_current_track,
        should_log_resize, MprisDiagnosticState, PanelDiagnosticSnapshot,
    };
    use crate::mpris::MprisFailureKind;
    use cosmic::iced::window::Id;

    #[test]
    fn library_result_only_applies_to_the_current_track() {
        assert!(result_matches_current_track(Some("current"), "current"));
        assert!(!result_matches_current_track(Some("current"), "stale"));
        assert!(!result_matches_current_track(None, "stale"));
    }

    #[test]
    fn popup_id_match_is_exact() {
        let popup = Id::unique();
        let other = Id::unique();
        assert!(is_popup_id(Some(popup), popup));
        assert!(!is_popup_id(Some(popup), other));
        assert!(!is_popup_id(None, popup));
    }

    #[test]
    fn panel_diagnostics_only_report_real_changes() {
        let first = PanelDiagnosticSnapshot::for_test("DP-1", 1.0, Some((320.0, 32.0)));
        let same = first.clone();
        let other_output = PanelDiagnosticSnapshot::for_test("HDMI-A-1", 1.0, Some((320.0, 32.0)));
        let other_scale = PanelDiagnosticSnapshot::for_test("DP-1", 1.5, Some((320.0, 32.0)));
        let other_bounds = PanelDiagnosticSnapshot::for_test("DP-1", 1.0, Some((420.0, 48.0)));

        assert!(!diagnostics_changed(&first, &same));
        assert!(diagnostics_changed(&first, &other_output));
        assert!(diagnostics_changed(&first, &other_scale));
        assert!(diagnostics_changed(&first, &other_bounds));
    }

    #[test]
    fn identical_resize_is_suppressed() {
        assert!(should_log_resize(None, (320.0, 32.0)));
        assert!(!should_log_resize(Some((320.0, 32.0)), (320.0, 32.0)));
        assert!(should_log_resize(Some((320.0, 32.0)), (420.0, 48.0)));
    }

    #[test]
    fn mpris_diagnostics_only_report_state_or_category_changes() {
        let connected = MprisDiagnosticState::Connected;
        let unavailable = MprisDiagnosticState::Unavailable;
        let finder_failed = MprisDiagnosticState::Failed(MprisFailureKind::Finder);
        let enumerate_failed = MprisDiagnosticState::Failed(MprisFailureKind::Enumeration);

        assert!(mpris_transition_changed(None, connected));
        assert!(!mpris_transition_changed(Some(connected), connected));
        assert!(mpris_transition_changed(Some(connected), unavailable));
        assert!(!mpris_transition_changed(
            Some(finder_failed),
            finder_failed
        ));
        assert!(mpris_transition_changed(
            Some(finder_failed),
            enumerate_failed
        ));
    }
}
