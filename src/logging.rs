// SPDX-License-Identifier: MIT

//! Process-wide structured logging configuration.

use std::{fmt, panic};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

pub const DEFAULT_FILTER: &str = "warn,cosmictify=info";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSink {
    Journald,
    Stderr,
}

impl fmt::Display for LogSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journald => f.write_str("journald"),
            Self::Stderr => f.write_str("stderr"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitError;

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a global logging subscriber is already initialized")
    }
}

impl std::error::Error for InitError {}

pub fn init() -> Result<LogSink, InitError> {
    let requested_filter = std::env::var("RUST_LOG").ok();

    let sink = match tracing_journald::layer() {
        Ok(journald) => {
            tracing_subscriber::registry()
                .with(build_filter(requested_filter.as_deref()))
                .with(journald)
                .try_init()
                .map_err(|_| InitError)?;
            LogSink::Journald
        }
        Err(_) => {
            tracing_subscriber::registry()
                .with(build_filter(requested_filter.as_deref()))
                .with(
                    tracing_subscriber::fmt::layer()
                        .compact()
                        .with_target(true)
                        .with_writer(std::io::stderr),
                )
                .try_init()
                .map_err(|_| InitError)?;
            LogSink::Stderr
        }
    };

    install_panic_hook();
    Ok(sink)
}

pub(crate) fn build_filter(requested: Option<&str>) -> EnvFilter {
    requested
        .filter(|filter| !filter.trim().is_empty())
        .and_then(|filter| EnvFilter::try_new(filter).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_FILTER))
}

fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");

        if let Some(location) = panic_info.location() {
            tracing::error!(
                thread = thread_name,
                file = location.file(),
                line = location.line(),
                column = location.column(),
                "application panicked"
            );
        } else {
            tracing::error!(thread = thread_name, "application panicked");
        }

        previous(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::{build_filter, DEFAULT_FILTER};

    #[test]
    fn default_filter_is_quiet_for_dependencies() {
        assert_eq!(DEFAULT_FILTER, "warn,cosmictify=info");
        assert_eq!(build_filter(None).to_string(), "cosmictify=info,warn");
    }

    #[test]
    fn runtime_filter_uses_valid_rust_log() {
        let filter = build_filter(Some("cosmictify=debug"));

        assert_eq!(filter.to_string(), "cosmictify=debug");
    }

    #[test]
    fn runtime_filter_falls_back_for_invalid_rust_log() {
        let filter = build_filter(Some("cosmictify=[invalid"));

        assert_eq!(filter.to_string(), "cosmictify=info,warn");
    }
}
