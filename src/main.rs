// SPDX-License-Identifier: MIT

mod app;
mod art;
mod config;
mod i18n;
mod logging;
mod marquee;
mod mpris;
mod spotify;

fn main() -> cosmic::iced::Result {
    match logging::init() {
        Ok(sink) => tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            pid = std::process::id(),
            sink = %sink,
            "Cosmictify process started"
        ),
        Err(error) => eprintln!("failed to initialize structured logging: {error}"),
    }

    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    i18n::init(&requested_languages);
    cosmic::applet::run::<app::AppModel>(())
}
