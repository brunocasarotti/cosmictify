// SPDX-License-Identifier: MIT

//! Download album art for iced image handles.

use cosmic::iced::widget::image::Handle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtLoadError {
    Worker,
    Request,
    Read,
    EmptyBody,
}

impl std::fmt::Display for ArtLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Worker => f.write_str("artwork worker failed"),
            Self::Request => f.write_str("artwork request failed"),
            Self::Read => f.write_str("artwork response read failed"),
            Self::EmptyBody => f.write_str("artwork response was empty"),
        }
    }
}

impl std::error::Error for ArtLoadError {}

pub async fn load_art(url: String) -> Result<Handle, ArtLoadError> {
    let bytes = tokio::task::spawn_blocking(move || fetch_bytes_blocking(&url))
        .await
        .map_err(|_| ArtLoadError::Worker)??;
    handle_from_bytes(bytes)
}

fn fetch_bytes_blocking(url: &str) -> Result<Vec<u8>, ArtLoadError> {
    // Lightweight blocking GET without pulling async reqwest into applet runtime paths.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout_read(std::time::Duration::from_secs(10))
        .build();
    let response = agent.get(url).call().map_err(|_| ArtLoadError::Request)?;
    let mut buf = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut buf)
        .map_err(|_| ArtLoadError::Read)?;
    Ok(buf)
}

fn handle_from_bytes(bytes: Vec<u8>) -> Result<Handle, ArtLoadError> {
    if bytes.is_empty() {
        Err(ArtLoadError::EmptyBody)
    } else {
        Ok(Handle::from_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::{handle_from_bytes, ArtLoadError};

    #[test]
    fn empty_artwork_body_is_rejected() {
        assert_eq!(handle_from_bytes(Vec::new()), Err(ArtLoadError::EmptyBody));
    }

    #[test]
    fn artwork_errors_do_not_contain_request_urls() {
        for error in [
            ArtLoadError::Worker,
            ArtLoadError::Request,
            ArtLoadError::Read,
            ArtLoadError::EmptyBody,
        ] {
            assert!(!error.to_string().contains("http"));
        }
    }
}
