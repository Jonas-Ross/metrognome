//! Library error type.
//!
//! The CLI wraps these with `anyhow` for human context; machine consumers get
//! the `kind()` discriminator, which is stable and safe to branch on.

use std::fmt;

/// Errors produced by the metrognome library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The audio bytes could not be demuxed or decoded.
    #[error("decode failed: {0}")]
    Decode(String),

    /// Decoding succeeded but produced audio unusable for analysis.
    #[error("audio unusable: {0}")]
    UnusableAudio(String),

    /// The iTunes API was reachable but returned nothing matching.
    #[error("no match for {artist} - {title}")]
    NoMatch {
        /// Artist as queried.
        artist: String,
        /// Title as queried.
        title: String,
    },

    /// A resolved track has no `previewUrl` (common for some territories).
    #[error("track {track_id} has no preview url")]
    NoPreview {
        /// iTunes store track ID.
        track_id: i64,
    },

    /// Network transport or non-success HTTP status.
    #[error("http error: {0}")]
    Http(String),

    /// The cache database could not be opened, migrated, or queried.
    #[error("cache error: {0}")]
    Cache(String),

    /// Caller passed something structurally invalid.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A bug in metrognome itself: a panicked task, a serialization failure.
    /// Never expected; present so that every emitted result object can carry a
    /// real [`ErrorKind`] rather than an invented string.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Stable, machine-readable discriminator for [`Error`].
///
/// selecta branches on this rather than parsing messages, so the strings here
/// are part of the public interface and must not change casually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// See [`Error::Decode`].
    Decode,
    /// See [`Error::UnusableAudio`].
    UnusableAudio,
    /// See [`Error::NoMatch`].
    NoMatch,
    /// See [`Error::NoPreview`].
    NoPreview,
    /// See [`Error::Http`].
    Http,
    /// See [`Error::Cache`].
    Cache,
    /// See [`Error::InvalidInput`].
    InvalidInput,
    /// See [`Error::Internal`].
    Internal,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorKind::Decode => "decode",
            ErrorKind::UnusableAudio => "unusable_audio",
            ErrorKind::NoMatch => "no_match",
            ErrorKind::NoPreview => "no_preview",
            ErrorKind::Http => "http",
            ErrorKind::Cache => "cache",
            ErrorKind::InvalidInput => "invalid_input",
            ErrorKind::Internal => "internal",
        };
        f.write_str(s)
    }
}

impl Error {
    /// The stable discriminator for this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::Decode(_) => ErrorKind::Decode,
            Error::UnusableAudio(_) => ErrorKind::UnusableAudio,
            Error::NoMatch { .. } => ErrorKind::NoMatch,
            Error::NoPreview { .. } => ErrorKind::NoPreview,
            Error::Http(_) => ErrorKind::Http,
            Error::Cache(_) => ErrorKind::Cache,
            Error::InvalidInput(_) => ErrorKind::InvalidInput,
            Error::Internal(_) => ErrorKind::Internal,
        }
    }
}

/// Convenience alias for library results.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_are_snake_case_and_stable() {
        assert_eq!(ErrorKind::NoMatch.to_string(), "no_match");
        assert_eq!(ErrorKind::UnusableAudio.to_string(), "unusable_audio");
        assert_eq!(
            Error::NoPreview { track_id: 7 }.kind(),
            ErrorKind::NoPreview
        );
    }
}
