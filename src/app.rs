// SPDX-License-Identifier: MIT

use crate::art;
use crate::config::Config;
use crate::fl;
use crate::marquee::Marquee;
use crate::mpris::{self, format_duration, MprisCommand, PlaybackStatus, TrackSnapshot};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::mouse::ScrollDelta;
use cosmic::iced::platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::time;
use cosmic::iced::widget::image::Handle;
use cosmic::iced::widget::text::Wrapping;
use cosmic::iced::{window::Id, Length, Limits, Subscription};
use cosmic::prelude::*;
use cosmic::widget::{self, space};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(750);
/// Fast tick for smooth marquee + progress bar.
const TICK_INTERVAL: Duration = Duration::from_millis(33);
const POPUP_ART: u16 = 128;
const SPOTIFY_GREEN: [f32; 4] = [0.114, 0.725, 0.329, 1.0];
const PANEL_PROGRESS_WIDTH: f32 = crate::marquee::VIEWPORT_WIDTH;

pub struct AppModel {
    core: cosmic::Core,
    popup: Option<Id>,
    config: Config,
    track: TrackSnapshot,
    /// Wall-clock when `track.position` was sampled (smooth progress while playing).
    position_sampled_at: Instant,
    album_art: Option<Handle>,
    current_art_url: Option<String>,
    /// Suppress MPRIS volume overwrite briefly after local drag.
    volume_override: Option<(f64, Instant)>,
    /// Scrolling "title — artist" in the panel.
    marquee: Marquee,
    /// Bumped on each tick so iced redraws the time-based marquee/progress.
    frame: u64,
}

impl Default for AppModel {
    fn default() -> Self {
        Self {
            core: cosmic::Core::default(),
            popup: None,
            config: Config::default(),
            track: TrackSnapshot::default(),
            position_sampled_at: Instant::now(),
            album_art: None,
            current_art_url: None,
            volume_override: None,
            marquee: Marquee::default(),
            frame: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    UpdateConfig(Config),
    MprisUpdate(TrackSnapshot),
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
        handle: Option<Handle>,
    },
    CommandDone,
    Scroll(ScrollDelta),
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
        let config = cosmic_config::Config::new(Self::APP_ID, Config::VERSION)
            .map(|context| match Config::get_entry(&context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();

        let app = AppModel {
            core,
            config,
            ..Default::default()
        };

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
        let content = if self.track.connected {
            self.panel_playing()
        } else {
            self.panel_offline()
        };

        let pad = self.core.applet.suggested_padding(true).0;
        let button = widget::button::custom(content)
            .padding([0, pad])
            .class(cosmic::theme::Button::AppletIcon);

        // Panel windows default to icon-sized bounds; autosize expands width for text.
        // Keep height capped to the panel so multi-line content cannot thicken the bar.
        let interactive = widget::mouse_area(button)
            .on_press(Message::TogglePopup)
            .on_middle_press(Message::PlayPause)
            .on_scroll(Message::Scroll);

        let (_sw, sh) = self.core.applet.suggested_window_size();
        let panel_h = sh.get() as f32;

        self.core
            .applet
            .autosize_window(interactive)
            .max_height(panel_h)
            .auto_height(false)
            .into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        let content: Element<'_, Message> = if !self.track.connected {
            widget::column::with_capacity(2)
                .spacing(12)
                .padding(16)
                .push(widget::text::title4(fl!("offline")))
                .push(widget::text::body(fl!("nothing-playing")))
                .width(Length::Fixed(320.0))
                .into()
        } else {
            self.popup_playing()
        };

        self.core.applet.popup_container(content).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch(vec![
            time::every(POLL_INTERVAL).map(|_| Message::PollMpris),
            time::every(TICK_INTERVAL).map(|_| Message::Tick),
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
        ])
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::UpdateConfig(config) => {
                self.config = config;
            }
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(380.0)
                        .min_width(320.0)
                        .min_height(220.0)
                        .max_height(520.0);
                    get_popup(popup_settings)
                };
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
            Message::PollMpris => {
                return Task::perform(poll_mpris(), |snap| {
                    cosmic::Action::App(Message::MprisUpdate(snap))
                });
            }
            Message::MprisUpdate(snap) => {
                return self.apply_snapshot(snap);
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
                if let Some(url) = self.track.url.clone() {
                    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                } else {
                    let _ = std::process::Command::new("xdg-open")
                        .arg("spotify:")
                        .spawn();
                }
            }
            Message::ArtLoaded { url, handle } => {
                if self.current_art_url.as_deref() == Some(url.as_str()) {
                    self.album_art = handle;
                }
            }
            Message::CommandDone => {
                return Task::perform(poll_mpris(), |snap| {
                    cosmic::Action::App(Message::MprisUpdate(snap))
                });
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

impl AppModel {
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

        self.track = snap;

        if self.track.connected {
            self.marquee.set_text(self.track.display_line());
        } else {
            self.marquee.clear();
        }

        if art_changed {
            if let Some(url) = self.track.art_url.clone() {
                self.current_art_url = Some(url.clone());
                return Task::perform(art::load_art(url.clone()), move |handle| {
                    cosmic::Action::App(Message::ArtLoaded { url, handle })
                });
            }
        }

        Task::none()
    }

    fn panel_offline(&self) -> Element<'_, Message> {
        let (w, _h) = self.core.applet.suggested_size(true);
        widget::icon::from_name("multimedia-player-symbolic")
            .size(w)
            .symbolic(true)
            .icon()
            .into()
    }

    fn panel_playing(&self) -> Element<'_, Message> {
        let (icon_w, icon_h) = self.core.applet.suggested_size(true);
        let art_size = f32::from(icon_w.max(icon_h).max(16));

        let art: Element<'_, Message> = if let Some(handle) = &self.album_art {
            widget::image(handle.clone())
                .width(Length::Fixed(art_size))
                .height(Length::Fixed(art_size))
                .into()
        } else {
            widget::icon::from_name("multimedia-player-symbolic")
                .size(icon_w)
                .symbolic(true)
                .icon()
                .into()
        };

