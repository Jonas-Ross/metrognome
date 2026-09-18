//! The JSON contract with selecta.
//!
//! These types are the interface, not an implementation detail. Two rules
//! follow from that:
//!
//! - Every emitted object carries [`SCHEMA_VERSION`], so a consumer can refuse
//!   a payload it does not understand instead of misreading it.
//! - Every feature carries its own `source` and `confidence`, and flags itself
//!   `uncertain` rather than presenting a guess as a fact. New features are
//!   added as new optional fields on [`Features`]; existing fields keep their
//!   meaning.

use serde::{Deserialize, Serialize};

/// Version of the JSON objects this binary emits.
///
/// Bumped only for breaking changes — adding an optional field is not one.
pub const SCHEMA_VERSION: u32 = 1;

/// Confidence at or below which a feature flags itself uncertain.
///
/// Set where a 30-second preview of a beatless intro lands: the consumer should
/// treat anything under this as a hint, not a measurement.
pub const UNCERTAIN_AT_OR_BELOW: f32 = 0.5;

/// One alternate reading of a feature, with how it relates to the chosen one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Alternate {
    /// The alternate value, in the same units as the chosen one.
    pub value: f64,
    /// Relation to the chosen value, e.g. `"half"`, `"double"`, `"relative_minor"`.
    pub relation: String,
    /// Raw score, comparable only against other alternates in the same list.
    pub score: f32,
}

/// Tempo in beats per minute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TempoEstimate {
    /// Estimated tempo, folded into [`Self::canonical_window_bpm`].
    pub bpm: f32,
    /// 0-1. See [`UNCERTAIN_AT_OR_BELOW`].
    pub confidence: f32,
    /// True when the estimate should be treated as a hint.
    pub uncertain: bool,
    /// Which algorithm produced this, versioned.
    pub source: String,
    /// Offset of the first beat from the start of the analyzed audio.
    pub beat_offset_secs: f32,
    /// The one-octave window the reported tempo is folded into.
    pub canonical_window_bpm: [f32; 2],
    /// Other plausible tempos, best first. Always includes the half and double
    /// of the chosen tempo so a consumer can override the fold.
    pub alternates: Vec<Alternate>,
}

/// Musical key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KeyEstimate {
    /// Standard notation, e.g. `"F minor"`.
    pub key: String,
    /// Tonic pitch class name, e.g. `"F"`.
    pub tonic: String,
    /// `"major"` or `"minor"`.
    pub mode: String,
    /// Camelot wheel position, e.g. `"4A"`.
    pub camelot: String,
    /// 0-1. See [`UNCERTAIN_AT_OR_BELOW`].
    pub confidence: f32,
    /// True when the estimate should be treated as a hint.
    pub uncertain: bool,
    /// Which algorithm and profile set produced this, versioned.
    pub source: String,
    /// Other plausible keys, best first.
    pub alternates: Vec<Alternate>,
}

/// Audio-derived features.
///
/// Deliberately a struct of options rather than a fixed pair: metrognome is
/// meant to grow other measurements (energy, spectral balance, loudness), and
/// each arrives as a new optional field without moving the existing ones.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Features {
    /// Tempo, when it could be estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempo: Option<TempoEstimate>,
    /// Key, when it could be estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyEstimate>,
}

/// Properties of the audio that was analyzed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioInfo {
    /// Duration in seconds.
    pub duration_secs: f64,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count before the mono downmix.
    pub source_channels: u16,
    /// Fraction of samples below -60 dBFS. A high value on a preview usually
    /// means the clip is an intro or an outro rather than the body of a track.
    pub silent_fraction: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_omit_absent_fields() {
        let json = serde_json::to_string(&Features::default()).unwrap();
        assert_eq!(json, "{}");
    }
}
