// SPDX-License-Identifier: MIT

//! Download album art for iced image handles.

use cosmic::iced::widget::image::Handle;

pub async fn load_art(url: String) -> Option<Handle> {
    let bytes = tokio::task::spawn_blocking(move || fetch_bytes_blocking(&url))
        .await
        .ok()??;
    Some(Handle::from_bytes(bytes))
}

fn fetch_bytes_blocking(url: &str) -> Option<Vec<u8>> {
    // Lightweight blocking GET without pulling async reqwest into applet runtime paths.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(10))
        .build();
    let response = agent.get(url).call().ok()?;
    let mut buf = Vec::new();
    response.into_reader().read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}
