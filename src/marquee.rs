// SPDX-License-Identifier: MIT

//! Horizontal scrolling marquee for the panel tray.

use cosmic::iced::Length;
use cosmic::iced::Padding;
use cosmic::prelude::*;
use cosmic::widget::{self, space};
use std::time::{Duration, Instant};

/// Visible width of the scrolling text region in the panel (px).
pub const VIEWPORT_WIDTH: f32 = 168.0;
/// Line box height for marquee text (fits XS panel with 24px art).
pub const LINE_HEIGHT: f32 = 16.0;
/// Gap between looped copies of the text.
pub const LOOP_GAP: f32 = 40.0;
/// Scroll speed in pixels per second.
pub const SPEED_PX_PER_SEC: f32 = 42.0;
/// Pause at the start of each loop before scrolling.
pub const START_PAUSE: Duration = Duration::from_millis(700);
/// Approximate average glyph advance for body text (px).
const AVG_CHAR_WIDTH: f32 = 6.8;

/// State driving the panel marquee.
#[derive(Debug, Clone)]
pub struct Marquee {
    text: String,
    started_at: Instant,
    text_width: f32,
}

impl Default for Marquee {
    fn default() -> Self {
        Self {
            text: String::new(),
            started_at: Instant::now(),
            text_width: 0.0,
        }
    }
}

impl Marquee {
    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text == self.text {
            return;
        }
        self.text_width = estimate_text_width(&text);
        self.text = text;
        self.started_at = Instant::now();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.text_width = 0.0;
        self.started_at = Instant::now();
    }

    pub fn needs_scroll(&self) -> bool {
        self.text_width > VIEWPORT_WIDTH + 4.0
    }

    pub fn offset_px(&self) -> f32 {
        if !self.needs_scroll() || self.text.is_empty() {
            return 0.0;
        }
        let elapsed = self.started_at.elapsed();
        if elapsed < START_PAUSE {
            return 0.0;
        }
        let moving = (elapsed - START_PAUSE).as_secs_f32();
        let period = self.text_width + LOOP_GAP;
        if period <= 0.0 {
            return 0.0;
        }
        (moving * SPEED_PX_PER_SEC) % period
    }

    /// `make_text` should return single-line text (`Wrapping::None`).
    pub fn view<'a, Message: 'a>(
        &'a self,
        make_text: impl Fn(&str) -> Element<'a, Message>,
    ) -> Element<'a, Message> {
        if self.text.is_empty() {
            return widget::container(space::horizontal())
                .width(Length::Fixed(VIEWPORT_WIDTH))
                .height(Length::Fixed(LINE_HEIGHT))
                .into();
        }

        if !self.needs_scroll() {
            return widget::container(make_text(&self.text))
                .width(Length::Fixed(VIEWPORT_WIDTH))
                .height(Length::Fixed(LINE_HEIGHT))
                .align_y(cosmic::iced::Alignment::Center)
                .clip(true)
                .into();
        }

        let offset = self.offset_px();
        let gap = space::horizontal().width(Length::Fixed(LOOP_GAP));
        let row = widget::row::with_capacity(3)
            .align_y(cosmic::iced::Alignment::Center)
            .push(make_text(&self.text))
            .push(gap)
            .push(make_text(&self.text));

        widget::container(row)
            .width(Length::Fixed(VIEWPORT_WIDTH))
            .height(Length::Fixed(LINE_HEIGHT))
            .align_y(cosmic::iced::Alignment::Center)
            .clip(true)
            .padding(Padding {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: -offset,
            })
            .into()
    }
}

fn estimate_text_width(text: &str) -> f32 {
    let mut w = 0.0_f32;
    for ch in text.chars() {
        w += if ch.is_ascii() {
            AVG_CHAR_WIDTH
        } else {
            AVG_CHAR_WIDTH * 1.35
        };
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_does_not_scroll() {
        let mut m = Marquee::default();
        m.set_text("Hi");
        assert!(!m.needs_scroll());
        assert_eq!(m.offset_px(), 0.0);
    }

    #[test]
    fn long_text_scrolls_after_pause() {
        let mut m = Marquee::default();
        m.set_text("A very long song title — Some Artist Name That Overflows");
        assert!(m.needs_scroll());
        assert_eq!(m.offset_px(), 0.0);
    }
}