        // Use applet-styled single-line text (Wrapping::None → marquee, not multi-line growth).
        let marquee = self.marquee.view(|s| {
            self.core
                .applet
                .text(s.to_owned())
                .wrapping(Wrapping::None)
                .into()
        });

        let progress = self.estimated_progress();
        let bar = progress_bar(progress, PANEL_PROGRESS_WIDTH, 3.0);

        let text_col = widget::column::with_capacity(2)
            .spacing(2)
            .push(marquee)
            .push(bar);

        // Center within panel height; never request more than icon row height.
        let (icon_w2, icon_h2) = self.core.applet.suggested_size(true);
        let row_h = f32::from(icon_h2.max(icon_w2));

        widget::container(
            widget::row::with_capacity(2)
                .spacing(8)
                .align_y(Vertical::Center)
                .push(art)
                .push(text_col),
        )
        .height(Length::Fixed(row_h))
        .align_y(Vertical::Center)
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
            "—"
        } else {
            self.track.title.as_str()
        };
        let artist_text = if self.track.artist.is_empty() {
            "—"
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

        let footer = widget::button::standard(fl!("open-spotify")).on_press(Message::OpenInSpotify);

        widget::column::with_capacity(6)
            .spacing(12)
            .padding(16)
            .width(Length::Fixed(340.0))
            .push(header)
            .push(time_row)
            .push(seek)
            .push(controls)
            .push(volume)
            .push(footer)
            .into()
    }
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

async fn poll_mpris() -> TrackSnapshot {
    tokio::task::spawn_blocking(mpris::fetch_snapshot)
        .await
        .unwrap_or_default()
}

fn run_command(cmd: MprisCommand) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move {
            let _ = tokio::task::spawn_blocking(move || mpris::apply_command(cmd)).await;
        },
        |_| cosmic::Action::App(Message::CommandDone),
    )
}

#[allow(dead_code)]
fn _status_playing(s: PlaybackStatus) -> bool {
    s.is_playing()
}
