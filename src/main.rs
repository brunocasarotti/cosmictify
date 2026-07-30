// SPDX-License-Identifier: MIT

mod app;
mod art;
mod config;
mod i18n;
mod marquee;
mod mpris;
mod spotify;

fn main() -> cosmic::iced::Result {
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    i18n::init(&requested_languages);
    cosmic::applet::run::<app::AppModel>(())
}
